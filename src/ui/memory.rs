//! "Memory" tab (Ubuntu / Linux Mint): the low-memory warning, saved to the
//! config with the Save button, and the Magic `SysRq` keys, a system setting
//! applied right away through the desktop's own password prompt.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use eframe::egui::{self, RichText};

use crate::config::MemoryWatch;
use crate::memory::{self, sysrq};

#[derive(Debug)]
pub struct MemoryTab {
    /// What the kernel uses right now, or why it couldn't be read.
    current: Result<u32, String>,
    /// What appkey saved to its sysctl.d file earlier.
    saved: Option<u32>,
    choice: u32,
    applying: Option<Receiver<Result<(), String>>>,
    status: Option<(String, bool)>,
    open_error: Option<String>,
}

impl MemoryTab {
    pub fn new() -> Self {
        let current = sysrq::read_current().map_err(|e| format!("{e:#}"));
        let saved = sysrq::read_saved();
        let now = current.as_ref().ok().copied();
        // Suggest turning on Alt+SysRq+F if it's off.
        let choice = match now {
            Some(v) if sysrq::allows(v, sysrq::SIGNAL) => v,
            _ => 240,
        };
        Self { current, saved, choice, applying: None, status: None, open_error: None }
    }

    pub fn show(&mut self, ui: &mut egui::Ui, watch: &mut MemoryWatch) {
        self.poll(ui.ctx());
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            if warning_section(ui, watch, self.open_error.as_deref()) {
                self.open_error = memory::show_window().err().map(|e| format!("{e:#}"));
            }
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            self.sysrq_section(ui);
        });
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.applying else { return };
        let result = match rx.try_recv() {
            Ok(r) => r,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(200));
                return;
            }
            Err(TryRecvError::Disconnected) => Err("the change was interrupted".into()),
        };
        self.applying = None;
        self.current = sysrq::read_current().map_err(|e| format!("{e:#}"));
        self.saved = sysrq::read_saved();
        self.status = Some(match result {
            Ok(()) => (format!("Done. SysRq is now {}.", self.choice), false),
            Err(e) => (format!("Not changed: {e}"), true),
        });
    }

    fn sysrq_section(&mut self, ui: &mut egui::Ui) {
        sysrq_keys(ui);
        match &self.current {
            Ok(v) => {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(format!("Right now: {v}")).strong());
                    ui.label(format!("({})", sysrq::describe(*v)));
                });
                if !sysrq::allows(*v, sysrq::SIGNAL) {
                    ui.label(
                        RichText::new("Alt+SysRq+F is turned off, so it does nothing yet.")
                            .color(ui.visuals().warn_fg_color),
                    );
                }
                if let Some(s) = self.saved.filter(|s| s != v) {
                    ui.label(
                        RichText::new(format!("{s} is saved and applies after a restart.")).weak(),
                    );
                }
            }
            Err(e) => {
                ui.label(RichText::new(format!("Can't read the current setting: {e}")).weak());
            }
        }
        ui.add_space(6.0);

        ui.label("Allow from the keyboard:");
        let busy = self.applying.is_some();
        ui.add_enabled_ui(!busy, |ui| {
            for p in sysrq::PRESETS {
                ui.radio_value(&mut self.choice, p.value, p.label);
            }
        });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let unchanged =
                self.current.as_ref().ok() == Some(&self.choice) && self.saved == Some(self.choice);
            let button =
                egui::Button::new(if busy { "Waiting for password…" } else { "Apply…" });
            if ui
                .add_enabled(!busy && !unchanged, button)
                .on_hover_text(
                    "Saves it in /etc/sysctl.d and applies it now. Asks for your password.",
                )
                .clicked()
            {
                let (tx, rx) = std::sync::mpsc::channel();
                let value = self.choice;
                std::thread::spawn(move || {
                    let _ = tx.send(sysrq::apply(value).map_err(|e| format!("{e:#}")));
                });
                self.applying = Some(rx);
                self.status = None;
            }
            if let Some((msg, err)) = &self.status {
                let color =
                    if *err { ui.visuals().error_fg_color } else { ui.visuals().weak_text_color() };
                ui.label(RichText::new(msg).color(color));
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Or run this in a terminal:").weak());
        ui.horizontal(|ui| {
            let cmd = sysrq::terminal_command(self.choice);
            ui.add(egui::Label::new(RichText::new(&cmd).monospace().small()).wrap());
            if ui.small_button("Copy").clicked() {
                ui.ctx().copy_text(cmd);
            }
        });
        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Why it's limited by default: on a shared computer, someone at a locked screen \
                 could close the lock screen, and Alt+PrtSc is also the window-screenshot key, \
                 so a slip could close programs. On a computer only you use, 240 is a good \
                 balance: it adds closing programs but keeps the debug dumps off.",
            )
            .weak()
            .small(),
        );
    }
}

fn sysrq_keys(ui: &mut egui::Ui) {
    ui.heading("Emergency keys (Magic SysRq)");
    ui.label(
        "These keys go straight to the Linux kernel, so they work even when the desktop \
         is frozen and the mouse doesn't move. Hold Alt and PrtSc (SysRq), then press:",
    );
    ui.add_space(4.0);
    egui::Grid::new("sysrq-keys").num_columns(2).spacing([14.0, 4.0]).show(ui, |ui| {
        for (keys, what) in [
            ("F", "Close the program using the most memory. Press once and wait a few seconds."),
            ("R E I S U B", "Safe restart when nothing else helps: press slowly, 2 s apart."),
            ("H", "Test: only writes help to the kernel log (sudo dmesg | tail)."),
        ] {
            ui.label(RichText::new(keys).monospace().strong());
            ui.label(what);
            ui.end_row();
        }
    });
    ui.label(RichText::new("On laptops you may need Fn as well to reach PrtSc.").weak().small());
    ui.add_space(8.0);
}

/// Returns `true` when "Show memory use now" was clicked.
fn warning_section(ui: &mut egui::Ui, watch: &mut MemoryWatch, open_error: Option<&str>) -> bool {
    let mut open = false;
    ui.heading("Low-memory warning");
    ui.checkbox(&mut watch.enabled, "Warn me before the computer runs out of memory");
    ui.label(
        RichText::new(
            "appkey then shows the programs using the most memory and lets you close them, \
             before the system starts to freeze.",
        )
        .weak(),
    );
    ui.add_space(4.0);
    ui.add_enabled_ui(watch.enabled, |ui| {
        egui::Grid::new("memory-thresholds").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
            ui.label("When free memory drops below");
            ui.add(
                egui::Slider::new(&mut watch.min_available_percent, MemoryWatch::AVAILABLE_RANGE)
                    .suffix(" %"),
            );
            ui.end_row();
            ui.label("or programs wait on memory for");
            ui.add(
                egui::Slider::new(&mut watch.max_pressure_percent, MemoryWatch::PRESSURE_RANGE)
                    .suffix(" % of the time")
                    .custom_formatter(
                        |v, _| if v == 0.0 { "never".into() } else { format!("{v:.0}") },
                    ),
            )
            .on_hover_text(
                "Linux pressure stall information, averaged over 10 seconds. 0 turns this off.",
            );
            ui.end_row();
        });
    });
    ui.add_space(6.0);
    let mut busy = watch.busy_background_minutes > 0;
    if ui
        .checkbox(
            &mut busy,
            "Also warn when a program left running in the background keeps the CPU busy",
        )
        .on_hover_text(
            "For example a dev server or script whose terminal or Claude session has closed. \
             Programs with a window don't count.",
        )
        .changed()
    {
        watch.busy_background_minutes = if busy { 10 } else { 0 };
    }
    if busy {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            ui.label("for at least");
            ui.add(
                egui::Slider::new(&mut watch.busy_background_minutes, MemoryWatch::BUSY_RANGE)
                    .suffix(" min"),
            );
        });
    }
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "Tip: give the memory window its own shortcut. In Shortcuts, choose app → \
             appkey → Memory window.",
        )
        .weak(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        open = ui.button("Show memory use now…").clicked();
        if let Some(e) = open_error {
            ui.label(RichText::new(e).color(ui.visuals().error_fg_color));
        } else if let Some(s) = memory::read_snapshot() {
            ui.label(RichText::new(s.summary()).weak());
        }
    });
    open
}

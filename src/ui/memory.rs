//! "Memory" tab (Ubuntu / Linux Mint): the low-memory warning and the memory limit
//! for apps appkey starts, saved to the config with the Save button, and the Magic
//! `SysRq` keys, a system setting applied right away through the desktop's own
//! password prompt.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};

use crate::config::{LimitSize, MemoryLimit, MemoryWatch};
use crate::memory::{self, CgroupMemory, sysrq};

#[derive(Debug)]
pub struct MemoryTab {
    /// What the kernel uses right now, or why it couldn't be read.
    current: Result<u32, String>,
    /// What appkey saved to its sysctl.d file earlier.
    saved: Option<u32>,
    choice: u32,
    applying: Option<Receiver<Result<(), String>>>,
    status: Option<(String, bool)>,
    /// What the capped apps use now (`None`: none started yet), re-read every second.
    slice: (Instant, Option<CgroupMemory>),
    total_bytes: u64,
    swap_bytes: u64,
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
        let mem = memory::read_snapshot().map(|s| s.mem);
        let total_bytes = mem.map_or(0, |m| m.total_kib * 1024);
        let swap_bytes = mem.map_or(0, |m| m.swap_total_kib * 1024);
        Self {
            current,
            saved,
            choice,
            applying: None,
            status: None,
            slice: (Instant::now(), read_slice()),
            total_bytes,
            swap_bytes,
        }
    }

    /// Returns `true` when "Show memory use now" was clicked.
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        watch: &mut MemoryWatch,
        limit: &mut MemoryLimit,
    ) -> bool {
        self.poll(ui.ctx());
        let mut open = false;
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            open = warning_section(ui, watch);
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            self.limit_section(ui, limit);
            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);
            self.sysrq_section(ui);
        });
        open
    }

    fn limit_section(&mut self, ui: &mut egui::Ui, limit: &mut MemoryLimit) {
        ui.heading("Memory limit for apps started by appkey");
        ui.checkbox(
            &mut limit.enabled,
            "Limit how much memory apps opened with appkey's shortcuts use together",
        );
        ui.label(
            RichText::new(
                "They share one limit, with everything they start (an IDE's builds, a \
                 browser's tabs), and never use more RAM than that: the rest stays free for \
                 the desktop, appkey and everything else. Near the limit Linux slows them \
                 down and moves what they don't use to swap, so you have time to close \
                 something. Only when their share of swap is full too does Linux end the \
                 biggest of them.",
            )
            .weak(),
        );
        ui.add_space(4.0);
        let (total, swap) = (self.total_bytes, self.swap_bytes);
        ui.add_enabled_ui(limit.enabled, |ui| limit_pickers(ui, limit, total, swap));
        ui.add_space(4.0);
        if self.slice.0.elapsed() >= Duration::from_secs(1) {
            self.slice = (Instant::now(), read_slice());
        }
        ui.ctx().request_repaint_after(Duration::from_secs(1));
        let now = match self.slice.1 {
            Some(CgroupMemory { current: Some(used), max, swap: in_swap, .. }) => {
                let cap =
                    max.map_or_else(|| "no limit now".into(), |m| format!("limit {:.1} GB", gb(m)));
                let in_swap = in_swap
                    .filter(|&b| b >= 1 << 20)
                    .map_or_else(String::new, |b| format!(" + {:.1} GB in swap", gb(b)));
                format!("Apps started with the limit use {:.1} GB now{in_swap} ({cap}).", gb(used))
            }
            _ => "No app has been started with the limit yet.".into(),
        };
        ui.label(RichText::new(now).weak());
        ui.label(
            RichText::new(
                "Applies to apps appkey starts from now on, after you save. Not limited: apps \
                 opened from the menu or already running, and snaps and Flatpaks (they move \
                 themselves into their own group). Group by cgroup on the Processes tab to \
                 see which are in it.",
            )
            .weak()
            .small(),
        );
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

/// The RAM limit and the swap allowance, and what they come to here.
fn limit_pickers(ui: &mut egui::Ui, limit: &mut MemoryLimit, total: u64, swap: u64) {
    egui::Grid::new("memory-limit").num_columns(2).spacing([10.0, 6.0]).show(ui, |ui| {
        ui.label("Memory (RAM)");
        let max_gb = (gb(total) * 0.95).max(1.0);
        size_picker(ui, "ram", &mut limit.limit, MemoryLimit::PERCENT_RANGE, 1.0..=max_gb, total);
        ui.end_row();
        ui.label("");
        if total > 0 {
            let cap = memory::cgroup::cap_properties(
                limit.limit.bytes(total),
                limit.swap.bytes(swap),
                total,
            );
            ui.label(
                RichText::new(format!(
                    "= {:.1} GB of {:.1} GB, never more. Slows down from {:.1} GB.",
                    gb(cap.max),
                    gb(total),
                    gb(cap.high)
                ))
                .weak(),
            );
        }
        ui.end_row();
        ui.label("Then swap, up to");
        if swap == 0 {
            ui.label(RichText::new("This computer has no swap.").weak());
        } else {
            size_picker(ui, "swap", &mut limit.swap, 0..=100, 0.0..=gb(swap).max(0.1), swap);
            ui.end_row();
            ui.label("");
            ui.label(
                RichText::new(format!(
                    "= {:.1} GB of {:.1} GB of swap, then Linux ends the biggest of them.",
                    gb(limit.swap.bytes(swap).min(swap)),
                    gb(swap)
                ))
                .weak(),
            );
        }
        ui.end_row();
    });
}

/// A share of `total` (a % slider) or a fixed size (GB).
fn size_picker(
    ui: &mut egui::Ui,
    id: &str,
    size: &mut LimitSize,
    percent_range: std::ops::RangeInclusive<u8>,
    gb_range: std::ops::RangeInclusive<f64>,
    total: u64,
) {
    ui.horizontal(|ui| {
        ui.push_id(id, |ui| {
            let mut percent = matches!(size, LimitSize::Percent(_));
            if ui.radio_value(&mut percent, true, "Share").clicked()
                && let LimitSize::Mib(_) = size
            {
                let p = size.bytes(total).saturating_mul(100).checked_div(total).unwrap_or(0);
                let (lo, hi) = (*percent_range.start(), *percent_range.end());
                #[allow(clippy::cast_possible_truncation)]
                let p = p.clamp(u64::from(lo), u64::from(hi)) as u8;
                *size = LimitSize::Percent(p);
            }
            if ui.radio_value(&mut percent, false, "Fixed size").clicked()
                && let LimitSize::Percent(_) = size
            {
                // Whole half-GBs read better.
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                let lo = (gb_range.start() * 1024.0) as u64;
                let mib = (size.bytes(total) / (1 << 20) / 512 * 512).max(lo);
                *size = LimitSize::Mib(mib);
            }
            match size {
                LimitSize::Percent(p) => {
                    ui.add(egui::Slider::new(p, percent_range).suffix(" %"));
                }
                LimitSize::Mib(mib) => {
                    #[allow(clippy::cast_precision_loss)]
                    let mut g = *mib as f64 / 1024.0;
                    if ui
                        .add(
                            egui::DragValue::new(&mut g)
                                .range(gb_range)
                                .speed(0.1)
                                .max_decimals(1)
                                .suffix(" GB"),
                        )
                        .changed()
                    {
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        {
                            *mib = ((g * 10.0).round() * 102.4).round() as u64;
                        }
                    }
                }
            }
        });
    });
}

fn gb(bytes: u64) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let b = bytes as f64;
    b / f64::from(1u32 << 30)
}

fn read_slice() -> Option<CgroupMemory> {
    memory::cap_cgroup().map(|p| memory::read_cgroup_memory(&p))
}

/// Returns `true` when "Show memory use now" was clicked.
fn warning_section(ui: &mut egui::Ui, watch: &mut MemoryWatch) -> bool {
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
            "Tip: give the Processes tab its own shortcut. In Shortcuts, choose app → \
             appkey → Memory window.",
        )
        .weak(),
    );
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        open = ui.button("Show memory use now").clicked();
        if let Some(s) = memory::read_snapshot() {
            ui.label(RichText::new(s.summary()).weak());
        }
    });
    open
}

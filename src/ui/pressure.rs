//! `appkey memory [--alert]`: the user's biggest programs, with buttons to close
//! them. The daemon opens it with `--alert` when memory runs low. Closing sends
//! `SIGTERM` so programs can save; "Force quit" (`SIGKILL`) is offered when one
//! doesn't react.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui::{self, Button, RichText};

use crate::config::{self, MemoryWatch};
use crate::memory::{self, Group, Snapshot, sysrq};

const REFRESH: Duration = Duration::from_secs(2);
/// How long a program gets to quit before "Force quit" appears.
const GRACE: Duration = Duration::from_secs(5);
const ROWS: usize = 10;
/// Smaller programs wouldn't free enough to matter.
const MIN_KIB: u64 = 20 * 1024;

/// Returns `true` if the user snoozed the warning.
pub fn run(alert: bool) -> Result<bool> {
    let snoozed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = snoozed.clone();
    let title = if alert { "appkey: memory is running low" } else { "appkey: memory use" };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_app_id("appkey")
            .with_inner_size([560.0, 460.0])
            .with_min_inner_size([440.0, 320.0])
            .with_window_level(if alert {
                egui::WindowLevel::AlwaysOnTop
            } else {
                egui::WindowLevel::Normal
            }),
        ..Default::default()
    };
    eframe::run_native(
        "appkey-memory",
        options,
        Box::new(move |cc| {
            super::font::install(&cc.egui_ctx)?;
            Ok(Box::new(Dialog::new(alert, flag)))
        }),
    )
    .map_err(|e| anyhow!("could not open the memory window: {e}"))?;
    Ok(snoozed.load(std::sync::atomic::Ordering::SeqCst))
}

#[derive(Debug)]
struct Dialog {
    alert: bool,
    snoozed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    watch: MemoryWatch,
    uid: u32,
    snapshot: Option<Snapshot>,
    groups: Vec<Group>,
    refreshed: Instant,
    selected: HashSet<String>,
    /// Programs we asked to quit, and when.
    closing: HashMap<String, Instant>,
    status: Option<String>,
    sysrq_f: bool,
    #[cfg(debug_assertions)]
    devshot: Option<super::devshot::DevShot>,
}

impl Dialog {
    fn new(alert: bool, snoozed: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        let watch = config::path()
            .ok()
            .and_then(|p| config::read_document(&p).ok())
            .and_then(|d| d.memory_watch)
            .unwrap_or_default();
        let mut d = Self {
            alert,
            snoozed,
            watch,
            uid: memory::current_uid(),
            snapshot: None,
            groups: Vec::new(),
            refreshed: Instant::now(),
            selected: HashSet::new(),
            closing: HashMap::new(),
            status: None,
            sysrq_f: sysrq::read_current().is_ok_and(|v| sysrq::allows(v, sysrq::SIGNAL)),
            #[cfg(debug_assertions)]
            devshot: super::devshot::DevShot::from_env(),
        };
        d.refresh();
        d
    }

    fn refresh(&mut self) {
        self.refreshed = Instant::now();
        self.snapshot = memory::read_snapshot();
        self.groups = memory::group(&memory::read_processes(), self.uid, std::process::id(), ROWS);
        self.groups.retain(|g| g.rss_kib >= MIN_KIB);
        let names: HashSet<&String> = self.groups.iter().map(|g| &g.name).collect();
        self.selected.retain(|n| names.contains(n));
        self.closing.retain(|n, _| names.contains(n));
    }

    fn close(&mut self, names: &[String], force: bool) {
        let mut count = 0;
        for g in self.groups.iter().filter(|g| names.contains(&g.name)) {
            count += memory::signal(&g.pids, &g.name, force);
            if !force {
                self.closing.insert(g.name.clone(), Instant::now());
            }
        }
        self.selected.retain(|n| !names.contains(n));
        let verb = if force { "Force-quit" } else { "Asked to quit:" };
        self.status = Some(format!("{verb} {} ({count} process(es)).", names.join(", ")));
        self.refresh();
    }

    fn header(&self, ui: &mut egui::Ui) {
        let Some(s) = &self.snapshot else {
            ui.label("Couldn't read memory use from /proc.");
            return;
        };
        let stressed = s.is_stressed(self.watch);
        let headline = match (self.alert, stressed) {
            (true, true) => "Your computer is running low on memory",
            (true, false) => "Memory is back to normal",
            (false, _) => "Programs using the most memory",
        };
        ui.heading(headline);
        ui.label(s.summary());
        if self.alert && stressed {
            ui.label(
                "Close a program you don't need, or the system may slow down and freeze. \
                 Programs get the chance to save first.",
            );
        }
    }

    fn table(&mut self, ui: &mut egui::Ui) -> Option<String> {
        let mut force = None;
        let max = self.groups.first().map_or(1, |g| g.rss_kib.max(1));
        egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(260.0).show(ui, |ui| {
            egui::Grid::new("hogs").num_columns(4).spacing([10.0, 6.0]).striped(true).show(
                ui,
                |ui| {
                    for g in &self.groups {
                        let mut on = self.selected.contains(&g.name);
                        let label = if g.pids.len() > 1 {
                            format!("{}  ({} processes)", g.name, g.pids.len())
                        } else {
                            g.name.clone()
                        };
                        if ui.checkbox(&mut on, label).changed() {
                            if on {
                                self.selected.insert(g.name.clone());
                            } else {
                                self.selected.remove(&g.name);
                            }
                        }
                        ui.label(memory::format_kib(g.rss_kib));
                        #[allow(clippy::cast_precision_loss)]
                        let share = g.rss_kib as f32 / max as f32;
                        ui.add(egui::ProgressBar::new(share).desired_width(120.0));
                        match self.closing.get(&g.name) {
                            Some(t) if t.elapsed() >= GRACE => {
                                if ui
                                    .small_button("Force quit")
                                    .on_hover_text("It didn't quit. Unsaved work in it is lost.")
                                    .clicked()
                                {
                                    force = Some(g.name.clone());
                                }
                            }
                            Some(_) => {
                                ui.label(RichText::new("closing…").weak());
                            }
                            None => {
                                ui.label("");
                            }
                        }
                        ui.end_row();
                    }
                },
            );
        });
        force
    }

    fn footer(&mut self, ui: &mut egui::Ui) {
        let tip = if self.sysrq_f {
            "If the desktop freezes before you can react: hold Alt+PrtSc and press F."
        } else {
            "Emergency key Alt+PrtSc+F is off. Turn it on in appkey settings → Memory."
        };
        ui.label(RichText::new(tip).weak().small());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let names: Vec<String> = self.selected.iter().cloned().collect();
            if ui
                .add_enabled(!names.is_empty(), Button::new("Close selected"))
                .on_hover_text("Asks the programs to quit, like closing their windows.")
                .clicked()
            {
                self.close(&names, false);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button(if self.alert { "Dismiss" } else { "Close" }).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if self.alert && ui.button("Snooze for an hour").clicked() {
                    self.snoozed.store(true, std::sync::atomic::Ordering::SeqCst);
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        if let Some(s) = &self.status {
            ui.label(RichText::new(s).weak());
        }
    }
}

impl eframe::App for Dialog {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(debug_assertions)]
        if let Some(shot) = &mut self.devshot {
            shot.tick(&ctx);
        }
        if self.refreshed.elapsed() >= REFRESH {
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(500));

        egui::Panel::bottom("memory-actions").show(ui, |ui| {
            ui.add_space(6.0);
            self.footer(ui);
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.header(ui);
            ui.add_space(8.0);
            if self.groups.is_empty() {
                ui.label("None of your programs is using much memory.");
            } else if let Some(name) = self.table(ui) {
                self.close(&[name], true);
            }
        });
    }
}

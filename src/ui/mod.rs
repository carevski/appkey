//! Settings window (`appkey settings`). Runs as its own process, so its event
//! loop never competes with the tray daemon's. It only edits the config file;
//! the daemon notices the change and reloads.

#[cfg(debug_assertions)]
mod devshot;
mod font;
mod keys;
#[cfg(target_os = "linux")]
mod memory;
mod picker;
#[cfg(target_os = "linux")]
mod pressure;

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use eframe::egui::{self, Button, Color32, RichText};

use crate::config::{self, Document, Entry, MemoryWatch};
use crate::platform::{self, AppInfo};
use keys::Recorded;
use picker::Picker;

pub fn run(config_path: PathBuf) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("appkey settings")
            .with_app_id("appkey")
            .with_inner_size([660.0, 560.0])
            .with_min_inner_size([520.0, 300.0]),
        ..Default::default()
    };
    eframe::run_native(
        "appkey",
        options,
        Box::new(move |cc| {
            font::install(&cc.egui_ctx)?;
            Ok(Box::new(Settings::load(config_path)))
        }),
    )
    .map_err(|e| anyhow!("could not open the settings window: {e}"))
}

#[cfg(target_os = "linux")]
pub use pressure::Mode as MemoryMode;

/// `appkey memory [--alert | --busy]`. Returns `true` if the user snoozed the warning.
#[cfg(target_os = "linux")]
pub fn memory_window(mode: MemoryMode) -> Result<bool> {
    pressure::run(mode)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Shortcuts,
    #[cfg(target_os = "linux")]
    Memory,
}

/// One editable shortcut (an [`Entry`] for this OS).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Row {
    keys: String,
    name: String,
    app: String,
    launch: Option<Vec<String>>,
    os: Option<Vec<String>>,
}

impl Row {
    fn from_entry(e: Entry) -> Self {
        Self {
            name: e.name.unwrap_or_else(|| e.app.clone()),
            keys: e.keys,
            app: e.app,
            launch: e.launch,
            os: e.os,
        }
    }

    fn to_entry(&self) -> Entry {
        let name = self.name.trim();
        Entry {
            keys: self.keys.clone(),
            name: (!name.is_empty() && name != self.app).then(|| name.to_owned()),
            app: self.app.clone(),
            launch: self.launch.clone(),
            os: self.os.clone(),
        }
    }

    fn app_info(&self) -> AppInfo {
        AppInfo { name: self.name.clone(), app: self.app.clone(), launch: self.launch.clone() }
    }
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // independent UI state flags
struct Settings {
    path: PathBuf,
    tab: Tab,
    rows: Vec<Row>,
    saved: Vec<Row>,
    /// Entries for other OSes: not shown, written back untouched.
    others: Vec<Entry>,
    memory_watch: MemoryWatch,
    saved_memory_watch: MemoryWatch,
    /// Whether the file has a `[memory_watch]` section (we don't add one for defaults).
    had_memory_watch: bool,
    /// `None` where the Memory tab isn't offered (not Ubuntu / Mint).
    #[cfg(target_os = "linux")]
    memory_tab: Option<memory::MemoryTab>,
    /// Set when the file couldn't be read; saving is disabled so we never clobber it.
    load_error: Option<String>,
    recording: Option<usize>,
    /// Combos the desktop uses (`HotKey` id, why), to warn while editing.
    desktop_shortcuts: Vec<(u32, String)>,
    /// Super / Win key held (egui reports it only on macOS); see [`keys::super_after`].
    super_down: bool,
    picker: Option<(usize, Picker)>,
    status: Option<(String, bool)>,
    confirm_close: bool,
    allow_close: bool,
    #[cfg(debug_assertions)]
    devshot: Option<devshot::DevShot>,
}

impl Settings {
    fn load(path: PathBuf) -> Self {
        let read = config::ensure_exists(&path).and_then(|_| config::read_document(&path));
        let (doc, load_error) = match read {
            Ok(d) => (d, None),
            Err(e) => (Document::default(), Some(format!("{e:#}"))),
        };
        let had_memory_watch = doc.memory_watch.is_some();
        let memory_watch = doc.memory_watch.unwrap_or_default();
        let (here, others): (Vec<Entry>, Vec<Entry>) =
            doc.shortcuts.into_iter().partition(Entry::applies_here);
        let rows: Vec<Row> = here.into_iter().map(Row::from_entry).collect();
        Self {
            path,
            tab: Tab::Shortcuts,
            saved: rows.clone(),
            rows,
            others,
            memory_watch,
            saved_memory_watch: memory_watch,
            had_memory_watch,
            #[cfg(target_os = "linux")]
            memory_tab: crate::memory::available().then(memory::MemoryTab::new),
            load_error,
            recording: None,
            super_down: false,
            desktop_shortcuts: platform::desktop_shortcuts(),
            picker: None,
            status: None,
            confirm_close: false,
            allow_close: false,
            #[cfg(debug_assertions)]
            devshot: devshot::DevShot::from_env(),
        }
    }

    fn dirty(&self) -> bool {
        self.rows != self.saved || self.memory_watch != self.saved_memory_watch
    }

    /// Per-row problem, if any. Saving is blocked while there are problems.
    fn problems(&self) -> Vec<Option<String>> {
        let ids: Vec<Option<u32>> = self.rows.iter().map(|r| keys::hotkey_id(&r.keys)).collect();
        self.rows
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if r.keys.is_empty() {
                    Some("Set a shortcut".into())
                } else if r.app.trim().is_empty() {
                    Some("Choose an app".into())
                } else {
                    (0..i)
                        .find(|&j| ids[j].is_some() && ids[j] == ids[i])
                        .map(|j| format!("Same shortcut as row {}", j + 1))
                }
            })
            .collect()
    }

    /// Why the desktop would keep this combo for itself, if it would.
    fn desktop_conflict(&self, id: Option<u32>) -> Option<String> {
        let id = id?;
        self.desktop_shortcuts.iter().find(|(d, _)| *d == id).map(|(_, why)| why.clone())
    }

    fn save(&mut self) -> bool {
        let mut shortcuts: Vec<Entry> = self.rows.iter().map(Row::to_entry).collect();
        let keep_watch = self.had_memory_watch || self.memory_watch != MemoryWatch::default();
        let memory_watch = keep_watch.then_some(self.memory_watch);
        let result =
            config::validate_document(Document { memory_watch, shortcuts: shortcuts.clone() })
                .and_then(|_| {
                    shortcuts.extend(self.others.iter().cloned());
                    config::save_document(&self.path, &Document { memory_watch, shortcuts })
                });
        match result {
            Ok(()) => {
                self.saved = self.rows.clone();
                self.saved_memory_watch = self.memory_watch;
                self.had_memory_watch = keep_watch;
                self.status =
                    Some(("Saved. appkey picks up the changes automatically.".into(), false));
                true
            }
            Err(e) => {
                self.status = Some((format!("Couldn't save: {e:#}"), true));
                false
            }
        }
    }

    fn shortcuts_tab(&mut self, ui: &mut egui::Ui) {
        ui.label("Press a shortcut anywhere to bring its app to the front, or open it if it isn't running.");
        ui.add_space(8.0);
        let problems = self.problems();
        // Not blocking: the desktop could free the combo, and the tray reports it anyway.
        let warnings: Vec<Option<String>> = self
            .rows
            .iter()
            .map(|r| {
                self.desktop_conflict(keys::hotkey_id(&r.keys)).map(|w| format!("Used by {w}"))
            })
            .collect();
        let mut remove = None;
        egui::ScrollArea::vertical().auto_shrink([false, true]).show(ui, |ui| {
            egui::Grid::new("shortcuts").num_columns(4).spacing([10.0, 8.0]).striped(true).show(
                ui,
                |ui| {
                    ui.label(RichText::new("Shortcut").strong());
                    ui.label(RichText::new("Application").strong());
                    ui.label("");
                    ui.label("");
                    ui.end_row();
                    let super_down = self.super_down;
                    for (i, row) in self.rows.iter_mut().enumerate() {
                        self.recording =
                            key_button(ui, i, row, self.recording, super_down, &mut self.status);
                        let app_text = if row.app.is_empty() {
                            "Choose app…".to_owned()
                        } else {
                            row.name.clone()
                        };
                        let app_btn =
                            ui.add(Button::new(app_text).min_size(egui::vec2(220.0, 0.0)));
                        if app_btn.clicked() {
                            self.picker = Some((
                                i,
                                Picker::new((!row.app.is_empty()).then(|| row.app_info()).as_ref()),
                            ));
                        }
                        if !row.app.is_empty() {
                            app_btn.on_hover_text(format!("Matches: {}", row.app));
                        }
                        if ui.button("Remove").clicked() {
                            remove = Some(i);
                        }
                        match problems[i].as_ref().or(warnings[i].as_ref()) {
                            Some(p) => ui.add(
                                egui::Label::new(
                                    RichText::new(p).color(ui.visuals().warn_fg_color),
                                )
                                .wrap(),
                            ),
                            None => ui.label(""),
                        };
                        ui.end_row();
                    }
                },
            );
            ui.add_space(8.0);
            if ui.button("+  Add shortcut").clicked() {
                self.rows
                    .push(Row { os: Some(vec![config::CURRENT_OS.to_owned()]), ..Row::default() });
                self.recording = Some(self.rows.len() - 1);
            }
        });
        if let Some(i) = remove {
            self.rows.remove(i);
            self.recording = None;
        }
    }

    fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let can_save = self.load_error.is_none()
            && self.dirty()
            && self.problems().iter().all(Option::is_none);
        ui.horizontal(|ui| {
            if let Some((msg, is_err)) = &self.status {
                let color = if *is_err {
                    ui.visuals().error_fg_color
                } else {
                    ui.visuals().weak_text_color()
                };
                ui.label(RichText::new(msg).color(color));
            } else if self.dirty() {
                ui.label(RichText::new("Unsaved changes").weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(can_save, Button::new("Save")).clicked() {
                    self.save();
                }
                if ui.add_enabled(self.dirty(), Button::new("Revert")).clicked() {
                    self.rows.clone_from(&self.saved);
                    self.memory_watch = self.saved_memory_watch;
                    self.recording = None;
                    self.status = None;
                }
            });
        });
    }

    fn handle_close(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && self.dirty() && !self.allow_close {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.confirm_close = true;
        }
        if !self.confirm_close {
            return;
        }
        egui::Modal::new(egui::Id::new("confirm-close")).show(ctx, |ui| {
            ui.label("Save your changes before closing?");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let can_save =
                    self.load_error.is_none() && self.problems().iter().all(Option::is_none);
                if ui.add_enabled(can_save, Button::new("Save")).clicked() && self.save() {
                    self.close(ctx);
                }
                if ui.button("Don't save").clicked() {
                    self.close(ctx);
                }
                if ui.button("Cancel").clicked() {
                    self.confirm_close = false;
                }
            });
        });
    }

    fn close(&mut self, ctx: &egui::Context) {
        self.confirm_close = false;
        self.allow_close = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }
}

/// The "press keys" button. Returns which row (if any) is recording afterwards.
fn key_button(
    ui: &mut egui::Ui,
    i: usize,
    row: &mut Row,
    recording: Option<usize>,
    super_down: bool,
    status: &mut Option<(String, bool)>,
) -> Option<usize> {
    let active = recording == Some(i);
    let text = if active {
        RichText::new("Press keys…  (Esc to cancel)").color(Color32::WHITE)
    } else if row.keys.is_empty() {
        RichText::new("Click to record").weak()
    } else {
        RichText::new(keys::label(&row.keys))
    };
    let mut button = Button::new(text).min_size(egui::vec2(180.0, 0.0));
    if active {
        button = button.fill(ui.visuals().selection.bg_fill);
    }
    let response = ui.add(button).on_hover_text("Click, then press the key combination");
    if response.clicked() {
        return if active { None } else { Some(i) };
    }
    if !active {
        return recording;
    }
    if response.clicked_elsewhere() {
        return None;
    }
    let pressed = ui.input_mut(|input| {
        let mut found = keys::scan(&input.events, super_down);
        // Some desktops swallow the Super key's own press; ask the OS (only on a key press).
        if found.is_some() && !super_down && platform::logo_key_down() {
            found = keys::scan(&input.events, true);
        }
        // Keep the press away from other widgets (Tab, Enter, Esc closing dialogs…).
        if found.is_some() {
            input.events.retain(|e| !matches!(e, egui::Event::Key { .. } | egui::Event::Text(_)));
        }
        found
    });
    match pressed {
        Some(Recorded::Combo(c)) => {
            row.keys = c;
            *status = None;
            None
        }
        Some(Recorded::Cancel) => None,
        Some(Recorded::Rejected(why)) => {
            *status = Some((why.into(), true));
            Some(i)
        }
        None => Some(i),
    }
}

impl eframe::App for Settings {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        // State at the start of this frame goes to the key buttons; remember the end state.
        let super_down = self.super_down;
        let super_after = ctx.input(|i| keys::super_after(&i.events, super_down));
        #[cfg(debug_assertions)]
        if let Some(shot) = &mut self.devshot {
            if shot.scene == "picker" && self.picker.is_none() && !self.rows.is_empty() {
                shot.scene.clear();
                self.picker = Some((0, Picker::new(Some(&self.rows[0].app_info()))));
            }
            #[cfg(target_os = "linux")]
            if shot.scene == "memory" && self.memory_tab.is_some() {
                shot.scene.clear();
                self.tab = Tab::Memory;
            }
            shot.tick(&ctx);
        }
        self.handle_close(&ctx);

        if let Some((row, picker)) = &mut self.picker {
            match picker.show(&ctx) {
                picker::Outcome::Open => {}
                picker::Outcome::Cancelled => self.picker = None,
                picker::Outcome::Chosen(info) => {
                    if let Some(r) = self.rows.get_mut(*row) {
                        r.name = info.name;
                        r.app = info.app;
                        r.launch = info.launch;
                    }
                    self.picker = None;
                }
            }
        }

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Shortcuts, "Shortcuts");
                #[cfg(target_os = "linux")]
                if self.memory_tab.is_some() {
                    ui.selectable_value(&mut self.tab, Tab::Memory, "Memory");
                }
            });
            ui.add_space(2.0);
        });
        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            self.bottom_bar(ui);
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(err) = &self.load_error {
                ui.label(
                    RichText::new("The config file couldn't be read, so editing is disabled:")
                        .strong(),
                );
                ui.label(RichText::new(err).color(ui.visuals().error_fg_color));
                ui.add_space(8.0);
                if ui.button("Open the file in a text editor").clicked() {
                    let _ = platform::open_in_editor(&self.path);
                }
                return;
            }
            match self.tab {
                Tab::Shortcuts => self.shortcuts_tab(ui),
                #[cfg(target_os = "linux")]
                Tab::Memory => {
                    if let Some(tab) = &mut self.memory_tab {
                        tab.show(ui, &mut self.memory_watch);
                    }
                }
            }
        });
        self.super_down = super_after;
    }
}

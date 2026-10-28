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
mod processes;
mod theme;

use std::path::PathBuf;

use anyhow::{Result, anyhow};
use eframe::egui::{self, Button, Color32, RichText};

use crate::config::{self, Document, Entry, MemoryLimit, MemoryWatch, Theme};
use crate::platform::{self, AppInfo};
use keys::Recorded;
use picker::Picker;

/// `appkey settings`.
pub fn run(config_path: PathBuf) -> Result<()> {
    open(config_path, None)
}

/// `appkey memory [--alert | --busy]`: settings, on the Processes tab. `Some(None)` is
/// opened by hand, `Some(Some(_))` for a warning from the daemon.
#[cfg(target_os = "linux")]
pub fn memory_window(config_path: PathBuf, warning: Option<crate::memory::Warning>) -> Result<()> {
    open(config_path, Some(warning))
}

#[cfg(target_os = "linux")]
type Start = Option<processes::Mode>;
#[cfg(not(target_os = "linux"))]
type Start = Option<()>;

fn open(config_path: PathBuf, start: Start) -> Result<()> {
    let title = window_title(start);
    let viewport = egui::ViewportBuilder::default()
        .with_title(title)
        .with_app_id("appkey")
        .with_inner_size(if start.is_some() { [940.0, 620.0] } else { [660.0, 560.0] })
        .with_min_inner_size([520.0, 300.0]);
    // A warning stays in view, but doesn't take the keyboard from what you're doing.
    #[cfg(target_os = "linux")]
    let viewport = if start.is_some_and(|m| m.is_some()) {
        viewport.with_window_level(egui::WindowLevel::AlwaysOnTop)
    } else {
        viewport
    };
    #[cfg(target_os = "linux")]
    requests::listen();
    eframe::run_native(
        "appkey",
        eframe::NativeOptions { viewport, ..Default::default() },
        Box::new(move |cc| {
            font::install(&cc.egui_ctx)?;
            #[cfg(target_os = "linux")]
            requests::wake_on_request(cc.egui_ctx.clone());
            #[cfg(target_os = "linux")]
            let settings = {
                let mut settings = Settings::load(config_path);
                if let Some(mode) = start {
                    settings.show_processes(mode);
                    settings.close_after_answer = mode.is_some();
                }
                settings
            };
            #[cfg(not(target_os = "linux"))]
            let settings = Settings::load(config_path);
            theme::apply(&cc.egui_ctx, settings.theme);
            Ok(Box::new(settings))
        }),
    )
    .map_err(|e| anyhow!("could not open the settings window: {e}"))
}

fn window_title(start: Start) -> &'static str {
    #[cfg(target_os = "linux")]
    {
        use crate::memory::Warning;
        match start {
            Some(Some(Warning::LowMemory)) => return "appkey: memory is running low",
            Some(Some(Warning::Busy)) => return "appkey: a background program is busy",
            _ => {}
        }
    }
    let _ = start;
    "appkey settings"
}

/// Requests from other appkey processes to show the Processes tab (see
/// `memory::window_signal`): a signal sets a flag, a thread wakes the UI to read it.
#[cfg(target_os = "linux")]
mod requests {
    use std::sync::atomic::{AtomicU8, Ordering};
    use std::time::Duration;

    use crate::memory::{Warning, window_signal};

    /// 0 = none, else 1 + the index in [`MODES`].
    static PENDING: AtomicU8 = AtomicU8::new(0);
    const MODES: [Option<Warning>; 3] = [None, Some(Warning::LowMemory), Some(Warning::Busy)];

    extern "C" fn on_signal(sig: libc::c_int) {
        // Only an atomic store: safe in a signal handler.
        if let Some(i) = MODES.iter().position(|m| window_signal(*m) == sig) {
            #[allow(clippy::cast_possible_truncation)]
            PENDING.store(i as u8 + 1, Ordering::SeqCst);
        }
    }

    /// Before the window opens, so a request sent right after we start isn't fatal (the
    /// default action of these signals ends the process).
    pub fn listen() {
        for mode in MODES {
            let handler = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
            // SAFETY: the handler only does an atomic store, which is async-signal-safe.
            unsafe { libc::signal(window_signal(mode), handler) };
        }
    }

    pub fn wake_on_request(ctx: eframe::egui::Context) {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(200));
                if PENDING.load(Ordering::SeqCst) != 0 {
                    ctx.request_repaint();
                }
            }
        });
    }

    /// The latest request, if one came since the last call.
    #[allow(clippy::option_option)] // "no request" vs. "show it, without a warning"
    pub fn take() -> Option<Option<Warning>> {
        match PENDING.swap(0, Ordering::SeqCst) {
            0 => None,
            i => MODES.get(usize::from(i) - 1).copied(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab {
    Shortcuts,
    #[cfg(target_os = "linux")]
    Memory,
    #[cfg(target_os = "linux")]
    Processes,
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
    memory_limit: MemoryLimit,
    saved_memory_limit: MemoryLimit,
    /// Whether the file has a `[memory_limit]` section.
    had_memory_limit: bool,
    /// Saved as soon as it's picked, not with the Save button.
    theme: Option<Theme>,
    /// Whether the file has a `[memory_watch]` section (we don't add one for defaults).
    had_memory_watch: bool,
    /// `None` where the Memory tab isn't offered (not Ubuntu / Mint).
    #[cfg(target_os = "linux")]
    memory_tab: Option<memory::MemoryTab>,
    /// Created the first time the Processes tab is shown.
    #[cfg(target_os = "linux")]
    processes: Option<processes::ProcessesTab>,
    /// Opened for a warning (`appkey memory --alert`): Dismiss / Snooze close the window.
    #[cfg(target_os = "linux")]
    close_after_answer: bool,
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
        let had_memory_limit = doc.memory_limit.is_some();
        let memory_limit = doc.memory_limit.unwrap_or_default();
        let theme = doc.theme;
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
            memory_limit,
            saved_memory_limit: memory_limit,
            had_memory_limit,
            theme,
            #[cfg(target_os = "linux")]
            memory_tab: crate::memory::available().then(memory::MemoryTab::new),
            #[cfg(target_os = "linux")]
            processes: None,
            #[cfg(target_os = "linux")]
            close_after_answer: false,
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
        self.rows != self.saved
            || self.memory_watch != self.saved_memory_watch
            || self.memory_limit != self.saved_memory_limit
    }

    /// Switches to the Processes tab, showing this warning (or none).
    #[cfg(target_os = "linux")]
    fn show_processes(&mut self, mode: processes::Mode) {
        if self.memory_tab.is_none() {
            return;
        }
        match &mut self.processes {
            Some(p) => p.set_mode(mode, self.saved_memory_watch),
            None => {
                self.processes = Some(processes::ProcessesTab::new(mode, self.saved_memory_watch));
            }
        }
        self.tab = Tab::Processes;
    }

    /// A request from the daemon or a shortcut (see [`requests`]).
    #[cfg(target_os = "linux")]
    fn handle_request(&mut self, ctx: &egui::Context) {
        let Some(mode) = requests::take() else { return };
        self.show_processes(mode);
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title(Some(mode)).into()));
        if mode.is_some() {
            ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(
                egui::WindowLevel::AlwaysOnTop,
            ));
            // The settings size is cramped for the table.
            let size = ctx.input(|i| i.viewport().inner_rect.map(|r| r.size()));
            if size.is_some_and(|s| s.x < 900.0 || s.y < 560.0) {
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(egui::vec2(940.0, 620.0)));
            }
        }
    }

    /// Tells the daemon what the user did about a warning; the window stays (unless it
    /// was opened just for it) and goes back to normal.
    #[cfg(target_os = "linux")]
    fn answer(&mut self, ctx: &egui::Context, answer: processes::Answer) {
        let text = if answer == processes::Answer::Snooze { "snooze" } else { "dismiss" };
        if let Err(e) = std::fs::write(crate::memory::alert_answer_path(), text) {
            eprintln!("appkey: couldn't tell the daemon: {e}");
        }
        if let Some(p) = &mut self.processes {
            p.set_mode(None, self.saved_memory_watch);
        }
        if self.close_after_answer {
            self.close_after_answer = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(window_title(None).into()));
        ctx.send_viewport_cmd(egui::ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
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

    /// Writes only the theme into the file as it is on disk, leaving unsaved edits unsaved.
    fn save_theme(&mut self) {
        if self.load_error.is_some() {
            return;
        }
        let result = config::read_document(&self.path).and_then(|mut doc| {
            doc.theme = self.theme;
            config::save_document(&self.path, &doc)
        });
        if let Err(e) = result {
            self.status = Some((format!("Couldn't save the theme: {e:#}"), true));
        }
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
        let keep_limit = self.had_memory_limit || self.memory_limit != MemoryLimit::default();
        let memory_limit = keep_limit.then_some(self.memory_limit);
        let result = config::validate_document(Document {
            theme: self.theme,
            memory_watch,
            memory_limit,
            shortcuts: shortcuts.clone(),
        })
        .and_then(|_| {
            shortcuts.extend(self.others.iter().cloned());
            config::save_document(
                &self.path,
                &Document { theme: self.theme, memory_watch, memory_limit, shortcuts },
            )
        });
        match result {
            Ok(()) => {
                self.saved = self.rows.clone();
                self.saved_memory_watch = self.memory_watch;
                self.had_memory_watch = keep_watch;
                self.saved_memory_limit = self.memory_limit;
                self.had_memory_limit = keep_limit;
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
                    self.memory_limit = self.saved_memory_limit;
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

    #[cfg(target_os = "linux")]
    fn tab_changed(&mut self, ctx: &egui::Context, before: Tab) {
        if self.tab == before {
            return;
        }
        if self.tab == Tab::Processes {
            self.show_processes(None);
        } else if self.processes.as_ref().is_some_and(|p| p.mode().is_some()) {
            // Looked at it and moved on.
            self.answer(ctx, processes::Answer::Dismiss);
        }
    }

    /// `APPKEY_SCREENSHOT_SCENE`: what to show before the screenshot.
    #[cfg(debug_assertions)]
    fn devshot_scene(&mut self, ctx: &egui::Context) {
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
            shot.tick(ctx);
        }
        #[cfg(target_os = "linux")]
        if let Some(scene) = self.devshot.as_mut().map(|s| std::mem::take(&mut s.scene)) {
            if matches!(scene.as_str(), "processes" | "cgroups") {
                self.show_processes(None);
                if scene == "cgroups"
                    && let Some(p) = &mut self.processes
                {
                    p.show_cgroups();
                }
            } else if let Some(shot) = &mut self.devshot {
                shot.scene = scene;
            }
        }
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
        self.devshot_scene(&ctx);
        self.handle_close(&ctx);
        #[cfg(target_os = "linux")]
        self.handle_request(&ctx);

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

        #[cfg(target_os = "linux")]
        let before = self.tab;
        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Shortcuts, "Shortcuts");
                #[cfg(target_os = "linux")]
                if self.memory_tab.is_some() {
                    ui.selectable_value(&mut self.tab, Tab::Memory, "Memory");
                    ui.selectable_value(&mut self.tab, Tab::Processes, "Processes");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::picker(ui, &mut self.theme) {
                        theme::apply(ui.ctx(), self.theme);
                        self.save_theme();
                    }
                });
            });
            ui.add_space(2.0);
        });
        #[cfg(target_os = "linux")]
        self.tab_changed(&ctx, before);
        egui::Panel::bottom("actions").show(ui, |ui| {
            ui.add_space(6.0);
            #[cfg(target_os = "linux")]
            if self.tab == Tab::Processes
                && let Some(p) = &mut self.processes
            {
                if let Some(answer) = p.footer(ui) {
                    self.answer(ui.ctx(), answer);
                }
                ui.add_space(6.0);
                return;
            }
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
                    if let Some(tab) = &mut self.memory_tab
                        && tab.show(ui, &mut self.memory_watch, &mut self.memory_limit)
                    {
                        self.show_processes(None);
                    }
                }
                #[cfg(target_os = "linux")]
                Tab::Processes => {
                    if let Some(p) = &mut self.processes {
                        p.show(ui);
                    }
                }
            }
        });
        #[cfg(target_os = "linux")]
        if let Some(p) = &mut self.processes
            && self.tab == Tab::Processes
        {
            p.finish(&ctx);
        }
        self.super_down = super_after;
    }
}

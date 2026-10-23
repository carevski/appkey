//! `appkey memory [--alert | --busy]`: the user's apps, grouped through the process
//! tree (Chrome with its tabs, an IDE with its helpers, each terminal tab), with memory,
//! CPU and running time, and buttons to close a whole app or one of its processes.
//! Apps left running in the background are listed first.
//!
//! The daemon opens it with `--alert` when memory runs low and with `--busy` when a
//! background program keeps the CPU busy. Closing sends `SIGTERM` so programs can
//! save; "Force quit" (`SIGKILL`) is offered when one doesn't react.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow};
use eframe::egui::{self, Button, RichText};

use crate::config::{self, MemoryWatch};
use crate::memory::procs::{self, AppGroup, Proc};
use crate::memory::{self, Snapshot, sysrq, uninstall};
use crate::platform;

const REFRESH: Duration = Duration::from_secs(2);
/// Window titles cost a round-trip to the desktop (a KWin script on KDE): less often.
const TITLES_EVERY: u32 = 3;
/// How long a program gets to quit before "Force quit" appears.
const GRACE: Duration = Duration::from_secs(5);
const MAX_APPS: usize = 15;
/// Processes listed under an expanded app before "… N more".
const MAX_CHILDREN: usize = 8;
/// Apps smaller than this (and idle) wouldn't free enough to matter.
const MIN_KIB: u64 = 20 * 1024;
/// Background leftovers are worth listing even when small.
const MIN_BACKGROUND_KIB: u64 = 5 * 1024;
/// Freshly started background programs aren't "left running" yet.
const MIN_BACKGROUND_AGE: f64 = 60.0;

/// Why the window was opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// By hand (`appkey memory`, the settings tab, a shortcut).
    Manual,
    /// Memory is running low.
    LowMemory,
    /// A background program keeps the CPU busy.
    Busy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Memory,
    Cpu,
}

/// Returns `true` if the user snoozed the warning.
pub fn run(mode: Mode) -> Result<bool> {
    let snoozed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flag = snoozed.clone();
    let title = match mode {
        Mode::Manual => "appkey: memory use",
        Mode::LowMemory => "appkey: memory is running low",
        Mode::Busy => "appkey: a background program is busy",
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_app_id("appkey")
            .with_inner_size([940.0, 620.0])
            .with_min_inner_size([560.0, 360.0])
            .with_window_level(if mode == Mode::Manual {
                egui::WindowLevel::Normal
            } else {
                egui::WindowLevel::AlwaysOnTop
            }),
        ..Default::default()
    };
    eframe::run_native(
        "appkey-memory",
        options,
        Box::new(move |cc| {
            super::font::install(&cc.egui_ctx)?;
            Ok(Box::new(Dialog::new(mode, flag)))
        }),
    )
    .map_err(|e| anyhow!("could not open the memory window: {e}"))?;
    Ok(snoozed.load(std::sync::atomic::Ordering::SeqCst))
}

/// What a click in the table asked for.
enum Action {
    Close(Vec<u32>),
    ForceGroup(u32),
    End(u32),
    ForceProc(u32),
    Show(u32),
    /// Start "Uninstall…" for the app with this root.
    Uninstall(u32),
}

#[derive(Debug)]
struct Dialog {
    mode: Mode,
    snoozed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    watch: MemoryWatch,
    uid: u32,
    home: String,
    hz: u64,
    snapshot: Option<Snapshot>,
    procs: HashMap<u32, Proc>,
    /// CPU use per pid since the previous refresh, in % of one core.
    cpu: HashMap<u32, f32>,
    last_ticks: HashMap<u32, u64>,
    last_sample: Instant,
    uptime: f64,
    titles: HashMap<u32, Vec<String>>,
    refreshes: u32,
    background: Vec<AppGroup>,
    apps: Vec<AppGroup>,
    refreshed: Instant,
    /// The pointer is in the window: the list stays still so a click hits what was aimed at.
    paused: bool,
    sort: SortBy,
    /// Selected apps, by root pid.
    selected: HashSet<u32>,
    /// Apps are expanded unless collapsed here.
    collapsed: HashSet<u32>,
    /// Apps showing all their processes, not just the biggest.
    show_all: HashSet<u32>,
    /// Apps (by root) and single processes we asked to quit, and when.
    closing_apps: HashMap<u32, Instant>,
    closing_procs: HashMap<u32, Instant>,
    status: Option<String>,
    sysrq_f: bool,
    uninstall: Option<Uninstall>,
    #[cfg(debug_assertions)]
    devshot: Option<super::devshot::DevShot>,
}

impl Dialog {
    fn new(mode: Mode, snoozed: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Self {
        let watch = config::path()
            .ok()
            .and_then(|p| config::read_document(&p).ok())
            .and_then(|d| d.memory_watch)
            .unwrap_or_default();
        let mut d = Self {
            mode,
            snoozed,
            watch,
            uid: memory::current_uid(),
            home: dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
            hz: memory::clock_ticks(),
            snapshot: None,
            procs: HashMap::new(),
            cpu: HashMap::new(),
            last_ticks: HashMap::new(),
            last_sample: Instant::now(),
            uptime: 0.0,
            titles: HashMap::new(),
            refreshes: 0,
            background: Vec::new(),
            apps: Vec::new(),
            refreshed: Instant::now(),
            paused: false,
            sort: if mode == Mode::Busy { SortBy::Cpu } else { SortBy::Memory },
            selected: HashSet::new(),
            collapsed: HashSet::new(),
            show_all: HashSet::new(),
            closing_apps: HashMap::new(),
            closing_procs: HashMap::new(),
            status: None,
            sysrq_f: sysrq::read_current().is_ok_and(|v| sysrq::allows(v, sysrq::SIGNAL)),
            uninstall: None,
            #[cfg(debug_assertions)]
            devshot: super::devshot::DevShot::from_env(),
        };
        d.refresh();
        // CPU needs two samples: take the second one soon.
        d.refreshed = Instant::now().checked_sub(REFRESH / 2).unwrap_or_else(Instant::now);
        d
    }

    fn refresh(&mut self) {
        let now = Instant::now();
        self.refreshed = now;
        self.snapshot = memory::read_snapshot();
        self.uptime = memory::uptime();
        if self.refreshes.is_multiple_of(TITLES_EVERY) {
            self.titles = platform::window_titles();
        }
        self.refreshes += 1;

        let list = memory::read_procs();
        let seconds = now.duration_since(self.last_sample).as_secs_f64();
        self.cpu = list
            .iter()
            .filter_map(|p| {
                let before = self.last_ticks.get(&p.pid)?;
                Some((p.pid, procs::cpu_percent(*before, p.cpu_ticks, seconds, self.hz)))
            })
            .collect();
        self.last_ticks = list.iter().map(|p| (p.pid, p.cpu_ticks)).collect();
        self.last_sample = now;

        let groups = procs::build_groups(
            &list,
            self.uid,
            std::process::id(),
            memory::is_protected,
            &self.titles,
            &self.home,
        );
        self.procs = list.into_iter().map(|p| (p.pid, p)).collect();
        let (mut background, mut apps): (Vec<AppGroup>, Vec<AppGroup>) =
            groups.into_iter().partition(|g| g.detached && self.age(g.root) >= MIN_BACKGROUND_AGE);
        background.retain(|g| g.rss_kib >= MIN_BACKGROUND_KIB || self.group_cpu(g) >= 1.0);
        apps.retain(|g| g.rss_kib >= MIN_KIB || self.group_cpu(g) >= 5.0);
        self.sort_groups(&mut background);
        self.sort_groups(&mut apps);
        apps.truncate(MAX_APPS);
        // Keep rows where they were, so a click never lands on a row that just moved.
        // Not before CPU use is known (second refresh), or sorting by CPU would stick.
        if self.refreshes > 2 {
            keep_order(&mut background, &self.background);
            keep_order(&mut apps, &self.apps);
        }
        self.background = background;
        self.apps = apps;

        let roots: HashSet<u32> = self.all_groups().map(|g| g.root).collect();
        self.selected.retain(|r| roots.contains(r));
        self.closing_apps.retain(|r, _| roots.contains(r));
        self.closing_procs.retain(|p, _| self.procs.contains_key(p));
    }

    fn all_groups(&self) -> impl Iterator<Item = &AppGroup> {
        self.background.iter().chain(&self.apps)
    }

    fn sort_groups(&self, groups: &mut [AppGroup]) {
        match self.sort {
            SortBy::Memory => groups.sort_by_key(|g| std::cmp::Reverse(g.rss_kib)),
            SortBy::Cpu => groups.sort_by(|a, b| self.group_cpu(b).total_cmp(&self.group_cpu(a))),
        }
    }

    fn group_cpu(&self, g: &AppGroup) -> f32 {
        g.members.iter().filter_map(|p| self.cpu.get(p)).sum()
    }

    #[allow(clippy::cast_precision_loss)]
    fn age(&self, pid: u32) -> f64 {
        self.procs
            .get(&pid)
            .map_or(0.0, |p| (self.uptime - p.start_ticks as f64 / self.hz.max(1) as f64).max(0.0))
    }

    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn age_text(&self, pid: u32) -> String {
        procs::format_age(self.age(pid) as u64)
    }

    fn targets(&self, pids: &[u32]) -> Vec<(u32, u64)> {
        pids.iter().filter_map(|p| self.procs.get(p).map(|p| (p.pid, p.start_ticks))).collect()
    }

    fn apply(&mut self, action: Action) {
        let now = Instant::now();
        // Showing a window or starting "Uninstall…" changes nothing we list (yet).
        let reread = !matches!(action, Action::Show(_) | Action::Uninstall(_));
        let find = |root: u32| self.all_groups().find(|g| g.root == root).cloned();
        match action {
            Action::Close(roots) => {
                let groups: Vec<AppGroup> = roots.iter().filter_map(|r| find(*r)).collect();
                let pids: Vec<u32> = groups.iter().flat_map(|g| g.members.clone()).collect();
                let n = memory::signal(&self.targets(&pids), false);
                let names: Vec<&str> = groups.iter().map(|g| g.label.as_str()).collect();
                self.status =
                    Some(format!("Asked to quit: {} ({n} process(es)).", names.join(", ")));
                for g in &groups {
                    self.closing_apps.insert(g.root, now);
                    self.selected.remove(&g.root);
                }
            }
            Action::ForceGroup(root) => {
                if let Some(g) = find(root) {
                    let n = memory::signal(&self.targets(&g.members), true);
                    self.status = Some(format!("Force-quit {} ({n} process(es)).", g.label));
                }
            }
            Action::End(pid) => {
                let n = memory::signal(&self.targets(&[pid]), false);
                self.status = Some(format!("Asked process {pid} to quit ({n})."));
                self.closing_procs.insert(pid, now);
            }
            Action::ForceProc(pid) => {
                memory::signal(&self.targets(&[pid]), true);
                self.status = Some(format!("Force-quit process {pid}."));
            }
            Action::Show(pid) => {
                if !matches!(platform::focus_pid(pid), Ok(true)) {
                    self.status = Some("Couldn't bring that window to the front.".into());
                }
            }
            Action::Uninstall(root) => {
                if let Some(g) = find(root) {
                    let (tx, rx) = std::sync::mpsc::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(uninstall::plan(root));
                    });
                    self.uninstall = Some(Uninstall::Checking { root, label: g.label, rx });
                }
            }
        }
        if reread {
            self.refresh();
        }
    }

    fn header(&self, ui: &mut egui::Ui) {
        let Some(s) = &self.snapshot else {
            ui.label("Couldn't read memory use from /proc.");
            return;
        };
        let stressed = s.is_stressed(self.watch);
        let headline = match (self.mode, stressed) {
            (Mode::LowMemory, true) => "Your computer is running low on memory",
            (Mode::LowMemory, false) => "Memory is back to normal",
            (Mode::Busy, _) => "A program in the background keeps the CPU busy",
            (Mode::Manual, _) => "What's running",
        };
        ui.heading(headline);
        ui.horizontal(|ui| {
            ui.label(s.summary());
            if self.paused {
                ui.label(
                    RichText::new("· updates paused while the pointer is here").weak().small(),
                );
            }
        });
        if self.mode == Mode::LowMemory && stressed {
            ui.label(
                "Close something you don't need, or the system may slow down and freeze. \
                 Programs get the chance to save first.",
            );
        }
    }

    fn section(&mut self, ui: &mut egui::Ui, background: bool) -> Option<Action> {
        let groups = if background { self.background.clone() } else { self.apps.clone() };
        let mut action = None;
        let max = groups.iter().map(|g| g.rss_kib).max().unwrap_or(1).max(1);
        egui::Grid::new(if background { "background" } else { "apps" })
            .num_columns(5)
            .spacing([8.0, 4.0])
            .striped(true)
            .min_col_width(0.0)
            .show(ui, |ui| {
                for h in ["", "Memory", "CPU", "Running", ""] {
                    ui.label(RichText::new(h).small().weak());
                }
                ui.end_row();
                for g in &groups {
                    if let Some(a) = self.group_row(ui, g, max) {
                        action = Some(a);
                    }
                    if g.members.len() > 1
                        && !self.collapsed.contains(&g.root)
                        && let Some(a) = self.member_rows(ui, g)
                    {
                        action = Some(a);
                    }
                }
            });
        action
    }

    fn group_row(&mut self, ui: &mut egui::Ui, g: &AppGroup, max: u64) -> Option<Action> {
        let mut action = None;
        ui.horizontal(|ui| {
            if g.members.len() > 1 {
                let open = !self.collapsed.contains(&g.root);
                if disclosure(ui, open).clicked() {
                    if open {
                        self.collapsed.insert(g.root);
                    } else {
                        self.collapsed.remove(&g.root);
                    }
                }
            } else {
                ui.add_space(18.0);
            }
            let mut on = self.selected.contains(&g.root);
            if ui.checkbox(&mut on, "").changed() {
                if on {
                    self.selected.insert(g.root);
                } else {
                    self.selected.remove(&g.root);
                }
            }
            let count = if g.members.len() > 1 {
                format!("  ({})", g.members.len())
            } else {
                String::new()
            };
            let name = ui.label(RichText::new(format!("{}{count}", g.label)).strong());
            if !g.detail.is_empty() {
                ui.label(RichText::new(truncate(&g.detail, 38)).weak());
            }
            name.on_hover_text(self.hover(g.root, g.has_window.then_some(g.detail.as_str())));
        });
        ui.horizontal(|ui| {
            #[allow(clippy::cast_precision_loss)]
            let share = g.rss_kib as f32 / max as f32;
            ui.add(egui::ProgressBar::new(share).desired_width(60.0));
            cell(ui, memory::format_kib(g.rss_kib), MEM_W);
        });
        cell(ui, cpu_text(self.group_cpu(g)), CPU_W);
        cell(ui, self.age_text(g.root), AGE_W);
        ui.horizontal(|ui| {
            if g.has_window {
                let window_pid = g.members.iter().copied().find(|p| self.titles.contains_key(p));
                if let Some(pid) = window_pid
                    && ui
                        .small_button("Show")
                        .on_hover_text("Bring its window to the front")
                        .clicked()
                {
                    action = Some(Action::Show(pid));
                }
            }
            match self.closing_apps.get(&g.root) {
                Some(t) if t.elapsed() >= GRACE => {
                    if ui
                        .small_button("Force quit")
                        .on_hover_text("It didn't quit. Unsaved work in it is lost.")
                        .clicked()
                    {
                        action = Some(Action::ForceGroup(g.root));
                    }
                }
                Some(_) => {
                    ui.label(RichText::new("closing…").weak());
                }
                None => {
                    if ui.small_button("Close").on_hover_text("Ask the whole app to quit").clicked()
                    {
                        action = Some(Action::Close(vec![g.root]));
                    }
                }
            }
            // Leftovers in the background are rarely installed apps; offer it for the rest.
            if !g.detached
                && ui
                    .small_button("Uninstall…")
                    .on_hover_text(
                        "Find out how it was installed and remove it (you confirm first)",
                    )
                    .clicked()
            {
                action = Some(Action::Uninstall(g.root));
            }
        });
        ui.end_row();
        action
    }

    /// The "Uninstall…" dialog: checking → confirm (or why not) → running → result.
    fn uninstall_dialog(&mut self, ctx: &egui::Context) {
        let Some(state) = self.uninstall.take() else { return };
        let next = match state {
            Uninstall::Checking { root, label, rx } => match rx.try_recv() {
                Ok(plan) => Some(Uninstall::Confirm { root, label, plan }),
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    modal(ctx, |ui| {
                        ui.heading(format!("Uninstall {label}"));
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Finding out how it was installed…");
                        });
                    });
                    ctx.request_repaint_after(Duration::from_millis(100));
                    Some(Uninstall::Checking { root, label, rx })
                }
                Err(_) => None,
            },
            Uninstall::Confirm { root, label, plan } => {
                let n = self.all_groups().find(|g| g.root == root).map_or(0, |g| g.members.len());
                let mut choice = None;
                modal(ctx, |ui| choice = confirm(ui, &label, &plan, n));
                match choice {
                    Some(true) => {
                        let members = self
                            .all_groups()
                            .find(|g| g.root == root)
                            .map(|g| g.members.clone())
                            .unwrap_or_default();
                        let targets = self.targets(&members);
                        let (tx, rx) = std::sync::mpsc::channel();
                        std::thread::spawn(move || {
                            let _ = tx.send(
                                uninstall::run(&plan, &targets).map_err(|e| format!("{e:#}")),
                            );
                        });
                        Some(Uninstall::Running { label, rx })
                    }
                    Some(false) => None,
                    None => Some(Uninstall::Confirm { root, label, plan }),
                }
            }
            Uninstall::Running { label, rx } => match rx.try_recv() {
                Ok(result) => {
                    self.refresh();
                    Some(Uninstall::Done { label, result })
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {
                    modal(ctx, |ui| {
                        ui.heading(format!("Uninstalling {label}"));
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Closing it, then removing it. Approve the password prompt if one appears.");
                        });
                    });
                    ctx.request_repaint_after(Duration::from_millis(100));
                    Some(Uninstall::Running { label, rx })
                }
                Err(_) => None,
            },
            Uninstall::Done { label, result } => {
                let mut ok = false;
                modal(ctx, |ui| {
                    ui.heading(label.as_str());
                    match &result {
                        Ok(msg) => ui.label(msg.as_str()),
                        Err(e) => ui.label(
                            RichText::new(format!("Not uninstalled: {e}"))
                                .color(ui.visuals().error_fg_color),
                        ),
                    };
                    ui.add_space(6.0);
                    ok = ui.button("OK").clicked();
                });
                (!ok).then_some(Uninstall::Done { label, result })
            }
        };
        self.uninstall = next;
    }

    fn member_rows(&mut self, ui: &mut egui::Ui, g: &AppGroup) -> Option<Action> {
        let mut action = None;
        let all = self.show_all.contains(&g.root);
        let shown = if all { g.members.len() } else { MAX_CHILDREN.min(g.members.len()) };
        for &pid in &g.members[..shown] {
            let Some(p) = self.procs.get(&pid).cloned() else { continue };
            ui.horizontal(|ui| {
                ui.add_space(46.0);
                let mut text = procs::describe(&p);
                if pid == g.root && g.members.len() > 1 {
                    text.push_str(" (main)");
                }
                ui.label(truncate(&text, 48)).on_hover_text(self.hover(pid, None));
            });
            ui.horizontal(|ui| {
                ui.add_space(64.0);
                cell(ui, RichText::new(memory::format_kib(p.rss_kib)).weak(), MEM_W);
            });
            cell(
                ui,
                RichText::new(cpu_text(self.cpu.get(&pid).copied().unwrap_or(0.0))).weak(),
                CPU_W,
            );
            cell(ui, RichText::new(self.age_text(pid)).weak(), AGE_W);
            ui.horizontal(|ui| match self.closing_procs.get(&pid) {
                Some(t) if t.elapsed() >= GRACE => {
                    if ui.small_button("Force quit").clicked() {
                        action = Some(Action::ForceProc(pid));
                    }
                }
                Some(_) => {
                    ui.label(RichText::new("closing…").weak());
                }
                None => {
                    let hint = if procs::describe(&p) == "Tab" {
                        "End this process. Its tab shows \"Aw, Snap!\" and can be reloaded."
                    } else {
                        "End just this process"
                    };
                    if ui.small_button("End").on_hover_text(hint).clicked() {
                        action = Some(Action::End(pid));
                    }
                }
            });
            ui.end_row();
        }
        if shown < g.members.len() {
            let rest = &g.members[shown..];
            let kib: u64 = rest.iter().filter_map(|p| self.procs.get(p)).map(|p| p.rss_kib).sum();
            ui.horizontal(|ui| {
                ui.add_space(46.0);
                let text = format!("… {} more ({})", rest.len(), memory::format_kib(kib));
                if ui.link(text).clicked() {
                    self.show_all.insert(g.root);
                }
            });
            ui.end_row();
        }
        action
    }

    /// Full details for a tooltip: what it runs, where, and its pid.
    fn hover(&self, pid: u32, titles: Option<&str>) -> String {
        let Some(p) = self.procs.get(&pid) else { return String::new() };
        let mut s = truncate(&p.cmdline.join(" "), 300);
        if let Some(cwd) = &p.cwd {
            let _ = write!(s, "\nin {}", procs::short_path(cwd, &self.home));
        }
        if let Some(t) = titles.filter(|t| !t.is_empty()) {
            let _ = write!(s, "\nwindows: {t}");
        }
        let _ = write!(s, "\npid {pid}");
        s
    }

    fn footer(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        let mut action = None;
        let tip = if self.sysrq_f {
            "If the desktop freezes before you can react: hold Alt+PrtSc and press F. \
             For Chrome's individual tabs, Chrome's own Task Manager: Shift+Esc."
        } else {
            "Emergency key Alt+PrtSc+F is off; turn it on in appkey settings → Memory. \
             For Chrome's individual tabs, Chrome's own Task Manager: Shift+Esc."
        };
        ui.label(RichText::new(tip).weak().small());
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let roots: Vec<u32> = self.selected.iter().copied().collect();
            if ui
                .add_enabled(!roots.is_empty(), Button::new("Close selected"))
                .on_hover_text("Asks the apps to quit, like closing their windows.")
                .clicked()
            {
                action = Some(Action::Close(roots));
            }
            ui.separator();
            ui.label("Sort by");
            let before = self.sort;
            ui.selectable_value(&mut self.sort, SortBy::Memory, "memory");
            ui.selectable_value(&mut self.sort, SortBy::Cpu, "CPU");
            if self.sort != before {
                let (mut b, mut a) =
                    (std::mem::take(&mut self.background), std::mem::take(&mut self.apps));
                self.sort_groups(&mut b);
                self.sort_groups(&mut a);
                (self.background, self.apps) = (b, a);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let alert = self.mode != Mode::Manual;
                if ui.button(if alert { "Dismiss" } else { "Close" }).clicked() {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if alert && ui.button("Snooze for an hour").clicked() {
                    self.snoozed.store(true, std::sync::atomic::Ordering::SeqCst);
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                }
            });
        });
        if let Some(s) = &self.status {
            ui.label(RichText::new(s).weak());
        }
        action
    }
}

/// Fixed column widths, so buttons don't move when numbers change.
const MEM_W: f32 = 56.0;
const CPU_W: f32 = 60.0;
const AGE_W: f32 = 60.0;

fn cell(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, width: f32) {
    ui.add_sized([width, 18.0], egui::Label::new(text));
}

fn cpu_text(pct: f32) -> String {
    if pct < 0.5 { "–".into() } else { format!("{pct:.0}% CPU") }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}

/// Puts groups (and their processes) that were already listed back in their previous
/// order; new ones follow in their sorted order.
fn keep_order(groups: &mut [AppGroup], previous: &[AppGroup]) {
    let rank = |root: u32| previous.iter().position(|g| g.root == root).unwrap_or(usize::MAX);
    groups.sort_by_key(|g| rank(g.root));
    for g in groups.iter_mut() {
        if let Some(old) = previous.iter().find(|p| p.root == g.root) {
            let at = |pid: &u32| old.members.iter().position(|m| m == pid).unwrap_or(usize::MAX);
            g.members.sort_by_key(at);
        }
    }
}

/// A ▸ / ▾ toggle drawn as a triangle, so it doesn't depend on the font's glyphs.
fn disclosure(ui: &mut egui::Ui, open: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::click());
    let color = if response.hovered() {
        ui.visuals().strong_text_color()
    } else {
        ui.visuals().text_color()
    };
    let c = rect.center();
    let r = 4.5;
    let points = if open {
        vec![
            c + egui::vec2(-r, -r * 0.6),
            c + egui::vec2(r, -r * 0.6),
            c + egui::vec2(0.0, r * 0.7),
        ]
    } else {
        vec![
            c + egui::vec2(-r * 0.6, -r),
            c + egui::vec2(r * 0.7, 0.0),
            c + egui::vec2(-r * 0.6, r),
        ]
    };
    ui.painter().add(egui::Shape::convex_polygon(points, color, egui::Stroke::NONE));
    response.on_hover_text(if open { "Collapse" } else { "Expand" })
}

impl eframe::App for Dialog {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        #[cfg(debug_assertions)]
        if let Some(shot) = &mut self.devshot {
            shot.tick(&ctx);
        }
        // Rows come and go (tabs, short-lived helpers) and would shift under the pointer.
        // The first two refreshes always run: CPU use needs two samples.
        self.paused = self.refreshes >= 2 && ctx.input(|i| i.pointer.has_pointer());
        if !self.paused && self.refreshed.elapsed() >= REFRESH {
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(500));

        let mut action = None;
        egui::Panel::bottom("memory-actions").show(ui, |ui| {
            ui.add_space(6.0);
            action = self.footer(ui);
            ui.add_space(6.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.header(ui);
            ui.add_space(8.0);
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                if !self.background.is_empty() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("Left running in the background").strong().color(ui.visuals().warn_fg_color));
                        if ui
                            .small_button("Close all of these")
                            .on_hover_text("Asks every program in this list to quit")
                            .clicked()
                        {
                            action = Some(Action::Close(self.background.iter().map(|g| g.root).collect()));
                        }
                    });
                    ui.label(
                        RichText::new(
                            "No window, and whatever started them (a terminal, Claude, an IDE) has closed.",
                        )
                        .weak()
                        .small(),
                    );
                    if let Some(a) = self.section(ui, true) {
                        action = Some(a);
                    }
                    ui.add_space(10.0);
                    ui.label(RichText::new("Apps").strong());
                }
                if self.apps.is_empty() {
                    ui.label("Nothing else is using much memory or CPU.");
                } else if let Some(a) = self.section(ui, false) {
                    action = Some(a);
                }
            });
        });
        self.uninstall_dialog(&ctx);
        if let Some(a) = action.filter(|_| self.uninstall.is_none()) {
            self.apply(a);
        }
    }
}

/// The confirmation (or why not): `Some(true)` to go ahead, `Some(false)` to close.
fn confirm(ui: &mut egui::Ui, label: &str, plan: &uninstall::Plan, n: usize) -> Option<bool> {
    let mut choice = None;
    ui.heading(format!("Uninstall {label}?"));
    ui.label(format!("Installed as: {}", plan.describe()));
    ui.add_space(6.0);
    if let Some(why) = &plan.refusal {
        ui.label(
            RichText::new(format!("appkey won't uninstall it: {why}"))
                .color(ui.visuals().warn_fg_color),
        );
        ui.add_space(6.0);
        return ui.button("OK").clicked().then_some(false);
    }
    ui.label(if matches!(plan.source, uninstall::Source::AppImage { .. }) {
        "Moves to the Trash (you can restore it from there):"
    } else {
        "Removes:"
    });
    for item in plan.removes.iter().take(16) {
        ui.label(RichText::new(format!("  {item}")).monospace());
    }
    ui.add_space(6.0);
    ui.label(format!(
        "appkey first closes {label} ({n} process{}), then runs:",
        if n == 1 { "" } else { "es" }
    ));
    let cmd = plan.command.as_ref().map(|c| c.join(" ")).unwrap_or_default();
    ui.label(RichText::new(cmd).monospace().small());
    if uninstall::needs_password(&plan.source) {
        ui.label(RichText::new("Your system will ask for your password.").weak());
    }
    if matches!(plan.source, uninstall::Source::Deb { .. }) {
        ui.label(RichText::new("Your settings for it are kept (apt remove, not purge).").weak());
    }
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        let go = egui::Button::new(RichText::new("Uninstall").color(egui::Color32::WHITE))
            .fill(ui.visuals().error_fg_color);
        if ui.add(go).clicked() {
            choice = Some(true);
        }
        if ui.button("Cancel").clicked() {
            choice = Some(false);
        }
    });
    choice
}

/// Where "Uninstall…" is.
enum Uninstall {
    Checking { root: u32, label: String, rx: std::sync::mpsc::Receiver<uninstall::Plan> },
    Confirm { root: u32, label: String, plan: uninstall::Plan },
    Running { label: String, rx: std::sync::mpsc::Receiver<Result<String, String>> },
    Done { label: String, result: Result<String, String> },
}

impl std::fmt::Debug for Uninstall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Checking { .. } => "Checking",
            Self::Confirm { .. } => "Confirm",
            Self::Running { .. } => "Running",
            Self::Done { .. } => "Done",
        })
    }
}

fn modal(ctx: &egui::Context, body: impl FnOnce(&mut egui::Ui)) {
    egui::Modal::new(egui::Id::new("uninstall")).show(ctx, |ui| {
        ui.set_max_width(520.0);
        body(ui);
    });
}

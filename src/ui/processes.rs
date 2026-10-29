//! "Processes" tab (`appkey memory [--alert | --busy]` opens settings on it): the
//! user's apps, grouped through the process tree (Chrome with its tabs, an IDE with its
//! helpers, each terminal tab), or by cgroup, with memory, CPU and running time, and
//! buttons to close a whole app or one of its processes. Apps left running in the
//! background are listed first.
//!
//! The daemon opens it with `--alert` when memory runs low and with `--busy` when a
//! background program keeps the CPU busy. Closing sends `SIGTERM` so programs can
//! save; "Force quit" (`SIGKILL`) is offered when one doesn't react.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use eframe::egui::{self, Button, RichText};

use crate::config::MemoryWatch;
use crate::memory::cgroup::{self, Node};
use crate::memory::procs::{self, AppGroup, Proc};
use crate::memory::{self, CgroupMemory, Snapshot, Warning, sysrq, uninstall};
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

/// Why the tab is showing: by hand (`None`), or for a warning from the daemon.
pub type Mode = Option<Warning>;

/// What the user said about a warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Dismiss,
    Snooze,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortBy {
    Memory,
    Cpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GroupBy {
    App,
    Cgroup,
}

/// What a click in the table asked for.
#[derive(Debug)]
enum Action {
    Close(Vec<u32>),
    ForceGroup(u32),
    End(u32),
    ForceProc(u32),
    Show(u32),
    /// Start "Uninstall…" for the app with this root.
    Uninstall(u32),
    /// Open the details popup for this process.
    Details(u32),
}

/// The details popup: the process as it was when opened (so it can still be read
/// after the process ends), and what's re-read from `/proc` while it's open.
#[derive(Debug)]
struct Details {
    proc: Proc,
    info: memory::ProcDetails,
    read: Instant,
    ended: bool,
}

#[derive(Debug)]
pub struct ProcessesTab {
    mode: Mode,
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
    /// The pointer is in the window: numbers keep updating, but rows don't come, go or
    /// move, so a click hits what was aimed at.
    holding: bool,
    /// Listed processes that exited while rows were held; shown as "ended".
    ended: HashSet<u32>,
    sort: SortBy,
    group_by: GroupBy,
    /// The user's processes by cgroup (built only while that view is shown).
    tree: Vec<Node>,
    /// What the kernel accounts to each cgroup in `tree`, by path.
    cgroup_memory: HashMap<String, CgroupMemory>,
    /// Cgroups whose open / closed state the user flipped (big ones start open).
    toggled: HashSet<String>,
    /// Cgroups showing all their processes.
    show_all_cgroups: HashSet<String>,
    pending: Option<Action>,
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
    details: Option<Details>,
    /// Which app each process belongs to: (its label, its root pid).
    app_of: HashMap<u32, (String, u32)>,
}

impl ProcessesTab {
    pub fn new(mode: Mode, watch: MemoryWatch) -> Self {
        let mut d = Self {
            mode,
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
            holding: false,
            ended: HashSet::new(),
            sort: if mode == Some(Warning::Busy) { SortBy::Cpu } else { SortBy::Memory },
            group_by: GroupBy::App,
            tree: Vec::new(),
            cgroup_memory: HashMap::new(),
            toggled: HashSet::new(),
            show_all_cgroups: HashSet::new(),
            pending: None,
            selected: HashSet::new(),
            collapsed: HashSet::new(),
            show_all: HashSet::new(),
            closing_apps: HashMap::new(),
            closing_procs: HashMap::new(),
            status: None,
            sysrq_f: sysrq::read_current().is_ok_and(|v| sysrq::allows(v, sysrq::SIGNAL)),
            uninstall: None,
            details: None,
            app_of: HashMap::new(),
        };
        d.refresh();
        // CPU needs two samples: take the second one soon.
        d.refreshed = Instant::now().checked_sub(REFRESH / 2).unwrap_or_else(Instant::now);
        d
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Shows a (new) warning, or none. The thresholds may have been saved since.
    pub fn set_mode(&mut self, mode: Mode, watch: MemoryWatch) {
        self.watch = watch;
        if mode != self.mode && mode.is_some() {
            self.sort = if mode == Some(Warning::Busy) { SortBy::Cpu } else { SortBy::Memory };
            self.group_by = GroupBy::App;
            self.refresh();
        }
        self.mode = mode;
    }

    #[cfg(debug_assertions)]
    pub fn show_cgroups(&mut self) {
        self.group_by = GroupBy::Cgroup;
        self.refresh();
    }

    /// Opens the details of the user's biggest process (for screenshots).
    #[cfg(debug_assertions)]
    pub fn show_details(&mut self) {
        let biggest = self
            .procs
            .values()
            .filter(|p| p.uid == self.uid && !p.cmdline.is_empty())
            .max_by_key(|p| p.rss_kib)
            .map(|p| p.pid);
        if let Some(pid) = biggest {
            self.apply(Action::Details(pid));
        }
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
        self.app_of = groups
            .iter()
            .flat_map(|g| g.members.iter().map(|&pid| (pid, (g.label.clone(), g.root))))
            .collect();
        if self.group_by == GroupBy::Cgroup {
            self.refresh_tree(&list);
        }
        let mut procs: HashMap<u32, Proc> = list.into_iter().map(|p| (p.pid, p)).collect();
        if self.holding && self.refreshes > 2 {
            // Same rows as before, with fresh numbers; what exited stays listed as ended.
            let mut held: Vec<u32> = self.all_groups().flat_map(|g| g.members.clone()).collect();
            held.extend(self.tree.iter().flat_map(Node::walk).flat_map(|n| n.procs.clone()));
            self.ended = held.into_iter().filter(|pid| !procs.contains_key(pid)).collect();
            for pid in &self.ended {
                if let Some(p) = self.procs.remove(pid) {
                    procs.insert(*pid, p);
                }
            }
            self.procs = procs;
            self.background = hold_rows(&self.background, &groups);
            self.apps = hold_rows(&self.apps, &groups);
            let rss = |pid: u32| self.procs.get(&pid).map(|p| p.rss_kib);
            for n in &mut self.tree {
                n.recount(&rss);
            }
            return;
        }
        self.ended.clear();
        self.procs = procs;
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

    /// Rebuilds the cgroup tree, unless rows are held (then only the kernel's numbers
    /// change; [`Node::recount`] updates the sums).
    fn refresh_tree(&mut self, list: &[Proc]) {
        // Built right away when the view was just switched on (the pointer is on the
        // switch, so rows would otherwise be held empty).
        if self.tree.is_empty() || !(self.holding && self.refreshes > 2) {
            let items: Vec<(u32, &str, u64)> = list
                .iter()
                .filter(|p| p.uid == self.uid && !p.cgroup.is_empty())
                .map(|p| (p.pid, p.cgroup.as_str(), p.rss_kib))
                .collect();
            self.tree = cgroup::build_tree(&items);
        }
        self.cgroup_memory = self
            .tree
            .iter()
            .flat_map(Node::walk)
            .map(|n| (n.path.clone(), memory::read_cgroup_memory(&n.path)))
            .collect();
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
        let reread = !matches!(action, Action::Show(_) | Action::Uninstall(_) | Action::Details(_));
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
            Action::Details(pid) => {
                if let Some(p) = self.procs.get(&pid) {
                    self.details = Some(Details {
                        proc: p.clone(),
                        info: memory::read_details(pid),
                        read: Instant::now(),
                        ended: self.ended.contains(&pid),
                    });
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
            (Some(Warning::LowMemory), true) => "Your computer is running low on memory",
            (Some(Warning::LowMemory), false) => "Memory is back to normal",
            (Some(Warning::Busy), _) => "A program in the background keeps the CPU busy",
            (None, _) => "What's running",
        };
        ui.heading(headline);
        ui.horizontal(|ui| {
            ui.label(s.summary());
            if self.holding {
                ui.label(
                    RichText::new("· rows stay in place while the pointer is here").weak().small(),
                );
            }
        });
        if self.mode == Some(Warning::LowMemory) && stressed {
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
            if g.has_window && !self.ended.contains(&g.root) {
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
                _ if self.ended.contains(&g.root) => {
                    ui.label(RichText::new("ended").weak());
                }
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
                && !self.ended.contains(&g.root)
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

    /// The result, and after `snap remove` stopped at "directory not empty", the offer to
    /// finish (close what still runs of it, move its data folder to the Trash, retry).
    fn done_dialog(
        &mut self,
        ctx: &egui::Context,
        label: String,
        plan: uninstall::Plan,
        result: Result<String, String>,
    ) -> Option<Uninstall> {
        // `snap remove` stops when the app's data folder isn't empty (something of it
        // was still writing there): offer to finish the job.
        let leftover = match (&plan.source, &result) {
            (uninstall::Source::Snap { name }, Err(e)) => {
                uninstall::snap_leftover(e, name, &self.home)
            }
            _ => None,
        };
        let (mut ok, mut finish) = (false, false);
        modal(ctx, |ui| {
            ui.heading(label.as_str());
            match &result {
                Ok(msg) => ui.label(msg.as_str()),
                Err(e) => ui.label(
                    RichText::new(format!("Not uninstalled: {e}"))
                        .color(ui.visuals().error_fg_color),
                ),
            };
            if let Some(dir) = &leftover {
                ui.add_space(6.0);
                ui.label(format!(
                    "snap stopped at the last step: it couldn't delete {label}'s data \
                     folder {dir}, because something was still writing to it. The app \
                     is already gone from your menu. appkey can close anything of it \
                     that still runs, move that folder to the Trash (snap deletes it \
                     anyway; this way you can still restore it) and finish."
                ));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if let Some(dir) = &leftover {
                    let short = procs::short_path(dir, &self.home);
                    finish = ui.button(format!("Move {short} to the Trash and finish")).clicked();
                }
                ok = ui.button(if leftover.is_some() { "Not now" } else { "OK" }).clicked();
            });
        });
        if finish && let Some(dir) = leftover {
            let (tx, rx) = std::sync::mpsc::channel();
            let job = plan.clone();
            std::thread::spawn(move || {
                let _ = tx
                    .send(uninstall::finish_snap_removal(&job, &dir).map_err(|e| format!("{e:#}")));
            });
            Some(Uninstall::Running { label, plan, rx })
        } else {
            (!ok).then_some(Uninstall::Done { label, plan, result })
        }
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
                        let job = plan.clone();
                        std::thread::spawn(move || {
                            let _ = tx
                                .send(uninstall::run(&job, &targets).map_err(|e| format!("{e:#}")));
                        });
                        Some(Uninstall::Running { label, plan, rx })
                    }
                    Some(false) => None,
                    None => Some(Uninstall::Confirm { root, label, plan }),
                }
            }
            Uninstall::Running { label, plan, rx } => match rx.try_recv() {
                Ok(result) => {
                    self.refresh();
                    Some(Uninstall::Done { label, plan, result })
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
                    Some(Uninstall::Running { label, plan, rx })
                }
                Err(_) => None,
            },
            Uninstall::Done { label, plan, result } => self.done_dialog(ctx, label, plan, result),
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
                if name_label(ui, &text).on_hover_text(self.hover(pid, None)).clicked() {
                    action = Some(Action::Details(pid));
                }
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
                _ if self.ended.contains(&pid) => {
                    ui.label(RichText::new("ended").weak());
                }
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
                    if details_button(ui) {
                        action = Some(Action::Details(pid));
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

    /// The user's processes in the cgroup tree: slices, services and scopes, with what
    /// the kernel counts for each (`memory.current`) and any limit set on it.
    fn cgroup_view(&mut self, ui: &mut egui::Ui) -> Option<Action> {
        ui.label(
            RichText::new(
                "Memory as the kernel accounts it, which includes file cache, so it can be \
                 more than the processes' own use. Limits are set by systemd (MemoryHigh, \
                 MemoryMax, MemorySwapMax).",
            )
            .weak()
            .small(),
        );
        ui.add_space(4.0);
        if self.tree.is_empty() {
            // Not built yet (the view was just switched on): build it from what's listed
            // now, so "couldn't read" below only shows when there's really nothing.
            let list: Vec<Proc> = self.procs.values().cloned().collect();
            self.refresh_tree(&list);
        }
        let tree = std::mem::take(&mut self.tree);
        let mut action = None;
        egui::Grid::new("cgroups")
            .num_columns(5)
            .spacing([8.0, 4.0])
            .striped(true)
            .min_col_width(0.0)
            .show(ui, |ui| {
                for h in ["", "Memory", "CPU", "Running", ""] {
                    ui.label(RichText::new(h).small().weak());
                }
                ui.end_row();
                for n in &tree {
                    if let Some(a) = self.cgroup_rows(ui, n, 0) {
                        action = Some(a);
                    }
                }
            });
        self.tree = tree;
        if self.tree.is_empty() {
            ui.label("Couldn't read the processes' cgroups (is this cgroup v2?).");
        }
        action
    }

    fn cgroup_rows(&mut self, ui: &mut egui::Ui, n: &Node, depth: usize) -> Option<Action> {
        let mut action = None;
        // Big ones start open; a click flips that.
        let open = (n.rss_kib >= 5 * MIN_KIB) != self.toggled.contains(&n.path);
        let kernel = self.cgroup_memory.get(&n.path).copied().unwrap_or_default();
        #[allow(clippy::cast_precision_loss)]
        let indent = 18.0 * depth as f32;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            if disclosure(ui, open).clicked() && !self.toggled.remove(&n.path) {
                self.toggled.insert(n.path.clone());
            }
            let ours = n.name.ends_with(cgroup::SLICE);
            let mut name = RichText::new(format!("{}  ({})", n.name, n.count)).strong();
            if ours {
                name = name.color(ui.visuals().selection.stroke.color);
            }
            let mut hover = n.path.clone();
            if let Some(d) = cgroup::describe(&n.name) {
                ui.add(egui::Label::new(name)).on_hover_text(&hover);
                ui.label(RichText::new(d).weak());
            } else {
                let _ = write!(hover, "\n{} process(es)", n.count);
                ui.add(egui::Label::new(name)).on_hover_text(hover);
            }
            if let Some(limit) = limit_text(kernel) {
                ui.label(RichText::new(limit).color(ui.visuals().warn_fg_color).small());
            }
        });
        let used = kernel.current.map_or(n.rss_kib, |b| b / 1024);
        cell(ui, memory::format_kib(used), MEM_W)
            .on_hover_text(format!("Processes' own memory: {}", memory::format_kib(n.rss_kib)));
        cell(ui, cpu_text(self.node_cpu(n)), CPU_W);
        cell(ui, "", AGE_W);
        ui.label("");
        ui.end_row();
        if !open {
            return None;
        }
        for c in &n.children {
            if let Some(a) = self.cgroup_rows(ui, c, depth + 1) {
                action = Some(a);
            }
        }
        let all = self.show_all_cgroups.contains(&n.path);
        let shown = if all { n.procs.len() } else { MAX_CHILDREN.min(n.procs.len()) };
        for &pid in &n.procs[..shown] {
            if let Some(a) = self.proc_row(ui, pid, indent + 36.0) {
                action = Some(a);
            }
        }
        if shown < n.procs.len() {
            let rest = &n.procs[shown..];
            let kib: u64 = rest.iter().filter_map(|p| self.procs.get(p)).map(|p| p.rss_kib).sum();
            ui.horizontal(|ui| {
                ui.add_space(indent + 36.0);
                let text = format!("… {} more ({})", rest.len(), memory::format_kib(kib));
                if ui.link(text).clicked() {
                    self.show_all_cgroups.insert(n.path.clone());
                }
            });
            ui.end_row();
        }
        action
    }

    /// One process in the cgroup view: what it is, its numbers, and End.
    fn proc_row(&self, ui: &mut egui::Ui, pid: u32, indent: f32) -> Option<Action> {
        let p = self.procs.get(&pid)?;
        let mut action = None;
        ui.horizontal(|ui| {
            ui.add_space(indent);
            let name = truncate(&self.qualified_name(pid, p), 56);
            if name_label(ui, &name).on_hover_text(self.hover(pid, None)).clicked() {
                action = Some(Action::Details(pid));
            }
        });
        cell(ui, RichText::new(memory::format_kib(p.rss_kib)).weak(), MEM_W);
        cell(ui, RichText::new(cpu_text(self.cpu.get(&pid).copied().unwrap_or(0.0))).weak(), CPU_W);
        cell(ui, RichText::new(self.age_text(pid)).weak(), AGE_W);
        ui.horizontal(|ui| match self.closing_procs.get(&pid) {
            _ if self.ended.contains(&pid) => {
                ui.label(RichText::new("ended").weak());
            }
            Some(t) if t.elapsed() >= GRACE => {
                if ui.small_button("Force quit").clicked() {
                    action = Some(Action::ForceProc(pid));
                }
            }
            Some(_) => {
                ui.label(RichText::new("closing…").weak());
            }
            None => {
                if ui.small_button("End").on_hover_text("End just this process").clicked() {
                    action = Some(Action::End(pid));
                }
                if details_button(ui) {
                    action = Some(Action::Details(pid));
                }
            }
        });
        ui.end_row();
        action
    }

    /// What a process is, with the app it belongs to when it's one of that app's helpers:
    /// "Chrome › Tab", "Slack › GPU", "`IntelliJ` IDEA › Java: sbt-launch.jar".
    fn qualified_name(&self, pid: u32, p: &Proc) -> String {
        let what = procs::describe(p);
        match self.app_of.get(&pid) {
            Some((app, root)) if *root != pid && !what.starts_with(app.as_str()) => {
                format!("{app} › {what}")
            }
            _ => what,
        }
    }

    fn node_cpu(&self, n: &Node) -> f32 {
        n.walk().iter().flat_map(|n| &n.procs).filter_map(|p| self.cpu.get(p)).sum()
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

    /// What the tab shows, in the current grouping, as plain text to paste somewhere:
    /// every process of each listed app (or cgroup), with its numbers, pid and command.
    fn tree_text(&self) -> String {
        let mut out = String::new();
        let grouping = if self.group_by == GroupBy::Cgroup { "cgroup" } else { "app" };
        let _ = write!(out, "Processes by {grouping}");
        if let Some(s) = &self.snapshot {
            let _ = write!(
                out,
                ", {} of {} free",
                memory::format_kib(s.mem.available_kib),
                memory::format_kib(s.mem.total_kib)
            );
        }
        out.push('\n');
        let mut rows = Vec::new();
        if self.group_by == GroupBy::Cgroup {
            out.push('\n');
            for n in &self.tree {
                self.cgroup_text(&mut rows, n, "", "");
            }
        } else {
            for (title, groups) in
                [("Left running in the background", &self.background), ("Apps", &self.apps)]
            {
                if groups.is_empty() {
                    continue;
                }
                rows.push(Row::heading(title));
                for g in groups {
                    let cpu = cpu_text(self.group_cpu(g));
                    let mut name = format!("{} ({})", g.label, g.members.len());
                    if !g.detail.is_empty() {
                        let _ = write!(name, "  {}", truncate(&g.detail, 60));
                    }
                    rows.push(Row::new(name, memory::format_kib(g.rss_kib), cpu, String::new()));
                    for (i, &pid) in g.members.iter().enumerate() {
                        let last = i + 1 == g.members.len();
                        rows.extend(self.proc_text(pid, if last { "└─ " } else { "├─ " }));
                    }
                }
            }
            rows.push(Row::heading(&format!(
                "(apps under {} and idle are not listed)",
                memory::format_kib(MIN_KIB)
            )));
        }
        out.push_str(&align_rows(&rows));
        out
    }

    fn cgroup_text(&self, rows: &mut Vec<Row>, n: &Node, prefix: &str, branch: &str) {
        let kernel = self.cgroup_memory.get(&n.path).copied().unwrap_or_default();
        let used = kernel.current.map_or(n.rss_kib, |b| b / 1024);
        let mut name = format!("{prefix}{branch}{} ({})", n.name, n.count);
        if let Some(limit) = limit_text(kernel) {
            let _ = write!(name, "  [{limit}]");
        }
        rows.push(Row::new(
            name,
            memory::format_kib(used),
            cpu_text(self.node_cpu(n)),
            String::new(),
        ));
        // What's below this node lines up under its name.
        let below = match branch {
            "├─ " => format!("{prefix}│  "),
            "└─ " => format!("{prefix}   "),
            _ => prefix.to_owned(),
        };
        let count = n.children.len() + n.procs.len();
        for (i, c) in n.children.iter().enumerate() {
            self.cgroup_text(rows, c, &below, if i + 1 == count { "└─ " } else { "├─ " });
        }
        for (i, &pid) in n.procs.iter().enumerate() {
            let last = n.children.len() + i + 1 == count;
            rows.extend(
                self.proc_text(pid, &format!("{below}{}", if last { "└─ " } else { "├─ " })),
            );
        }
    }

    fn proc_text(&self, pid: u32, prefix: &str) -> Option<Row> {
        let p = self.procs.get(&pid)?;
        let what = if self.group_by == GroupBy::Cgroup {
            self.qualified_name(pid, p)
        } else {
            procs::describe(p)
        };
        let mut name = format!("{prefix}{what}");
        if self.ended.contains(&pid) {
            name.push_str(" (ended)");
        }
        let cpu = cpu_text(self.cpu.get(&pid).copied().unwrap_or(0.0));
        let mut row = Row::new(name, memory::format_kib(p.rss_kib), cpu, self.age_text(pid));
        row.extra = format!("pid {pid}  {}", truncate(&p.cmdline.join(" "), 120));
        Some(row)
    }

    /// Close selected, sorting and grouping, and Dismiss / Snooze while warning.
    pub fn footer(&mut self, ui: &mut egui::Ui) -> Option<Answer> {
        let mut action = None;
        let mut answer = None;
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
            ui.label("Group by");
            let before = self.group_by;
            ui.selectable_value(&mut self.group_by, GroupBy::App, "app");
            ui.selectable_value(&mut self.group_by, GroupBy::Cgroup, "cgroup").on_hover_text(
                "How the system accounts memory: systemd slices, services and scopes. \
                 Apps started with the memory limit on share one slice.",
            );
            if self.group_by != before {
                self.tree.clear();
                self.refresh();
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
            ui.separator();
            if ui
                .button("Copy process tree")
                .on_hover_text(
                    "Copies what's listed, as grouped now, as text: names, memory, CPU, pids \
                     and commands. Commands can contain paths or tokens; check before sharing.",
                )
                .clicked()
            {
                let text = self.tree_text();
                let lines = text.lines().count();
                ui.ctx().copy_text(text);
                self.status = Some(format!("Copied the process tree ({lines} lines)."));
            }
            if self.mode.is_some() {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("Dismiss").clicked() {
                        answer = Some(Answer::Dismiss);
                    }
                    if ui.button("Snooze for an hour").clicked() {
                        answer = Some(Answer::Snooze);
                    }
                });
            }
        });
        if let Some(s) = &self.status {
            ui.label(RichText::new(s).weak());
        }
        if action.is_some() {
            self.pending = action;
        }
        answer
    }
}

/// Fixed column widths, so buttons don't move when numbers change.
const MEM_W: f32 = 56.0;
const CPU_W: f32 = 60.0;
const AGE_W: f32 = 60.0;

fn cell(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>, width: f32) -> egui::Response {
    ui.add_sized([width, 18.0], egui::Label::new(text))
}

/// "limit 12 GB" (throttled above) and the hard maximum, when systemd set them.
fn limit_text(m: CgroupMemory) -> Option<String> {
    let gb = |b: u64| memory::format_kib(b / 1024);
    let mut parts = Vec::new();
    if let Some(x) = m.max {
        parts.push(format!("limit {}", gb(x)));
    }
    if let Some(h) = m.high {
        parts.push(format!("slows from {}", gb(h)));
    }
    if let Some(s) = m.swap.filter(|&s| s >= 1 << 20) {
        parts.push(format!("{} in swap", gb(s)));
    }
    if let Some(s) = m.swap_max {
        parts.push(format!("swap up to {}", gb(s)));
    }
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// One line of the copied process tree.
struct Row {
    name: String,
    memory: String,
    cpu: String,
    age: String,
    extra: String,
    heading: bool,
}

impl Row {
    fn new(name: String, memory: String, cpu: String, age: String) -> Self {
        Self { name, memory, cpu, age, extra: String::new(), heading: false }
    }

    fn heading(text: &str) -> Self {
        Self {
            heading: true,
            ..Self::new(text.to_owned(), String::new(), String::new(), String::new())
        }
    }
}

/// The rows as text, with the number columns lined up after the longest name.
fn align_rows(rows: &[Row]) -> String {
    let width = |f: fn(&Row) -> &str| {
        rows.iter().filter(|r| !r.heading).map(|r| f(r).chars().count()).max().unwrap_or(0)
    };
    let (name_w, mem_w, cpu_w, age_w) =
        (width(|r| &r.name), width(|r| &r.memory), width(|r| &r.cpu), width(|r| &r.age));
    let mut out = String::new();
    for r in rows {
        if r.heading {
            let _ = writeln!(out, "\n{}", r.name);
            continue;
        }
        let pad = name_w - r.name.chars().count();
        let _ = writeln!(
            out,
            "{}{:pad$}  {:>mem_w$}  {:>cpu_w$}  {:>age_w$}  {}",
            r.name, "", r.memory, r.cpu, r.age, r.extra
        );
    }
    out.lines().map(str::trim_end).collect::<Vec<_>>().join("\n") + "\n"
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

/// The rows already listed, in the same order and with the same processes, but with
/// sizes and labels from `fresh`. Groups that are gone keep their last state.
fn hold_rows(listed: &[AppGroup], fresh: &[AppGroup]) -> Vec<AppGroup> {
    listed
        .iter()
        .map(|old| match fresh.iter().find(|g| g.root == old.root) {
            Some(g) => AppGroup { members: old.members.clone(), ..g.clone() },
            None => old.clone(),
        })
        .collect()
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

impl ProcessesTab {
    /// The tab's body. Call [`Self::finish`] after the footer.
    pub fn show(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        // Rows come and go (tabs, short-lived helpers) and would shift under the pointer,
        // so while it's here only the numbers change. Not before CPU use is known (two
        // samples), so the first order already reflects it.
        self.holding = self.refreshes >= 2 && ctx.input(|i| i.pointer.has_pointer());
        if self.refreshed.elapsed() >= REFRESH {
            self.refresh();
        }
        ctx.request_repaint_after(Duration::from_millis(500));
        let mut action = None;
        self.header(ui);
        ui.add_space(8.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            if self.group_by == GroupBy::Cgroup {
                action = self.cgroup_view(ui);
                return;
            }
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
        if action.is_some() {
            self.pending = action;
        }
    }

    /// Everything about one process, in a popup that stays until closed. While open its
    /// numbers are re-read every 2 s; if it ends, the last ones stay, marked as ended.
    fn details_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut d) = self.details.take() else { return };
        let pid = d.proc.pid;
        if !d.ended && d.read.elapsed() >= REFRESH {
            let same = |p: &&Proc| p.start_ticks == d.proc.start_ticks;
            match self.procs.get(&pid).filter(same).filter(|_| !self.ended.contains(&pid)) {
                Some(p) => {
                    d.proc = p.clone();
                    d.info = memory::read_details(pid);
                }
                None => d.ended = true,
            }
            d.read = Instant::now();
        }
        ctx.request_repaint_after(Duration::from_secs(1));
        let title = self.qualified_name(pid, &d.proc);
        let rows = self.details_rows(&d);
        let mut close = false;
        // Fits the window; long values and arguments wrap.
        let width = (ctx.content_rect().width() - 80.0).clamp(300.0, 720.0);
        let shown = egui::Modal::new(egui::Id::new("process-details")).show(ctx, |ui| {
            ui.set_width(width);
            ui.heading(&title);
            if d.ended {
                ui.label(
                    RichText::new("This process has ended. These are its last details.")
                        .color(ui.visuals().warn_fg_color),
                );
            }
            ui.add_space(6.0);
            egui::Grid::new("process-details-grid").num_columns(2).spacing([12.0, 4.0]).show(
                ui,
                |ui| {
                    for (k, v) in &rows {
                        ui.label(RichText::new(*k).weak());
                        ui.add(egui::Label::new(v).wrap());
                        ui.end_row();
                    }
                },
            );
            ui.add_space(8.0);
            let args = &d.proc.cmdline;
            ui.label(RichText::new(format!("Command, {} argument(s)", args.len())).weak());
            egui::ScrollArea::vertical().max_height(240.0).auto_shrink([false, true]).show(
                ui,
                |ui| {
                    for a in args {
                        ui.add(egui::Label::new(RichText::new(a).monospace()).wrap());
                    }
                },
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Copy").on_hover_text("Copies these details as text").clicked() {
                    ui.ctx().copy_text(details_text(&title, &rows, args));
                }
                if ui.button("Close").clicked() {
                    close = true;
                }
            });
        });
        if !(close || shown.should_close()) {
            self.details = Some(d);
        }
    }

    fn details_rows(&self, d: &Details) -> Vec<(&'static str, String)> {
        let p = &d.proc;
        let st = &d.info.status;
        let kib = |k: Option<u64>| k.map(memory::format_kib);
        let mut rows = Vec::new();
        if let Some((app, root)) = self.app_of.get(&p.pid) {
            let main = if *root == p.pid { ", its main process" } else { "" };
            rows.push(("App", format!("{app}{main}")));
        }
        rows.push(("Process", format!("{} (pid {})", p.name, p.pid)));
        let parent = self.procs.get(&p.ppid).map(|q| format!(", {}", procs::describe(q)));
        rows.push(("Parent", format!("pid {}{}", p.ppid, parent.unwrap_or_default())));
        let mut state = st.state.clone();
        if let Some(t) = st.threads {
            let _ = write!(state, ", {t} thread(s)");
        }
        rows.push(("State", state));
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let age = (self.uptime - p.start_ticks as f64 / self.hz.max(1) as f64).max(0.0) as u64;
        rows.push(("Running for", procs::format_age(age)));
        rows.push((
            "CPU",
            format!(
                "{} now, {} in total",
                cpu_text(self.cpu.get(&p.pid).copied().unwrap_or(0.0)),
                procs::format_age(p.cpu_ticks / self.hz.max(1))
            ),
        ));
        let mut ram = format!("{} in RAM", memory::format_kib(p.rss_kib));
        if let (Some(a), Some(f), Some(s)) =
            (kib(st.rss_anon_kib), kib(st.rss_file_kib), kib(st.rss_shmem_kib))
        {
            let _ = write!(ram, ": {a} its own, {f} files, {s} shared");
        }
        rows.push(("Memory", ram));
        if let Some(s) = kib(st.swap_kib) {
            rows.push(("In swap", s));
        }
        if let Some(h) = kib(st.peak_rss_kib) {
            rows.push(("Most in RAM", h));
        }
        if let Some(o) = d.info.oom_score {
            let mut text =
                format!("{o} of 1000 (when memory runs out, Linux ends the highest first)");
            if let Some(adj) = d.info.oom_score_adj.filter(|a| *a != 0) {
                let _ = write!(text, ", adjusted by {adj}");
            }
            rows.push(("OOM score", text));
        }
        if let Some(exe) = &d.info.exe {
            rows.push(("Program", exe.clone()));
        }
        if let Some(cwd) = &p.cwd {
            rows.push(("Folder", cwd.clone()));
        }
        if let Some(t) = self.titles.get(&p.pid).filter(|t| !t.is_empty()) {
            rows.push(("Windows", t.join("\n")));
        }
        if !p.cgroup.is_empty() {
            let mut c = p.cgroup.clone();
            let last = p.cgroup.rsplit('/').next().unwrap_or_default();
            if let Some(what) = cgroup::describe(last) {
                let _ = write!(c, "\n({what})");
            }
            rows.push(("cgroup", c));
        }
        rows.push(("Terminal", if p.tty { "yes" } else { "no" }.into()));
        rows
    }

    /// The uninstall dialog, and what was clicked in the tab this frame.
    pub fn finish(&mut self, ctx: &egui::Context) {
        self.uninstall_dialog(ctx);
        self.details_dialog(ctx);
        if let Some(a) = self.pending.take().filter(|_| self.uninstall.is_none()) {
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
    let whole = if matches!(
        plan.source,
        uninstall::Source::Snap { .. } | uninstall::Source::Flatpak { .. }
    ) {
        ", and anything else of it still running"
    } else {
        ""
    };
    ui.label(format!(
        "appkey first closes {label} ({n} process{}{whole}), then runs:",
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
    Checking {
        root: u32,
        label: String,
        rx: std::sync::mpsc::Receiver<uninstall::Plan>,
    },
    Confirm {
        root: u32,
        label: String,
        plan: uninstall::Plan,
    },
    Running {
        label: String,
        plan: uninstall::Plan,
        rx: std::sync::mpsc::Receiver<Result<String, String>>,
    },
    Done {
        label: String,
        plan: uninstall::Plan,
        result: Result<String, String>,
    },
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

/// A process's name, which opens its details when clicked.
fn name_label(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(egui::Label::new(text).sense(egui::Sense::click()))
        .on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// The "Details" button of a process row; `true` when clicked.
fn details_button(ui: &mut egui::Ui) -> bool {
    ui.small_button("Details")
        .on_hover_text("Everything about this process: the full command, memory, parent…")
        .clicked()
}

/// The details popup as text, for the clipboard.
fn details_text(title: &str, rows: &[(&str, String)], args: &[String]) -> String {
    let mut out = format!("{title}\n");
    for (k, v) in rows {
        let _ = writeln!(out, "{k}: {}", v.replace('\n', "\n    "));
    }
    out.push_str("Command:\n");
    for a in args {
        let _ = writeln!(out, "    {a}");
    }
    out
}

fn modal(ctx: &egui::Context, body: impl FnOnce(&mut egui::Ui)) {
    egui::Modal::new(egui::Id::new("uninstall")).show(ctx, |ui| {
        ui.set_max_width(520.0);
        body(ui);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(root: u32, members: &[u32], rss_kib: u64) -> AppGroup {
        AppGroup {
            root,
            label: format!("app {root}"),
            detail: String::new(),
            members: members.to_vec(),
            rss_kib,
            has_window: false,
            detached: false,
        }
    }

    #[test]
    fn copied_rows_line_up() {
        let mut rows = vec![
            Row::heading("Apps"),
            Row::new("Chrome (2)".into(), "1.2 GB".into(), "5% CPU".into(), String::new()),
            Row::new("└─ tab".into(), "300 MB".into(), "–".into(), "2 min".into()),
        ];
        rows[2].extra = "pid 7  chrome --type=renderer".into();
        assert_eq!(
            align_rows(&rows),
            "\nApps\nChrome (2)  1.2 GB  5% CPU\n└─ tab      300 MB       –  2 min  pid 7  chrome --type=renderer\n"
        );
    }

    #[test]
    fn held_rows_update_numbers_but_not_layout() {
        let listed = [group(1, &[1, 2, 3], 100), group(5, &[5], 50)];
        // 2 exited, 4 is new, 5 is gone, 9 is a new app.
        let fresh = [group(9, &[9], 900), group(1, &[4, 3, 1], 300)];
        let held = hold_rows(&listed, &fresh);
        assert_eq!(held.iter().map(|g| g.root).collect::<Vec<_>>(), [1, 5]);
        assert_eq!(held[0].members, [1, 2, 3]);
        assert_eq!(held[0].rss_kib, 300);
        assert_eq!(held[1], listed[1]);
    }
}

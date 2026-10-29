//! Ubuntu / Linux Mint only: the low-memory warning and the Magic `SysRq`
//! emergency keys (settings tab "Memory").
//!
//! The daemon reads `/proc/meminfo` and `/proc/pressure/memory` once a second.
//! When memory stays low it opens `appkey memory --alert`, which lists the
//! user's biggest programs and can ask them to quit (`SIGTERM`), or force them
//! (`SIGKILL`). Only the user's own processes: the kernel refuses anything else,
//! and we never elevate. Parsers are platform-neutral and unit-tested on every
//! host; reading `/proc` and signalling are Linux-only.

pub mod cgroup;
pub mod procs;
pub mod sysrq;
pub mod uninstall;

use std::collections::{HashMap, VecDeque};
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use crate::config::MemoryWatch;

/// Whether this is Ubuntu or a derivative (Linux Mint, Pop!_OS…), from `/etc/os-release`.
pub fn is_supported_distro(os_release: &str) -> bool {
    os_release
        .lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| matches!(k.trim(), "ID" | "ID_LIKE"))
        .flat_map(|(_, v)| v.trim().trim_matches(['"', '\'']).split_whitespace())
        .any(|id| matches!(id, "ubuntu" | "linuxmint"))
}

/// Sizes from `/proc/meminfo`, in KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_field_names)]
pub struct MemInfo {
    pub total_kib: u64,
    pub available_kib: u64,
    pub swap_total_kib: u64,
}

impl MemInfo {
    pub fn available_percent(&self) -> u64 {
        (self.available_kib * 100).checked_div(self.total_kib).unwrap_or(100)
    }
}

pub fn parse_meminfo(text: &str) -> Option<MemInfo> {
    let field = |name: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse().ok())
    };
    Some(MemInfo {
        total_kib: field("MemTotal")?,
        available_kib: field("MemAvailable")?,
        swap_total_kib: field("SwapTotal").unwrap_or(0),
    })
}

/// The `some avg10` value of `/proc/pressure/memory`: the share of the last 10 s
/// in which at least one task waited for memory.
pub fn parse_pressure(text: &str) -> Option<f32> {
    text.lines()
        .find_map(|l| l.strip_prefix("some "))?
        .split_whitespace()
        .find_map(|f| f.strip_prefix("avg10="))?
        .parse()
        .ok()
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub mem: MemInfo,
    /// `None` when the kernel has no PSI.
    pub pressure: Option<f32>,
}

impl Snapshot {
    pub fn is_stressed(&self, watch: MemoryWatch) -> bool {
        let low = self.mem.available_percent() < u64::from(watch.min_available_percent);
        let stalled = watch.max_pressure_percent > 0
            && self.pressure.is_some_and(|p| p >= f32::from(watch.max_pressure_percent));
        low || stalled
    }

    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} of {} free",
            format_kib(self.mem.available_kib),
            format_kib(self.mem.total_kib)
        );
        if let Some(p) = self.pressure.filter(|p| *p >= 1.0) {
            let _ = write!(s, " · programs waited on memory {p:.0}% of the last 10 s");
        }
        s
    }
}

/// The parts of `/proc/<pid>/status` we need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcStatus {
    pub name: String,
    pub uid: u32,
    pub rss_kib: u64,
}

/// Kernel threads have no `VmRSS`, so they come back as `None`.
pub fn parse_status(text: &str) -> Option<ProcStatus> {
    let field = |name: &str| text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':'));
    Some(ProcStatus {
        name: field("Name")?.trim().to_owned(),
        uid: field("Uid")?.split_whitespace().next()?.parse().ok()?,
        rss_kib: field("VmRSS")?.split_whitespace().next()?.parse().ok()?,
    })
}

/// More of `/proc/<pid>/status`, for a process's details. Sizes in KiB.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StatusDetails {
    /// e.g. "S (sleeping)".
    pub state: String,
    pub threads: Option<u64>,
    /// Its own memory (heap, stacks).
    pub rss_anon_kib: Option<u64>,
    /// Mapped files: its program, libraries, files it maps.
    pub rss_file_kib: Option<u64>,
    /// Shared memory and tmpfs.
    pub rss_shmem_kib: Option<u64>,
    pub swap_kib: Option<u64>,
    /// The most it has ever been resident.
    pub peak_rss_kib: Option<u64>,
}

pub fn parse_status_details(text: &str) -> StatusDetails {
    let field = |name: &str| {
        text.lines().find_map(|l| l.strip_prefix(name)?.strip_prefix(':')).map(str::trim)
    };
    let kib = |name: &str| field(name)?.split_whitespace().next()?.parse().ok();
    StatusDetails {
        state: field("State").unwrap_or_default().to_owned(),
        threads: field("Threads").and_then(|t| t.parse().ok()),
        rss_anon_kib: kib("RssAnon"),
        rss_file_kib: kib("RssFile"),
        rss_shmem_kib: kib("RssShmem"),
        swap_kib: kib("VmSwap"),
        peak_rss_kib: kib("VmHWM"),
    }
}

/// Desktop and session processes (as `comm`, max 15 chars): closing them would end
/// or break the session, so the dialog never offers them.
const PROTECTED: &[&str] = &[
    "appkey",
    "systemd",
    "dbus-daemon",
    "dbus-broker",
    "Xorg",
    "Xwayland",
    "cinnamon",
    "cinnamon-sessio",
    "cinnamon-launch",
    "nemo-desktop",
    "muffin",
    "gnome-shell",
    "gnome-session-b",
    "mate-session",
    "marco",
    "xfce4-session",
    "xfwm4",
    "xfdesktop",
    "plasmashell",
    "ksmserver",
    "kwin_x11",
    "kwin_wayland",
    "pipewire",
    "pipewire-pulse",
    "wireplumber",
    "pulseaudio",
];
const PROTECTED_PREFIXES: &[&str] = &["csd-", "gsd-", "gvfs", "at-spi", "xdg-", "ibus"];

pub fn is_protected(name: &str) -> bool {
    PROTECTED.contains(&name) || PROTECTED_PREFIXES.iter().any(|p| name.starts_with(p))
}

#[allow(clippy::cast_precision_loss)]
pub fn format_kib(kib: u64) -> String {
    let mib = kib as f64 / 1024.0;
    if mib >= 1024.0 { format!("{:.1} GB", mib / 1024.0) } else { format!("{mib:.0} MB") }
}

/// When to open the warning: memory must stay low for a few seconds, and after
/// the dialog closes we stay quiet for a while (longer when snoozed).
#[derive(Debug, Default)]
pub struct Debounce {
    streak: u32,
    quiet_until: Option<Instant>,
}

impl Debounce {
    const STREAK: u32 = 3;
    const COOLDOWN: Duration = Duration::from_mins(5);
    const SNOOZE: Duration = Duration::from_hours(1);

    /// Call once a second. Returns `true` when the dialog should open.
    pub fn update(&mut self, stressed: bool, now: Instant) -> bool {
        if !stressed {
            self.streak = 0;
            return false;
        }
        self.streak += 1;
        if self.streak < Self::STREAK || self.quiet_until.is_some_and(|t| now < t) {
            return false;
        }
        self.streak = 0;
        true
    }

    pub fn closed(&mut self, now: Instant, snoozed: bool) {
        self.quiet_until = Some(now + if snoozed { Self::SNOOZE } else { Self::COOLDOWN });
    }
}

/// Share of one core a background program must use, on average, to count as busy.
const BUSY_SHARE: f64 = 0.9;

/// CPU time samples per process, to spot background programs that keep a core busy.
#[derive(Debug, Default)]
pub struct BusyTracker {
    /// `(pid, start_ticks)` → `(seconds since boot, cpu_ticks)`, oldest first.
    samples: HashMap<(u32, u64), VecDeque<(f64, u64)>>,
}

impl BusyTracker {
    /// Records `(pid, start_ticks, cpu_ticks)` of the current candidates at `now`
    /// (seconds since boot) and returns the pids that used at least 90% of a core over
    /// the whole last `window` seconds.
    #[allow(clippy::cast_precision_loss)]
    pub fn update(
        &mut self,
        now: f64,
        procs: &[(u32, u64, u64)],
        window: f64,
        hz: u64,
    ) -> Vec<u32> {
        let live: std::collections::HashSet<(u32, u64)> =
            procs.iter().map(|&(pid, start, _)| (pid, start)).collect();
        self.samples.retain(|k, _| live.contains(k));
        let mut busy = Vec::new();
        for &(pid, start, ticks) in procs {
            let s = self.samples.entry((pid, start)).or_default();
            s.push_back((now, ticks));
            // Keep one sample from before the window as the baseline.
            while s.len() > 2 && s[1].0 <= now - window {
                s.pop_front();
            }
            if let Some(&(t0, ticks0)) = s.front().filter(|(t0, _)| *t0 <= now - window) {
                let used = ticks.saturating_sub(ticks0) as f64 / hz.max(1) as f64;
                if used / (now - t0) >= BUSY_SHARE {
                    busy.push(pid);
                }
            }
        }
        busy
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::process::{Child, Command};
    use std::time::Instant;

    use anyhow::{Context, Result};

    use super::procs::{self, Proc};
    use super::{BusyTracker, Debounce, Snapshot, parse_meminfo, parse_pressure};
    use crate::config::MemoryWatch;

    /// Ubuntu, Linux Mint and other Ubuntu derivatives.
    pub fn available() -> bool {
        ["/etc/os-release", "/usr/lib/os-release"]
            .iter()
            .find_map(|p| fs::read_to_string(p).ok())
            .is_some_and(|t| super::is_supported_distro(&t))
    }

    pub fn read_snapshot() -> Option<Snapshot> {
        let mem = parse_meminfo(&fs::read_to_string("/proc/meminfo").ok()?)?;
        let pressure =
            fs::read_to_string("/proc/pressure/memory").ok().and_then(|t| parse_pressure(&t));
        Some(Snapshot { mem, pressure })
    }

    /// Every process (others' too, so the tree is complete). Details only for ours.
    pub fn read_procs() -> Vec<Proc> {
        let uid = current_uid();
        let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
        dir.filter_map(|e| read_proc(e.ok()?.file_name().to_str()?.parse().ok()?, uid)).collect()
    }

    fn read_proc(pid: u32, uid: u32) -> Option<Proc> {
        let dir = format!("/proc/{pid}");
        let stat = procs::parse_stat(&fs::read_to_string(format!("{dir}/stat")).ok()?)?;
        // Kernel threads have no memory of their own and are never anyone's app.
        let status = super::parse_status(&fs::read_to_string(format!("{dir}/status")).ok()?)?;
        let mut p = Proc {
            pid,
            ppid: stat.ppid,
            uid: status.uid,
            name: status.name,
            rss_kib: status.rss_kib,
            cpu_ticks: stat.cpu_ticks,
            start_ticks: stat.start_ticks,
            tty: stat.tty,
            ..Proc::default()
        };
        if p.uid == uid {
            p.cmdline =
                procs::parse_cmdline(&fs::read(format!("{dir}/cmdline")).unwrap_or_default());
            p.cwd =
                fs::read_link(format!("{dir}/cwd")).ok().map(|c| c.to_string_lossy().into_owned());
            if let Ok(c) = fs::read_to_string(format!("{dir}/cgroup")) {
                p.service = procs::is_service(&c);
                p.cgroup = super::cgroup::parse_proc_cgroup(&c).unwrap_or_default();
            }
        }
        Some(p)
    }

    /// What the details popup shows beyond [`Proc`], read when it's open.
    #[derive(Debug, Clone, Default)]
    pub struct ProcDetails {
        pub status: super::StatusDetails,
        /// The program file (`/proc/<pid>/exe`).
        pub exe: Option<String>,
        /// How likely Linux is to end it when memory runs out (0–1000, higher first).
        pub oom_score: Option<u32>,
        pub oom_score_adj: Option<i32>,
    }

    pub fn read_details(pid: u32) -> ProcDetails {
        let read = |file: &str| fs::read_to_string(format!("/proc/{pid}/{file}")).ok();
        ProcDetails {
            status: read("status").map(|t| super::parse_status_details(&t)).unwrap_or_default(),
            exe: fs::read_link(format!("/proc/{pid}/exe"))
                .ok()
                .map(|e| e.to_string_lossy().into_owned()),
            oom_score: read("oom_score").and_then(|t| t.trim().parse().ok()),
            oom_score_adj: read("oom_score_adj").and_then(|t| t.trim().parse().ok()),
        }
    }

    /// Clock ticks per second (for `cpu_ticks` and `start_ticks`).
    pub fn clock_ticks() -> u64 {
        // SAFETY: sysconf only reads a system constant.
        u64::try_from(unsafe { libc::sysconf(libc::_SC_CLK_TCK) }).unwrap_or(100)
    }

    /// Seconds since boot.
    pub fn uptime() -> f64 {
        fs::read_to_string("/proc/uptime")
            .ok()
            .and_then(|t| t.split_whitespace().next()?.parse().ok())
            .unwrap_or(0.0)
    }

    pub fn current_uid() -> u32 {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    /// Sends `SIGTERM` (or `SIGKILL` when `force`) to each `(pid, start_ticks)` that is
    /// still that same process and ours, so a pid reused since the list was read is left
    /// alone. Returns how many were signalled.
    pub fn signal(targets: &[(u32, u64)], force: bool) -> usize {
        let uid = current_uid();
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        targets
            .iter()
            .filter(|&&(pid, start)| {
                read_proc(pid, u32::MAX).is_some_and(|p| p.start_ticks == start && p.uid == uid)
            })
            .filter_map(|&(pid, _)| libc::pid_t::try_from(pid).ok())
            // SAFETY: kill only sends a signal; the kernel checks we may signal this pid.
            .filter(|&pid| unsafe { libc::kill(pid, sig) } == 0)
            .count()
    }

    /// Which warning the Processes tab shows.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Warning {
        LowMemory,
        Busy,
    }

    /// Asks an open settings window to show its Processes tab: `None` as opened by hand,
    /// or with this warning. A signal, so the window needn't poll anything.
    pub fn window_signal(warning: Option<Warning>) -> libc::c_int {
        match warning {
            None => libc::SIGUSR1,
            Some(Warning::LowMemory) => libc::SIGUSR2,
            Some(Warning::Busy) => libc::SIGRTMIN() + 2,
        }
    }

    /// Where the settings window says the user dismissed (`dismiss`) or snoozed
    /// (`snooze`) a warning, for the daemon to read.
    pub fn alert_answer_path() -> std::path::PathBuf {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map_or_else(std::env::temp_dir, std::path::PathBuf::from);
        dir.join(format!("appkey-{}-alert", current_uid()))
    }

    /// Background CPU use is sampled every this many ticks (seconds).
    const BUSY_SAMPLE_TICKS: u32 = 10;

    /// The settings window showing a warning: one we started, or one that was open.
    #[derive(Debug)]
    enum Shown {
        Child(Child),
        Open { pid: u32, start: u64 },
    }

    impl Shown {
        fn running(&mut self) -> bool {
            match self {
                Self::Child(c) => matches!(c.try_wait(), Ok(None)),
                Self::Open { pid, start } => {
                    read_proc(*pid, u32::MAX).is_some_and(|p| p.start_ticks == *start)
                }
            }
        }
    }

    /// Daemon side: checks memory every tick (and background CPU use every 10) and opens
    /// the warning in the settings window's Processes tab.
    #[derive(Debug)]
    pub struct Watcher {
        supported: bool,
        debounce: Debounce,
        busy_debounce: Debounce,
        busy: BusyTracker,
        ticks: u32,
        dialog: Option<(Shown, Warning)>,
    }

    impl Default for Watcher {
        fn default() -> Self {
            Self {
                supported: available(),
                debounce: Debounce::default(),
                busy_debounce: Debounce::default(),
                busy: BusyTracker::default(),
                ticks: 0,
                dialog: None,
            }
        }
    }

    impl Watcher {
        pub fn tick(&mut self, watch: MemoryWatch) -> Result<()> {
            if !self.supported {
                return Ok(());
            }
            let now = Instant::now();
            if let Some((shown, kind)) = &mut self.dialog {
                // Answered in the window (which may stay open), or the window closed.
                let answer = fs::read_to_string(alert_answer_path()).ok();
                if answer.is_none() && shown.running() {
                    return Ok(());
                }
                let _ = fs::remove_file(alert_answer_path());
                let snoozed = answer.is_some_and(|a| a.trim() == "snooze");
                match kind {
                    Warning::LowMemory => self.debounce.closed(now, snoozed),
                    Warning::Busy => self.busy_debounce.closed(now, snoozed),
                }
                self.dialog = None;
            }
            if watch.enabled {
                let stressed = read_snapshot().is_some_and(|s| s.is_stressed(watch));
                if self.debounce.update(stressed, now) {
                    return self.open(Warning::LowMemory, now);
                }
            }
            self.ticks = self.ticks.wrapping_add(1);
            if watch.busy_background_minutes > 0 && self.ticks.is_multiple_of(BUSY_SAMPLE_TICKS) {
                let window = f64::from(watch.busy_background_minutes) * 60.0;
                let busy = self.background_busy(window);
                if self.busy_debounce.update(busy, now) {
                    return self.open(Warning::Busy, now);
                }
            }
            Ok(())
        }

        /// Whether a program left running in the background (no window, its starter
        /// gone) has kept a core busy for the last `window` seconds.
        fn background_busy(&mut self, window: f64) -> bool {
            let list = read_procs();
            let home =
                dirs::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default();
            // Windows are only looked up once something is busy (cheap on every desktop).
            let no_windows = std::collections::HashMap::new();
            let groups = procs::build_groups(
                &list,
                current_uid(),
                std::process::id(),
                super::is_protected,
                &no_windows,
                &home,
            );
            let candidates: Vec<(u32, u64, u64)> = groups
                .iter()
                .filter(|g| g.detached)
                .flat_map(|g| &g.members)
                .filter_map(|pid| list.iter().find(|p| p.pid == *pid))
                .map(|p| (p.pid, p.start_ticks, p.cpu_ticks))
                .collect();
            let busy = self.busy.update(uptime(), &candidates, window, clock_ticks());
            if busy.is_empty() {
                return false;
            }
            let titles = crate::platform::window_titles();
            groups.iter().any(|g| {
                g.detached
                    && g.members.iter().any(|p| busy.contains(p))
                    && !g.members.iter().any(|p| titles.contains_key(p))
            })
        }

        fn open(&mut self, kind: Warning, now: Instant) -> Result<()> {
            let _ = fs::remove_file(alert_answer_path());
            let shown = match find_window() {
                Some(p) if signal_window(&p, Some(kind)) => {
                    Ok(Shown::Open { pid: p.pid, start: p.start_ticks })
                }
                _ => {
                    let flag = if kind == Warning::Busy { "--busy" } else { "--alert" };
                    std::env::current_exe()
                        .and_then(|exe| Command::new(exe).args(["memory", flag]).spawn())
                        .map(Shown::Child)
                }
            };
            match shown {
                Ok(shown) => {
                    self.dialog = Some((shown, kind));
                    Ok(())
                }
                Err(e) => {
                    match kind {
                        Warning::LowMemory => self.debounce.closed(now, false),
                        Warning::Busy => self.busy_debounce.closed(now, false),
                    }
                    Err(e).context("couldn't open appkey's settings window")
                }
            }
        }
    }

    /// An open settings window (`appkey settings` or `appkey memory …`) of ours.
    pub fn find_window() -> Option<Proc> {
        read_procs().into_iter().find(|p| {
            p.pid != std::process::id()
                && p.uid == current_uid()
                && p.cmdline.get(1).is_some_and(|a| a == "settings" || a == "memory")
                && p.cmdline.first().is_some_and(|a| a.ends_with("appkey"))
        })
    }

    /// Switches an open settings window to its Processes tab and brings it forward.
    fn signal_window(p: &Proc, warning: Option<Warning>) -> bool {
        let Ok(pid) = libc::pid_t::try_from(p.pid) else { return false };
        // SAFETY: kill only sends a signal; the kernel checks we may signal this pid.
        if unsafe { libc::kill(pid, window_signal(warning)) } != 0 {
            return false;
        }
        // A warning stays on top without taking the keyboard; by hand, bring it forward.
        if warning.is_none() {
            let _ = crate::platform::focus_pid(p.pid);
        }
        true
    }

    /// The Processes tab, in the open settings window or a new one (shortcut, tray).
    pub fn show_window() -> Result<()> {
        if let Some(p) = find_window()
            && signal_window(&p, None)
        {
            return Ok(());
        }
        let exe = std::env::current_exe()?;
        crate::platform::spawn(&[exe.to_string_lossy().into_owned(), "memory".into()])
    }

    /// What the settings window shows about a cgroup.
    #[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
    pub struct CgroupMemory {
        /// Bytes in use (`memory.current`), when the memory controller is on for it.
        pub current: Option<u64>,
        pub high: Option<u64>,
        pub max: Option<u64>,
        /// Bytes in swap (`memory.swap.current`) and the swap limit.
        pub swap: Option<u64>,
        pub swap_max: Option<u64>,
    }

    /// Where cgroup v2 is mounted: `/sys/fs/cgroup`, or `…/unified` on hybrid systems.
    fn cgroup_root() -> &'static str {
        static ROOT: std::sync::OnceLock<&str> = std::sync::OnceLock::new();
        ROOT.get_or_init(|| {
            if std::path::Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
                "/sys/fs/cgroup"
            } else {
                "/sys/fs/cgroup/unified"
            }
        })
    }

    pub fn read_cgroup_memory(path: &str) -> CgroupMemory {
        let read = |file: &str| fs::read_to_string(format!("{}{path}/{file}", cgroup_root())).ok();
        CgroupMemory {
            current: read("memory.current").and_then(|t| t.trim().parse().ok()),
            high: read("memory.high").and_then(|t| super::cgroup::parse_limit(&t)),
            max: read("memory.max").and_then(|t| super::cgroup::parse_limit(&t)),
            swap: read("memory.swap.current").and_then(|t| t.trim().parse().ok()),
            swap_max: read("memory.swap.max").and_then(|t| super::cgroup::parse_limit(&t)),
        }
    }

    /// The slice for capped apps, if the user's systemd has created it.
    pub fn cap_cgroup() -> Option<String> {
        let path = format!(
            "/user.slice/user-{uid}.slice/user@{uid}.service/app.slice/{}",
            super::cgroup::SLICE,
            uid = current_uid()
        );
        std::path::Path::new(&format!("{}{path}", cgroup_root())).is_dir().then_some(path)
    }

    /// Whether apps this process launches go into the shared cap (set from the config).
    static CAPPED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    /// Uses `[memory_limit]` for apps this process launches.
    pub fn use_limit(limit: crate::config::MemoryLimit) {
        CAPPED.store(limit.enabled && available(), std::sync::atomic::Ordering::Relaxed);
    }

    pub fn launch_capped() -> bool {
        CAPPED.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Daemon side: sets the shared cap on the slice through the user's systemd, or lifts
    /// it. Apps already in it keep running either way.
    pub fn apply_limit(limit: crate::config::MemoryLimit) -> Result<()> {
        let cap = if limit.enabled && available() {
            let mem = read_snapshot().context("couldn't read /proc/meminfo")?.mem;
            let (total, swap) = (mem.total_kib * 1024, mem.swap_total_kib * 1024);
            Some(super::cgroup::cap_properties(
                limit.limit.bytes(total),
                limit.swap.bytes(swap),
                total,
            ))
        } else if cap_cgroup().is_some() {
            None
        } else {
            return Ok(()); // never used: nothing to lift
        };
        let argv = super::cgroup::set_property_argv(cap);
        let out = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .output()
            .context("couldn't run systemctl to set the memory cap")?;
        if !out.status.success() {
            anyhow::bail!(
                "couldn't set the memory cap: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(())
    }

    /// Starts `argv` through the user's systemd in the capped slice. Falls back to
    /// starting it directly (uncapped) when systemd can't, so the shortcut still works.
    pub fn spawn_capped(argv: &[String], app: &str) -> Result<()> {
        let (program, rest) = argv.split_first().context("empty command")?;
        // systemd searches its own PATH, not ours: hand it the full path.
        let Some(exe) = find_program(program) else {
            anyhow::bail!("starting {program:?}: not found");
        };
        let env: Vec<String> =
            std::env::vars_os().filter_map(|(k, _)| k.into_string().ok()).collect();
        let unit = super::cgroup::unit_name(app, random());
        let run = super::cgroup::run_argv(
            &unit,
            &format!("{app} (started by appkey)"),
            &exe.to_string_lossy(),
            rest,
            &env,
            systemd_version().is_some_and(|v| v >= 250),
        );
        let started = Command::new(&run[0])
            .args(&run[1..])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .output();
        match started {
            Ok(out) if out.status.success() => Ok(()),
            other => {
                let why = match other {
                    Ok(out) => String::from_utf8_lossy(&out.stderr).trim().to_owned(),
                    Err(e) => e.to_string(),
                };
                eprintln!(
                    "appkey: couldn't start {app} in the memory cap ({why}); starting it uncapped"
                );
                crate::platform::spawn(argv)
            }
        }
    }

    fn systemd_version() -> Option<u32> {
        static VERSION: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
        *VERSION.get_or_init(|| {
            let out = Command::new("/usr/bin/systemctl").arg("--version").output().ok()?;
            super::cgroup::parse_systemd_version(&String::from_utf8_lossy(&out.stdout))
        })
    }

    /// `program` itself if it's a path, else the first executable of that name on `PATH`.
    fn find_program(program: &str) -> Option<std::path::PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        let executable = |p: &std::path::Path| {
            fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        };
        if program.contains('/') {
            let p = std::path::PathBuf::from(program);
            return executable(&p).then(|| fs::canonicalize(&p).unwrap_or(p));
        }
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|d| d.join(program))
            .find(|p| executable(p))
    }

    /// Enough to keep unit names apart.
    fn random() -> u32 {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        nanos ^ std::process::id().rotate_left(16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_ubuntu_and_mint() {
        let ubuntu = "NAME=\"Ubuntu\"\nID=ubuntu\nID_LIKE=debian\n";
        let mint = "NAME=\"Linux Mint\"\nID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        let lmde = "ID=linuxmint\nID_LIKE=debian\n";
        let pop = "ID=pop\nID_LIKE=\"ubuntu debian\"\n";
        let fedora = "ID=fedora\n";
        let debian = "ID=debian\n";
        for t in [ubuntu, mint, lmde, pop] {
            assert!(is_supported_distro(t), "{t}");
        }
        for t in [fedora, debian, ""] {
            assert!(!is_supported_distro(t), "{t}");
        }
    }

    #[test]
    fn parses_meminfo_and_pressure() {
        let mem = parse_meminfo("MemTotal: 16000000 kB\nMemFree: 1 kB\nMemAvailable: 1200000 kB\n")
            .unwrap();
        assert_eq!(
            mem,
            MemInfo { total_kib: 16_000_000, available_kib: 1_200_000, swap_total_kib: 0 }
        );
        assert_eq!(mem.available_percent(), 7);
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);
    }

    #[test]
    fn status_details() {
        let text = "Name:\tjava\nState:\tS (sleeping)\nVmHWM:\t 9000 kB\nVmRSS:\t 8000 kB\n\
                    RssAnon:\t 7000 kB\nRssFile:\t 900 kB\nRssShmem:\t 100 kB\nVmSwap:\t 50 kB\n\
                    Threads:\t42\n";
        assert_eq!(
            parse_status_details(text),
            StatusDetails {
                state: "S (sleeping)".into(),
                threads: Some(42),
                rss_anon_kib: Some(7000),
                rss_file_kib: Some(900),
                rss_shmem_kib: Some(100),
                swap_kib: Some(50),
                peak_rss_kib: Some(9000),
            }
        );
        assert_eq!(parse_status_details("Name:\tkthreadd\n"), StatusDetails::default());

        let psi = "some avg10=23.50 avg60=4.00 avg300=1.00 total=1\nfull avg10=9.00 avg60=0 avg300=0 total=0\n";
        assert_eq!(parse_pressure(psi), Some(23.5));
        assert_eq!(parse_pressure(""), None);
    }

    #[test]
    fn stress_uses_either_threshold() {
        let watch = MemoryWatch {
            enabled: true,
            min_available_percent: 10,
            max_pressure_percent: 20,
            ..MemoryWatch::default()
        };
        let snap = |avail, pressure| Snapshot {
            mem: MemInfo { total_kib: 1000, available_kib: avail, swap_total_kib: 0 },
            pressure,
        };
        assert!(!snap(500, Some(0.0)).is_stressed(watch));
        assert!(snap(90, None).is_stressed(watch));
        assert!(snap(500, Some(25.0)).is_stressed(watch));
        let no_psi = MemoryWatch { max_pressure_percent: 0, ..watch };
        assert!(!snap(500, Some(80.0)).is_stressed(no_psi));
        assert_eq!(
            snap(500, Some(40.0)).summary(),
            "0 MB of 1 MB free · programs waited on memory 40% of the last 10 s"
        );
    }

    #[test]
    fn parses_status() {
        let p = |name: &str, uid, rss| ProcStatus { name: name.into(), uid, rss_kib: rss };
        let status =
            "Name:\tfirefox\nUmask:\t0002\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t  2048 kB\n";
        assert_eq!(parse_status(status), Some(p("firefox", 1000, 2048)));
        assert_eq!(parse_status("Name:\tkthreadd\nUid:\t0\t0\t0\t0\n"), None);
        assert!(is_protected("cinnamon") && is_protected("gsd-power") && !is_protected("chrome"));
    }

    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn busy_needs_the_whole_window() {
        let mut t = BusyTracker::default();
        let hz = 100;
        // One process at 100% of a core, one at 50%, sampled every 10 s.
        let mut busy = Vec::new();
        for i in 0..=36u64 {
            let now = 1000.0 + 10.0 * i as f64;
            busy = t.update(now, &[(1, 7, i * 1000), (2, 8, i * 500)], 300.0, hz);
            if i < 30 {
                assert!(busy.is_empty(), "not busy before 5 minutes passed (i = {i})");
            }
        }
        assert_eq!(busy, vec![1]);
        // A new process with the same pid (different start) starts from scratch.
        assert_eq!(t.update(1400.0, &[(1, 9, 999_999)], 300.0, hz), Vec::<u32>::new());
    }

    #[test]
    fn formats_sizes() {
        assert_eq!(format_kib(512 * 1024), "512 MB");
        assert_eq!(format_kib(3 * 1024 * 1024 + 300 * 1024), "3.3 GB");
    }

    #[test]
    fn debounce_waits_then_cools_down() {
        let t0 = Instant::now();
        let mut d = Debounce::default();
        let at = |s| t0 + Duration::from_secs(s);
        assert!(!d.update(true, at(0)));
        assert!(!d.update(true, at(1)));
        assert!(d.update(true, at(2)), "opens after 3 stressed seconds");
        d.closed(at(10), false);
        assert!(!(0..3).any(|i| d.update(true, at(20 + i))), "quiet during cooldown");
        assert!(!d.update(false, at(400)));
        assert!(!d.update(true, at(401)), "a dip resets the streak");
        assert!(!d.update(true, at(402)));
        assert!(d.update(true, at(403)));
        d.closed(at(500), true);
        assert!(!(0..3).any(|i| d.update(true, at(1000 + i))), "snooze lasts an hour");
        assert!(d.update(true, at(4200)), "still low when the snooze ends: open right away");
    }
}

//! Ubuntu / Linux Mint only: the low-memory warning and the Magic `SysRq`
//! emergency keys (settings tab "Memory").
//!
//! The daemon reads `/proc/meminfo` and `/proc/pressure/memory` once a second.
//! When memory stays low it opens `appkey memory --alert`, which lists the
//! user's biggest programs and can ask them to quit (`SIGTERM`), or force them
//! (`SIGKILL`). Only the user's own processes: the kernel refuses anything else,
//! and we never elevate. Parsers are platform-neutral and unit-tested on every
//! host; reading `/proc` and signalling are Linux-only.

pub mod sysrq;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kib: u64,
    pub available_kib: u64,
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
    Some(MemInfo { total_kib: field("MemTotal")?, available_kib: field("MemAvailable")? })
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

/// All processes of one program, e.g. every `firefox` content process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub name: String,
    pub pids: Vec<u32>,
    /// Sum of resident memory. Shared pages are counted per process, so this
    /// overstates a little, but the ranking is right.
    pub rss_kib: u64,
}

/// The user's own closable programs, biggest first.
pub fn group(procs: &[(u32, ProcStatus)], uid: u32, own_pid: u32, limit: usize) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for (pid, p) in procs {
        if p.uid != uid || *pid == own_pid || is_protected(&p.name) {
            continue;
        }
        match groups.iter_mut().find(|g| g.name == p.name) {
            Some(g) => {
                g.pids.push(*pid);
                g.rss_kib += p.rss_kib;
            }
            None => {
                groups.push(Group { name: p.name.clone(), pids: vec![*pid], rss_kib: p.rss_kib });
            }
        }
    }
    groups.sort_by(|a, b| b.rss_kib.cmp(&a.rss_kib).then_with(|| a.name.cmp(&b.name)));
    groups.truncate(limit);
    groups
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

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::process::{Child, Command};
    use std::time::Instant;

    use anyhow::{Context, Result};

    use super::{Debounce, ProcStatus, Snapshot, parse_meminfo, parse_pressure};
    use crate::config::MemoryWatch;

    /// Exit code of `appkey memory --alert` when the user chose "Snooze for an hour".
    pub const SNOOZE_EXIT: i32 = 3;

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

    pub fn read_processes() -> Vec<(u32, ProcStatus)> {
        let Ok(dir) = fs::read_dir("/proc") else { return Vec::new() };
        dir.filter_map(|e| {
            let pid: u32 = e.ok()?.file_name().to_str()?.parse().ok()?;
            Some((pid, read_status(pid)?))
        })
        .collect()
    }

    fn read_status(pid: u32) -> Option<ProcStatus> {
        super::parse_status(&fs::read_to_string(format!("/proc/{pid}/status")).ok()?)
    }

    pub fn current_uid() -> u32 {
        // SAFETY: getuid has no preconditions and cannot fail.
        unsafe { libc::getuid() }
    }

    /// Sends `SIGTERM` (or `SIGKILL` when `force`) to each pid that still runs the
    /// program `name` as us, so a pid reused since the list was read is left alone.
    /// Returns how many were signalled.
    pub fn signal(pids: &[u32], name: &str, force: bool) -> usize {
        let uid = current_uid();
        let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
        pids.iter()
            .filter(|&&pid| read_status(pid).is_some_and(|p| p.name == name && p.uid == uid))
            .filter_map(|&pid| libc::pid_t::try_from(pid).ok())
            // SAFETY: kill only sends a signal; the kernel checks we may signal this pid.
            .filter(|&pid| unsafe { libc::kill(pid, sig) } == 0)
            .count()
    }

    /// Daemon side: checks memory every tick and opens the warning dialog.
    #[derive(Debug)]
    pub struct Watcher {
        supported: bool,
        debounce: Debounce,
        dialog: Option<Child>,
    }

    impl Default for Watcher {
        fn default() -> Self {
            Self { supported: available(), debounce: Debounce::default(), dialog: None }
        }
    }

    impl Watcher {
        pub fn tick(&mut self, watch: MemoryWatch) -> Result<()> {
            if !watch.enabled || !self.supported {
                return Ok(());
            }
            let now = Instant::now();
            if let Some(child) = &mut self.dialog {
                match child.try_wait() {
                    Ok(None) => return Ok(()),
                    Ok(Some(status)) => {
                        self.debounce.closed(now, status.code() == Some(SNOOZE_EXIT));
                    }
                    Err(_) => self.debounce.closed(now, false),
                }
                self.dialog = None;
            }
            let stressed = read_snapshot().is_some_and(|s| s.is_stressed(watch));
            if !self.debounce.update(stressed, now) {
                return Ok(());
            }
            let spawned = std::env::current_exe()
                .and_then(|exe| Command::new(exe).args(["memory", "--alert"]).spawn());
            match spawned {
                Ok(child) => {
                    self.dialog = Some(child);
                    Ok(())
                }
                Err(e) => {
                    self.debounce.closed(now, false);
                    Err(e).context("memory is low, but the warning window could not be opened")
                }
            }
        }
    }

    /// Opens the memory window by hand (settings tab button).
    pub fn open_window() -> Result<()> {
        let exe = std::env::current_exe()?;
        crate::platform::spawn(&[exe.to_string_lossy().into_owned(), "memory".into()])
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
        assert_eq!(mem, MemInfo { total_kib: 16_000_000, available_kib: 1_200_000 });
        assert_eq!(mem.available_percent(), 7);
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);

        let psi = "some avg10=23.50 avg60=4.00 avg300=1.00 total=1\nfull avg10=9.00 avg60=0 avg300=0 total=0\n";
        assert_eq!(parse_pressure(psi), Some(23.5));
        assert_eq!(parse_pressure(""), None);
    }

    #[test]
    fn stress_uses_either_threshold() {
        let watch =
            MemoryWatch { enabled: true, min_available_percent: 10, max_pressure_percent: 20 };
        let snap = |avail, pressure| Snapshot {
            mem: MemInfo { total_kib: 1000, available_kib: avail },
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
    fn groups_own_closable_processes() {
        let p = |name: &str, uid, rss| ProcStatus { name: name.into(), uid, rss_kib: rss };
        let status =
            "Name:\tfirefox\nUmask:\t0002\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t  2048 kB\n";
        assert_eq!(parse_status(status), Some(p("firefox", 1000, 2048)));
        assert_eq!(parse_status("Name:\tkthreadd\nUid:\t0\t0\t0\t0\n"), None);

        let procs = [
            (10, p("firefox", 1000, 300)),
            (11, p("firefox", 1000, 400)),
            (12, p("code", 1000, 500)),
            (13, p("Xorg", 1000, 9000)),
            (14, p("csd-power", 1000, 9000)),
            (15, p("postgres", 120, 9000)),
            (16, p("appkey-like", 1000, 1)),
            (99, p("me", 1000, 9000)),
        ];
        let groups = group(&procs, 1000, 99, 2);
        assert_eq!(
            groups,
            vec![
                Group { name: "firefox".into(), pids: vec![10, 11], rss_kib: 700 },
                Group { name: "code".into(), pids: vec![12], rss_kib: 500 },
            ]
        );
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

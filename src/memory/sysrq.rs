//! Magic `SysRq`: Alt+PrtSc+<letter> is handled by the kernel itself, so it works
//! even when the desktop is frozen. `kernel.sysrq` is a bitmask of what the
//! keyboard may do; Ubuntu and Mint ship 176 (sync, remount read-only, reboot),
//! which leaves out killing processes (Alt+SysRq+F, the OOM killer).
//!
//! Changing it needs root. appkey never elevates itself: it runs `pkexec` with
//! a fixed argv, so the desktop shows its own password prompt for exactly
//! `tee <drop-in>` and `sysctl -p <drop-in>`.

/// The persistent setting. Sorts after Ubuntu's `10-magic-sysrq.conf`, so it wins.
pub const DROP_IN: &str = "/etc/sysctl.d/90-appkey-sysrq.conf";
/// Bit that allows Alt+SysRq+F / E / I.
pub const SIGNAL: u32 = 64;

/// The kernel's bits (Documentation/admin-guide/sysrq.rst), with what they allow.
const BITS: [(u32, &str); 8] = [
    (2, "console log level"),
    (4, "keyboard control"),
    (8, "debug dumps"),
    (16, "sync disks"),
    (32, "remount read-only"),
    (64, "kill processes"),
    (128, "reboot / power off"),
    (256, "renice real-time tasks"),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Preset {
    pub value: u32,
    pub label: &'static str,
}

pub const PRESETS: [Preset; 4] = [
    Preset { value: 240, label: "Recommended: also allow closing programs (240)" },
    Preset { value: 176, label: "Ubuntu / Mint default: sync, read-only, reboot only (176)" },
    Preset { value: 1, label: "Everything, including debug dumps (1)" },
    Preset { value: 0, label: "Off: ignore all SysRq keys (0)" },
];

pub fn allows(mask: u32, bit: u32) -> bool {
    mask == 1 || mask & bit != 0
}

/// "sync disks, remount read-only, reboot / power off" for 176.
pub fn describe(mask: u32) -> String {
    match mask {
        0 => "all SysRq keys are off".into(),
        1 => "all SysRq keys are on".into(),
        _ => {
            let on: Vec<&str> =
                BITS.iter().filter(|(b, _)| mask & b != 0).map(|(_, name)| *name).collect();
            if on.is_empty() { "nothing useful".into() } else { on.join(", ") }
        }
    }
}

pub fn drop_in_contents(value: u32) -> String {
    format!(
        "# Written by appkey (Settings → Memory). Magic SysRq keys allowed from the keyboard.\n\
         kernel.sysrq = {value}\n"
    )
}

/// The `kernel.sysrq` value in a sysctl.d file, if it sets one.
pub fn parse_drop_in(text: &str) -> Option<u32> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.starts_with(['#', ';']))
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| k.trim() == "kernel.sysrq")
        .filter_map(|(_, v)| v.trim().parse().ok())
        .next_back()
}

/// The same change, for people who'd rather paste it into a terminal.
pub fn terminal_command(value: u32) -> String {
    format!("echo 'kernel.sysrq = {value}' | sudo tee {DROP_IN} && sudo sysctl -p {DROP_IN}")
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

    use anyhow::{Context, Result, bail};

    use super::{DROP_IN, drop_in_contents, parse_drop_in};

    pub fn read_current() -> Result<u32> {
        let text = std::fs::read_to_string("/proc/sys/kernel/sysrq")
            .context("this kernel doesn't expose /proc/sys/kernel/sysrq")?;
        text.trim().parse().context("unexpected contents in /proc/sys/kernel/sysrq")
    }

    /// What appkey saved earlier, if anything.
    pub fn read_saved() -> Option<u32> {
        parse_drop_in(&std::fs::read_to_string(DROP_IN).ok()?)
    }

    /// Saves `value` to the drop-in and applies it now. Each step is a `pkexec`, so the
    /// desktop asks for the password (once or twice, depending on its polkit agent).
    pub fn apply(value: u32) -> Result<()> {
        let mut tee = Command::new("pkexec")
            .args(["/usr/bin/tee", DROP_IN])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .context("pkexec is missing; install policykit-1, or use the terminal command")?;
        tee.stdin
            .take()
            .context("no stdin for pkexec")?
            .write_all(drop_in_contents(value).as_bytes())?;
        check("saving the setting", &tee.wait_with_output()?)?;
        let sysctl = Command::new("pkexec")
            .args(["/usr/sbin/sysctl", "-q", "-p", DROP_IN])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()?;
        check("applying it (it still takes effect after a restart)", &sysctl)
    }

    fn check(step: &str, out: &std::process::Output) -> Result<()> {
        match out.status.code() {
            Some(0) => Ok(()),
            // pkexec: 126 = the user dismissed the prompt, 127 = not authorized / no agent.
            Some(126) => bail!("cancelled; nothing was changed by {step}"),
            Some(127) => bail!(
                "not authorized while {step}. Your account may not be an administrator, or no \
                 password prompt could be shown; try the terminal command"
            ),
            _ => bail!("{step} failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describes_masks() {
        assert_eq!(describe(176), "sync disks, remount read-only, reboot / power off");
        assert_eq!(
            describe(240),
            "sync disks, remount read-only, kill processes, reboot / power off"
        );
        assert_eq!(describe(0), "all SysRq keys are off");
        assert!(allows(240, SIGNAL) && allows(1, SIGNAL));
        assert!(!allows(176, SIGNAL) && !allows(0, SIGNAL));
        assert!(PRESETS.iter().any(|p| p.value == 240));
    }

    #[test]
    fn drop_in_round_trips() {
        assert_eq!(parse_drop_in(&drop_in_contents(240)), Some(240));
        assert_eq!(parse_drop_in("# kernel.sysrq = 1\nkernel.sysrq=176\n"), Some(176));
        assert_eq!(parse_drop_in("vm.swappiness = 10\n"), None);
        assert!(terminal_command(240).contains("kernel.sysrq = 240"));
    }
}

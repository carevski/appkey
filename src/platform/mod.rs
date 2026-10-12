//! OS-specific "focus or launch". Each backend exposes the same functions:
//!
//! - `focus_or_launch(app, launch) -> Result<Outcome>`: focus the most recently used
//!   window of `app`, or start it (with `launch`, or a per-OS default)
//! - `running_apps() -> Result<Vec<AppInfo>>`: apps with an open window
//! - `installed_apps() -> Vec<AppInfo>`: apps the user could launch (for the picker)
//! - `frontmost_app() -> Result<Option<AppInfo>>`: the focused app, if it isn't us
//! - `focus_pid(pid) -> Result<bool>`: bring a process's window forward (our settings window)
//! - `diagnostics() -> Vec<String>`: environment/permission notes for `appkey check`
//! - `ensure_supported() -> Result<()>`: fail early in environments we can't drive
//! - `open_in_editor(&Path) -> Result<()>`

use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[cfg(any(target_os = "linux", test))]
mod desktop_entry;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(any(windows, test))]
mod lnk;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::*;
#[cfg(target_os = "macos")]
pub use macos::*;
#[cfg(windows)]
pub use windows::*;

/// An app as the settings picker shows it, and what to write to the config.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppInfo {
    /// Human-readable name.
    pub name: String,
    /// Value for the config's `app` (how a running instance is recognised).
    pub app: String,
    /// Value for the config's `launch`, when the default wouldn't work.
    pub launch: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Focused,
    Launched,
}

/// Starts a program without a shell and without tying it to our lifetime.
pub(crate) fn spawn(argv: &[String]) -> Result<()> {
    let (program, rest) = argv.split_first().context("empty command")?;
    let mut child = Command::new(program)
        .args(rest)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("starting {program:?}"))?;
    // Reap it so it doesn't linger as a zombie on Unix.
    std::thread::spawn(move || child.wait());
    Ok(())
}

//! OS-specific "focus or launch". Each backend exposes the same functions:
//!
//! - `focus_or_launch(app, launch) -> Result<Outcome>`: focus the most recently used
//!   window of `app`, or start it (with `launch`, or a per-OS default)
//! - `running_apps() -> Result<Vec<String>>`: names usable as `app` in the config
//! - `diagnostics() -> Vec<String>`: environment/permission notes for `appkey check`
//! - `ensure_supported() -> Result<()>`: fail early in environments we can't drive
//! - `open_in_editor(&Path) -> Result<()>`

use std::process::{Command, Stdio};

use anyhow::{Context, Result};

#[cfg(target_os = "linux")]
mod linux;
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

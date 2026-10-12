//! macOS backend.
//!
//! Needs no special permissions: hotkeys use Carbon `RegisterEventHotKey` (no
//! Accessibility / Input Monitoring prompt), and activation goes through
//! `LaunchServices` (`/usr/bin/open`), the same path the Dock uses. That also
//! sidesteps macOS 14+ "cooperative activation", which can ignore
//! `NSRunningApplication.activate` calls coming from a background app.
//! Activating an app brings forward its most recently used window.

use std::path::Path;
use std::process::Command;

use anyhow::{Result, bail};
use objc2_app_kit::{NSApplicationActivationPolicy, NSWorkspace};
use objc2_foundation::NSString;

use super::{Outcome, spawn};
use crate::matcher::matches;

const OPEN: &str = "/usr/bin/open";

struct App {
    name: String,
    bundle_id: Option<String>,
    bundle_path: Option<String>,
}

fn regular_apps() -> Vec<App> {
    let to_string = |s: &NSString| s.to_string();
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .filter(|a| a.activationPolicy() == NSApplicationActivationPolicy::Regular)
        .filter_map(|a| {
            Some(App {
                name: a.localizedName().as_deref().map(to_string)?,
                bundle_id: a.bundleIdentifier().as_deref().map(to_string),
                bundle_path: a.bundleURL().and_then(|u| u.path()).as_deref().map(to_string),
            })
        })
        .collect()
}

pub fn focus_or_launch(app: &str, launch: Option<&[String]>) -> Result<Outcome> {
    let running = regular_apps().into_iter().find(|a| {
        matches(app, &a.name) || a.bundle_id.as_deref().is_some_and(|id| matches(app, id))
    });

    if let Some(app) = running {
        let args = match (&app.bundle_id, &app.bundle_path) {
            (Some(id), _) => ["-b", id.as_str()],
            (None, Some(path)) => ["-a", path.as_str()],
            (None, None) => ["-a", app.name.as_str()],
        };
        run_open(&args)?;
        return Ok(Outcome::Focused);
    }

    match launch {
        Some(argv) => spawn(argv)?,
        None if looks_like_bundle_id(app) => run_open(&["-b", app])?,
        None => run_open(&["-a", app])?,
    }
    Ok(Outcome::Launched)
}

fn looks_like_bundle_id(s: &str) -> bool {
    s.matches('.').count() >= 2 && !s.contains(['/', ' '])
}

fn run_open(args: &[&str]) -> Result<()> {
    let out = Command::new(OPEN).args(args).output()?;
    if !out.status.success() {
        bail!("`open {}` failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn running_apps() -> Result<Vec<String>> {
    Ok(regular_apps()
        .into_iter()
        .map(|a| match a.bundle_id {
            Some(id) => format!("{}  ({id})", a.name),
            None => a.name,
        })
        .collect())
}

pub fn diagnostics() -> Vec<String> {
    vec!["macOS: no extra permissions required.".into()]
}

#[allow(clippy::unnecessary_wraps)]
pub fn ensure_supported() -> Result<()> {
    Ok(())
}

pub fn open_in_editor(path: &Path) -> Result<()> {
    run_open(&["-t", &path.to_string_lossy()])
}

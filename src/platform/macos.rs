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
use objc2_app_kit::{
    NSApplicationActivationOptions, NSApplicationActivationPolicy, NSRunningApplication,
    NSWorkspace,
};
use objc2_foundation::{NSBundle, NSString};

use super::{AppInfo, Outcome, spawn};
use crate::matcher::matches;

const OPEN: &str = "/usr/bin/open";

struct App {
    pid: i32,
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
                pid: a.processIdentifier(),
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

impl App {
    fn info(self) -> AppInfo {
        // Bundle ids survive renames and localisation, so prefer them for matching.
        AppInfo {
            app: self.bundle_id.unwrap_or_else(|| self.name.clone()),
            name: self.name,
            launch: None,
        }
    }
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn running_apps() -> Result<Vec<AppInfo>> {
    let mut apps: Vec<AppInfo> = regular_apps().into_iter().map(App::info).collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    Ok(apps)
}

pub fn installed_apps() -> Vec<AppInfo> {
    let home = dirs::home_dir().map(|h| h.join("Applications"));
    let dirs = [
        "/Applications",
        "/Applications/Utilities",
        "/System/Applications",
        "/System/Applications/Utilities",
    ]
    .into_iter()
    .map(std::path::PathBuf::from)
    .chain(home);
    let mut apps: Vec<AppInfo> = dirs
        .filter_map(|d| std::fs::read_dir(d).ok())
        .flat_map(|entries| entries.flatten().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "app"))
        .filter_map(|p| {
            let name = p.file_stem()?.to_string_lossy().into_owned();
            let bundle = NSBundle::bundleWithPath(&NSString::from_str(&p.to_string_lossy()))?;
            let app = bundle.bundleIdentifier().map_or_else(|| name.clone(), |id| id.to_string());
            Some(AppInfo { name, app, launch: None })
        })
        .collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.app == b.app);
    apps
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn frontmost_app() -> Result<Option<AppInfo>> {
    let me = i32::try_from(std::process::id()).unwrap_or_default();
    let Some(front) = NSWorkspace::sharedWorkspace().frontmostApplication() else {
        return Ok(None);
    };
    if front.processIdentifier() == me {
        return Ok(None);
    }
    Ok(regular_apps().into_iter().find(|a| a.pid == front.processIdentifier()).map(App::info))
}

#[allow(clippy::unnecessary_wraps)] // same signature on every platform
pub fn focus_pid(pid: u32) -> Result<bool> {
    let Some(app) = i32::try_from(pid)
        .ok()
        .and_then(NSRunningApplication::runningApplicationWithProcessIdentifier)
    else {
        return Ok(false);
    };
    #[allow(deprecated)] // still the only way to come forward when another app is active
    Ok(app.activateWithOptions(
        NSApplicationActivationOptions::ActivateAllWindows
            | NSApplicationActivationOptions::ActivateIgnoringOtherApps,
    ))
}

/// egui reports Cmd in its modifiers on macOS, so there's nothing to add.
pub fn logo_key_down() -> bool {
    false
}

/// The system theme reaches egui through winit here, so there's nothing to add.
pub fn dark_mode() -> Option<bool> {
    None
}

/// Window titles per pid; only the Linux memory window uses them.
#[allow(dead_code)] // same API on every platform
pub fn window_titles() -> std::collections::HashMap<u32, Vec<String>> {
    std::collections::HashMap::new()
}

/// Registering reports system shortcuts as unavailable; nothing to list up front.
pub fn desktop_shortcuts() -> Vec<(u32, String)> {
    Vec::new()
}

pub fn hotkeys(sink: crate::hotkeys::Sink) -> Result<Box<dyn crate::hotkeys::Hotkeys>> {
    Ok(Box::new(crate::hotkeys::GlobalHotkeys::new(sink)?))
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

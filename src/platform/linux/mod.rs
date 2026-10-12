//! Linux backend. The session decides how we talk to the desktop:
//!
//! | Session            | Windows (list / focus / active)      | Global shortcuts            |
//! |--------------------|--------------------------------------|-----------------------------|
//! | X11 (any WM)       | EWMH (`x11.rs`)                      | `XGrabKey` via global-hotkey |
//! | Wayland: KDE       | KWin scripting (`kde.rs`)            | GlobalShortcuts portal      |
//!
//! Each backend implements [`WindowSystem`]; matching, most-recently-used choice
//! and launching are shared here.

mod kde;
mod portal;
mod x11;
mod xkb;

use std::env;
use std::path::Path;

use anyhow::{Result, bail};

use super::desktop_entry::{self, DesktopApp};
use super::{AppInfo, Outcome, spawn};
use crate::hotkeys::{GlobalHotkeys, Hotkeys, Sink};
use crate::matcher::matches;

/// A top-level window as a backend reports it.
#[derive(Debug, Clone)]
pub(crate) struct Win {
    /// Backend-specific handle used to activate it.
    pub handle: String,
    /// Every name the window goes by (`WM_CLASS` parts, Wayland app id, desktop file id).
    pub ids: Vec<String>,
    pub pid: Option<u32>,
}

pub(crate) trait WindowSystem {
    /// Application windows, most recently used first.
    fn windows(&self) -> Result<Vec<Win>>;
    /// Brings the window forward (switching workspace / un-minimizing as needed).
    fn activate(&self, w: &Win) -> Result<()>;
    /// The focused window, if any.
    fn active(&self) -> Result<Option<Win>>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Session {
    X11,
    Wayland,
    Unknown,
}

fn var(k: &str) -> Option<String> {
    env::var(k).ok().filter(|v| !v.is_empty())
}

fn session() -> Session {
    match var("XDG_SESSION_TYPE").as_deref() {
        Some("wayland") => Session::Wayland,
        Some("x11") => Session::X11,
        _ if var("WAYLAND_DISPLAY").is_some() => Session::Wayland,
        _ if var("DISPLAY").is_some() => Session::X11,
        _ => Session::Unknown,
    }
}

fn desktop() -> String {
    var("XDG_CURRENT_DESKTOP").unwrap_or_else(|| "unknown".into())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Backend {
    X11,
    Kde,
}

fn backend() -> Result<Backend> {
    match session() {
        Session::X11 => Ok(Backend::X11),
        Session::Wayland if desktop().split(':').any(|d| d.eq_ignore_ascii_case("KDE")) => {
            Ok(Backend::Kde)
        }
        Session::Wayland => bail!(
            "this Wayland desktop ({}) isn't supported yet.\n\
             Supported: any X11 session; on Wayland: KDE Plasma. Options for now:\n\
             - log in with an X11/Xorg session, or\n\
             - use your desktop's own keyboard settings to run `appkey open <app>`.",
            desktop()
        ),
        Session::Unknown => {
            bail!("no graphical session found (neither DISPLAY nor WAYLAND_DISPLAY is set)")
        }
    }
}

fn window_system() -> Result<Box<dyn WindowSystem>> {
    match backend()? {
        Backend::X11 => Ok(Box::new(x11::X::connect()?)),
        Backend::Kde => Ok(Box::new(kde::Kwin::connect()?)),
    }
}

pub fn ensure_supported() -> Result<()> {
    backend().map(|_| ())
}

pub fn hotkeys(sink: Sink) -> Result<Box<dyn Hotkeys>> {
    match backend()? {
        Backend::X11 => Ok(Box::new(GlobalHotkeys::new(sink)?)),
        Backend::Kde => {
            Ok(Box::new(portal::PortalHotkeys::new(sink, Some(kde::forget_stale_shortcuts))?))
        }
    }
}

/// Every name `app` could appear under: itself, plus all ids of installed apps it
/// refers to (so `gnome-terminal` also finds a window whose app id is `org.gnome.Terminal`).
fn aliases(app: &str, installed: &[DesktopApp]) -> Vec<String> {
    let mut out = vec![app.to_owned()];
    for d in installed.iter().filter(|d| d.ids.iter().any(|i| matches(app, i))) {
        out.extend(d.ids.iter().cloned());
    }
    out
}

fn window_matches(w: &Win, aliases: &[String]) -> bool {
    w.ids.iter().any(|id| aliases.iter().any(|a| matches(a, id)))
}

/// The window's app, named after its installed `.desktop` entry when one matches.
fn app_info(w: &Win, installed: &[DesktopApp]) -> Option<AppInfo> {
    let known =
        installed.iter().find(|d| d.ids.iter().any(|i| w.ids.iter().any(|c| matches(i, c))));
    match (known, w.ids.first()) {
        (Some(d), _) => Some(d.info.clone()),
        (None, Some(id)) => Some(AppInfo { name: id.clone(), app: id.clone(), launch: None }),
        (None, None) => None,
    }
}

fn installed() -> Vec<DesktopApp> {
    desktop_entry::scan(&desktop_entry::search_dirs())
}

pub fn focus_or_launch(app: &str, launch: Option<&[String]>) -> Result<Outcome> {
    let ws = window_system()?;
    let names = aliases(app, &installed());
    if let Some(w) = ws.windows()?.into_iter().find(|w| window_matches(w, &names)) {
        ws.activate(&w)?;
        return Ok(Outcome::Focused);
    }
    match launch {
        Some(argv) => spawn(argv)?,
        None => spawn(&[app.to_owned()])?,
    }
    Ok(Outcome::Launched)
}

pub fn running_apps() -> Result<Vec<AppInfo>> {
    let ws = window_system()?;
    let installed = installed();
    let mut apps: Vec<AppInfo> =
        ws.windows()?.iter().filter_map(|w| app_info(w, &installed)).collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.app == b.app);
    Ok(apps)
}

pub fn installed_apps() -> Vec<AppInfo> {
    installed().into_iter().map(|d| d.info).collect()
}

pub fn frontmost_app() -> Result<Option<AppInfo>> {
    let ws = window_system()?;
    match ws.active()? {
        Some(w) if w.pid != Some(std::process::id()) => Ok(app_info(&w, &installed())),
        _ => Ok(None),
    }
}

pub fn focus_pid(pid: u32) -> Result<bool> {
    let ws = window_system()?;
    match ws.windows()?.into_iter().find(|w| w.pid == Some(pid)) {
        Some(w) => ws.activate(&w).map(|()| true),
        None => Ok(false),
    }
}

pub fn diagnostics() -> Vec<String> {
    let mut out = vec![format!("Linux session: {:?}, desktop: {}", session(), desktop())];
    match backend() {
        Ok(b) => out.push(format!("Backend: {b:?}")),
        Err(e) => out.push(format!("NOT SUPPORTED: {e}")),
    }
    out.push(
        "Tray icon uses StatusNotifierItem; GNOME needs the AppIndicator extension \
         (preinstalled on Ubuntu)."
            .into(),
    );
    out
}

pub fn open_in_editor(path: &Path) -> Result<()> {
    spawn(&["xdg-open".to_owned(), path.to_string_lossy().into_owned()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_app(app: &str, ids: &[&str]) -> DesktopApp {
        DesktopApp {
            info: AppInfo { name: app.into(), app: app.into(), launch: None },
            ids: ids.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn win(ids: &[&str]) -> Win {
        Win { handle: String::new(), ids: ids.iter().map(|s| (*s).to_owned()).collect(), pid: None }
    }

    #[test]
    fn aliases_bridge_exec_names_and_app_ids() {
        let installed = [desktop_app("gnome-terminal", &["gnome-terminal", "org.gnome.Terminal"])];
        let names = aliases("gnome-terminal", &installed);
        assert!(window_matches(&win(&["org.gnome.Terminal"]), &names));
        assert!(!window_matches(&win(&["firefox"]), &names));
        // Unknown apps still match by their own name.
        assert!(window_matches(&win(&["Foo"]), &aliases("foo", &installed)));
    }

    #[test]
    fn app_info_prefers_installed_entry() {
        let installed = [desktop_app("code", &["Code", "code"])];
        assert_eq!(app_info(&win(&["code", "Code"]), &installed).unwrap().app, "code");
        assert_eq!(app_info(&win(&["xterm", "XTerm"]), &installed).unwrap().app, "xterm");
        assert!(app_info(&win(&[]), &installed).is_none());
    }
}

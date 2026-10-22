//! Linux backend. The session decides how we talk to the desktop:
//!
//! | Session            | Windows (list / focus / active)      | Global shortcuts            |
//! |--------------------|--------------------------------------|-----------------------------|
//! | X11 (any WM)       | EWMH (`x11.rs`)                      | `XGrabKey` via global-hotkey |
//! | Wayland: KDE       | KWin scripting (`kde.rs`)            | GlobalShortcuts portal      |
//! | Wayland: GNOME     | appkey's Shell extension (`gnome.rs`) | the same extension          |
//! | Wayland: Hyprland  | IPC socket (`hyprland.rs`)           | runtime `bind` → `appkey __hotkey` |
//!
//! Each backend implements [`WindowSystem`]; matching, most-recently-used choice
//! and launching are shared here.

mod cinnamon;
mod gnome;
mod hyprland;
mod kde;
mod portal;
mod reserved;
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
    /// Window title (the memory window uses it to say which Chrome / IDE window is which).
    pub title: String,
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
    Hyprland,
    Gnome,
}

fn backend() -> Result<Backend> {
    match session() {
        Session::X11 => Ok(Backend::X11),
        Session::Wayland if desktop().split(':').any(|d| d.eq_ignore_ascii_case("KDE")) => {
            Ok(Backend::Kde)
        }
        Session::Wayland if var("HYPRLAND_INSTANCE_SIGNATURE").is_some() => Ok(Backend::Hyprland),
        // Ubuntu reports `ubuntu:GNOME`.
        Session::Wayland if desktop().split(':').any(|d| d.eq_ignore_ascii_case("GNOME")) => {
            Ok(Backend::Gnome)
        }
        Session::Wayland => bail!(
            "this Wayland desktop ({}) isn't supported yet.\n\
             Supported: any X11 session; on Wayland: GNOME, KDE Plasma, Hyprland. Options for now:\n\
             - log in with an X11/Xorg session, or\n\
             - use your desktop's own keyboard settings to run `appkey open <app>`.",
            desktop()
        ),
        Session::Unknown => {
            bail!("no graphical session found (neither DISPLAY nor WAYLAND_DISPLAY is set)")
        }
    }
}

/// `XDG_CURRENT_DESKTOP` is a `:`-separated list (`ubuntu:GNOME`, `X-Cinnamon`).
fn desktop_is(names: &[&str]) -> bool {
    desktop().split(':').any(|d| names.iter().any(|n| d.eq_ignore_ascii_case(n)))
}

fn is_cinnamon() -> bool {
    desktop_is(&["X-Cinnamon", "Cinnamon"])
}

/// Where the desktop's own shortcuts can be read, if we know how.
fn reserved_lookup() -> Option<reserved::Lookup> {
    if is_cinnamon() {
        Some(cinnamon::reserved)
    } else if desktop_is(&["GNOME"]) {
        Some(gnome::reserved)
    } else if desktop_is(&["KDE"]) {
        Some(kde::reserved)
    } else {
        None
    }
}

fn window_system() -> Result<Box<dyn WindowSystem>> {
    match backend()? {
        Backend::X11 => Ok(Box::new(x11::X::connect()?)),
        Backend::Kde => Ok(Box::new(kde::Kwin::connect()?)),
        Backend::Hyprland => Ok(Box::new(hyprland::Hypr::connect()?)),
        Backend::Gnome => Ok(Box::new(gnome::Shell::connect()?)),
    }
}

pub fn ensure_supported() -> Result<()> {
    backend().map(|_| ())
}

pub fn hotkeys(sink: Sink) -> Result<Box<dyn Hotkeys>> {
    let inner = backend_hotkeys(sink)?;
    Ok(match reserved_lookup() {
        Some(lookup) => Box::new(reserved::Reserved::new(inner, lookup)),
        None => inner,
    })
}

fn backend_hotkeys(sink: Sink) -> Result<Box<dyn Hotkeys>> {
    match backend()? {
        // Cinnamon's menu swallows Super; those shortcuts go through Cinnamon itself.
        Backend::X11 if is_cinnamon() => Ok(Box::new(cinnamon::CinnamonHotkeys::new(sink)?)),
        Backend::X11 => Ok(Box::new(GlobalHotkeys::new(sink)?)),
        Backend::Kde => {
            Ok(Box::new(portal::PortalHotkeys::new(sink, Some(kde::forget_stale_shortcuts))?))
        }
        // Hyprland runs `appkey __hotkey <id>` itself; nothing to report back.
        Backend::Hyprland => Ok(Box::new(hyprland::HyprHotkeys::new()?)),
        Backend::Gnome => Ok(Box::new(gnome::ShellHotkeys::new(sink)?)),
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

/// Shortcuts the desktop itself uses (Cinnamon, GNOME, KDE), as `(HotKey id, "GNOME: Show
/// desktop")`, so the settings window can warn before one is picked.
pub fn desktop_shortcuts() -> Vec<(u32, String)> {
    reserved_lookup().map(|lookup| lookup()).unwrap_or_default()
}

/// Super held right now, for the shortcut recorder when the desktop swallowed the key's
/// own press. Only X11 can tell; Wayland doesn't expose other keys' state.
pub fn logo_key_down() -> bool {
    session() == Session::X11 && x11::logo_key_down()
}

/// Titles of each process's windows, for the memory window. Errors leave it empty:
/// the window still works, it just can't name windows.
pub fn window_titles() -> std::collections::HashMap<u32, Vec<String>> {
    let mut out: std::collections::HashMap<u32, Vec<String>> = std::collections::HashMap::new();
    for w in window_system().and_then(|ws| ws.windows()).unwrap_or_default() {
        if let Some(pid) = w.pid
            && !w.title.trim().is_empty()
        {
            out.entry(pid).or_default().push(w.title);
        }
    }
    out
}

pub fn diagnostics() -> Vec<String> {
    let mut out = vec![format!("Linux session: {:?}, desktop: {}", session(), desktop())];
    match backend() {
        Ok(b) => {
            out.push(format!("Backend: {b:?}"));
            if b == Backend::Gnome {
                let dirs = gnome::extension_dirs();
                out.push(match dirs.first() {
                    Some(d) => format!("GNOME helper extension: {}", d.display()),
                    None => {
                        "GNOME helper extension: not installed yet (appkey installs it on start)"
                            .into()
                    }
                });
                if let Err(e) = gnome::Shell::connect() {
                    out.push(format!("NOT READY: {e}"));
                }
            }
        }
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
        Win {
            handle: String::new(),
            ids: ids.iter().map(|s| (*s).to_owned()).collect(),
            pid: None,
            title: String::new(),
        }
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

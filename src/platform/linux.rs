//! Linux backend (X11 / EWMH).
//!
//! Windows are matched on `WM_CLASS` (instance or class). `_NET_CLIENT_LIST_STACKING`
//! is ordered bottom to top, so the last match is the most recently used window.
//! Activation is a `_NET_ACTIVE_WINDOW` request with source indication 2 ("pager"),
//! which window managers honour without focus-stealing prevention, and which
//! also switches workspace / un-minimizes as needed.
//!
//! Wayland: there is no standard protocol for global hotkeys or for focusing
//! another app's window, so we detect it and refuse with an explanation.
//! See docs/DESIGN.md for the Wayland roadmap.

use std::env;
use std::path::Path;

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, PropMode,
    Timestamp, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{AppInfo, Outcome, desktop_entry, spawn};
use crate::matcher::matches;

#[derive(Debug, PartialEq, Eq)]
enum Session {
    X11,
    /// Wayland session. `xwayland` = an X server is reachable too, which is not
    /// enough: X11 key grabs only fire while an X11 window is focused.
    Wayland {
        xwayland: bool,
    },
    Unknown,
}

fn session() -> Session {
    let var = |k| env::var(k).ok().filter(|v| !v.is_empty());
    let has_x = var("DISPLAY").is_some();
    match var("XDG_SESSION_TYPE").as_deref() {
        Some("wayland") => Session::Wayland { xwayland: has_x },
        Some("x11") => Session::X11,
        _ if var("WAYLAND_DISPLAY").is_some() => Session::Wayland { xwayland: has_x },
        _ if has_x => Session::X11,
        _ => Session::Unknown,
    }
}

pub fn ensure_supported() -> Result<()> {
    match session() {
        Session::X11 => Ok(()),
        Session::Wayland { .. } => bail!(
            "this is a Wayland session; appkey currently supports X11 only.\n\
             Wayland has no standard way for an app to register global hotkeys or focus\n\
             other apps' windows. Options for now:\n\
             - log in with an X11/Xorg session (e.g. \"Cinnamon\" rather than \"Cinnamon (Wayland)\"), or\n\
             - use your desktop's own keyboard settings to bind a key to the app."
        ),
        Session::Unknown => {
            bail!("no graphical session found (neither DISPLAY nor WAYLAND_DISPLAY is set)")
        }
    }
}

struct X {
    conn: RustConnection,
    root: Window,
    stacking: Atom,
    client_list: Atom,
    active: Atom,
    wm_pid: Atom,
}

impl X {
    fn connect() -> Result<Self> {
        ensure_supported()?;
        let (conn, screen) = x11rb::connect(None).context("connecting to the X server")?;
        let root = conn.setup().roots[screen].root;
        let atom =
            |name: &[u8]| -> Result<Atom> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        let stacking = atom(b"_NET_CLIENT_LIST_STACKING")?;
        let client_list = atom(b"_NET_CLIENT_LIST")?;
        let active = atom(b"_NET_ACTIVE_WINDOW")?;
        let wm_pid = atom(b"_NET_WM_PID")?;
        Ok(Self { conn, root, stacking, client_list, active, wm_pid })
    }

    /// Managed windows, most recently used first (falls back to mapping order).
    fn windows_mru(&self) -> Result<Vec<Window>> {
        for prop in [self.stacking, self.client_list] {
            let reply = self
                .conn
                .get_property(false, self.root, prop, AtomEnum::WINDOW, 0, u32::MAX)?
                .reply()?;
            if let Some(list) = reply.value32() {
                let mut list: Vec<Window> = list.collect();
                list.reverse();
                if !list.is_empty() {
                    return Ok(list);
                }
            }
        }
        bail!(
            "the window manager does not publish a window list (_NET_CLIENT_LIST); is it EWMH-compliant?"
        )
    }

    fn pid(&self, w: Window) -> Option<u32> {
        let reply = self
            .conn
            .get_property(false, w, self.wm_pid, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        reply.value32()?.next()
    }

    /// The window's app, named after its installed `.desktop` entry when one matches.
    fn app_info(&self, w: Window, installed: &[AppInfo]) -> Option<AppInfo> {
        let class = self.wm_class(w);
        let known = installed.iter().find(|a| class.iter().any(|c| matches(&a.app, c)));
        match (known, class.last()) {
            (Some(a), _) => Some(a.clone()),
            (None, Some(c)) => Some(AppInfo { name: c.clone(), app: c.clone(), launch: None }),
            (None, None) => None,
        }
    }

    /// `WM_CLASS` is two NUL-terminated strings: instance, then class.
    fn wm_class(&self, w: Window) -> Vec<String> {
        let Ok(Ok(reply)) = self
            .conn
            .get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
            .map(x11rb::cookie::Cookie::reply)
        else {
            return Vec::new();
        };
        parse_wm_class(&reply.value)
    }

    /// Current X server time. Window managers with focus-stealing prevention (Muffin,
    /// Mutter, Openbox) ignore activation requests without a real timestamp once the
    /// user has interacted with another window. The standard way to get one: touch a
    /// property on a throwaway window and read the time off the `PropertyNotify`.
    fn server_time(&self) -> Result<Timestamp> {
        let win = self.conn.generate_id()?;
        let aux = CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE);
        self.conn.create_window(
            0,
            win,
            self.root,
            -1,
            -1,
            1,
            1,
            0,
            WindowClass::INPUT_ONLY,
            0,
            &aux,
        )?;
        self.conn.change_property8(PropMode::APPEND, win, self.active, AtomEnum::STRING, &[])?;
        self.conn.flush()?;
        let time = loop {
            if let Event::PropertyNotify(e) = self.conn.wait_for_event()?
                && e.window == win
            {
                break e.time;
            }
        };
        self.conn.destroy_window(win)?;
        Ok(time)
    }

    fn current_active(&self) -> Window {
        self.conn
            .get_property(false, self.root, self.active, AtomEnum::WINDOW, 0, 1)
            .ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut v| v.next()))
            .unwrap_or(0)
    }

    fn activate(&self, w: Window) -> Result<()> {
        let time = self.server_time()?;
        // data: [source indication = 2 (pager), timestamp, requester's currently active window]
        let event =
            ClientMessageEvent::new(32, w, self.active, [2, time, self.current_active(), 0, 0]);
        self.conn.send_event(
            false,
            self.root,
            EventMask::SUBSTRUCTURE_REDIRECT | EventMask::SUBSTRUCTURE_NOTIFY,
            event,
        )?;
        // Round-trip before we drop the connection: a request still unprocessed when
        // the client disconnects can be discarded by the server (seen with Openbox).
        self.conn.get_input_focus()?.reply()?;
        Ok(())
    }
}

fn parse_wm_class(raw: &[u8]) -> Vec<String> {
    raw.split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .collect()
}

pub fn focus_or_launch(app: &str, launch: Option<&[String]>) -> Result<Outcome> {
    let x = X::connect()?;
    for w in x.windows_mru()? {
        if x.wm_class(w).iter().any(|c| matches(app, c)) {
            x.activate(w)?;
            return Ok(Outcome::Focused);
        }
    }
    match launch {
        Some(argv) => spawn(argv)?,
        None => spawn(&[app.to_owned()])?,
    }
    Ok(Outcome::Launched)
}

pub fn running_apps() -> Result<Vec<AppInfo>> {
    let x = X::connect()?;
    let installed = installed_apps();
    let mut apps: Vec<AppInfo> =
        x.windows_mru()?.into_iter().filter_map(|w| x.app_info(w, &installed)).collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps.dedup_by(|a, b| a.app == b.app);
    Ok(apps)
}

pub fn installed_apps() -> Vec<AppInfo> {
    desktop_entry::scan(&desktop_entry::search_dirs())
}

pub fn frontmost_app() -> Result<Option<AppInfo>> {
    let x = X::connect()?;
    let w = x.current_active();
    if w == 0 || x.pid(w) == Some(std::process::id()) {
        return Ok(None);
    }
    Ok(x.app_info(w, &installed_apps()))
}

pub fn focus_pid(pid: u32) -> Result<bool> {
    let x = X::connect()?;
    match x.windows_mru()?.into_iter().find(|&w| x.pid(w) == Some(pid)) {
        Some(w) => x.activate(w).map(|()| true),
        None => Ok(false),
    }
}

pub fn diagnostics() -> Vec<String> {
    let desktop = env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into());
    let mut out = vec![format!("Linux session: {:?}, desktop: {desktop}", session())];
    if let Err(e) = ensure_supported() {
        out.push(format!("NOT SUPPORTED: {e}"));
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
    use super::parse_wm_class;

    #[test]
    fn wm_class_parsing() {
        assert_eq!(
            parse_wm_class(b"gnome-terminal-server\0Gnome-terminal\0"),
            ["gnome-terminal-server", "Gnome-terminal"]
        );
        assert_eq!(parse_wm_class(b""), Vec::<String>::new());
    }
}

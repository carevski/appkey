//! X11 / EWMH windows.
//!
//! Windows are matched on `WM_CLASS` (instance or class). `_NET_CLIENT_LIST_STACKING`
//! is ordered bottom to top, so reversed it is most-recently-used first.
//! Activation is a `_NET_ACTIVE_WINDOW` request with source indication 2 ("pager"),
//! which window managers honour without focus-stealing prevention, and which
//! also switches workspace / un-minimizes as needed.

use anyhow::{Context, Result, bail};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ClientMessageEvent, ConnectionExt, CreateWindowAux, EventMask, KeyButMask,
    PropMode, Timestamp, Window, WindowClass,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;

use super::{Win, WindowSystem};

/// Whether Super (Mod4) is held right now, from the server's keyboard state. Cinnamon
/// (Muffin) grabs the Super key's own press for its menu, so our window never sees it.
pub(super) fn logo_key_down() -> bool {
    let Ok((conn, screen)) = x11rb::connect(None) else { return false };
    let root = conn.setup().roots[screen].root;
    conn.query_pointer(root)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.mask.contains(KeyButMask::MOD4))
}

pub(super) struct X {
    conn: RustConnection,
    root: Window,
    stacking: Atom,
    client_list: Atom,
    active: Atom,
    wm_pid: Atom,
    wm_name: Atom,
    utf8: Atom,
}

impl X {
    pub fn connect() -> Result<Self> {
        let (conn, screen) = x11rb::connect(None).context("connecting to the X server")?;
        let root = conn.setup().roots[screen].root;
        let atom =
            |name: &[u8]| -> Result<Atom> { Ok(conn.intern_atom(false, name)?.reply()?.atom) };
        let stacking = atom(b"_NET_CLIENT_LIST_STACKING")?;
        let client_list = atom(b"_NET_CLIENT_LIST")?;
        let active = atom(b"_NET_ACTIVE_WINDOW")?;
        let wm_pid = atom(b"_NET_WM_PID")?;
        let wm_name = atom(b"_NET_WM_NAME")?;
        let utf8 = atom(b"UTF8_STRING")?;
        Ok(Self { conn, root, stacking, client_list, active, wm_pid, wm_name, utf8 })
    }

    /// Managed windows, most recently used first (falls back to mapping order).
    fn windows_mru(&self) -> Result<Vec<Window>> {
        // A published but empty list just means no windows are open (yet).
        let mut published = false;
        for prop in [self.stacking, self.client_list] {
            let reply = self
                .conn
                .get_property(false, self.root, prop, AtomEnum::WINDOW, 0, u32::MAX)?
                .reply()?;
            if let Some(list) = reply.value32() {
                published = true;
                let mut list: Vec<Window> = list.collect();
                list.reverse();
                if !list.is_empty() {
                    return Ok(list);
                }
            }
        }
        if published {
            return Ok(Vec::new());
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

    /// `_NET_WM_NAME` (UTF-8), else the legacy `WM_NAME`.
    fn title(&self, w: Window) -> String {
        for (prop, ty) in
            [(self.wm_name, self.utf8), (AtomEnum::WM_NAME.into(), AtomEnum::ANY.into())]
        {
            if let Ok(Ok(reply)) = self
                .conn
                .get_property(false, w, prop, ty, 0, 1024)
                .map(x11rb::cookie::Cookie::reply)
                && !reply.value.is_empty()
            {
                return String::from_utf8_lossy(&reply.value).into_owned();
            }
        }
        String::new()
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

    fn activate_window(&self, w: Window) -> Result<()> {
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

impl X {
    fn win(&self, w: Window) -> Win {
        Win { handle: w.to_string(), ids: self.wm_class(w), pid: self.pid(w), title: self.title(w) }
    }
}

fn handle(w: &Win) -> Result<Window> {
    w.handle.parse().context("not an X11 window id")
}

impl WindowSystem for X {
    fn windows(&self) -> Result<Vec<Win>> {
        Ok(self.windows_mru()?.into_iter().map(|w| self.win(w)).collect())
    }

    fn activate(&self, w: &Win) -> Result<()> {
        self.activate_window(handle(w)?)
    }

    fn active(&self) -> Result<Option<Win>> {
        Ok(Some(self.current_active()).filter(|&w| w != 0).map(|w| self.win(w)))
    }
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

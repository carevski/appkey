//! Global shortcuts through the XDG desktop portal
//! (`org.freedesktop.portal.GlobalShortcuts`). The desktop owns the key grab and
//! may show the user a dialog to approve or change the keys.
//!
//! Shortcut ids are the `HotKey::id()` in decimal, so a changed key combination
//! is a new shortcut as far as the desktop is concerned (desktops remember the
//! keys per id and may otherwise keep the old ones).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use zbus::blocking::{Connection, MessageIterator, Proxy};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use super::xkb;
use crate::config::Shortcut;
use crate::hotkeys::{Hotkeys, Sink};

const DEST: &str = "org.freedesktop.portal.Desktop";
const PATH: &str = "/org/freedesktop/portal/desktop";
const IFACE: &str = "org.freedesktop.portal.GlobalShortcuts";
/// The id our `.desktop` file is installed under; portals need it to identify us.
pub const APP_ID: &str = "appkey";

type Options = HashMap<String, OwnedValue>;

/// Desktop-specific tidy-up after a successful bind, given the ids now in use.
pub type Cleanup = fn(&Connection, &[String]);

pub struct PortalHotkeys {
    conn: Connection,
    cleanup: Option<Cleanup>,
    session: Arc<Mutex<Option<OwnedObjectPath>>>,
    counter: u32,
}

impl std::fmt::Debug for PortalHotkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PortalHotkeys").field("session", &self.session).finish_non_exhaustive()
    }
}

impl PortalHotkeys {
    pub fn new(sink: Sink, cleanup: Option<Cleanup>) -> Result<Self> {
        let conn = Connection::session().context("connecting to the session D-Bus")?;
        if let Err(e) = ensure_desktop_file() {
            crate::app::log(&format!("could not install appkey.desktop: {e:#}"));
        }
        // Host (non-Flatpak) apps must say who they are before using portals
        // (xdg-desktop-portal ≥ 1.19; GNOME 50 refuses unidentified callers).
        // Older portals don't have the Registry; then the call fails harmlessly.
        if let Err(e) = conn.call_method(
            Some(DEST),
            PATH,
            Some("org.freedesktop.host.portal.Registry"),
            "Register",
            &(APP_ID, Options::new()),
        ) {
            crate::app::log(&format!("portal app registration failed: {e}"));
        }
        let proxy = Proxy::new(&conn, DEST, PATH, IFACE)?;
        let version: u32 = proxy.get_property("version").context(
            "this desktop has no GlobalShortcuts portal (install xdg-desktop-portal and your desktop's portal backend)",
        )?;
        crate::app::log(&format!("using the GlobalShortcuts portal (version {version})"));

        let session: Arc<Mutex<Option<OwnedObjectPath>>> = Arc::default();
        let current = session.clone();
        let signals = proxy.receive_signal("Activated")?;
        std::thread::spawn(move || {
            for msg in signals {
                let Ok((handle, id, _ts, _opts)) =
                    msg.body().deserialize::<(OwnedObjectPath, String, u64, Options)>()
                else {
                    continue;
                };
                let is_current = current.lock().is_ok_and(|s| s.as_ref() == Some(&handle));
                if let (true, Ok(id)) = (is_current, id.parse::<u32>()) {
                    sink(id);
                }
            }
        });
        Ok(Self { conn, cleanup, session, counter: 0 })
    }

    fn token(&mut self) -> String {
        self.counter += 1;
        format!("appkey{}_{}", std::process::id(), self.counter)
    }

    /// Calls a portal method that answers through a `Request` object, and waits
    /// for its `Response`.
    fn request<B>(&mut self, method: &str, build: impl FnOnce(String) -> B) -> Result<Options>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let token = self.token();
        let sender = self
            .conn
            .unique_name()
            .context("no D-Bus name")?
            .trim_start_matches(':')
            .replace('.', "_");
        let path = format!("{PATH}/request/{sender}/{token}");
        // Subscribe before calling, so a fast response can't be missed.
        let rule = zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.portal.Request")?
            .member("Response")?
            .path(path.clone())?
            .build();
        let mut responses = MessageIterator::for_match_rule(rule, &self.conn, Some(4))?;
        self.conn.call_method(Some(DEST), PATH, Some(IFACE), method, &build(token))?;

        // The user may be looking at an approval dialog; give them time.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(responses.next());
        });
        let msg = rx
            .recv_timeout(Duration::from_mins(2))
            .map_err(|_| anyhow!("no answer from the desktop portal"))?
            .ok_or_else(|| anyhow!("portal connection closed"))??;
        let (code, results): (u32, Options) = msg.body().deserialize()?;
        match code {
            0 => Ok(results),
            1 => bail!("shortcuts were not approved"),
            _ => bail!("the desktop portal refused the request"),
        }
    }

    /// Binds `shortcuts`; returns the ids the desktop left without keys.
    fn bind(&mut self, shortcuts: &[Shortcut]) -> Result<Vec<String>> {
        if let Some(old) = self.session.lock().map_err(|_| anyhow!("poisoned"))?.take() {
            let _ = self.conn.call_method(
                Some(DEST),
                &old,
                Some("org.freedesktop.portal.Session"),
                "Close",
                &(),
            );
        }
        if shortcuts.is_empty() {
            return Ok(Vec::new());
        }
        debug("portal: CreateSession");
        let results = self.request("CreateSession", |token| {
            let mut opts = HashMap::new();
            opts.insert("handle_token", Value::from(token.clone()));
            opts.insert("session_handle_token", Value::from(token));
            (opts,)
        })?;
        let handle: String = results
            .get("session_handle")
            .and_then(|v| v.downcast_ref::<String>().ok())
            .context("portal returned no session")?;
        let handle = OwnedObjectPath::try_from(handle)?;
        *self.session.lock().map_err(|_| anyhow!("poisoned"))? = Some(handle.clone());

        let list: Vec<(String, HashMap<&str, Value>)> = shortcuts
            .iter()
            .map(|s| {
                let mut props = HashMap::new();
                props.insert("description", Value::from(format!("Open {}", s.app)));
                if let Some(c) = xkb::parse(&s.keys) {
                    props.insert("preferred_trigger", Value::from(c.portal()));
                }
                (s.hotkey.id().to_string(), props)
            })
            .collect();
        let session_path = ObjectPath::try_from(handle.as_str())?;
        debug(&format!("portal: BindShortcuts on {handle}"));
        let results = self.request("BindShortcuts", |token| {
            let mut opts = HashMap::new();
            opts.insert("handle_token", Value::from(token));
            (session_path, list, "", opts)
        })?;
        debug("portal: bound");
        // A desktop can accept a shortcut but assign it no keys (KDE does after the
        // user once cancelled its dialog). Report those instead of claiming success.
        let unassigned = results
            .get("shortcuts")
            .and_then(|v| v.try_clone().ok())
            .and_then(|v| Vec::<(String, HashMap<String, OwnedValue>)>::try_from(v).ok())
            .map(|bound| {
                bound
                    .into_iter()
                    .filter(|(_, props)| {
                        props
                            .get("trigger_description")
                            .and_then(|t| t.downcast_ref::<String>().ok())
                            .is_none_or(|t| t.trim().is_empty())
                    })
                    .map(|(id, _)| id)
                    .collect()
            })
            .unwrap_or_default();
        if let Some(cleanup) = self.cleanup {
            let ids: Vec<String> = shortcuts.iter().map(|s| s.hotkey.id().to_string()).collect();
            cleanup(&self.conn, &ids);
        }
        Ok(unassigned)
    }
}

fn debug(msg: &str) {
    if std::env::var_os("APPKEY_DEBUG").is_some() {
        crate::app::log(msg);
    }
}

/// Portals identify apps by their `.desktop` file. Packages install one; for the
/// plain `.tar.gz` we write `~/.local/share/applications/appkey.desktop` pointing
/// at the running binary.
fn ensure_desktop_file() -> Result<()> {
    let file = format!("{APP_ID}.desktop");
    if super::desktop_entry::search_dirs().iter().any(|d| d.join(&file).exists()) {
        return Ok(());
    }
    let dir = dirs::data_dir().context("no data dir")?.join("applications");
    std::fs::create_dir_all(&dir)?;
    let exe = std::env::current_exe()?;
    let exe = exe.to_string_lossy();
    // Exec quoting per the desktop entry spec.
    let quoted = format!(
        "\"{}\"",
        exe.replace('\\', "\\\\").replace('"', "\\\"").replace('`', "\\`").replace('$', "\\$")
    );
    std::fs::write(
        dir.join(&file),
        format!(
            "[Desktop Entry]\nType=Application\nName=appkey\nComment=Global shortcuts that open or focus your apps\n\
             Exec={quoted}\nIcon=appkey\nTerminal=false\nCategories=Utility;\n"
        ),
    )?;
    crate::app::log(&format!("installed {}", dir.join(&file).display()));
    Ok(())
}

impl Hotkeys for PortalHotkeys {
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String> {
        match self.bind(shortcuts) {
            Ok(unassigned) => shortcuts
                .iter()
                .filter(|s| unassigned.contains(&s.hotkey.id().to_string()))
                .map(|s| format!("{} (no keys assigned; set them in your desktop's shortcut settings, on KDE: System Settings → Shortcuts → appkey)", s.keys))
                .collect(),
            Err(e) => shortcuts.iter().map(|s| format!("{} ({e:#})", s.keys)).collect(),
        }
    }
}

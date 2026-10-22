//! KDE Plasma (KWin, Wayland) windows via KWin scripting.
//!
//! KWin has no D-Bus call that lists or focuses windows, but it runs JavaScript
//! sent over D-Bus. We load a tiny generated script that reports back by calling
//! a method on our own D-Bus connection (`callDBus`), then unload it. No
//! permissions are involved: KWin accepts scripts from the user's own session.

use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde::Deserialize;
use zbus::blocking::Connection;

use super::{Win, WindowSystem};

const RESULT_PATH: &str = "/io/github/carevski/appkey/kwin";
const RESULT_IFACE: &str = "io.github.carevski.Appkey.KWin";

#[derive(Debug)]
pub(super) struct Kwin {
    conn: Connection,
    results: std::sync::mpsc::Receiver<String>,
}

/// Receives the JSON a script sends back.
struct Receiver(Sender<String>);

#[zbus::interface(name = "io.github.carevski.Appkey.KWin")]
impl Receiver {
    fn result(&self, json: String) {
        let _ = self.0.send(json);
    }
}

#[derive(Debug, Deserialize)]
struct KWindow {
    id: String,
    class: String,
    name: String,
    desktop: String,
    pid: i64,
}

impl From<KWindow> for Win {
    fn from(w: KWindow) -> Self {
        let mut ids = Vec::new();
        for id in [w.desktop, w.class, w.name] {
            if !id.is_empty() && !ids.contains(&id) {
                ids.push(id);
            }
        }
        Win { handle: w.id, ids, pid: u32::try_from(w.pid).ok().filter(|&p| p > 0) }
    }
}

/// Shared JS: KWin 6 names first, KWin 5 fallbacks.
const PRELUDE: &str = r"
const stack = workspace.stackingOrder || (workspace.windowList ? workspace.windowList() : workspace.clientList());
const apps = stack.filter(w => w.normalWindow && !w.skipTaskbar && !w.deleted);
const info = w => ({ id: String(w.internalId), class: String(w.resourceClass || ''), name: String(w.resourceName || ''),
                     desktop: String(w.desktopFileName || ''), pid: w.pid || 0 });
const active = () => workspace.activeWindow !== undefined ? workspace.activeWindow : workspace.activeClient;
";

impl Kwin {
    pub fn connect() -> Result<Self> {
        let conn = Connection::session().context("connecting to the session D-Bus")?;
        let (tx, results) = channel();
        conn.object_server().at(RESULT_PATH, Receiver(tx))?;
        Ok(Self { conn, results })
    }

    /// Runs `body` (JS with `reply(value)` available) and returns what it replied.
    fn run(&self, body: &str) -> Result<String> {
        let me = self.conn.unique_name().context("no D-Bus name")?.to_string();
        let script = format!(
            "{PRELUDE}\nconst reply = v => callDBus({me}, {RESULT_PATH:?}, {RESULT_IFACE:?}, 'Result', JSON.stringify(v));\n{body}\n",
            me = serde_json::to_string(&me)?,
        );
        // The user's private runtime dir; KWin reads the script from a path.
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(std::env::temp_dir, Into::into);
        let plugin = format!("appkey-{}-{}", std::process::id(), rand_suffix());
        let path = std::path::Path::new(&dir).join(format!("{plugin}.js"));
        std::fs::write(&path, script)?;

        let id: i32 = self
            .call(
                "/Scripting",
                "org.kde.kwin.Scripting",
                "loadScript",
                &(path.to_string_lossy().into_owned(), plugin.clone()),
            )?
            .body()
            .deserialize()?;
        // KWin 6 exposes scripts at /Scripting/ScriptN, KWin 5 at /N.
        let ran = self
            .call(&format!("/Scripting/Script{id}"), "org.kde.kwin.Script", "run", &())
            .or_else(|_| self.call(&format!("/{id}"), "org.kde.kwin.Script", "run", &()));
        let result = ran.map_err(anyhow::Error::from).and_then(|_| {
            self.results
                .recv_timeout(Duration::from_secs(3))
                .map_err(|_| anyhow!("KWin script did not answer"))
        });
        let _ = self.call("/Scripting", "org.kde.kwin.Scripting", "unloadScript", &(plugin,));
        let _ = std::fs::remove_file(&path);
        result
    }
}

impl Kwin {
    fn call<B>(
        &self,
        path: &str,
        iface: &str,
        method: &str,
        body: &B,
    ) -> zbus::Result<zbus::Message>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        self.conn.call_method(Some("org.kde.KWin"), path, Some(iface), method, body)
    }
}

/// KDE keeps every shortcut an app ever bound through the portal (and keeps
/// grabbing its keys). After a bind, drop the ones we no longer use.
pub(super) fn forget_stale_shortcuts(conn: &Connection, keep: &[String]) {
    let component = format!("/component/{}", super::portal::APP_ID);
    let Ok(reply) = conn.call_method(
        Some("org.kde.kglobalaccel"),
        component.as_str(),
        Some("org.kde.kglobalaccel.Component"),
        "shortcutNames",
        &(),
    ) else {
        return;
    };
    let Ok(names) = reply.body().deserialize::<Vec<String>>() else { return };
    for name in names.iter().filter(|n| !keep.contains(n)) {
        let _ = conn.call_method(
            Some("org.kde.kglobalaccel"),
            "/kglobalaccel",
            Some("org.kde.KGlobalAccel"),
            "unregister",
            &(super::portal::APP_ID, name.as_str()),
        );
    }
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

impl WindowSystem for Kwin {
    fn windows(&self) -> Result<Vec<Win>> {
        // Stacking order is bottom → top; reversed it's most recently used first.
        let json = self.run("reply(apps.map(info).reverse());")?;
        let list: Vec<KWindow> = serde_json::from_str(&json)?;
        Ok(list.into_iter().map(Win::from).collect())
    }

    fn activate(&self, w: &Win) -> Result<()> {
        let id = serde_json::to_string(&w.handle)?;
        let json = self.run(&format!(
            "const w = apps.find(w => String(w.internalId) === {id});\n\
             if (w) {{ w.minimized = false; workspace.activeWindow !== undefined ? workspace.activeWindow = w : workspace.activeClient = w; }}\n\
             reply(!!w);"
        ))?;
        if json == "true" { Ok(()) } else { Err(anyhow!("window is gone")) }
    }

    fn active(&self) -> Result<Option<Win>> {
        let json = self.run("const a = active(); reply(a ? info(a) : null);")?;
        Ok(serde_json::from_str::<Option<KWindow>>(&json)?.map(Win::from))
    }
}

/// One `KGlobalShortcutInfo`: action, component and context names (unique, friendly
/// each), then active and default keys. Plasma 5 sends each key as an `int`…
type InfoV5 = (String, String, String, String, String, String, Vec<i32>, Vec<i32>);
/// …Plasma 6 as a key sequence: a struct of up to four `int`s, or a bare array.
type InfoV6 = (String, String, String, String, String, String, Vec<(Vec<i32>,)>, Vec<(Vec<i32>,)>);
type InfoV6Bare = (String, String, String, String, String, String, Vec<Vec<i32>>, Vec<Vec<i32>>);

/// `(component, action, active key sequences)` from one `allShortcutInfos` reply.
fn shortcut_infos(body: &zbus::message::Body) -> Vec<(String, String, Vec<Vec<i32>>)> {
    if let Ok(v) = body.deserialize::<Vec<InfoV5>>() {
        return v
            .into_iter()
            .map(|i| (i.2, i.1, i.6.into_iter().map(|k| vec![k]).collect()))
            .collect();
    }
    if let Ok(v) = body.deserialize::<Vec<InfoV6>>() {
        return v
            .into_iter()
            .map(|i| (i.2, i.1, i.6.into_iter().map(|(k,)| k).collect()))
            .collect();
    }
    body.deserialize::<Vec<InfoV6Bare>>()
        .map(|v| v.into_iter().map(|i| (i.2, i.1, i.6)).collect())
        .unwrap_or_default()
}

/// Every global shortcut KDE components hold (KWin, Plasma, apps), except appkey's own.
pub(super) fn reserved() -> Vec<(u32, String)> {
    let Ok(conn) = Connection::session() else { return Vec::new() };
    let call = |path: &str, iface: &str, method: &str| {
        conn.call_method(Some("org.kde.kglobalaccel"), path, Some(iface), method, &())
    };
    let Ok(reply) = call("/kglobalaccel", "org.kde.KGlobalAccel", "allComponents") else {
        return Vec::new();
    };
    let paths: Vec<zbus::zvariant::OwnedObjectPath> =
        reply.body().deserialize().unwrap_or_default();
    let mut found = Vec::new();
    for path in paths {
        let Ok(reply) = call(path.as_str(), "org.kde.kglobalaccel.Component", "allShortcutInfos")
        else {
            continue;
        };
        for (component, action, sequences) in shortcut_infos(&reply.body()) {
            if component == super::portal::APP_ID {
                continue;
            }
            // Only single-key sequences can clash with a global shortcut; 0 pads unused slots.
            for seq in sequences {
                if let [code] = seq.iter().copied().filter(|&c| c != 0).collect::<Vec<_>>()[..]
                    && let Some(keys) = super::xkb::from_qt(code)
                {
                    found.push((keys, action.clone()));
                }
            }
        }
    }
    super::reserved::to_ids(found, "KDE")
}

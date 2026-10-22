//! GNOME Shell (Wayland) via appkey's GNOME Shell extension
//! (`packaging/gnome-extension`), which runs inside the shell and offers window
//! listing/focusing and key grabs over D-Bus. GNOME has no other way to do this.
//!
//! One-time setup: if no package installed the extension, we install it into
//! `~/.local/share/gnome-shell/extensions` and enable it. GNOME only discovers
//! new extensions at login, so the user logs out and back in once.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use zbus::blocking::{Connection, Proxy};

use super::{Win, WindowSystem, xkb};
use crate::config::Shortcut;
use crate::hotkeys::{Hotkeys, Sink};

pub const UUID: &str = "appkey@carevski.github.io";
const DEST: &str = "org.gnome.Shell";
const PATH: &str = "/io/github/carevski/appkey";
const IFACE: &str = "io.github.carevski.Appkey";

const METADATA: &str =
    include_str!("../../../packaging/gnome-extension/appkey@carevski.github.io/metadata.json");
const EXTENSION_JS: &str =
    include_str!("../../../packaging/gnome-extension/appkey@carevski.github.io/extension.js");

const NOT_RUNNING: &str = "appkey's GNOME helper isn't running yet. Log out and back in once to \
     finish setup (GNOME only loads new extensions at login). If it still doesn't work, check it's \
     enabled in the Extensions app";

#[derive(Debug)]
pub(super) struct Shell {
    conn: Connection,
}

#[derive(Debug, Deserialize)]
struct ShellWindow {
    id: String,
    ids: Vec<String>,
    pid: i64,
}

impl From<ShellWindow> for Win {
    fn from(w: ShellWindow) -> Self {
        Win { handle: w.id, ids: w.ids, pid: u32::try_from(w.pid).ok().filter(|&p| p > 0) }
    }
}

impl Shell {
    pub fn connect() -> Result<Self> {
        let conn = Connection::session().context("connecting to the session D-Bus")?;
        let shell = Self { conn };
        if shell.proxy()?.get_property::<u32>("Version").is_err() {
            if let Err(e) = setup() {
                crate::app::log(&format!("GNOME helper setup: {e:#}"));
            }
            bail!("{NOT_RUNNING}");
        }
        Ok(shell)
    }

    fn proxy(&self) -> Result<Proxy<'_>> {
        Ok(Proxy::new(&self.conn, DEST, PATH, IFACE)?)
    }

    fn call_json<T: serde::de::DeserializeOwned>(&self, method: &str) -> Result<T> {
        let json: String = self.proxy()?.call(method, &())?;
        Ok(serde_json::from_str(&json)?)
    }
}

impl WindowSystem for Shell {
    fn windows(&self) -> Result<Vec<Win>> {
        Ok(self.call_json::<Vec<ShellWindow>>("ListWindows")?.into_iter().map(Win::from).collect())
    }

    fn activate(&self, w: &Win) -> Result<()> {
        let found: bool = self.proxy()?.call("Activate", &(w.handle.as_str(),))?;
        if found { Ok(()) } else { Err(anyhow!("window is gone")) }
    }

    fn active(&self) -> Result<Option<Win>> {
        Ok(self.call_json::<Option<ShellWindow>>("ActiveWindow")?.map(Win::from))
    }
}

/// Installs the extension for this user (unless a package did) and enables it.
fn setup() -> Result<()> {
    let installed =
        super::desktop_entry::search_dirs().iter().filter_map(|apps| apps.parent()).any(|share| {
            share.join("gnome-shell/extensions").join(UUID).join("metadata.json").exists()
        });
    if !installed {
        let dir =
            dirs::data_dir().context("no data dir")?.join("gnome-shell/extensions").join(UUID);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("metadata.json"), METADATA)?;
        std::fs::write(dir.join("extension.js"), EXTENSION_JS)?;
        crate::app::log(&format!("installed the GNOME helper extension in {}", dir.display()));
    }
    enable()
}

/// Adds us to `org.gnome.shell enabled-extensions` (takes effect at next login if
/// the shell doesn't know the extension yet), and asks a running shell directly.
fn enable() -> Result<()> {
    let conn = Connection::session()?;
    let _ = conn.call_method(
        Some(DEST),
        "/org/gnome/Shell",
        Some("org.gnome.Shell.Extensions"),
        "EnableExtension",
        &(UUID,),
    );
    let out = std::process::Command::new("gsettings")
        .args(["get", "org.gnome.shell", "enabled-extensions"])
        .output()
        .context("running gsettings")?;
    let current = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    if current.contains(UUID) {
        return Ok(());
    }
    let list = parse_gvariant_strv(&current);
    let mut items: Vec<String> = list.iter().map(|s| format!("'{s}'")).collect();
    items.push(format!("'{UUID}'"));
    let status = std::process::Command::new("gsettings")
        .args(["set", "org.gnome.shell", "enabled-extensions", &format!("[{}]", items.join(", "))])
        .status()?;
    if !status.success() {
        bail!("gsettings could not enable the extension");
    }
    Ok(())
}

/// `['a', 'b']` or `@as []` → `["a", "b"]`. Extension uuids contain no quotes.
pub(super) fn parse_gvariant_strv(s: &str) -> Vec<String> {
    s.trim_start_matches("@as")
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split(',')
        .map(|item| item.trim().trim_matches('\'').to_owned())
        .filter(|item| !item.is_empty())
        .collect()
}

/// Every shortcut GNOME uses itself (Shell, Mutter, settings daemon, and the Ubuntu
/// Dock's Super+number keys when they're on), plus the user's custom ones from
/// Settings → Keyboard.
pub(super) fn reserved() -> Vec<(u32, String)> {
    use super::reserved::{custom_shortcut, gsettings, schema_shortcuts, to_ids};
    const MEDIA: &str = "org.gnome.settings-daemon.plugins.media-keys";
    const DOCK: &str = "org.gnome.shell.extensions.dash-to-dock";
    let mut found: Vec<(String, String)> = [
        "org.gnome.desktop.wm.keybindings",
        "org.gnome.shell.keybindings",
        "org.gnome.mutter.keybindings",
        "org.gnome.mutter.wayland.keybindings",
        MEDIA,
    ]
    .iter()
    .flat_map(|s| schema_shortcuts(s))
    .collect();
    if gsettings(&["get", DOCK, "hot-keys"]).is_ok_and(|v| v == "true") {
        let dock = schema_shortcuts(DOCK);
        found.extend(
            dock.into_iter().map(|(k, w)| (k, format!("Ubuntu Dock {}", w.to_lowercase()))),
        );
    }
    let paths = gsettings(&["get", MEDIA, "custom-keybindings"]).unwrap_or_default();
    for path in parse_gvariant_strv(&paths) {
        found.extend(custom_shortcut(&format!("{MEDIA}.custom-keybinding:{path}")));
    }
    to_ids(found, "GNOME")
}

/// Shortcuts grabbed by the extension; it reports presses as `Activated(action)`.
pub(super) struct ShellHotkeys {
    conn: Connection,
    /// `(accelerator, hotkey id)` currently wanted, re-grabbed when the extension restarts.
    wanted: Arc<Mutex<Vec<(String, u32)>>>,
    /// Grab action → hotkey id.
    actions: Arc<Mutex<HashMap<u32, u32>>>,
}

impl std::fmt::Debug for ShellHotkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellHotkeys").field("wanted", &self.wanted).finish_non_exhaustive()
    }
}

/// Grabs `wanted` and records the action ids; returns the accelerators that failed.
fn grab(
    conn: &Connection,
    wanted: &[(String, u32)],
    actions: &Mutex<HashMap<u32, u32>>,
) -> Result<Vec<String>> {
    let proxy = Proxy::new(conn, DEST, PATH, IFACE)?;
    proxy.call::<_, _, ()>("UngrabAll", &())?;
    let accels: Vec<&str> = wanted.iter().map(|(a, _)| a.as_str()).collect();
    let got: Vec<u32> = proxy.call("GrabAccelerators", &(accels,))?;
    let mut map = actions.lock().map_err(|_| anyhow!("poisoned"))?;
    map.clear();
    let mut failed = Vec::new();
    for ((accel, id), action) in wanted.iter().zip(got) {
        if action == 0 {
            failed.push(accel.clone());
        } else {
            map.insert(action, *id);
        }
    }
    Ok(failed)
}

impl ShellHotkeys {
    pub fn new(sink: Sink) -> Result<Self> {
        Shell::connect()?; // fails with setup instructions if the extension isn't running
        let conn = Connection::session()?;
        let wanted: Arc<Mutex<Vec<(String, u32)>>> = Arc::default();
        let actions: Arc<Mutex<HashMap<u32, u32>>> = Arc::default();

        let proxy = Proxy::new(&conn, DEST, PATH, IFACE)?;
        let activated = proxy.receive_signal("Activated")?;
        let map = actions.clone();
        std::thread::spawn(move || {
            for msg in activated {
                if let Ok(action) = msg.body().deserialize::<u32>()
                    && let Some(&id) =
                        map.lock().ok().and_then(|m| m.get(&action).copied()).as_ref()
                {
                    sink(id);
                }
            }
        });
        // The shell disables extensions while the screen is locked; when ours comes
        // back its grabs are gone, so redo them.
        let started = proxy.receive_signal("Started")?;
        let (conn2, wanted2, actions2) = (conn.clone(), wanted.clone(), actions.clone());
        std::thread::spawn(move || {
            for _ in started {
                if let Ok(w) = wanted2.lock().map(|w| w.clone()) {
                    let _ = grab(&conn2, &w, &actions2);
                }
            }
        });
        Ok(Self { conn, wanted, actions })
    }
}

impl Hotkeys for ShellHotkeys {
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String> {
        let mut failed = Vec::new();
        let mut wanted = Vec::new();
        for s in shortcuts {
            match xkb::parse(&s.keys) {
                Some(c) => wanted.push((c.gtk(), s.hotkey.id())),
                None => failed.push(format!("{} (key not supported on GNOME)", s.keys)),
            }
        }
        if let Ok(mut w) = self.wanted.lock() {
            w.clone_from(&wanted);
        }
        match grab(&self.conn, &wanted, &self.actions) {
            Ok(taken) => {
                for accel in taken {
                    let keys = shortcuts
                        .iter()
                        .find(|s| xkb::parse(&s.keys).is_some_and(|c| c.gtk() == accel));
                    failed.push(format!(
                        "{} (already used by GNOME or another app)",
                        keys.map_or(&accel, |s| &s.keys)
                    ));
                }
            }
            Err(e) => failed.extend(shortcuts.iter().map(|s| format!("{} ({e:#})", s.keys))),
        }
        failed
    }
}

impl Drop for ShellHotkeys {
    fn drop(&mut self) {
        let _ = Proxy::new(&self.conn, DEST, PATH, IFACE)
            .and_then(|p| p.call::<_, _, ()>("UngrabAll", &()));
    }
}

/// Where the extension files live, for `appkey check`.
pub fn extension_dirs() -> Vec<PathBuf> {
    super::desktop_entry::search_dirs()
        .iter()
        .filter_map(|apps| apps.parent())
        .map(|share| share.join("gnome-shell/extensions").join(UUID))
        .filter(|d| d.exists())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::parse_gvariant_strv;

    #[test]
    fn gsettings_lists() {
        assert_eq!(parse_gvariant_strv("@as []"), Vec::<String>::new());
        assert_eq!(
            parse_gvariant_strv("['a@b', 'ubuntu-dock@ubuntu.com']"),
            ["a@b", "ubuntu-dock@ubuntu.com"]
        );
    }
}

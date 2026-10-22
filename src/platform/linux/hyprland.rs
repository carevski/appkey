//! Hyprland: windows and shortcuts over its IPC socket.
//!
//! - Windows: `j/clients` (with `focusHistoryID`, Hyprland's own most-recently-used
//!   order), `dispatch focuswindow address:…`, `j/activewindow`.
//! - Shortcuts: runtime `keyword bind …,exec,<appkey> __hotkey <id>`. The command
//!   only ever contains our binary's path and a number, never config text. Runtime
//!   binds vanish when Hyprland reloads its config, so we listen on the event
//!   socket and re-apply them.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::{Win, WindowSystem, xkb};
use crate::config::Shortcut;
use crate::hotkeys::Hotkeys;

#[derive(Debug, Clone)]
pub(super) struct Hypr {
    dir: PathBuf,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Client {
    address: String,
    #[serde(default)]
    class: String,
    #[serde(default)]
    initial_class: String,
    #[serde(default)]
    pid: i64,
    #[serde(default)]
    title: String,
    #[serde(rename = "focusHistoryID", default)]
    focus_history_id: i64,
    #[serde(default = "yes")]
    mapped: bool,
}

const fn yes() -> bool {
    true
}

impl From<Client> for Win {
    fn from(c: Client) -> Self {
        let mut ids = vec![c.class];
        if !c.initial_class.is_empty() && !ids.contains(&c.initial_class) {
            ids.push(c.initial_class);
        }
        let pid = u32::try_from(c.pid).ok().filter(|&p| p > 0);
        Win { handle: c.address, ids, pid, title: c.title }
    }
}

impl Hypr {
    pub fn connect() -> Result<Self> {
        let sig = std::env::var("HYPRLAND_INSTANCE_SIGNATURE")
            .context("HYPRLAND_INSTANCE_SIGNATURE is not set (is Hyprland running?)")?;
        // Current location, then the pre-0.40 one.
        let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        let dir = runtime
            .map(|r| r.join("hypr").join(&sig))
            .into_iter()
            .chain([PathBuf::from("/tmp/hypr").join(&sig)])
            .find(|d| d.join(".socket.sock").exists())
            .context("Hyprland's IPC socket not found")?;
        Ok(Self { dir })
    }

    fn request(&self, cmd: &str) -> Result<String> {
        let mut s =
            UnixStream::connect(self.dir.join(".socket.sock")).context("connecting to Hyprland")?;
        s.write_all(cmd.as_bytes())?;
        let mut out = String::new();
        s.read_to_string(&mut out)?;
        Ok(out)
    }

    fn expect_ok(&self, cmd: &str) -> Result<()> {
        let reply = self.request(cmd)?;
        if reply.trim() == "ok" { Ok(()) } else { bail!("Hyprland: {}", reply.trim()) }
    }
}

impl WindowSystem for Hypr {
    fn windows(&self) -> Result<Vec<Win>> {
        let mut clients: Vec<Client> = serde_json::from_str(&self.request("j/clients")?)?;
        clients.retain(|c| c.mapped);
        clients.sort_by_key(|c| c.focus_history_id);
        Ok(clients.into_iter().map(Win::from).collect())
    }

    fn activate(&self, w: &Win) -> Result<()> {
        // Addresses are hex numbers from Hyprland itself; refuse anything else.
        if !w.handle.starts_with("0x") || !w.handle[2..].chars().all(|c| c.is_ascii_hexdigit()) {
            bail!("unexpected Hyprland window address {:?}", w.handle);
        }
        self.expect_ok(&format!("dispatch focuswindow address:{}", w.handle))
    }

    fn active(&self) -> Result<Option<Win>> {
        let reply = self.request("j/activewindow")?;
        // `{}` when nothing is focused.
        Ok(serde_json::from_str::<Client>(&reply).ok().map(Win::from))
    }
}

/// Shortcuts as Hyprland binds that run `appkey __hotkey <id>`.
#[derive(Debug)]
pub(super) struct HyprHotkeys {
    hypr: Hypr,
    /// `(mods, key, command)` for each bind we own, re-applied after config reloads.
    binds: Arc<Mutex<Vec<(String, String, String)>>>,
}

impl HyprHotkeys {
    pub fn new() -> Result<Self> {
        let hypr = Hypr::connect()?;
        let binds: Arc<Mutex<Vec<(String, String, String)>>> = Arc::default();
        let (listener, owned) = (hypr.clone(), binds.clone());
        std::thread::spawn(move || {
            let Ok(events) = UnixStream::connect(listener.dir.join(".socket2.sock")) else {
                return;
            };
            for line in BufReader::new(events).lines().map_while(Result::ok) {
                if line.starts_with("configreloaded>>")
                    && let Ok(list) = owned.lock()
                {
                    for (mods, key, cmd) in list.iter() {
                        let _ =
                            listener.expect_ok(&format!("keyword bind {mods},{key},exec,{cmd}"));
                    }
                }
            }
        });
        Ok(Self { hypr, binds })
    }
}

/// Single-quotes a string for `/bin/sh -c` (Hyprland `exec`) or `g_shell_parse_argv` (Cinnamon).
pub(super) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

impl Hotkeys for HyprHotkeys {
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String> {
        let Ok(mut binds) = self.binds.lock() else { return vec!["internal error".into()] };
        for (mods, key, _) in binds.drain(..) {
            let _ = self.hypr.expect_ok(&format!("keyword unbind {mods},{key}"));
        }
        let exe = match std::env::current_exe() {
            Ok(p) => sh_quote(&p.to_string_lossy()),
            Err(e) => return vec![format!("all shortcuts ({e})")],
        };
        let mut failed = Vec::new();
        for s in shortcuts {
            let Some(combo) = xkb::parse(&s.keys) else {
                failed.push(format!("{} (key not supported on Hyprland)", s.keys));
                continue;
            };
            let (mods, key) = (combo.hyprland_mods(), combo.keysym);
            let cmd = format!("{exe} __hotkey {}", s.hotkey.id());
            match self.hypr.expect_ok(&format!("keyword bind {mods},{key},exec,{cmd}")) {
                Ok(()) => binds.push((mods, key, cmd)),
                Err(e) => failed.push(format!("{} ({e})", s.keys)),
            }
        }
        failed
    }
}

impl Drop for HyprHotkeys {
    fn drop(&mut self) {
        let _ = self.set(&[]);
    }
}

#[cfg(test)]
mod tests {
    use super::sh_quote;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("/usr/bin/appkey"), "'/usr/bin/appkey'");
        assert_eq!(sh_quote("/home/o'neil/appkey"), r"'/home/o'\''neil/appkey'");
    }
}

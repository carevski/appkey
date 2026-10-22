//! Cinnamon (X11): shortcuts with Super go through Cinnamon's own custom keyboard shortcuts.
//!
//! Cinnamon's menu holds the Super key: Muffin grabs the keyboard when Super goes down
//! and replays the next key to the focused window, which skips every other client's
//! `XGrabKey`. So `super+…` can only work as a Cinnamon keybinding, which runs
//! `appkey __hotkey <id>` (like Hyprland). Everything else keeps using `XGrabKey`.
//!
//! Our entries are `custom-appkey0`, `custom-appkey1`… in `custom-list`, so they also show up in
//! System Settings → Keyboard → Shortcuts → Custom. They're replaced on every reload
//! (which also clears leftovers from a crash) and removed on exit.

use anyhow::{Context, Result};

use super::gnome::parse_gvariant_strv;
use super::hyprland::sh_quote;
use super::reserved::{custom_shortcut, gsettings, schema_shortcuts, to_ids};
use super::xkb;
use crate::config::Shortcut;
use crate::hotkeys::{GlobalHotkeys, Hotkeys, Sink};

const SCHEMA: &str = "org.cinnamon.desktop.keybindings";
const ENTRY_SCHEMA: &str = "org.cinnamon.desktop.keybindings.custom-keybinding";
/// Cinnamon only unbinds entries whose id contains "custom" when the list changes
/// (`remove_custom_keybindings` in `js/ui/keybindings.js`); anything else keeps its
/// keys and old command until Cinnamon restarts.
const PREFIX: &str = "custom-appkey";

fn entry_path(id: &str) -> String {
    format!("/org/cinnamon/desktop/keybindings/custom-keybindings/{id}/")
}

/// A string in `GVariant` text form, which `gsettings set` expects: `"a 'b'"`.
fn gstr(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A `strv` in `GVariant` text form: `["a", "b"]`.
fn strv(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|s| gstr(s)).collect();
    format!("[{}]", quoted.join(", "))
}

/// Every shortcut Cinnamon uses: built-in, and the user's custom ones (not ours).
pub(super) fn reserved() -> Vec<(u32, String)> {
    let mut found: Vec<(String, String)> = [
        SCHEMA,
        "org.cinnamon.desktop.keybindings.wm",
        "org.cinnamon.desktop.keybindings.media-keys",
    ]
    .iter()
    .flat_map(|s| schema_shortcuts(s))
    .collect();
    let customs = gsettings(&["get", SCHEMA, "custom-list"]).unwrap_or_default();
    for id in parse_gvariant_strv(&customs).iter().filter(|id| !id.starts_with(PREFIX)) {
        found.extend(custom_shortcut(&format!("{ENTRY_SCHEMA}:{}", entry_path(id))));
    }
    to_ids(found, "Cinnamon")
}

/// `XGrabKey` for most shortcuts, Cinnamon keybindings for the ones with Super.
pub(super) struct CinnamonHotkeys {
    grabs: GlobalHotkeys,
    /// Our ids currently in `custom-list`.
    entries: Vec<String>,
}

impl std::fmt::Debug for CinnamonHotkeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CinnamonHotkeys").field("entries", &self.entries).finish_non_exhaustive()
    }
}

impl CinnamonHotkeys {
    pub fn new(sink: Sink) -> Result<Self> {
        Ok(Self { grabs: GlobalHotkeys::new(sink)?, entries: Vec::new() })
    }

    /// Replaces our entries with `binds` (`(accelerator, shortcut id, keys)`).
    fn apply(&mut self, binds: &[(String, u32, String)]) -> Result<()> {
        let current = gsettings(&["get", SCHEMA, "custom-list"])?;
        let mut list: Vec<String> = parse_gvariant_strv(&current);
        for old in list.iter().filter(|id| id.starts_with(PREFIX)) {
            let _ =
                gsettings(&["reset-recursively", &format!("{ENTRY_SCHEMA}:{}", entry_path(old))]);
        }
        list.retain(|id| !id.starts_with(PREFIX));
        self.entries.clear();
        if !binds.is_empty() {
            let exe = std::env::current_exe().context("finding appkey's own path")?;
            let exe = sh_quote(&exe.to_string_lossy());
            for (i, (accel, hotkey_id, _)) in binds.iter().enumerate() {
                let id = format!("{PREFIX}{i}");
                let schema = format!("{ENTRY_SCHEMA}:{}", entry_path(&id));
                gsettings(&["set", &schema, "name", &gstr(&format!("appkey shortcut {i}"))])?;
                let command = format!("{exe} __hotkey {hotkey_id}");
                gsettings(&["set", &schema, "command", &gstr(&command)])?;
                gsettings(&["set", &schema, "binding", &strv(std::slice::from_ref(accel))])?;
                list.push(id.clone());
                self.entries.push(id);
            }
        }
        // Cinnamon (re)binds when the list changes, so write it last.
        gsettings(&["set", SCHEMA, "custom-list", &strv(&list)])?;
        Ok(())
    }
}

impl Hotkeys for CinnamonHotkeys {
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String> {
        let (with_super, plain): (Vec<&Shortcut>, Vec<&Shortcut>) =
            shortcuts.iter().partition(|s| xkb::parse(&s.keys).is_some_and(|c| c.logo));
        let plain: Vec<Shortcut> = plain.into_iter().cloned().collect();
        let mut failed = self.grabs.set(&plain);
        let binds: Vec<(String, u32, String)> = with_super
            .into_iter()
            .filter_map(|s| Some((xkb::parse(&s.keys)?.gtk(), s.hotkey.id(), s.keys.clone())))
            .collect();
        if let Err(e) = self.apply(&binds) {
            failed.extend(binds.iter().map(|(_, _, keys)| format!("{keys} ({e:#})")));
        }
        failed
    }
}

impl Drop for CinnamonHotkeys {
    fn drop(&mut self) {
        if !self.entries.is_empty() {
            let _ = self.apply(&[]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gvariant_text() {
        assert_eq!(strv(&["a".into(), "appkey0".into()]), r#"["a", "appkey0"]"#);
        assert_eq!(gstr(r#"'/o"x\appkey' __hotkey 1"#), r#""'/o\"x\\appkey' __hotkey 1""#);
    }
}

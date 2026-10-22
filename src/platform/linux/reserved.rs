//! Shortcuts the desktop keeps for itself (Cinnamon, GNOME), read from `GSettings`.
//!
//! Such a combo never reaches us, so the settings window flags it and the daemon
//! reports it as unavailable instead of registering it ([`Reserved`]).

use std::process::Command;
use std::str::FromStr as _;

use anyhow::{Context, Result, bail};
use global_hotkey::hotkey::HotKey;

use super::gnome::parse_gvariant_strv;
use super::xkb;
use crate::config::Shortcut;
use crate::hotkeys::Hotkeys;

pub(super) fn gsettings(args: &[&str]) -> Result<String> {
    let out = Command::new("gsettings").args(args).output().context("running gsettings")?;
    if !out.status.success() {
        bail!("gsettings {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `show-desktop` → `Show desktop`.
fn pretty(key: &str) -> String {
    let mut words = key.replace(['-', '_'], " ");
    if let Some(first) = words.get_mut(..1) {
        first.make_ascii_uppercase();
    }
    words
}

/// `(config keys, what for)` for every accelerator in `gsettings list-recursively`
/// output, from lines of exactly `schema`.
fn from_listing(listing: &str, schema: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in listing.lines() {
        let mut parts = line.splitn(3, ' ');
        let (Some(s), Some(key), Some(value)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        // The custom lists hold ids/paths, not keys.
        if s != schema || key.starts_with("custom-") {
            continue;
        }
        for accel in parse_gvariant_strv(value) {
            if let Some(keys) = xkb::from_gtk(&accel) {
                out.push((keys, pretty(key)));
            }
        }
    }
    out
}

/// The accelerators in `schema` (nothing if it isn't installed).
pub(super) fn schema_shortcuts(schema: &str) -> Vec<(String, String)> {
    from_listing(&gsettings(&["list-recursively", schema]).unwrap_or_default(), schema)
}

/// A user-defined shortcut's binding (`'<Super>t'` or `['<Super>t']`) and name.
pub(super) fn custom_shortcut(schema: &str) -> Vec<(String, String)> {
    let name = gsettings(&["get", schema, "name"]).unwrap_or_default();
    let name = name.trim_matches(['\'', '"']);
    let binding = gsettings(&["get", schema, "binding"]).unwrap_or_default();
    parse_gvariant_strv(&binding)
        .iter()
        .filter_map(|accel| xkb::from_gtk(accel))
        .map(|keys| (keys, format!("your shortcut \"{name}\"")))
        .collect()
}

/// `(HotKey id, "GNOME: Show desktop")`, the form the UI and the daemon use.
pub(super) fn to_ids(found: Vec<(String, String)>, desktop: &str) -> Vec<(u32, String)> {
    found
        .into_iter()
        .filter_map(|(keys, what)| {
            Some((HotKey::from_str(&keys).ok()?.id(), format!("{desktop}: {what}")))
        })
        .collect()
}

/// Reads the desktop's own shortcuts as `(HotKey id, "GNOME: Show desktop")`.
pub(super) type Lookup = fn() -> Vec<(u32, String)>;

/// Wraps a desktop's [`Hotkeys`]: combos the desktop uses itself are reported, not registered.
pub(super) struct Reserved {
    inner: Box<dyn Hotkeys>,
    lookup: Lookup,
}

impl Reserved {
    pub fn new(inner: Box<dyn Hotkeys>, lookup: Lookup) -> Self {
        Self { inner, lookup }
    }
}

impl Hotkeys for Reserved {
    fn set(&mut self, shortcuts: &[Shortcut]) -> Vec<String> {
        // Read on every reload: the user may have changed the desktop's shortcuts.
        let reserved = (self.lookup)();
        let mut failed = Vec::new();
        let mut usable = Vec::new();
        for s in shortcuts {
            match reserved.iter().find(|(id, _)| *id == s.hotkey.id()) {
                Some((_, why)) => failed.push(format!("{} (used by {why})", s.keys)),
                None => usable.push(s.clone()),
            }
        }
        failed.extend(self.inner.set(&usable));
        failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_listings() {
        let listing = "\
org.cinnamon.desktop.keybindings.wm show-desktop ['<Super>d']
org.cinnamon.desktop.keybindings.wm switch-to-workspace-right ['<Control><Alt>Right', '<Super>Page_Down']
org.cinnamon.desktop.keybindings.wm toggle-recording @as []
org.cinnamon.desktop.keybindings.wm volume-mute ['XF86AudioMute']
org.cinnamon.desktop.keybindings.wm custom-list ['custom0']
org.cinnamon.desktop.keybindings looking-glass-keybinding ['<Super>l']
";
        assert_eq!(
            from_listing(listing, "org.cinnamon.desktop.keybindings.wm"),
            [
                ("super+d".into(), "Show desktop".into()),
                ("ctrl+alt+right".into(), "Switch to workspace right".into()),
                ("super+pagedown".into(), "Switch to workspace right".into()),
            ]
        );
        // GNOME: plain strings (older settings-daemon), and other value types.
        let gnome = "\
org.gnome.settings-daemon.plugins.media-keys home '<Super>e'
org.gnome.settings-daemon.plugins.media-keys custom-keybindings ['/org/gnome/x/custom0/']
org.gnome.settings-daemon.plugins.media-keys max-screencast-length uint32 30
org.gnome.settings-daemon.plugins.media-keys screensaver ['<Super>l']
";
        assert_eq!(
            from_listing(gnome, "org.gnome.settings-daemon.plugins.media-keys"),
            [("super+e".into(), "Home".into()), ("super+l".into(), "Screensaver".into())]
        );
        let ids = to_ids(vec![("super+d".into(), "Show desktop".into())], "GNOME");
        assert_eq!(
            ids,
            [(HotKey::from_str("super+KeyD").unwrap().id(), "GNOME: Show desktop".into())]
        );
    }
}

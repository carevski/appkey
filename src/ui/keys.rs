//! Turning a key press in the settings window into a config `keys` string, and
//! back into a readable label.

use std::str::FromStr;

use eframe::egui::{Key, Modifiers};
use global_hotkey::hotkey::HotKey;

/// Result of a key press while recording a shortcut.
#[derive(Debug, PartialEq, Eq)]
pub enum Recorded {
    /// A usable combination, in config syntax (`ctrl+alt+t`).
    Combo(String),
    /// Esc on its own: stop recording, keep the old value.
    Cancel,
    /// Not usable; the message says why.
    Rejected(&'static str),
}

/// egui's key name → global-hotkey's key token, where they differ.
fn token(key: Key) -> String {
    match key {
        Key::Backtick => "backquote".into(),
        Key::OpenBracket => "bracketleft".into(),
        Key::CloseBracket => "bracketright".into(),
        Key::Equals => "equal".into(),
        other => other.name().to_lowercase(),
    }
}

fn is_function_key(key: Key) -> bool {
    let name = key.name();
    name.len() > 1 && name.starts_with('F') && name[1..].bytes().all(|b| b.is_ascii_digit())
}

pub fn record(key: Key, m: Modifiers) -> Recorded {
    let has_modifier = m.ctrl || m.alt || m.shift || m.mac_cmd;
    if key == Key::Escape && !has_modifier {
        return Recorded::Cancel;
    }
    // A bare letter would stop that letter working in every app.
    if !has_modifier && !is_function_key(key) {
        return Recorded::Rejected("Add a modifier: Ctrl, Alt, Shift or Cmd (F-keys work alone)");
    }
    let mut parts = Vec::new();
    if m.ctrl {
        parts.push("ctrl");
    }
    if m.alt {
        parts.push("alt");
    }
    if m.shift {
        parts.push("shift");
    }
    if m.mac_cmd {
        parts.push("cmd");
    }
    let key = token(key);
    let combo = parts.into_iter().chain([key.as_str()]).collect::<Vec<_>>().join("+");
    if HotKey::from_str(&combo).is_err() {
        return Recorded::Rejected("That key can't be used in a global shortcut");
    }
    Recorded::Combo(combo)
}

/// `ctrl+alt+t` → `Ctrl + Alt + T` (or `⌃ ⌥ T`-style names on macOS).
pub fn label(keys: &str) -> String {
    keys.split('+')
        .map(|t| match t.trim().to_lowercase().as_str() {
            "ctrl" | "control" => "Ctrl".into(),
            "alt" | "option" if cfg!(target_os = "macos") => "Option".into(),
            "alt" | "option" => "Alt".into(),
            "shift" => "Shift".into(),
            "cmd" | "command" | "super" if cfg!(target_os = "macos") => "Cmd".into(),
            "cmd" | "command" | "super" if cfg!(windows) => "Win".into(),
            "cmd" | "command" | "super" => "Super".into(),
            "backquote" => "`".into(),
            "bracketleft" => "[".into(),
            "bracketright" => "]".into(),
            "equal" => "=".into(),
            other if other.len() == 1 => other.to_uppercase(),
            other => {
                let mut c = other.chars();
                c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
            }
        })
        .collect::<Vec<String>>()
        .join(" + ")
}

/// The id global-hotkey would register, to spot duplicates while editing.
pub fn hotkey_id(keys: &str) -> Option<u32> {
    HotKey::from_str(keys).ok().map(|h| h.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_ALT: Modifiers = Modifiers { alt: true, ctrl: true, ..Modifiers::NONE };

    #[test]
    fn every_egui_key_either_records_or_is_rejected_cleanly() {
        for &key in Key::ALL {
            match record(key, CTRL_ALT) {
                Recorded::Combo(c) => assert!(HotKey::from_str(&c).is_ok(), "{c}"),
                Recorded::Rejected(_) => {}
                Recorded::Cancel => panic!("{key:?} with modifiers must not cancel"),
            }
        }
        // The common ones must work.
        for key in [Key::T, Key::Num1, Key::F5, Key::Space, Key::Enter, Key::ArrowUp, Key::Backtick]
        {
            assert!(matches!(record(key, CTRL_ALT), Recorded::Combo(_)), "{key:?}");
        }
    }

    #[test]
    fn recording_rules() {
        assert_eq!(record(Key::T, CTRL_ALT), Recorded::Combo("ctrl+alt+t".into()));
        assert_eq!(record(Key::Escape, Modifiers::NONE), Recorded::Cancel);
        assert!(matches!(record(Key::T, Modifiers::NONE), Recorded::Rejected(_)));
        assert_eq!(record(Key::F5, Modifiers::NONE), Recorded::Combo("f5".into()));
        let cmd_shift = Modifiers { shift: true, mac_cmd: true, command: true, ..Modifiers::NONE };
        assert_eq!(record(Key::B, cmd_shift), Recorded::Combo("shift+cmd+b".into()));
    }

    #[test]
    fn labels_and_ids() {
        assert!(label("ctrl+alt+t").starts_with("Ctrl + "));
        assert!(label("ctrl+alt+t").ends_with(" + T"));
        assert_eq!(label("ctrl+f5"), "Ctrl + F5");
        assert_eq!(hotkey_id("ctrl+alt+t"), hotkey_id("alt+control+T"));
        assert_eq!(hotkey_id("nonsense+"), None);
    }
}

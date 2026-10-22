//! Turning a key press in the settings window into a config `keys` string, and
//! back into a readable label.

use std::str::FromStr;

use eframe::egui::{Event, Key, Modifiers};
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

fn is_super(key: Key) -> bool {
    matches!(key, Key::SuperLeft | Key::SuperRight)
}

/// Modifier keys arrive as key events of their own; they only count together with another key.
fn is_modifier(key: Key) -> bool {
    is_super(key)
        || matches!(
            key,
            Key::ShiftLeft
                | Key::ShiftRight
                | Key::ControlLeft
                | Key::ControlRight
                | Key::AltLeft
                | Key::AltRight
        )
}

/// Whether Super / Win / Cmd is held after this frame's events. egui only puts it in
/// `Modifiers` on macOS, so we follow the key's own press and release events.
pub fn super_after(events: &[Event], mut down: bool) -> bool {
    for e in events {
        match e {
            Event::Key { key, pressed, .. } if is_super(*key) => down = *pressed,
            Event::WindowFocused(false) => down = false,
            _ => {}
        }
    }
    down
}

/// The first real key press in this frame's events, as a shortcut. `super_down` is the
/// Super key's state at the start of the frame (see [`super_after`]).
pub fn scan(events: &[Event], mut super_down: bool) -> Option<Recorded> {
    for e in events {
        match e {
            Event::Key { key, pressed, .. } if is_super(*key) => super_down = *pressed,
            Event::Key { key, pressed: true, repeat: false, modifiers, .. }
                if !is_modifier(*key) =>
            {
                return Some(record(*key, *modifiers, super_down));
            }
            _ => {}
        }
    }
    None
}

fn is_function_key(key: Key) -> bool {
    let name = key.name();
    name.len() > 1 && name.starts_with('F') && name[1..].bytes().all(|b| b.is_ascii_digit())
}

pub fn record(key: Key, m: Modifiers, super_down: bool) -> Recorded {
    let logo = m.mac_cmd || super_down;
    let has_modifier = m.ctrl || m.alt || m.shift || logo;
    if key == Key::Escape && !has_modifier {
        return Recorded::Cancel;
    }
    // A bare letter would stop that letter working in every app.
    if !has_modifier && !is_function_key(key) {
        return Recorded::Rejected(if cfg!(target_os = "macos") {
            "Add a modifier: Ctrl, Option, Shift or Cmd (F-keys work alone)"
        } else if cfg!(windows) {
            "Add a modifier: Ctrl, Alt, Shift or Win (F-keys work alone)"
        } else {
            "Add a modifier: Ctrl, Alt, Shift or Super (F-keys work alone)"
        });
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
    if logo {
        parts.push(if cfg!(target_os = "macos") { "cmd" } else { "super" });
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
            match record(key, CTRL_ALT, false) {
                Recorded::Combo(c) => assert!(HotKey::from_str(&c).is_ok(), "{c}"),
                Recorded::Rejected(_) => {}
                Recorded::Cancel => panic!("{key:?} with modifiers must not cancel"),
            }
        }
        // The common ones must work.
        for key in [Key::T, Key::Num1, Key::F5, Key::Space, Key::Enter, Key::ArrowUp, Key::Backtick]
        {
            assert!(matches!(record(key, CTRL_ALT, false), Recorded::Combo(_)), "{key:?}");
        }
    }

    #[test]
    fn recording_rules() {
        assert_eq!(record(Key::T, CTRL_ALT, false), Recorded::Combo("ctrl+alt+t".into()));
        assert_eq!(record(Key::Escape, Modifiers::NONE, false), Recorded::Cancel);
        assert!(matches!(record(Key::T, Modifiers::NONE, false), Recorded::Rejected(_)));
        assert_eq!(record(Key::F5, Modifiers::NONE, false), Recorded::Combo("f5".into()));
        let cmd_shift = Modifiers { shift: true, mac_cmd: true, command: true, ..Modifiers::NONE };
        let logo = if cfg!(target_os = "macos") { "cmd" } else { "super" };
        assert_eq!(record(Key::B, cmd_shift, false), Recorded::Combo(format!("shift+{logo}+b")));
        assert_eq!(record(Key::T, Modifiers::NONE, true), Recorded::Combo(format!("{logo}+t")));
        assert_eq!(record(Key::F13, Modifiers::NONE, false), Recorded::Combo("f13".into()));
    }

    fn key(key: Key, pressed: bool, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }
    }

    #[test]
    fn super_is_followed_across_events() {
        let none = Modifiers::NONE;
        let logo = if cfg!(target_os = "macos") { "cmd" } else { "super" };
        // Super, T, and both releases, all in one frame (e.g. xdotool).
        let burst = [
            key(Key::SuperLeft, true, none),
            key(Key::T, true, none),
            key(Key::T, false, none),
            key(Key::SuperLeft, false, none),
        ];
        assert_eq!(scan(&burst, false), Some(Recorded::Combo(format!("{logo}+t"))));
        assert!(!super_after(&burst, false), "released by the end of the frame");
        // Super held in an earlier frame.
        let held = [key(Key::SuperRight, true, none)];
        assert_eq!(scan(&held, false), None, "a modifier alone records nothing yet");
        assert!(super_after(&held, false));
        assert_eq!(
            scan(&[key(Key::T, true, none)], true),
            Some(Recorded::Combo(format!("{logo}+t")))
        );
        assert!(!super_after(&[Event::WindowFocused(false)], true));
        // Ctrl pressed before the letter doesn't trigger an error.
        let ctrl = Modifiers { ctrl: true, ..none };
        let frames = [key(Key::ControlLeft, true, ctrl), key(Key::AltLeft, true, CTRL_ALT)];
        assert_eq!(scan(&frames, false), None);
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

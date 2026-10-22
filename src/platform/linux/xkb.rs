//! Converts config `keys` (`ctrl+alt+t`) into the shortcut syntaxes Wayland
//! desktops use, all of which name keys by their xkb keysym.

/// Modifiers and the keysym name of the main key.
#[derive(Debug, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // one flag per modifier key
pub struct Combo {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub logo: bool,
    pub keysym: String,
}

pub fn parse(keys: &str) -> Option<Combo> {
    let mut combo =
        Combo { ctrl: false, alt: false, shift: false, logo: false, keysym: String::new() };
    let tokens: Vec<String> = keys.split('+').map(|t| t.trim().to_lowercase()).collect();
    let (key, mods) = tokens.split_last()?;
    for m in mods {
        match m.as_str() {
            "ctrl" | "control" => combo.ctrl = true,
            "alt" | "option" => combo.alt = true,
            "shift" => combo.shift = true,
            "super" | "cmd" | "command" | "meta" | "win" => combo.logo = true,
            _ => return None,
        }
    }
    combo.keysym = keysym(key)?;
    Some(combo)
}

/// Config key token → xkb keysym name.
fn keysym(key: &str) -> Option<String> {
    let key = key.strip_prefix("key").filter(|k| k.len() == 1).unwrap_or(key);
    let key = key.strip_prefix("digit").filter(|k| k.len() == 1).unwrap_or(key);
    let named = match key {
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => k,
        "space" => "space",
        "enter" | "return" => "Return",
        "tab" => "Tab",
        "escape" | "esc" => "Escape",
        "backspace" => "BackSpace",
        "delete" => "Delete",
        "insert" => "Insert",
        "home" => "Home",
        "end" => "End",
        "pageup" => "Prior",
        "pagedown" => "Next",
        "up" | "arrowup" => "Up",
        "down" | "arrowdown" => "Down",
        "left" | "arrowleft" => "Left",
        "right" | "arrowright" => "Right",
        "minus" | "-" => "minus",
        "equal" | "=" => "equal",
        "comma" | "," => "comma",
        "period" | "." => "period",
        "slash" | "/" => "slash",
        "backslash" | "\\" => "backslash",
        "semicolon" | ";" => "semicolon",
        "quote" | "'" => "apostrophe",
        "backquote" | "`" => "grave",
        "bracketleft" | "[" => "bracketleft",
        "bracketright" | "]" => "bracketright",
        k if k.starts_with('f') && k[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) => {
            return Some(k.to_uppercase());
        }
        _ => return None,
    };
    Some(named.to_owned())
}

/// GTK accelerator (as GNOME / Cinnamon store them, `<Control><Alt>Right`) → config
/// `keys` (`ctrl+alt+right`). `None` for keys a shortcut can't use (media keys, `Super_L`).
pub fn from_gtk(accel: &str) -> Option<String> {
    let mut mods = Vec::new();
    let mut rest = accel.trim();
    while let Some(after) = rest.strip_prefix('<') {
        let (m, tail) = after.split_once('>')?;
        mods.push(match m.to_lowercase().as_str() {
            "control" | "ctrl" | "primary" => "ctrl",
            "alt" | "mod1" => "alt",
            "shift" => "shift",
            "super" | "mod4" => "super",
            _ => return None,
        });
        rest = tail;
    }
    let key = match rest {
        k if k.len() == 1 && k.chars().all(|c| c.is_ascii_alphanumeric()) => k.to_lowercase(),
        k if k.len() > 1 && k.starts_with(['F', 'f']) && k[1..].parse::<u8>().is_ok() => {
            k.to_lowercase()
        }
        "Left" | "Right" | "Up" | "Down" | "Home" | "End" | "Delete" | "Insert" | "Tab"
        | "Escape" | "space" | "minus" | "equal" | "comma" | "period" | "slash" | "backslash"
        | "semicolon" | "bracketleft" | "bracketright" => rest.to_lowercase(),
        "Return" | "KP_Enter" => "enter".into(),
        "BackSpace" => "backspace".into(),
        "Page_Up" | "Prior" => "pageup".into(),
        "Page_Down" | "Next" => "pagedown".into(),
        "Print" => "printscreen".into(),
        "apostrophe" => "quote".into(),
        "grave" => "backquote".into(),
        _ => return None,
    };
    mods.dedup();
    Some(mods.into_iter().map(str::to_owned).chain([key]).collect::<Vec<_>>().join("+"))
}

/// A Qt key code (`Qt::Key | Qt::KeyboardModifier`, as KDE's kglobalaccel reports
/// shortcuts) → config `keys`. `None` for keys a shortcut can't use (media keys…).
pub fn from_qt(code: i32) -> Option<String> {
    const SHIFT: i32 = 0x0200_0000;
    const CTRL: i32 = 0x0400_0000;
    const ALT: i32 = 0x0800_0000;
    const META: i32 = 0x1000_0000;
    let key = code & 0x01ff_ffff;
    let name = match key {
        0x30..=0x39 | 0x41..=0x5a => {
            char::from(u8::try_from(key).ok()?).to_ascii_lowercase().to_string()
        }
        0x0100_0030..=0x0100_0047 => format!("f{}", key - 0x0100_0030 + 1),
        0x20 => "space".into(),
        0x27 => "quote".into(),
        0x2c => "comma".into(),
        0x2d => "minus".into(),
        0x2e => "period".into(),
        0x2f => "slash".into(),
        0x3b => "semicolon".into(),
        0x3d => "equal".into(),
        0x5b => "bracketleft".into(),
        0x5c => "backslash".into(),
        0x5d => "bracketright".into(),
        0x60 => "backquote".into(),
        0x0100_0000 => "escape".into(),
        0x0100_0001 => "tab".into(),
        0x0100_0003 => "backspace".into(),
        0x0100_0004 | 0x0100_0005 => "enter".into(),
        0x0100_0006 => "insert".into(),
        0x0100_0007 => "delete".into(),
        0x0100_0009 => "printscreen".into(),
        0x0100_0010 => "home".into(),
        0x0100_0011 => "end".into(),
        0x0100_0012 => "left".into(),
        0x0100_0013 => "up".into(),
        0x0100_0014 => "right".into(),
        0x0100_0015 => "down".into(),
        0x0100_0016 => "pageup".into(),
        0x0100_0017 => "pagedown".into(),
        _ => return None,
    };
    let mut parts: Vec<String> = [(CTRL, "ctrl"), (ALT, "alt"), (SHIFT, "shift"), (META, "super")]
        .iter()
        .filter(|(bit, _)| code & bit != 0)
        .map(|(_, m)| (*m).to_owned())
        .collect();
    parts.push(name);
    Some(parts.join("+"))
}

impl Combo {
    /// XDG shortcuts spec, used by the GlobalShortcuts portal: `CTRL+ALT+t`.
    pub fn portal(&self) -> String {
        let mut parts: Vec<&str> = Vec::new();
        for (on, name) in
            [(self.shift, "SHIFT"), (self.ctrl, "CTRL"), (self.alt, "ALT"), (self.logo, "LOGO")]
        {
            if on {
                parts.push(name);
            }
        }
        parts.push(&self.keysym);
        parts.join("+")
    }

    /// GTK accelerator, used by GNOME Shell: `<Control><Alt>t`.
    pub fn gtk(&self) -> String {
        let mut s = String::new();
        for (on, name) in [
            (self.shift, "<Shift>"),
            (self.ctrl, "<Control>"),
            (self.alt, "<Alt>"),
            (self.logo, "<Super>"),
        ] {
            if on {
                s.push_str(name);
            }
        }
        s + &self.keysym
    }

    /// Hyprland `bind` modifiers: `CTRL ALT`.
    pub fn hyprland_mods(&self) -> String {
        let mut parts = Vec::new();
        for (on, name) in
            [(self.shift, "SHIFT"), (self.ctrl, "CTRL"), (self.alt, "ALT"), (self.logo, "SUPER")]
        {
            if on {
                parts.push(name);
            }
        }
        parts.join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        let c = parse("ctrl+alt+t").unwrap();
        assert_eq!(c.portal(), "CTRL+ALT+t");
        assert_eq!(c.gtk(), "<Control><Alt>t");
        assert_eq!(c.hyprland_mods(), "CTRL ALT");
        let c = parse("super+shift+Enter").unwrap();
        assert_eq!(
            (c.portal().as_str(), c.gtk().as_str()),
            ("SHIFT+LOGO+Return", "<Shift><Super>Return")
        );
        assert_eq!(parse("ctrl+f5").unwrap().keysym, "F5");
        assert_eq!(parse("alt+KeyQ").unwrap().keysym, "q");
        assert_eq!(parse("ctrl+backquote").unwrap().keysym, "grave");
        assert_eq!(parse("cmd+1").unwrap().keysym, "1");
    }

    #[test]
    fn reads_gtk_accelerators() {
        assert_eq!(from_gtk("<Control><Alt>Right").as_deref(), Some("ctrl+alt+right"));
        assert_eq!(from_gtk("<Super>d").as_deref(), Some("super+d"));
        assert_eq!(from_gtk("<Primary><Shift>Page_Up").as_deref(), Some("ctrl+shift+pageup"));
        assert_eq!(from_gtk("<Shift><Super>Tab").as_deref(), Some("shift+super+tab"));
        assert_eq!(from_gtk("<Alt>F4").as_deref(), Some("alt+f4"));
        assert_eq!(from_gtk("Print").as_deref(), Some("printscreen"));
        for unusable in ["XF86AudioMute", "Super_L", "<Hyper>a", "<Alt>Above_Tab", "", "<Super"] {
            assert_eq!(from_gtk(unusable), None, "{unusable}");
        }
        // Round trip with what we write for GNOME/Cinnamon.
        for keys in ["ctrl+alt+t", "super+i", "shift+super+f5"] {
            assert_eq!(from_gtk(&parse(keys).unwrap().gtk()).as_deref(), Some(keys));
        }
    }

    #[test]
    fn reads_qt_key_codes() {
        assert_eq!(from_qt(0x1000_0044).as_deref(), Some("super+d")); // Meta+D
        assert_eq!(from_qt(0x0C00_0054).as_deref(), Some("ctrl+alt+t")); // Ctrl+Alt+T
        assert_eq!(from_qt(0x1300_0012).as_deref(), Some("shift+super+left"));
        assert_eq!(from_qt(0x0800_0000 | 0x0100_0033).as_deref(), Some("alt+f4"));
        assert_eq!(from_qt(0x1000_0031).as_deref(), Some("super+1"));
        assert_eq!(from_qt(0x0100_0009).as_deref(), Some("printscreen"));
        assert_eq!(from_qt(0x0100_0072), None, "Volume Mute");
        assert_eq!(from_qt(0), None);
    }

    #[test]
    fn rejects_unknown() {
        assert!(parse("ctrl+nosuchkey").is_none());
        assert!(parse("hyper+t").is_none());
        assert!(parse("").is_none());
    }

    #[test]
    fn every_recordable_key_converts() {
        // Everything the settings window can record must work on Wayland too.
        for key in [
            "a",
            "z",
            "0",
            "9",
            "f1",
            "f24",
            "space",
            "enter",
            "tab",
            "escape",
            "backspace",
            "delete",
            "insert",
            "home",
            "end",
            "pageup",
            "pagedown",
            "up",
            "down",
            "left",
            "right",
            "minus",
            "equal",
            "comma",
            "period",
            "slash",
            "backslash",
            "semicolon",
            "quote",
            "backquote",
            "bracketleft",
            "bracketright",
        ] {
            assert!(parse(&format!("ctrl+{key}")).is_some(), "{key}");
        }
    }
}

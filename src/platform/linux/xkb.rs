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

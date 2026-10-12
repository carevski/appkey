//! Config file: location, parsing, validation and the first-run example.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use global_hotkey::hotkey::HotKey;
use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    #[serde(default, rename = "shortcut")]
    shortcuts: Vec<RawShortcut>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShortcut {
    keys: String,
    app: String,
    launch: Option<Vec<String>>,
    os: Option<Vec<String>>,
}

/// A validated shortcut for the current OS.
#[derive(Debug, Clone)]
pub struct Shortcut {
    pub hotkey: HotKey,
    /// Original `keys` text, for messages.
    pub keys: String,
    /// What identifies a running instance (see README "Finding the app name").
    pub app: String,
    /// Command to start the app when no instance is running. `None` means a per-OS default.
    pub launch: Option<Vec<String>>,
}

#[derive(Debug, Default)]
pub struct Config {
    pub shortcuts: Vec<Shortcut>,
}

pub const CURRENT_OS: &str = std::env::consts::OS;
const KNOWN_OS: [&str; 3] = ["macos", "windows", "linux"];

pub fn path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine the user config directory")?;
    Ok(dir.join("appkey").join("config.toml"))
}

/// Loads the config, creating the example file on first run.
pub fn load_or_init(path: &Path) -> Result<Config> {
    if !path.exists() {
        write_example(path)?;
        eprintln!("appkey: created example config at {}", path.display());
    }
    check_permissions(path)?;
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    parse(&text).with_context(|| format!("in {}", path.display()))
}

pub fn parse(text: &str) -> Result<Config> {
    let raw: RawConfig = toml::from_str(text)?;
    let mut seen: HashMap<u32, String> = HashMap::new();
    let mut shortcuts = Vec::new();

    for (i, s) in raw.shortcuts.into_iter().enumerate() {
        let n = i + 1;
        if let Some(os) = &s.os {
            if let Some(bad) = os.iter().find(|o| !KNOWN_OS.contains(&o.as_str())) {
                bail!("shortcut #{n}: unknown os {bad:?} (expected one of {KNOWN_OS:?})");
            }
            if !os.iter().any(|o| o == CURRENT_OS) {
                continue;
            }
        }
        let hotkey = HotKey::from_str(&s.keys)
            .map_err(|e| anyhow::anyhow!("shortcut #{n}: invalid keys {:?}: {e}", s.keys))?;
        if s.app.trim().is_empty() {
            bail!("shortcut #{n} ({}): `app` must not be empty", s.keys);
        }
        if s.launch.as_ref().is_some_and(|l| l.first().is_none_or(|c| c.trim().is_empty())) {
            bail!("shortcut #{n} ({}): `launch` must start with a program", s.keys);
        }
        if let Some(prev) = seen.insert(hotkey.id(), s.keys.clone()) {
            bail!("shortcut #{n}: {:?} is the same key combination as {prev:?}", s.keys);
        }
        shortcuts.push(Shortcut { hotkey, keys: s.keys, app: s.app, launch: s.launch });
    }
    Ok(Config { shortcuts })
}

/// The config decides which programs we execute, so on Unix refuse a file that
/// other users could have modified.
#[cfg(unix)]
fn check_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode();
    if mode & 0o022 != 0 {
        bail!(
            "{} is writable by other users (mode {:o}); fix with: chmod 600 {}",
            path.display(),
            mode & 0o777,
            path.display()
        );
    }
    Ok(())
}

#[cfg(not(unix))]
#[allow(clippy::unnecessary_wraps)]
fn check_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

fn write_example(path: &Path) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    std::io::Write::write_all(&mut file, EXAMPLE.as_bytes())?;
    Ok(())
}

pub const EXAMPLE: &str = r#"# appkey config. Reload from the tray menu after editing.
#
# keys   = modifiers + key, e.g. "ctrl+alt+t", "cmd+shift+b", "super+1", "ctrl+F5"
#          (modifiers: ctrl, alt/option, shift, super/cmd/win)
# app    = what identifies a running instance; run `appkey list` to see what's open.
#          macOS: app name or bundle id   Windows: exe name   Linux: WM_CLASS
# launch = (optional) command to start it when nothing is running, as a list of
#          arguments. No shell is involved. Default: macOS `open -a <app>`, else `<app>`.
# os     = (optional) only use this shortcut on these systems: "macos", "windows", "linux"
#
# Tip: avoid combinations apps already use (ctrl+t is "new tab" in browsers).

[[shortcut]]
keys = "ctrl+alt+t"
app = "Terminal"
os = ["macos"]

[[shortcut]]
keys = "ctrl+alt+t"
app = "WindowsTerminal.exe"
launch = ["wt.exe"]
os = ["windows"]

[[shortcut]]
keys = "ctrl+alt+t"
app = "gnome-terminal"
os = ["linux"]
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_config_is_valid() {
        let cfg = parse(EXAMPLE).expect("example must parse");
        assert_eq!(cfg.shortcuts.len(), 1, "exactly one example per OS");
    }

    #[test]
    fn filters_by_os() {
        let cfg = parse(
            r#"
            [[shortcut]]
            keys = "ctrl+alt+a"
            app = "a"
            os = ["plan9-is-not-real"]
            "#,
        );
        assert!(cfg.is_err(), "unknown os names are rejected");

        let other = KNOWN_OS.iter().find(|o| **o != CURRENT_OS).unwrap();
        let cfg = parse(&format!(
            "[[shortcut]]\nkeys = \"ctrl+alt+a\"\napp = \"a\"\nos = [\"{other}\"]\n"
        ))
        .unwrap();
        assert!(cfg.shortcuts.is_empty());
    }

    #[test]
    fn rejects_bad_input() {
        let bad = [
            ("unknown field", "[[shortcut]]\nkeys = \"ctrl+a\"\napp = \"x\"\nfoo = 1"),
            ("bad keys", "[[shortcut]]\nkeys = \"ctrl+nope\"\napp = \"x\""),
            ("empty app", "[[shortcut]]\nkeys = \"ctrl+a\"\napp = \" \""),
            ("empty launch", "[[shortcut]]\nkeys = \"ctrl+a\"\napp = \"x\"\nlaunch = []"),
            (
                "duplicate keys",
                "[[shortcut]]\nkeys = \"ctrl+a\"\napp = \"x\"\n[[shortcut]]\nkeys = \"control+A\"\napp = \"y\"",
            ),
        ];
        for (name, text) in bad {
            assert!(parse(text).is_err(), "{name} should be rejected");
        }
    }

    #[test]
    fn parses_launch_and_keys() {
        let cfg = parse(
            "[[shortcut]]\nkeys = \"ctrl+alt+t\"\napp = \"term\"\nlaunch = [\"term\", \"--new\"]",
        )
        .unwrap();
        let s = &cfg.shortcuts[0];
        assert_eq!(s.hotkey, HotKey::from_str("alt+control+KeyT").unwrap());
        assert_eq!(s.launch.as_deref(), Some(&["term".to_owned(), "--new".to_owned()][..]));
    }

    #[cfg(unix)]
    #[test]
    fn refuses_world_writable_config() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("appkey-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        load_or_init(&path).expect("first run creates a valid 0600 file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(load_or_init(&path).is_err());
        fs::remove_dir_all(&dir).unwrap();
    }
}

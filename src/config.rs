//! Config file: location, parsing, validation and the first-run example.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};

/// The whole file exactly as written (unvalidated). The settings window edits
/// this and writes it back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    /// Light or dark windows; `None` follows the system.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_watch: Option<MemoryWatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_limit: Option<MemoryLimit>,
    #[serde(default, rename = "shortcut")]
    pub shortcuts: Vec<Entry>,
}

/// `theme = "light" | "dark"` for appkey's own windows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    Dark,
}

/// `[memory_watch]`: warn before the system runs out of memory (Ubuntu / Linux Mint only;
/// kept but ignored elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryWatch {
    pub enabled: bool,
    /// Warn when available memory drops below this share of RAM.
    pub min_available_percent: u8,
    /// Also warn when tasks stall on memory this share of the time (PSI "some", 10 s
    /// average). 0 turns this check off.
    pub max_pressure_percent: u8,
    /// Warn when a program left running in the background keeps a CPU core busy for this
    /// many minutes. 0 turns this check off.
    pub busy_background_minutes: u8,
}

impl Default for MemoryWatch {
    fn default() -> Self {
        Self {
            enabled: false,
            min_available_percent: 10,
            max_pressure_percent: 20,
            busy_background_minutes: 0,
        }
    }
}

impl MemoryWatch {
    pub const AVAILABLE_RANGE: std::ops::RangeInclusive<u8> = 2..=50;
    pub const PRESSURE_RANGE: std::ops::RangeInclusive<u8> = 0..=90;
    pub const BUSY_RANGE: std::ops::RangeInclusive<u8> = 1..=60;

    pub fn validate(self) -> Result<()> {
        if !Self::AVAILABLE_RANGE.contains(&self.min_available_percent) {
            bail!(
                "memory_watch: min_available_percent must be between {} and {}",
                Self::AVAILABLE_RANGE.start(),
                Self::AVAILABLE_RANGE.end()
            );
        }
        if !Self::PRESSURE_RANGE.contains(&self.max_pressure_percent) {
            bail!(
                "memory_watch: max_pressure_percent must be between {} and {} (0 = off)",
                Self::PRESSURE_RANGE.start(),
                Self::PRESSURE_RANGE.end()
            );
        }
        if self.busy_background_minutes > *Self::BUSY_RANGE.end() {
            bail!(
                "memory_watch: busy_background_minutes must be at most {} (0 = off)",
                Self::BUSY_RANGE.end()
            );
        }
        Ok(())
    }
}

/// `[memory_limit]`: apps started by appkey's shortcuts share one memory cap, so they
/// can't take the whole machine (Ubuntu / Linux Mint; kept but ignored elsewhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryLimit {
    pub enabled: bool,
    /// `"70%"` of RAM, or a size such as `"12 GB"` or `"8192 MB"`.
    pub limit: LimitSize,
}

impl Default for MemoryLimit {
    fn default() -> Self {
        Self { enabled: false, limit: LimitSize::Percent(70) }
    }
}

impl MemoryLimit {
    pub const PERCENT_RANGE: std::ops::RangeInclusive<u8> = 10..=95;
    /// Smallest fixed cap, in MiB: less wouldn't fit one browser.
    pub const MIN_MIB: u64 = 1024;

    pub fn validate(self) -> Result<()> {
        match self.limit {
            LimitSize::Percent(p) if !Self::PERCENT_RANGE.contains(&p) => bail!(
                "memory_limit: limit must be between {}% and {}% of RAM",
                Self::PERCENT_RANGE.start(),
                Self::PERCENT_RANGE.end()
            ),
            LimitSize::Mib(m) if m < Self::MIN_MIB => {
                bail!("memory_limit: limit must be at least {} MB", Self::MIN_MIB)
            }
            _ => Ok(()),
        }
    }
}

/// A share of RAM or a fixed size. Written as `"70%"`, `"12 GB"`, `"1.5 GB"`, `"8192 MB"`
/// (binary units, like the sizes appkey shows).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(try_from = "String", into = "String")]
pub enum LimitSize {
    Percent(u8),
    Mib(u64),
}

impl LimitSize {
    /// The size in bytes on a machine with `total` bytes of RAM.
    pub fn bytes(self, total: u64) -> u64 {
        match self {
            Self::Percent(p) => total / 100 * u64::from(p),
            Self::Mib(m) => m.saturating_mul(1024 * 1024),
        }
    }
}

impl TryFrom<String> for LimitSize {
    type Error = String;

    fn try_from(s: String) -> Result<Self, String> {
        let bad = || format!("memory_limit: {s:?} isn't a size; write e.g. \"70%\" or \"12 GB\"");
        let t = s.trim();
        if let Some(p) = t.strip_suffix('%') {
            return p.trim().parse().map(Self::Percent).map_err(|_| bad());
        }
        let split = t.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(t.len());
        let (num, unit) = t.split_at(split);
        let num: f64 = num.parse().map_err(|_| bad())?;
        let factor = match unit.trim().to_ascii_lowercase().as_str() {
            "g" | "gb" | "gib" => 1024.0,
            "m" | "mb" | "mib" => 1.0,
            _ => return Err(bad()),
        };
        let mib = (num * factor).round();
        if !(0.0..1e12).contains(&mib) {
            return Err(bad());
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        Ok(Self::Mib(mib as u64))
    }
}

impl From<LimitSize> for String {
    fn from(l: LimitSize) -> Self {
        match l {
            LimitSize::Percent(p) => format!("{p}%"),
            LimitSize::Mib(m) if m % 1024 == 0 => format!("{} GB", m / 1024),
            LimitSize::Mib(m) if (m * 10) % 1024 == 0 => {
                let tenths = m * 10 / 1024;
                format!("{}.{} GB", tenths / 10, tenths % 10)
            }
            LimitSize::Mib(m) => format!("{m} MB"),
        }
    }
}

/// The `app` of a shortcut that opens appkey's memory window (Ubuntu / Linux Mint).
pub const MEMORY_ACTION: &str = "appkey:memory";

/// One `[[shortcut]]` exactly as written in the file (unvalidated). The settings
/// window edits these and writes them back.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub keys: String,
    /// Display name for the settings window; not used for matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<Vec<String>>,
}

impl Entry {
    /// Whether this entry is active on the OS we're running on.
    pub fn applies_here(&self) -> bool {
        self.os.as_ref().is_none_or(|os| os.iter().any(|o| o == CURRENT_OS))
    }
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
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub memory_watch: MemoryWatch,
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub memory_limit: MemoryLimit,
}

pub const CURRENT_OS: &str = std::env::consts::OS;
const KNOWN_OS: [&str; 3] = ["macos", "windows", "linux"];

pub fn path() -> Result<PathBuf> {
    let dir = dirs::config_dir().context("could not determine the user config directory")?;
    Ok(dir.join("appkey").join("config.toml"))
}

/// Creates the example config if there is none. Returns `true` if it was created
/// (i.e. this is the first run).
pub fn ensure_exists(path: &Path) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    write_example(path)?;
    eprintln!("appkey: created example config at {}", path.display());
    Ok(true)
}

/// Loads and validates the config, creating the example file on first run.
pub fn load_or_init(path: &Path) -> Result<Config> {
    ensure_exists(path)?;
    validate_document(read_document(path)?).with_context(|| format!("in {}", path.display()))
}

/// Reads the whole file (shortcuts for all OSes) without validating it.
pub fn read_document(path: &Path) -> Result<Document> {
    check_permissions(path)?;
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    toml::from_str(&text).with_context(|| format!("in {}", path.display()))
}

/// Writes the file atomically (temp file + rename), readable only by the user.
pub fn save_document(path: &Path, doc: &Document) -> Result<()> {
    let body = toml::to_string(doc)?;
    let tmp = path.with_extension("toml.tmp");
    let _ = fs::remove_file(&tmp);
    write_new(&tmp, &format!("{SAVED_HEADER}\n{body}"))?;
    fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

const SAVED_HEADER: &str = "\
# appkey config, written by the settings window (appkey settings).
# Hand edits are fine but comments are not kept. Format: see the README.
";

#[cfg(test)]
fn parse(text: &str) -> Result<Config> {
    validate_document(toml::from_str(text)?)
}

/// Validates everything in the file that applies to this OS.
pub fn validate_document(doc: Document) -> Result<Config> {
    let memory_watch = doc.memory_watch.unwrap_or_default();
    memory_watch.validate()?;
    let memory_limit = doc.memory_limit.unwrap_or_default();
    memory_limit.validate()?;
    Ok(Config { memory_watch, memory_limit, ..validate(doc.shortcuts)? })
}

/// Checks entries for the current OS and turns them into registrable shortcuts.
pub fn validate(entries: Vec<Entry>) -> Result<Config> {
    let mut seen: HashMap<u32, String> = HashMap::new();
    let mut shortcuts = Vec::new();

    for (i, s) in entries.into_iter().enumerate() {
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
    Ok(Config { shortcuts, ..Config::default() })
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
    write_new(path, EXAMPLE)
}

/// Creates `path` (must not exist) with mode 0600 on Unix.
fn write_new(path: &Path, contents: &str) -> Result<()> {
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut file = opts.open(path).with_context(|| format!("creating {}", path.display()))?;
    std::io::Write::write_all(&mut file, contents.as_bytes())?;
    file.sync_all()?;
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
    fn memory_watch_defaults_and_limits() {
        assert_eq!(parse("").unwrap().memory_watch, MemoryWatch::default());
        let cfg = parse("[memory_watch]\nenabled = true\nmin_available_percent = 5").unwrap();
        assert_eq!(
            cfg.memory_watch,
            MemoryWatch {
                enabled: true,
                min_available_percent: 5,
                max_pressure_percent: 20,
                busy_background_minutes: 0
            }
        );
        assert!(parse("[memory_watch]\nmin_available_percent = 0").is_err());
        assert!(parse("[memory_watch]\nmax_pressure_percent = 100").is_err());
        assert!(parse("[memory_watch]\nbusy_background_minutes = 61").is_err());
        assert_eq!(
            parse("[memory_watch]\nbusy_background_minutes = 5")
                .unwrap()
                .memory_watch
                .busy_background_minutes,
            5
        );
        assert!(parse("[memory_watch]\nfoo = 1").is_err());
    }

    #[test]
    fn memory_limit_sizes() {
        assert_eq!(parse("").unwrap().memory_limit, MemoryLimit::default());
        let size = |t: &str| LimitSize::try_from(t.to_owned());
        assert_eq!(size("70%"), Ok(LimitSize::Percent(70)));
        assert_eq!(size(" 12 GB"), Ok(LimitSize::Mib(12 * 1024)));
        assert_eq!(size("1.5G"), Ok(LimitSize::Mib(1536)));
        assert_eq!(size("8192 MiB"), Ok(LimitSize::Mib(8192)));
        for bad in ["", "%", "12", "12 TB", "-1 GB", "lots"] {
            assert!(size(bad).is_err(), "{bad:?}");
        }
        for (l, text) in [
            (LimitSize::Percent(70), "70%"),
            (LimitSize::Mib(12 * 1024), "12 GB"),
            (LimitSize::Mib(1536), "1.5 GB"),
            (LimitSize::Mib(1500), "1500 MB"),
        ] {
            assert_eq!(String::from(l), text);
            assert_eq!(size(text), Ok(l));
        }
        assert_eq!(LimitSize::Percent(50).bytes(1000), 500);
        assert_eq!(LimitSize::Mib(2).bytes(1), 2 << 20);

        let cfg = parse("[memory_limit]\nenabled = true\nlimit = \"6 GB\"").unwrap();
        assert_eq!(cfg.memory_limit, MemoryLimit { enabled: true, limit: LimitSize::Mib(6144) });
        assert!(parse("[memory_limit]\nlimit = \"5%\"").is_err());
        assert!(parse("[memory_limit]\nlimit = \"512 MB\"").is_err());
        assert!(parse("[memory_limit]\nlimit = 70").is_err());
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

    #[test]
    fn save_round_trips_and_keeps_other_os_entries() {
        let dir = std::env::temp_dir().join(format!("appkey-save-{}", std::process::id()));
        let path = dir.join("config.toml");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let entries = vec![
            Entry {
                keys: "ctrl+alt+t".into(),
                name: Some("Terminal".into()),
                app: "term".into(),
                launch: Some(vec!["term".into(), "--x".into()]),
                os: None,
            },
            Entry {
                keys: "ctrl+alt+t".into(),
                app: "other".into(),
                os: Some(vec!["plan9".into()]),
                ..Entry::default()
            },
        ];
        let doc = Document {
            theme: Some(Theme::Light),
            shortcuts: entries,
            memory_watch: Some(MemoryWatch { enabled: true, ..MemoryWatch::default() }),
            memory_limit: Some(MemoryLimit { enabled: true, limit: LimitSize::Mib(1536) }),
        };
        save_document(&path, &doc).unwrap();
        assert_eq!(read_document(&path).unwrap(), doc);
        let one = Document { shortcuts: doc.shortcuts[..1].to_vec(), ..Document::default() };
        save_document(&path, &one).unwrap(); // overwrite works
        assert_eq!(read_document(&path).unwrap(), one);
        fs::remove_dir_all(&dir).unwrap();
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

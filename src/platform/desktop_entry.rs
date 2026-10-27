//! Installed-app discovery on Linux via freedesktop `.desktop` files.
//! Platform-neutral so it is unit-tested on every host.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use super::AppInfo;

/// An installed app plus every name its windows might report: the desktop file id
/// (Wayland apps usually use it as their app id), `StartupWMClass` and the program name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DesktopApp {
    pub info: AppInfo,
    pub ids: Vec<String>,
}

impl std::ops::Deref for DesktopApp {
    type Target = AppInfo;
    fn deref(&self) -> &AppInfo {
        &self.info
    }
}

/// Parses one `.desktop` file. `id` is the file name without `.desktop`
/// (e.g. `org.mozilla.firefox`). Returns `None` for entries that aren't
/// launchable, visible GUI applications.
pub fn parse(text: &str, id: &str) -> Option<DesktopApp> {
    let mut in_entry = false;
    let mut kv = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry || line.starts_with('#') {
            continue;
        }
        // Unlocalized keys only: `Name[de]=…` is skipped.
        if let Some((k, v)) = line.split_once('=')
            && !k.contains('[')
        {
            kv.insert(k.trim(), v.trim());
        }
    }
    let flag = |k| kv.get(k).is_some_and(|v| *v == "true");
    if kv.get("Type") != Some(&"Application")
        || flag("NoDisplay")
        || flag("Hidden")
        || flag("Terminal")
    {
        return None;
    }
    let name = (*kv.get("Name")?).to_owned();
    let launch = exec_argv(kv.get("Exec")?);
    let program = launch.first()?;
    let program_name = program.rsplit('/').next().unwrap_or(program);

    // What the window will report as WM_CLASS, best guess first.
    let app = match kv.get("StartupWMClass") {
        Some(class) if !class.is_empty() => (*class).to_owned(),
        // Wrappers: the program name says nothing about the window.
        _ if matches!(program_name, "env" | "flatpak" | "sh" | "bash") => id.to_owned(),
        _ => program_name.to_owned(),
    };
    // A browser web app (Chrome/Edge PWA) runs the browser itself: its program
    // name would make it match every browser window, and the browser match it.
    let web_app = launch.iter().any(|a| a.starts_with("--app-id=") || a.starts_with("--app="));
    let names: &[&str] =
        if web_app { &[app.as_str(), id] } else { &[app.as_str(), id, program_name] };
    let mut ids: Vec<String> = Vec::new();
    for &candidate in names {
        if !ids.iter().any(|i| i == candidate) {
            ids.push(candidate.to_owned());
        }
    }
    Some(DesktopApp { info: AppInfo { name, app, launch: Some(launch) }, ids })
}

/// Splits an `Exec=` value into argv: honours double quotes and backslash
/// escapes, drops field codes (`%f`, `%U`, …) and flatpak's `@@` markers.
pub fn exec_argv(exec: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut cur = String::new();
    let (mut quoted, mut any) = (false, false);
    let mut chars = exec.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted => cur.extend(chars.next()),
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    args.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        args.push(cur);
    }
    args.into_iter()
        .filter(|a| !is_field_code(a) && !a.starts_with("@@"))
        .map(|a| a.replace("%%", "%"))
        .collect()
}

/// `%f`, `%U`, … (but not the escaped `%%`).
fn is_field_code(arg: &str) -> bool {
    arg.len() == 2 && arg.starts_with('%') && arg != "%%"
}

#[cfg_attr(test, allow(dead_code))] // used by the Linux backend
/// `$XDG_DATA_HOME/applications` then each `$XDG_DATA_DIRS/applications`, in priority order.
pub fn search_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    home.into_iter()
        .chain(data_dirs.split(':').map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect()
}

/// All visible apps in `dirs`. Earlier directories win for the same desktop id.
pub fn scan(dirs: &[PathBuf]) -> Vec<DesktopApp> {
    let mut by_id: BTreeMap<String, DesktopApp> = BTreeMap::new();
    for dir in dirs {
        let mut files = Vec::new();
        collect(dir, dir, &mut files);
        for (id, path) in files {
            if by_id.contains_key(&id) {
                continue;
            }
            // An unparseable/hidden entry still shadows lower-priority ones.
            let info = std::fs::read_to_string(&path).ok().and_then(|t| parse(&t, &id));
            by_id.insert(id, info.unwrap_or_default());
        }
    }
    let mut apps: Vec<DesktopApp> = by_id.into_values().filter(|a| !a.app.is_empty()).collect();
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

/// Desktop ids per the spec: path relative to the dir, `/` → `-`, without `.desktop`.
fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(root, &path, out);
        } else if path.extension().is_some_and(|e| e == "desktop")
            && let Ok(rel) = path.strip_prefix(root)
        {
            let id = rel.with_extension("").to_string_lossy().replace('/', "-");
            out.push((id, path));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_parsing() {
        assert_eq!(exec_argv("firefox %u"), ["firefox"]);
        assert_eq!(
            exec_argv(r#""/opt/My App/app" --flag "a \"b\"" %F"#),
            ["/opt/My App/app", "--flag", r#"a "b""#]
        );
        assert_eq!(
            exec_argv("/usr/bin/flatpak run --command=firefox org.mozilla.firefox @@u %u @@"),
            ["/usr/bin/flatpak", "run", "--command=firefox", "org.mozilla.firefox"]
        );
        assert_eq!(exec_argv("app --percent=50%%"), ["app", "--percent=50%"]);
    }

    #[test]
    fn entry_parsing() {
        let firefox = "[Desktop Entry]\nType=Application\nName=Firefox\nName[de]=Feuerfuchs\n\
                       Exec=firefox %u\n\n[Desktop Action new-window]\nName=New Window\nExec=firefox --new-window\n";
        let a = parse(firefox, "firefox").unwrap();
        assert_eq!((a.name.as_str(), a.app.as_str()), ("Firefox", "firefox"));
        assert_eq!(a.launch.as_deref().unwrap(), ["firefox"]);
        let term = "[Desktop Entry]\nType=Application\nName=Terminal\nExec=gnome-terminal\n";
        assert_eq!(
            parse(term, "org.gnome.Terminal").unwrap().ids,
            ["gnome-terminal", "org.gnome.Terminal"]
        );

        let code = "[Desktop Entry]\nType=Application\nName=Code\nExec=/usr/share/code/code %F\nStartupWMClass=Code\n";
        assert_eq!(parse(code, "code").unwrap().app, "Code");

        let pwa = "[Desktop Entry]\nType=Application\nName=Outlook\n\
                   Exec=/opt/google/chrome/google-chrome --profile-directory=Default --app-id=faoln\n\
                   StartupWMClass=crx_faoln\n";
        let a = parse(pwa, "chrome-faoln-Default").unwrap();
        assert_eq!(a.app, "crx_faoln");
        assert_eq!(a.ids, ["crx_faoln", "chrome-faoln-Default"]);

        let flatpak = "[Desktop Entry]\nType=Application\nName=GIMP\nExec=/usr/bin/flatpak run org.gimp.GIMP @@ %f @@\n";
        assert_eq!(parse(flatpak, "org.gimp.GIMP").unwrap().app, "org.gimp.GIMP");

        for hidden in ["NoDisplay=true", "Hidden=true", "Terminal=true", "Type=Link"] {
            let t = format!("[Desktop Entry]\nType=Application\nName=X\nExec=x\n{hidden}\n");
            assert!(parse(&t, "x").is_none(), "{hidden}");
        }
    }

    #[test]
    fn scan_prefers_earlier_dirs() {
        let base = std::env::temp_dir().join(format!("appkey-desktop-{}", std::process::id()));
        let (user, system) = (base.join("user"), base.join("system"));
        std::fs::create_dir_all(user.join("sub")).unwrap();
        std::fs::create_dir_all(&system).unwrap();
        let entry =
            |name: &str| format!("[Desktop Entry]\nType=Application\nName={name}\nExec=prog\n");
        std::fs::write(user.join("a.desktop"), entry("User A")).unwrap();
        std::fs::write(system.join("a.desktop"), entry("System A")).unwrap();
        std::fs::write(user.join("sub/b.desktop"), entry("B")).unwrap();
        std::fs::write(system.join("hidden.desktop"), "[Desktop Entry]\nNoDisplay=true").unwrap();
        let names: Vec<String> = scan(&[user, system]).into_iter().map(|a| a.info.name).collect();
        assert_eq!(names, ["B", "User A"]);
        std::fs::remove_dir_all(base).unwrap();
    }
}

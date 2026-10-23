//! "Uninstall…" for a running app: find out how it was installed (snap, Flatpak,
//! .deb, `AppImage`…) from the process itself, then remove it the matching way.
//!
//! Safety: apt removals are dry-run first and refused when they'd take the desktop or
//! the system with them; names are validated; commands are fixed argvs with absolute
//! paths and no shell; root only through `pkexec` (the desktop's own password prompt).
//! `AppImages` go to the Trash. Parsers are pure and tested on every host; reading `/proc`
//! and running commands is in `memory::linux`.

/// How an app was installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Snap {
        name: String,
    },
    Flatpak {
        id: String,
        user: bool,
    },
    Deb {
        package: String,
    },
    AppImage {
        path: String,
    },
    /// Installed and updated by `JetBrains` Toolbox, which should remove it too.
    Toolbox,
    /// None of the above (a tarball in `/opt`, a script…): where its program lives.
    Unknown {
        path: Option<String>,
    },
}

/// What we know about a running app's main process.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `/proc/<pid>/exe`, resolved.
    pub exe: Option<String>,
    /// `APPIMAGE` from its environment (set by the `AppImage` runtime).
    pub appimage: Option<String>,
    /// `/proc/<pid>/root/.flatpak-info` (only exists inside a Flatpak sandbox).
    pub flatpak_info: Option<String>,
    /// The package owning `exe`, from `dpkg -S`.
    pub deb_package: Option<String>,
}

pub fn detect(f: &Facts) -> Source {
    if let Some(info) = &f.flatpak_info
        && let Some((id, user)) = parse_flatpak_info(info)
    {
        return Source::Flatpak { id, user };
    }
    if let Some(path) = f.appimage.as_ref().filter(|p| p.starts_with('/')) {
        return Source::AppImage { path: path.clone() };
    }
    if let Some(name) = f.exe.as_deref().and_then(snap_name) {
        return Source::Snap { name };
    }
    if f.exe.as_deref().is_some_and(|e| e.contains("/JetBrains/Toolbox/apps/")) {
        return Source::Toolbox;
    }
    if let Some(package) = f.deb_package.clone().filter(|p| valid_package(p)) {
        return Source::Deb { package };
    }
    Source::Unknown { path: f.exe.clone() }
}

/// `/snap/spotify/83/usr/share/spotify/spotify` → `spotify`.
fn snap_name(exe: &str) -> Option<String> {
    let name = exe.strip_prefix("/snap/")?.split('/').next()?;
    valid_snap(name).then(|| name.to_owned())
}

/// App id and whether it's a per-user installation, from `.flatpak-info`.
pub fn parse_flatpak_info(text: &str) -> Option<(String, bool)> {
    let mut section = "";
    let (mut id, mut user) = (None, false);
    for line in text.lines().map(str::trim) {
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s;
        } else if let Some((k, v)) = line.split_once('=') {
            match (section, k.trim()) {
                ("Application", "name") => id = Some(v.trim().to_owned()),
                // System installs live in /var/lib/flatpak, user ones in ~/.local/share/flatpak.
                ("Instance", "app-path") => user = !v.trim().starts_with("/var/lib/flatpak/"),
                _ => {}
            }
        }
    }
    id.filter(|i| valid_flatpak(i)).map(|i| (i, user))
}

/// `dpkg -S` output: `google-chrome-stable: /opt/google/chrome/chrome` (or `pkg:amd64: …`).
pub fn parse_dpkg_search(out: &str) -> Option<String> {
    let line = out.lines().find(|l| !l.starts_with("diversion "))?;
    let (owner, path) = line.split_once(": ")?;
    // Error messages share the "name: text" shape; an answer always ends in a path.
    if !path.starts_with('/') {
        return None;
    }
    // Several packages share a directory as "a, b: /dir"; a program file has one owner.
    if owner.contains(',') {
        return None;
    }
    let package = owner.split(':').next()?.trim();
    valid_package(package).then(|| package.to_owned())
}

/// Packages an `apt-get -s remove` would remove (`Remv name [version]` lines).
pub fn parse_apt_simulation(out: &str) -> Vec<String> {
    out.lines()
        .filter_map(|l| l.strip_prefix("Remv "))
        .filter_map(|l| l.split_whitespace().next())
        .map(|p| p.split(':').next().unwrap_or(p).to_owned())
        .collect()
}

/// apt removals larger than this are left to the package manager.
pub const MAX_REMOVALS: usize = 15;

/// Packages (prefixes) whose removal would break the desktop or the system.
const ESSENTIAL: &[&str] = &[
    "ubuntu-desktop",
    "ubuntu-minimal",
    "ubuntu-standard",
    "ubuntu-session",
    "mint-meta",
    "mintsystem",
    "cinnamon",
    "muffin",
    "nemo",
    "gnome-shell",
    "gnome-session",
    "gdm3",
    "mutter",
    "plasma-",
    "kde-plasma",
    "kwin",
    "sddm",
    "lightdm",
    "mate-desktop",
    "mate-session",
    "xfce4",
    "xserver-xorg",
    "xorg",
    "xwayland",
    "systemd",
    "init",
    "libc6",
    "linux-image",
    "linux-generic",
    "network-manager",
    "sudo",
    "apt",
    "dpkg",
    "pkexec",
    "polkit",
    "policykit",
    "dbus",
    "snapd",
    "flatpak",
    "appkey",
];

/// Why an apt removal is refused, if it is.
pub fn refusal(removes: &[String]) -> Option<String> {
    if removes.is_empty() {
        return Some("apt wouldn't remove anything (is it still installed?)".into());
    }
    if let Some(p) =
        removes.iter().find(|p| ESSENTIAL.iter().any(|e| p.as_str() == *e || p.starts_with(e)))
    {
        return Some(format!(
            "this would also remove {p}, which your desktop or system needs. If you're sure, \
             use your package manager."
        ));
    }
    if removes.len() > MAX_REMOVALS {
        return Some(format!(
            "this would remove {} packages. If you're sure, use your package manager.",
            removes.len()
        ));
    }
    None
}

/// Debian package names: lowercase letters, digits, `+ - .`, starting alphanumeric.
pub fn valid_package(s: &str) -> bool {
    s.len() >= 2
        && s.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "+-.".contains(c))
}

fn valid_snap(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Reverse-DNS ids like `com.spotify.Client`.
fn valid_flatpak(s: &str) -> bool {
    s.split('.').count() >= 2
        && s.split('.').all(|part| {
            !part.is_empty()
                && !part.starts_with('-')
                && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        })
}

/// The command that removes it, as an argv (no shell). `None` when there's nothing to run.
pub fn command(source: &Source) -> Option<Vec<String>> {
    let v = |args: &[&str]| Some(args.iter().map(|a| (*a).to_owned()).collect());
    match source {
        Source::Snap { name } => v(&["pkexec", "/usr/bin/snap", "remove", name]),
        Source::Flatpak { id, user } => v(&[
            "/usr/bin/flatpak",
            "uninstall",
            "--noninteractive",
            "-y",
            if *user { "--user" } else { "--system" },
            id,
        ]),
        Source::Deb { package } => v(&[
            "pkexec",
            "/usr/bin/env",
            "DEBIAN_FRONTEND=noninteractive",
            "/usr/bin/apt-get",
            "remove",
            "-y",
            package,
        ]),
        Source::AppImage { path } => v(&["/usr/bin/gio", "trash", path]),
        Source::Toolbox | Source::Unknown { .. } => None,
    }
}

/// "Snap package spotify", "Debian package google-chrome-stable"…
pub fn describe(source: &Source) -> String {
    match source {
        Source::Snap { name } => format!("Snap package {name}"),
        Source::Flatpak { id, user: true } => format!("Flatpak {id} (just for you)"),
        Source::Flatpak { id, user: false } => format!("Flatpak {id} (for all users)"),
        Source::Deb { package } => format!("Debian package {package} (apt)"),
        Source::AppImage { path } => format!("AppImage {path}"),
        Source::Toolbox => "JetBrains Toolbox app".into(),
        Source::Unknown { .. } => "unknown".into(),
    }
}

/// Whether removing needs the password prompt.
pub fn needs_password(source: &Source) -> bool {
    matches!(source, Source::Snap { .. } | Source::Deb { .. } | Source::Flatpak { user: false, .. })
}

/// Menu entries (`.desktop`) that start this `AppImage`, from their text.
pub fn launches_appimage(desktop_entry: &str, appimage: &str) -> bool {
    desktop_entry.lines().any(|l| {
        l.strip_prefix("Exec=").is_some_and(|cmd| {
            let first = cmd.trim().trim_matches('"');
            first == appimage
                || first.starts_with(&format!("{appimage} "))
                || cmd.trim().starts_with(&format!("\"{appimage}\""))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(exe: &str) -> Facts {
        Facts { exe: Some(exe.into()), ..Facts::default() }
    }

    #[test]
    fn detects_install_sources() {
        assert_eq!(
            detect(&facts("/snap/spotify/83/usr/share/spotify/spotify")),
            Source::Snap { name: "spotify".into() }
        );
        let flatpak = "[Application]\nname=com.spotify.Client\nruntime=runtime/org.freedesktop.Platform/x86_64/24.08\n\n\
                       [Instance]\ninstance-id=123\napp-path=/var/lib/flatpak/app/com.spotify.Client/x86_64/stable/abc/files\n";
        assert_eq!(
            detect(&Facts { flatpak_info: Some(flatpak.into()), ..facts("/app/bin/spotify") }),
            Source::Flatpak { id: "com.spotify.Client".into(), user: false }
        );
        let user = flatpak.replace("/var/lib/flatpak", "/home/me/.local/share/flatpak");
        assert_eq!(parse_flatpak_info(&user), Some(("com.spotify.Client".into(), true)));
        assert_eq!(
            detect(&Facts {
                appimage: Some("/home/me/Apps/Obsidian.AppImage".into()),
                ..facts("/tmp/.mount_ObsXy/obsidian")
            }),
            Source::AppImage { path: "/home/me/Apps/Obsidian.AppImage".into() }
        );
        assert_eq!(
            detect(&facts("/home/me/.local/share/JetBrains/Toolbox/apps/intellij-idea/bin/idea")),
            Source::Toolbox
        );
        assert_eq!(
            detect(&Facts {
                deb_package: Some("google-chrome-stable".into()),
                ..facts("/opt/google/chrome/chrome")
            }),
            Source::Deb { package: "google-chrome-stable".into() }
        );
        assert_eq!(
            detect(&facts("/opt/someapp/someapp")),
            Source::Unknown { path: Some("/opt/someapp/someapp".into()) }
        );
    }

    #[test]
    fn parses_tool_output() {
        assert_eq!(parse_dpkg_search("geany: /usr/bin/geany\n").as_deref(), Some("geany"));
        assert_eq!(
            parse_dpkg_search("libfoo1:amd64: /usr/lib/x/libfoo.so.1\n").as_deref(),
            Some("libfoo1")
        );
        assert_eq!(parse_dpkg_search("a, b: /usr/share/doc\n"), None);
        assert_eq!(parse_dpkg_search("dpkg-query: no path found matching pattern /opt/x\n"), None);
        let sim =
            "Reading package lists...\nRemv geany [2.0-1build2]\nRemv geany-common [2.0-1build2]\n";
        assert_eq!(parse_apt_simulation(sim), ["geany", "geany-common"]);
    }

    #[test]
    fn refuses_dangerous_removals() {
        let s = |v: &[&str]| v.iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
        assert_eq!(refusal(&s(&["geany", "geany-common"])), None);
        assert!(refusal(&s(&["firefox", "cinnamon-core", "mint-meta-cinnamon"])).is_some());
        assert!(refusal(&s(&["foo", "xserver-xorg-core"])).is_some());
        assert!(refusal(&[]).is_some());
        let many: Vec<String> = (0..=MAX_REMOVALS).map(|i| format!("pkg{i}")).collect();
        assert!(refusal(&many).is_some());
    }

    #[test]
    fn validates_names_and_builds_commands() {
        assert!(valid_package("google-chrome-stable") && valid_package("g++-13"));
        for bad in ["", "Foo", "-x", "a;rm -rf /", "a b", "../x"] {
            assert!(!valid_package(bad), "{bad}");
        }
        assert_eq!(snap_name("/snap/a;b/1/x"), None);
        assert!(parse_flatpak_info("[Application]\nname=com.x;rm\n").is_none());
        assert_eq!(
            command(&Source::Deb { package: "geany".into() }).unwrap(),
            [
                "pkexec",
                "/usr/bin/env",
                "DEBIAN_FRONTEND=noninteractive",
                "/usr/bin/apt-get",
                "remove",
                "-y",
                "geany"
            ]
        );
        assert_eq!(
            command(&Source::Flatpak { id: "org.x.Y".into(), user: true }).unwrap(),
            ["/usr/bin/flatpak", "uninstall", "--noninteractive", "-y", "--user", "org.x.Y"]
        );
        assert_eq!(command(&Source::Toolbox), None);
        assert_eq!(describe(&Source::Snap { name: "spotify".into() }), "Snap package spotify");
        assert_eq!(
            describe(&Source::Deb { package: "geany".into() }),
            "Debian package geany (apt)"
        );
        assert!(needs_password(&Source::Snap { name: "x".into() }));
        assert!(!needs_password(&Source::AppImage { path: "/x".into() }));
    }

    #[test]
    fn finds_appimage_menu_entries() {
        let entry = "[Desktop Entry]\nName=Obsidian\nExec=/home/me/Apps/Obsidian.AppImage %U\n";
        assert!(launches_appimage(entry, "/home/me/Apps/Obsidian.AppImage"));
        assert!(launches_appimage(
            "Exec=\"/home/me/My Apps/O.AppImage\" %U\n",
            "/home/me/My Apps/O.AppImage"
        ));
        assert!(!launches_appimage(entry, "/home/me/Apps/Obs"));
    }
}

#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use anyhow::{Result, bail};

    use super::{
        Facts, Source, command, describe, detect, launches_appimage, parse_apt_simulation,
    };

    /// What "Uninstall" would do for one app, shown before anything happens.
    #[derive(Debug, Clone)]
    pub struct Plan {
        pub source: Source,
        /// What goes: packages (apt), the snap / Flatpak, or files (`AppImage`).
        pub removes: Vec<String>,
        /// Why it can't (or shouldn't) be done here; then nothing runs.
        pub refusal: Option<String>,
        pub command: Option<Vec<String>>,
    }

    impl Plan {
        pub fn describe(&self) -> String {
            describe(&self.source)
        }
    }

    /// Runs a read-only tool with an English locale; stdout if it succeeded.
    fn query(program: &str, args: &[&str]) -> Option<String> {
        let out = Command::new(program)
            .args(args)
            .env("LC_ALL", "C")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    pub fn facts(pid: u32) -> Facts {
        let dir = format!("/proc/{pid}");
        // An updated program shows as "/path (deleted)" until it restarts.
        let exe = fs::read_link(format!("{dir}/exe")).ok().map(|p| {
            let p = p.to_string_lossy().into_owned();
            p.strip_suffix(" (deleted)").map(str::to_owned).unwrap_or(p)
        });
        let appimage = fs::read(format!("{dir}/environ")).ok().and_then(|env| {
            env.split(|&b| b == 0)
                .find_map(|kv| kv.strip_prefix(b"APPIMAGE="))
                .map(|v| String::from_utf8_lossy(v).into_owned())
        });
        let flatpak_info = fs::read_to_string(format!("{dir}/root/.flatpak-info")).ok();
        let deb_package = exe.as_deref().filter(|e| !e.starts_with("/snap/")).and_then(|e| {
            let mut paths = vec![e.to_owned()];
            // Merged /usr: dpkg may know the file under its old name.
            if let Some(old) = e.strip_prefix("/usr") {
                paths.push(old.to_owned());
            }
            paths
                .iter()
                .find_map(|p| query("dpkg", &["-S", p]).and_then(|o| super::parse_dpkg_search(&o)))
        });
        Facts { exe, appimage, flatpak_info, deb_package }
    }

    /// Works out what uninstalling the app whose main process is `pid` would do.
    pub fn plan(pid: u32) -> Plan {
        let source = detect(&facts(pid));
        let mut plan =
            Plan { command: command(&source), source, removes: Vec::new(), refusal: None };
        let missing = |tool: &str| !std::path::Path::new(tool).exists();
        match plan.source.clone() {
            Source::Deb { package } => {
                match query("apt-get", &["-s", "remove", &package]) {
                    Some(out) => plan.removes = parse_apt_simulation(&out),
                    None => plan.refusal = Some("apt couldn't work out what to remove.".into()),
                }
                if plan.refusal.is_none() {
                    plan.refusal = super::refusal(&plan.removes);
                }
                if missing("/usr/bin/pkexec") {
                    plan.refusal = Some(
                        "pkexec is missing (install it with: sudo apt install pkexec).".into(),
                    );
                }
            }
            Source::Snap { name } => {
                plan.removes = vec![name];
                if missing("/usr/bin/snap") || missing("/usr/bin/pkexec") {
                    plan.refusal = Some("the snap or pkexec command is missing.".into());
                }
            }
            Source::Flatpak { id, .. } => {
                plan.removes = vec![id];
                if missing("/usr/bin/flatpak") {
                    plan.refusal = Some("the flatpak command is missing.".into());
                }
            }
            Source::AppImage { path } => {
                let ours = fs::metadata(&path).is_ok_and(|m| {
                    std::os::unix::fs::MetadataExt::uid(&m) == super::super::current_uid()
                        && m.is_file()
                });
                if !ours {
                    plan.refusal =
                        Some(format!("{path} isn't a file of yours, so appkey leaves it alone."));
                } else if missing("/usr/bin/gio") {
                    plan.refusal =
                        Some("the gio command (to move it to the Trash) is missing.".into());
                }
                let mut files = vec![path.clone()];
                files.extend(menu_entries_for(&path));
                if let Some(cmd) = plan.command.as_mut() {
                    cmd.truncate(2);
                    cmd.extend(files.iter().cloned());
                }
                plan.removes = files;
            }
            Source::Toolbox => {
                plan.refusal = Some(
                    "it was installed by JetBrains Toolbox. Remove it there: open Toolbox, click ⋮ \
                     next to the IDE, then Uninstall."
                        .into(),
                );
            }
            Source::Unknown { path } => {
                plan.refusal = Some(match path {
                    Some(p) => format!(
                        "appkey can't tell how it was installed (not a snap, Flatpak, .deb or \
                         AppImage). Its program is {p}."
                    ),
                    None => "appkey can't tell how it was installed.".into(),
                });
            }
        }
        plan
    }

    /// `~/.local/share/applications/*.desktop` entries that launch this `AppImage`.
    fn menu_entries_for(appimage: &str) -> Vec<String> {
        let Some(dir) = dirs::data_dir().map(|d| d.join("applications")) else { return Vec::new() };
        let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
        entries
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "desktop"))
            .filter(|p| fs::read_to_string(p).is_ok_and(|t| launches_appimage(&t, appimage)))
            .map(|p| p.to_string_lossy().into_owned())
            .collect()
    }

    /// Closes the app (`targets`: its processes as `(pid, start_ticks)`), politely then
    /// forcefully after 5 s, and runs the plan's command. Blocks; run it on a thread.
    pub fn run(plan: &Plan, targets: &[(u32, u64)]) -> Result<String> {
        let Some(cmd) = plan.command.as_ref().filter(|_| plan.refusal.is_none()) else {
            bail!("nothing to run");
        };
        crate::memory::signal(targets, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        let alive = || {
            targets
                .iter()
                .filter(|&&(pid, start)| {
                    fs::read_to_string(format!("/proc/{pid}/stat"))
                        .ok()
                        .and_then(|s| crate::memory::procs::parse_stat(&s))
                        .is_some_and(|s| s.start_ticks == start)
                })
                .copied()
                .collect::<Vec<_>>()
        };
        while !alive().is_empty() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(200));
        }
        let rest = alive();
        if !rest.is_empty() {
            crate::memory::signal(&rest, true);
            std::thread::sleep(Duration::from_millis(500));
        }

        let out = Command::new(&cmd[0]).args(&cmd[1..]).stdin(Stdio::null()).output()?;
        match out.status.code() {
            Some(0) => Ok(format!("Uninstalled ({}).", plan.describe())),
            // pkexec: 126 = the password prompt was dismissed, 127 = not authorized / no agent.
            Some(126) if cmd[0] == "pkexec" => {
                bail!("cancelled at the password prompt; nothing was removed")
            }
            Some(127) if cmd[0] == "pkexec" => {
                bail!(
                    "not authorized, or no password prompt could be shown (is your account an \
                     administrator?); nothing was removed"
                )
            }
            _ => {
                let err = String::from_utf8_lossy(&out.stderr);
                let last: Vec<&str> =
                    err.lines().filter(|l| !l.trim().is_empty()).rev().take(2).collect();
                bail!("{} failed: {}", cmd[0], last.into_iter().rev().collect::<Vec<_>>().join(" "))
            }
        }
    }
}

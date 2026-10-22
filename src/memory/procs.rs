//! What each process is, and which app it belongs to, for the memory window.
//!
//! Processes are grouped by app through the process tree: Chrome with its tab
//! processes, `IntelliJ` with its helpers, each terminal tab with what runs in it.
//! A group is "left running" when it was detached from whatever started it (its
//! parent is now `init` or the user's `systemd`), isn't a service, and has no window.
//! Everything here is pure and unit-tested on every host; `/proc` is read in
//! `memory::linux`.

use std::collections::HashMap;
use std::fmt::Write as _;

/// One process, as read from `/proc/<pid>/{stat,status,cmdline,cwd,cgroup}`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Proc {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    /// `comm` (at most 15 bytes).
    pub name: String,
    pub cmdline: Vec<String>,
    pub rss_kib: u64,
    /// `utime + stime`, in clock ticks.
    pub cpu_ticks: u64,
    /// Start time since boot, in clock ticks. With the pid, identifies the process.
    pub start_ticks: u64,
    /// Has a controlling terminal.
    pub tty: bool,
    pub cwd: Option<String>,
    /// Runs in a systemd `.service` (started on purpose, managed by systemd).
    pub service: bool,
}

/// The fields of `/proc/<pid>/stat` we need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stat {
    pub ppid: u32,
    pub tty: bool,
    pub cpu_ticks: u64,
    pub start_ticks: u64,
}

/// `comm` may contain spaces and parentheses, so fields are counted after the last `)`.
pub fn parse_stat(text: &str) -> Option<Stat> {
    let rest = &text[text.rfind(')')? + 1..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // f[0] is field 3 (state); field n is f[n - 3].
    let num = |n: usize| f.get(n - 3)?.parse::<u64>().ok();
    Some(Stat {
        ppid: u32::try_from(num(4)?).ok()?,
        tty: num(7)? != 0,
        cpu_ticks: num(14)? + num(15)?,
        start_ticks: num(22)?,
    })
}

/// `/proc/<pid>/cmdline`: NUL-separated arguments. Chrome, Electron and Node rewrite
/// theirs into one space-separated string (setproctitle); that is split on spaces.
pub fn parse_cmdline(raw: &[u8]) -> Vec<String> {
    let args: Vec<String> = raw
        .split(|&b| b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    match args.as_slice() {
        [one] if one.contains(" --") => one.split_whitespace().map(str::to_owned).collect(),
        _ => args,
    }
}

/// `/proc/<pid>/cgroup` (cgroup v2: `0::/user.slice/…/app.slice/foo.service`).
pub fn is_service(cgroup: &str) -> bool {
    cgroup.lines().any(|l| l.trim_end().ends_with(".service"))
}

fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

const SHELLS: &[&str] = &["bash", "sh", "dash", "zsh", "fish", "ksh", "tcsh", "nu"];

/// A shell, also when running a script (`comm` is then the script's name).
fn is_shell(p: &Proc) -> bool {
    SHELLS.contains(&p.name.as_str())
        || p.cmdline.first().is_some_and(|a| SHELLS.contains(&basename(a)))
}

/// Programs that only start other apps; each app they start is its own group.
const LAUNCHERS: &[&str] = &["jetbrains-toolb", "gio-launch-desk", "xdg-open", "flatpak-portal"];

/// Small background helpers that are meant to run detached and aren't worth flagging.
const AGENTS: &[&str] = &["ssh-agent", "gpg-agent", "gnome-keyring-d", "dbus-launch", "keyboxd"];

/// Browsers built on Chromium: their `--type=renderer` processes are tabs.
const CHROMIUM_BROWSERS: &[(&str, &str)] = &[
    ("chrome", "Chrome"),
    ("google-chrome", "Chrome"),
    ("chromium", "Chromium"),
    ("chromium-browser", "Chromium"),
    ("brave", "Brave"),
    ("msedge", "Edge"),
    ("vivaldi-bin", "Vivaldi"),
    ("opera", "Opera"),
];

/// Native launchers of `JetBrains` IDEs (`bin/idea`, `bin/pycharm`…).
const JETBRAINS: &[(&str, &str)] = &[
    ("idea", "IntelliJ IDEA"),
    ("pycharm", "PyCharm"),
    ("webstorm", "WebStorm"),
    ("goland", "GoLand"),
    ("clion", "CLion"),
    ("rider", "Rider"),
    ("phpstorm", "PhpStorm"),
    ("rubymine", "RubyMine"),
    ("datagrip", "DataGrip"),
    ("rustrover", "RustRover"),
    ("studio", "Android Studio"),
];

/// `-Didea.platform.prefix=…` → product, for IDEs started through `java`.
const JETBRAINS_PREFIXES: &[(&str, &str)] = &[
    ("Idea", "IntelliJ IDEA"),
    ("PyCharm", "PyCharm"),
    ("PyCharmCore", "PyCharm"),
    ("WebStorm", "WebStorm"),
    ("GoLand", "GoLand"),
    ("CLion", "CLion"),
    ("Rider", "Rider"),
    ("PhpStorm", "PhpStorm"),
    ("Ruby", "RubyMine"),
    ("DataGrip", "DataGrip"),
    ("RustRover", "RustRover"),
    ("AndroidStudio", "Android Studio"),
];

/// The full program name: `comm` is cut at 15 bytes, `argv[0]` isn't.
fn program(p: &Proc) -> String {
    match p.cmdline.first().map(|a| basename(a)) {
        Some(exe) if !exe.is_empty() && exe.starts_with(p.name.as_str()) => exe.to_owned(),
        _ => p.name.clone(),
    }
}

fn arg_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.iter().find_map(|a| a.strip_prefix(key))
}

/// The class or jar a `java` command runs.
fn java_main(args: &[String]) -> Option<String> {
    const WITH_VALUE: &[&str] = &[
        "-cp",
        "-classpath",
        "--class-path",
        "-p",
        "--module-path",
        "--add-opens",
        "--add-exports",
        "--add-modules",
        "--add-reads",
    ];
    let mut i = 1;
    while let Some(a) = args.get(i) {
        if a == "-jar" || a == "-m" || a == "--module" {
            return args.get(i + 1).map(|j| basename(j).to_owned());
        }
        if WITH_VALUE.contains(&a.as_str()) {
            i += 2;
        } else if a.starts_with('-') {
            i += 1;
        } else {
            return Some(a.clone());
        }
    }
    None
}

fn describe_java(args: &[String]) -> String {
    let main = java_main(args).unwrap_or_default();
    if main.starts_with("com.intellij.") {
        let prefix = arg_value(args, "-Didea.platform.prefix=").unwrap_or("Idea");
        return JETBRAINS_PREFIXES
            .iter()
            .find(|(p, _)| *p == prefix)
            .map_or("JetBrains IDE", |(_, name)| name)
            .to_owned();
    }
    match main.as_str() {
        "org.gradle.launcher.daemon.bootstrap.GradleDaemon" => "Gradle daemon".into(),
        "org.gradle.wrapper.GradleWrapperMain" | "org.gradle.launcher.GradleMain" => {
            "Gradle".into()
        }
        "org.jetbrains.kotlin.daemon.KotlinCompileDaemon" => "Kotlin daemon".into(),
        "org.codehaus.plexus.classworlds.launcher.Launcher" => "Maven".into(),
        "" => "Java".into(),
        m if std::path::Path::new(m).extension().is_some_and(|e| e.eq_ignore_ascii_case("jar")) => {
            format!("Java: {m}")
        }
        m => format!("Java: {}", m.rsplit('.').next().unwrap_or(m)),
    }
}

/// `node …/node_modules/vite/bin/vite.js` → `vite`; Claude Code; `npm run dev`.
fn describe_node(args: &[String]) -> String {
    let Some(script) = args.iter().skip(1).find(|a| !a.starts_with('-')) else {
        return "Node.js".into();
    };
    if basename(script) == "claude" {
        return "Claude Code".into();
    }
    let pkg = script.rsplit_once("node_modules/").map(|(_, rest)| {
        let mut parts = rest.split('/');
        match (parts.next(), parts.next()) {
            (Some(scope), Some(name)) if scope.starts_with('@') => format!("{scope}/{name}"),
            (Some(name), _) => name.to_owned(),
            _ => rest.to_owned(),
        }
    });
    match pkg.as_deref() {
        Some("@anthropic-ai/claude-code") => "Claude Code".into(),
        Some(".bin") => basename(script).to_owned(),
        Some("npm") => {
            let rest: Vec<&str> = args
                .iter()
                .skip_while(|a| *a != script)
                .skip(1)
                .take(2)
                .map(String::as_str)
                .collect();
            format!("npm {}", rest.join(" ")).trim_end().to_owned()
        }
        Some(p) => p.to_owned(),
        None => {
            let b = basename(script);
            b.strip_suffix(".js").or_else(|| b.strip_suffix(".mjs")).unwrap_or(b).to_owned()
        }
    }
}

fn describe_python(args: &[String]) -> String {
    let mut it = args.iter().skip(1).skip_while(|a| a.starts_with('-') && *a != "-m");
    match it.next().map(String::as_str) {
        Some("-m") => format!("Python: {}", it.next().map_or("", String::as_str)),
        Some(script) => format!("Python: {}", basename(script)),
        None => "Python".into(),
    }
}

/// What a single process is, in a few words: "Tab", "Gradle daemon", "Claude Code"…
pub fn describe(p: &Proc) -> String {
    let prog = program(p);
    let args = &p.cmdline;
    if let Some(kind) = arg_value(args, "--type=") {
        let browser = CHROMIUM_BROWSERS.iter().any(|(b, _)| *b == prog || *b == p.name);
        return match kind {
            "renderer" if args.iter().any(|a| a == "--extension-process") => "Extension".into(),
            "renderer" if browser => "Tab".into(),
            "renderer" => "Renderer".into(),
            "gpu-process" => "GPU".into(),
            "zygote" => "Zygote".into(),
            "crashpad-handler" => "Crash reporter".into(),
            "utility" => {
                let sub = arg_value(args, "--utility-sub-type=").unwrap_or("utility");
                let sub = sub.split('.').next().unwrap_or(sub);
                format!("Utility: {sub}")
            }
            other => other.to_owned(),
        };
    }
    if args.iter().any(|a| a == "-contentproc") {
        // Firefox: the process type is the last argument.
        return match args.last().map(String::as_str) {
            Some("tab") => "Tab".into(),
            Some(t) => {
                let mut t = t.to_owned();
                if let Some(first) = t.get_mut(..1) {
                    first.make_ascii_uppercase();
                }
                t
            }
            None => "Content".into(),
        };
    }
    if let Some((_, name)) = CHROMIUM_BROWSERS.iter().find(|(b, _)| *b == prog) {
        return (*name).into();
    }
    if let Some((_, name)) = JETBRAINS.iter().find(|(b, _)| *b == prog) {
        return (*name).into();
    }
    match prog.as_str() {
        "java" => describe_java(args),
        "node" | "nodejs" | "bun" | "deno" => describe_node(args),
        p if p.starts_with("python") => describe_python(args),
        "claude" => "Claude Code".into(),
        "firefox" | "firefox-bin" => "Firefox".into(),
        "code" => "VS Code".into(),
        s if SHELLS.contains(&s) => match args.iter().position(|a| a == "-c") {
            Some(i) => {
                let cmd = args.get(i + 1).map_or("", String::as_str);
                let first = cmd.split_whitespace().next().unwrap_or("");
                format!("{s}: {}", basename(first))
            }
            None => s.to_owned(),
        },
        _ => prog,
    }
}

/// An app: a process and everything it started (that isn't its own app).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppGroup {
    /// The topmost process of the app; its pid identifies the group.
    pub root: u32,
    /// "Chrome", "`IntelliJ` IDEA", "Terminal: Claude Code"…
    pub label: String,
    /// Window titles, or the working folder of a windowless app.
    pub detail: String,
    /// All processes, biggest first.
    pub members: Vec<u32>,
    pub rss_kib: u64,
    pub has_window: bool,
    /// Left running in the background: see the module docs.
    pub detached: bool,
}

/// How a process relates to its parent.
enum Link {
    /// Part of the parent's app.
    Child,
    /// Starts its own app.
    Root { detached: bool },
}

fn link(
    p: &Proc,
    parent: Option<&Proc>,
    uid: u32,
    protected: fn(&str) -> bool,
    has_window: impl Fn(u32) -> bool,
) -> Link {
    let Some(q) = parent else { return Link::Root { detached: p.ppid <= 1 } };
    if q.pid <= 1 || q.name == "systemd" {
        return Link::Root { detached: true };
    }
    // An app with its own windows is its own app, even when a terminal or script started it.
    if has_window(p.pid) && !has_window(q.pid) {
        return Link::Root { detached: false };
    }
    let terminal_tab = p.tty && !q.tty;
    if q.uid != uid || protected(&q.name) || LAUNCHERS.contains(&q.name.as_str()) || terminal_tab {
        return Link::Root { detached: false };
    }
    Link::Child
}

/// `/home/me/work/shop` → `~/work/shop`.
pub fn short_path(path: &str, home: &str) -> String {
    match path.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => path.to_owned(),
    }
}

/// "a", "a, b, c +2 more", each title cut to a readable length.
fn join_titles(titles: &[String]) -> String {
    let short: Vec<String> = titles
        .iter()
        .take(3)
        .map(|t| {
            if t.chars().count() > 40 {
                format!("{}…", t.chars().take(39).collect::<String>())
            } else {
                t.clone()
            }
        })
        .collect();
    let mut s = short.join(", ");
    if titles.len() > 3 {
        let _ = write!(s, " +{} more", titles.len() - 3);
    }
    s
}

/// Groups the user's processes (owned by `uid`, not protected, not `own_pid`) into apps,
/// biggest first. `procs` should hold every process, so parents owned by others are known.
/// `windows` maps a pid to the titles of its windows.
pub fn build_groups(
    procs: &[Proc],
    uid: u32,
    own_pid: u32,
    protected: fn(&str) -> bool,
    windows: &HashMap<u32, Vec<String>>,
    home: &str,
) -> Vec<AppGroup> {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    // pid 1 is init (or a container's first process): never an app.
    let mine = |p: &Proc| p.uid == uid && p.pid > 1 && p.pid != own_pid && !protected(&p.name);
    let has_window = |pid: u32| windows.contains_key(&pid);

    // Each process's group root, and whether that root was detached.
    let mut roots: HashMap<u32, (u32, bool)> = HashMap::new();
    for p in procs.iter().filter(|p| mine(p)) {
        let mut cur: &Proc = p;
        let mut chain = Vec::new();
        let found = loop {
            if let Some(&r) = roots.get(&cur.pid) {
                break r;
            }
            chain.push(cur.pid);
            let parent = by_pid.get(&cur.ppid).copied();
            match link(cur, parent, uid, protected, has_window) {
                Link::Root { detached } => break (cur.pid, detached),
                Link::Child => match parent {
                    Some(q) if mine(q) && chain.len() < 512 => cur = q,
                    _ => break (cur.pid, false),
                },
            }
        };
        for pid in chain {
            roots.insert(pid, found);
        }
    }

    let mut members: HashMap<u32, Vec<&Proc>> = HashMap::new();
    for p in procs.iter().filter(|p| mine(p)) {
        if let Some(&(root, _)) = roots.get(&p.pid) {
            members.entry(root).or_default().push(p);
        }
    }

    let mut groups: Vec<AppGroup> = members
        .into_iter()
        .filter_map(|(root, mut list)| {
            list.sort_by(|a, b| b.rss_kib.cmp(&a.rss_kib).then(a.pid.cmp(&b.pid)));
            let root_proc = *by_pid.get(&root)?;
            let titles: Vec<String> =
                list.iter().filter_map(|p| windows.get(&p.pid)).flatten().cloned().collect();
            // A shell (terminal tab, launcher script) is named after what runs in it.
            let main = if is_shell(root_proc) {
                list.iter().copied().find(|p| !is_shell(p)).unwrap_or(root_proc)
            } else {
                root_proc
            };
            let mut label = describe(main);
            if root_proc.tty && is_shell(root_proc) {
                label =
                    if is_shell(main) { "Terminal".into() } else { format!("Terminal: {label}") };
            }
            let detail = if titles.is_empty() {
                main.cwd
                    .as_deref()
                    .or(root_proc.cwd.as_deref())
                    .map(|c| short_path(c, home))
                    .unwrap_or_default()
            } else {
                join_titles(&titles)
            };
            let detached = roots.get(&root).is_some_and(|r| r.1)
                && titles.is_empty()
                && !root_proc.service
                && !AGENTS.contains(&root_proc.name.as_str());
            Some(AppGroup {
                root,
                label,
                detail,
                rss_kib: list.iter().map(|p| p.rss_kib).sum(),
                members: list.iter().map(|p| p.pid).collect(),
                has_window: !titles.is_empty(),
                detached,
            })
        })
        .collect();
    groups.sort_by(|a, b| b.rss_kib.cmp(&a.rss_kib).then(a.root.cmp(&b.root)));
    groups
}

/// CPU use between two samples, in percent of one core (can exceed 100).
#[allow(clippy::cast_precision_loss)]
pub fn cpu_percent(before_ticks: u64, after_ticks: u64, seconds: f64, hz: u64) -> f32 {
    if seconds <= 0.0 || hz == 0 {
        return 0.0;
    }
    let used = after_ticks.saturating_sub(before_ticks) as f64 / hz as f64;
    #[allow(clippy::cast_possible_truncation)]
    let pct = (used / seconds * 100.0) as f32;
    pct
}

/// "45 s", "12 min", "3 h 5 min", "2 d 4 h".
pub fn format_age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs / 60),
        3600..86_400 => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
        _ => format!("{} d {} h", secs / 86_400, secs % 86_400 / 3600),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    fn proc(pid: u32, parent: u32, name: &str, cmd: &str, rss: u64) -> Proc {
        Proc {
            pid,
            ppid: parent,
            uid: 1000,
            name: name.into(),
            cmdline: args(cmd),
            rss_kib: rss,
            ..Proc::default()
        }
    }

    fn named(name: &str, cmd: &str) -> String {
        describe(&proc(10, 1, name, cmd, 1))
    }

    #[test]
    fn parses_proc_files() {
        let stat = "4242 (Web Content (x)) S 4200 4242 4200 34817 4242 4194560 100 0 0 0 \
                    120 30 0 0 20 0 25 0 987654 1000000 500 18446744073709551615";
        assert_eq!(
            parse_stat(stat),
            Some(Stat { ppid: 4200, tty: true, cpu_ticks: 150, start_ticks: 987_654 })
        );
        assert_eq!(parse_cmdline(b"node\0/x/cli.js\0--flag\0"), args("node /x/cli.js --flag"));
        assert!(is_service(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/gpg-agent.service\n"
        ));
        assert!(!is_service(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/vte-spawn-1.scope\n"
        ));
    }

    #[test]
    fn describes_processes() {
        assert_eq!(named("chrome", "/opt/google/chrome/chrome --type=renderer --lang=en"), "Tab");
        assert_eq!(
            named("chrome", "/opt/google/chrome/chrome --type=renderer --extension-process"),
            "Extension"
        );
        assert_eq!(named("chrome", "/opt/google/chrome/chrome --type=gpu-process"), "GPU");
        assert_eq!(
            named(
                "chrome",
                "/opt/google/chrome/chrome --type=utility --utility-sub-type=network.mojom.NetworkService"
            ),
            "Utility: network"
        );
        assert_eq!(
            named("chrome", "/opt/google/chrome/chrome --profile-directory=Default"),
            "Chrome"
        );
        assert_eq!(named("code", "/usr/share/code/code --type=renderer"), "Renderer");
        assert_eq!(
            named("Isolated Web Co", "/usr/lib/firefox/firefox -contentproc -childID 3 tab"),
            "Tab"
        );
        assert_eq!(
            named(
                "java",
                "/opt/idea/jbr/bin/java -Xmx2g -cp /opt/idea/lib/a.jar -Didea.platform.prefix=PyCharm com.intellij.idea.Main"
            ),
            "PyCharm"
        );
        assert_eq!(
            named("java", "/opt/idea/jbr/bin/java -cp x com.intellij.idea.Main"),
            "IntelliJ IDEA"
        );
        assert_eq!(named("idea", "/opt/idea/bin/idea"), "IntelliJ IDEA");
        assert_eq!(
            named(
                "java",
                "/usr/lib/jvm/bin/java -Xmx512m -cp g.jar org.gradle.launcher.daemon.bootstrap.GradleDaemon 8.5"
            ),
            "Gradle daemon"
        );
        assert_eq!(
            named("java", "java -cp k.jar org.jetbrains.kotlin.daemon.KotlinCompileDaemon --x"),
            "Kotlin daemon"
        );
        assert_eq!(named("java", "java -jar /srv/app/server.jar"), "Java: server.jar");
        assert_eq!(
            named("node", "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js"),
            "Claude Code"
        );
        assert_eq!(named("claude", "claude --resume"), "Claude Code");
        assert_eq!(
            named("node", "node /x/claude-code/../../shop/node_modules/vite/bin/vite.js"),
            "vite",
            "a path through Claude's folder isn't Claude"
        );
        assert_eq!(
            named("node", "node /home/me/shop/node_modules/vite/bin/vite.js --port 3000"),
            "vite"
        );
        assert_eq!(named("node", "node /home/me/shop/node_modules/.bin/next dev"), "next");
        assert_eq!(
            named("node", "node /usr/lib/node_modules/npm/bin/npm-cli.js run dev"),
            "npm run dev"
        );
        assert_eq!(
            named(
                "node",
                "node /home/me/.npm/_npx/1/node_modules/@modelcontextprotocol/server-github/dist/index.js"
            ),
            "@modelcontextprotocol/server-github"
        );
        assert_eq!(named("node", "node server.js"), "server");
        assert_eq!(named("python3", "python3 -u -m http.server 8000"), "Python: http.server");
        assert_eq!(
            named("python3", "/usr/bin/python3 /home/me/train.py --epochs 3"),
            "Python: train.py"
        );
        assert_eq!(named("bash", "bash -c sleep 600"), "bash: sleep");
        assert_eq!(named("gnome-text-edit", "gnome-text-editor"), "gnome-text-editor");
    }

    #[test]
    fn groups_by_app() {
        let mut procs = vec![
            Proc { uid: 0, ..proc(1, 0, "systemd", "/sbin/init", 10) },
            proc(100, 1, "systemd", "/lib/systemd/systemd --user", 10),
            proc(200, 100, "cinnamon", "cinnamon --replace", 300_000),
            // Chrome, started from the menu, with tabs.
            proc(300, 200, "chrome", "/opt/google/chrome/chrome", 400_000),
            proc(301, 300, "chrome", "/opt/google/chrome/chrome --type=renderer", 900_000),
            proc(302, 300, "chrome", "/opt/google/chrome/chrome --type=renderer", 100_000),
            // A terminal with a tab running Claude Code and its background shell.
            proc(400, 200, "gnome-terminal-", "/usr/libexec/gnome-terminal-server", 50_000),
            Proc { tty: true, ..proc(401, 400, "bash", "bash", 5_000) },
            Proc {
                tty: true,
                ..proc(
                    402,
                    401,
                    "node",
                    "node /usr/lib/node_modules/@anthropic-ai/claude-code/cli.js",
                    300_000,
                )
            },
            proc(403, 402, "bash", "bash -c npm run dev", 3_000),
            // Left behind by an earlier Claude session: reparented to systemd --user.
            Proc {
                cwd: Some("/home/me/shop".into()),
                ..proc(
                    500,
                    100,
                    "node",
                    "node /home/me/shop/node_modules/vite/bin/vite.js",
                    250_000,
                )
            },
            // A deliberate user service: not flagged.
            Proc { service: true, ..proc(600, 100, "gpg-agent", "gpg-agent --supervised", 30_000) },
            // Someone else's process.
            Proc { uid: 1001, ..proc(700, 1, "postgres", "postgres", 900_000) },
        ];
        procs.push(proc(999, 200, "appkey", "appkey memory", 40_000));
        let windows =
            HashMap::from([(300, vec!["GitHub - appkey".to_owned(), "Gmail".to_owned()])]);
        let protected = |n: &str| ["systemd", "cinnamon", "appkey"].contains(&n);
        let groups = build_groups(&procs, 1000, 999, protected, &windows, "/home/me");
        let summary: Vec<(u32, &str, &str, Vec<u32>, bool)> = groups
            .iter()
            .map(|g| (g.root, g.label.as_str(), g.detail.as_str(), g.members.clone(), g.detached))
            .collect();
        assert_eq!(
            summary,
            vec![
                (300, "Chrome", "GitHub - appkey, Gmail", vec![301, 300, 302], false),
                (401, "Terminal: Claude Code", "", vec![402, 401, 403], false),
                (500, "vite", "~/shop", vec![500], true),
                (400, "gnome-terminal-server", "", vec![400], false),
                (600, "gpg-agent", "", vec![600], false),
            ]
        );
        assert_eq!(groups[0].rss_kib, 1_400_000);
    }

    #[test]
    fn formats_cpu_and_age() {
        assert!((cpu_percent(100, 300, 2.0, 100) - 100.0).abs() < 0.01);
        assert!(cpu_percent(300, 100, 2.0, 100).abs() < 0.01);
        assert_eq!(format_age(42), "42 s");
        assert_eq!(format_age(12 * 60 + 5), "12 min");
        assert_eq!(format_age(3 * 3600 + 5 * 60), "3 h 5 min");
        assert_eq!(format_age(2 * 86_400 + 4 * 3600), "2 d 4 h");
        assert_eq!(short_path("/home/me/work", "/home/me"), "~/work");
        assert_eq!(short_path("/home/meme", "/home/me"), "/home/meme");
    }
}

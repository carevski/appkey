//! The user's processes by cgroup (v2), for the Processes tab's tree view, and the
//! memory cap that apps started by appkey share (`[memory_limit]`).
//!
//! systemd puts every process in a cgroup: the desktop and what its menu opens in the
//! login session's `session-N.scope`, services in `user@UID.service/…`. Apps appkey
//! starts while the cap is on run in [`SLICE`], whose `MemoryHigh` / `MemoryMax` /
//! `MemorySwapMax` the daemon sets through the user's own systemd (no root). Everything here is pure and
//! unit-tested on every host; `/proc`, `/sys/fs/cgroup` and `systemctl` are in
//! `memory::linux`.

/// The slice appkey starts capped apps in (a child of the user manager's `app.slice`).
pub const SLICE: &str = "app-appkey.slice";

/// The cgroup v2 path in `/proc/<pid>/cgroup` (the `0::` line).
pub fn parse_proc_cgroup(text: &str) -> Option<String> {
    text.lines().find_map(|l| l.strip_prefix("0::")).map(|p| p.trim_end().to_owned())
}

/// `memory.high` / `memory.max`: a number of bytes, or `max` for none.
pub fn parse_limit(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// One cgroup and what's in it. Chains of cgroups that only hold one other cgroup are
/// merged into one node (`user.slice/user-1000.slice`), so the tree stays short.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Node {
    /// Full path of the (last) cgroup, e.g. `/user.slice/user-1000.slice`.
    pub path: String,
    /// What's shown: the last path segment(s).
    pub name: String,
    /// Processes directly in it, biggest first.
    pub procs: Vec<u32>,
    pub children: Vec<Node>,
    /// Resident memory of every process in it and below.
    pub rss_kib: u64,
    /// Processes in it and below.
    pub count: usize,
}

impl Node {
    /// Every node, depth first.
    pub fn walk(&self) -> Vec<&Node> {
        let mut out = vec![self];
        for c in &self.children {
            out.extend(c.walk());
        }
        out
    }

    /// Recomputes sizes from fresh numbers, keeping the layout (processes that are gone
    /// count as 0).
    pub fn recount(&mut self, rss: &impl Fn(u32) -> Option<u64>) {
        for c in &mut self.children {
            c.recount(rss);
        }
        self.rss_kib = self.procs.iter().filter_map(|p| rss(*p)).sum::<u64>()
            + self.children.iter().map(|c| c.rss_kib).sum::<u64>();
    }
}

/// The tree of `(pid, cgroup path, rss)`: the top-level nodes, biggest first.
pub fn build_tree(items: &[(u32, &str, u64)]) -> Vec<Node> {
    let mut root = Node::default();
    for &(pid, path, _) in items {
        let mut node = &mut root;
        let mut full = String::new();
        for seg in path.split('/').filter(|s| !s.is_empty()) {
            full.push('/');
            full.push_str(seg);
            let at = if let Some(i) = node.children.iter().position(|c| c.name == seg) {
                i
            } else {
                node.children.push(Node {
                    path: full.clone(),
                    name: seg.to_owned(),
                    ..Node::default()
                });
                node.children.len() - 1
            };
            node = &mut node.children[at];
        }
        node.procs.push(pid);
    }
    let rss = |pid: u32| items.iter().find(|i| i.0 == pid).map(|i| i.2);
    finish(&mut root, &rss);
    let mut top = std::mem::take(&mut root.children);
    // Processes in the root cgroup itself (rare: in containers).
    if !root.procs.is_empty() {
        top.push(Node { path: "/".into(), name: "/".into(), ..root });
    }
    top
}

/// Sums, sorts and merges single-child chains, bottom up.
fn finish(node: &mut Node, rss: &impl Fn(u32) -> Option<u64>) {
    for c in &mut node.children {
        finish(c, rss);
    }
    for c in &mut node.children {
        while c.procs.is_empty() && c.children.len() == 1 {
            let only = c.children.remove(0);
            *c = Node { name: format!("{}/{}", c.name, only.name), ..only };
        }
    }
    node.procs.sort_by_key(|p| std::cmp::Reverse(rss(*p).unwrap_or(0)));
    node.children.sort_by_key(|c| std::cmp::Reverse(c.rss_kib));
    node.rss_kib = node.procs.iter().filter_map(|p| rss(*p)).sum::<u64>()
        + node.children.iter().map(|c| c.rss_kib).sum::<u64>();
    node.count = node.procs.len() + node.children.iter().map(|c| c.count).sum::<usize>();
}

/// What a well-known cgroup is, for people who don't know systemd's names.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // unit names, not files
pub fn describe(name: &str) -> Option<&'static str> {
    let last = name.rsplit('/').next().unwrap_or(name);
    Some(match last {
        SLICE => "apps started by appkey, sharing its memory cap",
        "app.slice" => "apps and services started by your user's systemd",
        "session.slice" => "essential session services",
        "background.slice" => "background services",
        "init.scope" => "your user's systemd",
        _ if last.starts_with("session-") && last.ends_with(".scope") => {
            "your login session: the desktop and apps opened from its menu"
        }
        _ if last.starts_with("user@") && last.ends_with(".service") => {
            "everything your user's systemd runs"
        }
        _ if last.starts_with("app-flatpak-") => "a Flatpak app",
        _ if last.starts_with("snap.") => "a snap",
        _ => return None,
    })
}

/// The shared cap's systemd properties, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cap {
    /// `MemoryHigh`: from here the kernel slows the apps down and reclaims their memory
    /// (drops their cache, swaps out what they don't use), not the rest of the system's.
    pub high: u64,
    /// `MemoryMax`: the limit itself. RAM the group never exceeds; the kernel ends the
    /// biggest process of the group when it can't reclaim more.
    pub max: u64,
    /// `MemorySwapMax`: swap the group may fill before that happens.
    pub swap: u64,
}

/// The cap for a `limit` of RAM (held below 95% of `total`) and `swap` bytes of swap.
/// Slowing down starts at 90% of the limit, so there's time to close something.
pub fn cap_properties(limit: u64, swap: u64, total: u64) -> Cap {
    let max = limit.min(total / 100 * 95);
    Cap { high: max / 10 * 9, max, swap }
}

/// `systemctl --user set-property` argv that applies the cap (`None`: lifts it), only
/// until the user's systemd restarts (`--runtime`; the daemon applies it on start).
pub fn set_property_argv(cap: Option<Cap>) -> Vec<String> {
    let [high, max, swap] = match cap {
        Some(c) => [c.high, c.max, c.swap].map(|b| b.to_string()),
        None => ["infinity"; 3].map(str::to_owned),
    };
    [
        "/usr/bin/systemctl",
        "--user",
        "set-property",
        "--runtime",
        SLICE,
        &format!("MemoryHigh={high}"),
        &format!("MemoryMax={max}"),
        &format!("MemorySwapMax={swap}"),
    ]
    .map(str::to_owned)
    .to_vec()
}

/// Environment variables that describe *our* place in systemd, not the app's.
const NOT_PASSED: &[&str] = &[
    "INVOCATION_ID",
    "JOURNAL_STREAM",
    "LISTEN_FDNAMES",
    "LISTEN_FDS",
    "LISTEN_PID",
    "MANAGERPID",
    "NOTIFY_SOCKET",
    "SYSTEMD_EXEC_PID",
];

/// `systemd-run` argv that makes the user's systemd start `program args…` as a service
/// in [`SLICE`], with our environment (the user manager's own lacks the desktop's) and
/// working directory. `cgroup_exit` (systemd ≥ 250): the unit lasts while any of its
/// processes runs, so a launcher that starts the app and exits doesn't take it down.
/// Older systemd keeps the unit around as "exited" instead (`RemainAfterExit`).
pub fn run_argv(
    unit: &str,
    description: &str,
    program: &str,
    args: &[String],
    env: &[String],
    cgroup_exit: bool,
) -> Vec<String> {
    let mut out: Vec<String> = [
        "/usr/bin/systemd-run",
        "--user",
        "--quiet",
        "--collect",
        "--same-dir",
        &format!("--slice={SLICE}"),
        &format!("--unit={unit}"),
        &format!("--description={description}"),
        // Report a program that can't be started, instead of a unit that failed later.
        "--property=Type=exec",
        if cgroup_exit { "--property=ExitType=cgroup" } else { "--property=RemainAfterExit=yes" },
    ]
    .map(str::to_owned)
    .to_vec();
    out.extend(
        env.iter()
            .filter(|n| !NOT_PASSED.contains(&n.as_str()) && is_env_name(n))
            .map(|n| format!("--setenv={n}")),
    );
    out.push("--".into());
    out.push(program.into());
    out.extend(args.iter().cloned());
    out
}

fn is_env_name(n: &str) -> bool {
    !n.is_empty() && n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// A unit name as the XDG convention has it, `app-appkey-<app>-<random>.service`.
pub fn unit_name(app: &str, random: u32) -> String {
    let app: String = app
        .chars()
        .filter_map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '_' => Some(c),
            ' ' | '-' => Some('_'),
            _ => None,
        })
        .take(40)
        .collect();
    let app = if app.is_empty() { "app".into() } else { app };
    format!("app-appkey-{app}-{random:08x}.service")
}

/// The version from `systemctl --version` ("systemd 255 (255.4-1ubuntu8)").
pub fn parse_systemd_version(text: &str) -> Option<u32> {
    text.lines().next()?.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_files() {
        let v1_and_v2 = "12:memory:/user.slice\n0::/user.slice/user-1000.slice/session-c1.scope\n";
        assert_eq!(
            parse_proc_cgroup(v1_and_v2).as_deref(),
            Some("/user.slice/user-1000.slice/session-c1.scope")
        );
        assert_eq!(parse_proc_cgroup("3:cpu:/\n"), None);
        assert_eq!(parse_limit("max\n"), None);
        assert_eq!(parse_limit("8589934592\n"), Some(8_589_934_592));
    }

    #[test]
    fn builds_a_merged_tree() {
        let u = "/user.slice/user-1000.slice";
        let session = format!("{u}/session-c1.scope");
        let init = format!("{u}/user@1000.service/init.scope");
        let idea = format!("{u}/user@1000.service/app.slice/{SLICE}/app-appkey-idea-1.service");
        let items = [
            (10, session.as_str(), 300),
            (11, session.as_str(), 500),
            (20, init.as_str(), 10),
            (30, idea.as_str(), 2000),
        ];
        let top = build_tree(&items);
        assert_eq!(top.len(), 1);
        let user = &top[0];
        assert_eq!(user.name, "user.slice/user-1000.slice");
        assert_eq!((user.rss_kib, user.count), (2810, 4));
        // Biggest first: user@1000.service (2010) before the session (800).
        let names: Vec<&str> = user.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["user@1000.service", "session-c1.scope"]);
        assert_eq!(user.children[1].procs, [11, 10]);
        let service = &user.children[0];
        let names: Vec<&str> = service.children.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            [format!("app.slice/{SLICE}/app-appkey-idea-1.service").as_str(), "init.scope"]
        );
        assert_eq!(service.children[0].path, idea);
        assert_eq!(user.walk().len(), 5);

        let mut tree = top.clone();
        tree[0].recount(&|pid| (pid != 30).then_some(1));
        assert_eq!(tree[0].rss_kib, 3);
        assert_eq!(tree[0].children[0].name, service.name, "layout is kept");
    }

    #[test]
    fn describes_known_cgroups() {
        assert!(describe("user.slice/user-1000.slice/session-c2.scope").unwrap().contains("menu"));
        assert!(describe("app.slice/app-appkey.slice").unwrap().contains("appkey"));
        assert_eq!(describe("app-gnome-firefox-1.scope"), None);
    }

    #[test]
    fn cap_is_never_exceeded() {
        let gib = 1 << 30;
        let cap = cap_properties(10 * gib, 2 * gib, 16 * gib);
        assert_eq!(cap, Cap { high: 9 * gib, max: 10 * gib, swap: 2 * gib });
        // A fixed size bigger than RAM is held below it.
        let cap = cap_properties(64 * gib, 0, 16 * gib);
        assert!(cap.high < cap.max && cap.max < 16 * gib);
        assert_eq!(
            set_property_argv(None)[4..],
            [SLICE, "MemoryHigh=infinity", "MemoryMax=infinity", "MemorySwapMax=infinity"]
        );
        assert_eq!(
            set_property_argv(Some(Cap { high: 1, max: 2, swap: 0 }))[5..],
            ["MemoryHigh=1", "MemoryMax=2", "MemorySwapMax=0"]
        );
    }

    #[test]
    fn run_argv_passes_env_and_keeps_the_command_apart() {
        let env = ["DISPLAY", "INVOCATION_ID", "WAYLAND_DISPLAY", "BAD=NAME"].map(String::from);
        let argv = run_argv(
            "app-appkey-x-1.service",
            "appkey: X",
            "/usr/bin/x",
            &["--flag".into(), "--".into()],
            &env,
            true,
        );
        assert_eq!(argv[0], "/usr/bin/systemd-run");
        assert!(argv.contains(&format!("--slice={SLICE}")));
        assert!(argv.contains(&"--property=ExitType=cgroup".into()));
        assert!(argv.contains(&"--setenv=DISPLAY".into()));
        assert!(!argv.iter().any(|a| a.contains("INVOCATION_ID") || a.contains("BAD")));
        let sep = argv.iter().position(|a| a == "--").unwrap();
        assert_eq!(argv[sep + 1..], ["/usr/bin/x", "--flag", "--"]);
        let old = run_argv("u", "d", "/x", &[], &[], false);
        assert!(old.contains(&"--property=RemainAfterExit=yes".into()));
    }

    #[test]
    fn unit_names_are_valid() {
        assert_eq!(unit_name("jetbrains-idea", 0xab), "app-appkey-jetbrains_idea-000000ab.service");
        assert_eq!(
            unit_name("Visual Studio Code!", 1),
            "app-appkey-Visual_Studio_Code-00000001.service"
        );
        assert_eq!(unit_name("/", 1), "app-appkey-app-00000001.service");
        assert_eq!(parse_systemd_version("systemd 249 (249.11-0ubuntu3.12)\n+PAM"), Some(249));
    }
}

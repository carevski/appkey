# Design

## Goal

Press a global shortcut → the configured app comes to the front. If it isn't running, start it. If several windows/instances exist, focus the **most recently used** one. Never start a second instance when one is already running.

Constraints: tiny binary (~3 MB including the settings UI), few dependencies, no runtime system libraries beyond what each OS ships, and no weakening of OS security.

## Architecture

```
main.rs ─┬─► app.rs (daemon) ─────────────► platform::{macos,windows,linux}
 (CLI)   │    tray + hotkeys, event loop     focus_or_launch, running_apps,
         │    watches config, spawns ──┐     installed_apps, frontmost_app, …
         │                             ▼
         └─► ui/ (`appkey settings`, separate process, egui)
                    │ edits
                    ▼
              config.rs (TOML) ◄── read + reload by the daemon
```

The daemon and the settings window are **two processes from the same binary**. Each needs to own the main-thread event loop (tao vs. winit), so separating them avoids any conflict. It also keeps the always-running daemon's memory small: GL and egui are only loaded while settings are open. They communicate only through the config file. The daemon checks its modification time every second and reloads, which also picks up hand edits.

| Concern | macOS | Windows | Linux (X11) |
|---|---|---|---|
| Global hotkey | Carbon `RegisterEventHotKey` (via `global-hotkey`) | `RegisterHotKey` | `XGrabKey` on its own thread |
| Event loop | `tao` on main thread | `tao` on main thread | none; `mpsc` channel |
| Tray | `NSStatusItem` | `Shell_NotifyIcon` | StatusNotifierItem over D-Bus (`ksni`, no GTK) |
| Find running app | `NSWorkspace.runningApplications` (name or bundle id) | `EnumWindows` + process image name | `_NET_CLIENT_LIST_STACKING` + `WM_CLASS` |
| "Most recent" | the app's own key window | z-order (first match) | stacking order (topmost match) |
| Focus | `/usr/bin/open -b <id>` (LaunchServices) | `ShowWindow(SW_RESTORE)` + `SetForegroundWindow` | `_NET_ACTIVE_WINDOW`, source = pager |
| Launch default | `open -a <app>` | `<app>` | `<app>` |

## Decisions

- **"Last one open" means most recently *used*.** It's what z-order gives us on every OS for free, and it matches what users expect when switching back. If it should mean most recently *created*, X11 can do it via `_NET_CLIENT_LIST` order, while Windows/macOS would need per-window creation tracking.
- **macOS activates via LaunchServices, not `NSRunningApplication.activate`.** Since macOS 14, activation requests from a background app can be ignored. `open` is what the Dock uses and also un-minimizes. Trade-off: if an app has several *processes* with the same bundle id (rare, `open -n`), LaunchServices picks which one.
- **No shell for launching.** `launch` is an argv array, so config values can't inject shell syntax.
- **Config must not be writable by other users** (Unix). It decides what we execute.
- **Linux without GTK.** `tao` would pull GTK in, so on Linux we skip it. `global-hotkey` and the `ksni` tray both run their own threads.
- **X11 requests are confirmed with a round-trip** before the connection closes, and `_NET_ACTIVE_WINDOW` carries a real server timestamp. Without the round-trip, ~70% of focus requests were silently dropped (found by `scripts/test-x11.sh`).
- **Settings UI: egui/eframe (glow), one codebase for all three OSes.** Considered: Slint (royalty-free licence needs attribution; similar size), iced (bigger), Tauri/webview (needs WebKitGTK on Linux), native per-OS UIs (3× the work), FLTK (C++ build). egui adds ~1.6 MB with system fonts. egui's bundled fonts (1.4 MB) are disabled; `ui/font.rs` loads the OS font (SF / Segoe UI / fontconfig's sans), which also looks more native. AccessKit stays on (+90 KB) for screen readers.
- **Linux binaries link glibc (built on Ubuntu 22.04), not static musl.** winit/glutin `dlopen` X11, xkbcommon and GL, which a static musl binary can't do. Minimum is glibc 2.35 (Ubuntu 22.04, Mint 21, Debian 12).
- **App picker sources:** macOS scans `/Applications` and friends (bundle ids via `NSBundle`); Windows reads Start Menu `.lnk` targets (`platform/lnk.rs`, a tiny parser); Linux parses `.desktop` files (`platform/desktop_entry.rs`, `StartupWMClass` → `app`). Both parsers are platform-neutral and unit-tested on every host. Browser web apps (Chrome/Edge PWAs: `--app-id=` in `Exec`, windows `crx_<id>` with the browser's class) are matched only by their `crx_` name, so they and the browser never stand in for each other. "Pick a window" waits 3 s and reads the frontmost app. It's the cross-platform alternative to a crosshair picker, which Wayland and macOS wouldn't allow without extra permissions.
- **Light/dark follows the desktop, or the `theme` in the config.** winit reports the system theme on macOS and Windows but not on X11, where egui fell back to dark. On Linux `platform::dark_mode()` reads the desktop's setting: GSettings `color-scheme` (`prefer-dark`/`prefer-light`, GNOME, or `org.x.apps.portal` on Mint), else whether the GTK theme's name contains "dark" (Cinnamon's own `gtk-theme` on Cinnamon, e.g. `Mint-Y-Dark-Aqua`), and `ColorScheme` in `kdeglobals` on KDE. Read once when a window opens. The System/Light/Dark buttons save `theme` right away by rewriting only that key in the file as it is on disk, so unsaved shortcut edits stay unsaved.
- **Left click on the tray icon opens settings; the menu is on right click.** macOS/Windows: `with_menu_on_left_click(false)` plus a `TrayIconEvent::Click` (left, up). Linux: the host decides; Cinnamon, KDE and the GNOME AppIndicator extension call the item's `Activate`, which `tray-icon`'s ksni backend reports as the same left click.
- **The tray is optional.** If it can't be created (no StatusNotifier host / D-Bus), shortcuts still work, and settings open with `appkey settings`.
- **Hand-rolled CLI.** Five subcommands don't justify `clap`'s size.

## Low memory and Magic SysRq (Ubuntu / Linux Mint)

The settings window has a **Memory** tab, shown only when `/etc/os-release` says Ubuntu or a derivative (`ID`/`ID_LIKE` contains `ubuntu` or `linuxmint`). The code lives in `src/memory/` (parsers, shared by all hosts for tests), `ui/memory.rs` (the tab) and `ui/pressure.rs` (the warning window).

- **Detection: `MemAvailable` and PSI, once a second in the daemon.** We warn when available memory is below `min_available_percent` of RAM, or when `/proc/pressure/memory` `some avg10` reaches `max_pressure_percent`. The condition must hold for 3 consecutive ticks, then we stay quiet for 5 min after the window closes (1 h if snoozed; the window exits with code 3 to say so). Reading two small `/proc` files a second costs nothing measurable.
- **The warning is a separate process** (`appkey memory --alert`), like settings, so the daemon never loads GL. It starts while memory is low but not yet exhausted, which is why the default thresholds are early ones. It's always-on-top, but doesn't take focus by force.
- **Apps are found through the process tree (`memory/procs.rs`), not by name.** Each process belongs to its parent's app unless it starts one: its parent is init or the user's `systemd` (then it's *detached*), another user's process, a protected desktop process or a launcher (JetBrains Toolbox); it has windows and its parent doesn't (an app started from a terminal or script); or it has a terminal and its parent doesn't (a terminal tab, also inside an IDE). A shell root is named after what runs in it ("Terminal: Claude Code"). Labels come from the command line: Chromium `--type=` (renderer = tab), Firefox `-contentproc`, the Java main class (IntelliJ via `-Didea.platform.prefix`, Gradle/Kotlin daemons, Maven), the Node package (`@anthropic-ai/claude-code`, `vite`, `npm run …`), Python scripts. Chrome/Electron/Node rewrite argv into one space-separated string; `parse_cmdline` splits it back.
- **"Left running in the background"** = detached, not a systemd `.service` (those are deliberate; read from `/proc/<pid>/cgroup`), no window, not a small agent (`ssh-agent`…), running for at least a minute.
- **Per-tab Chrome info isn't available without remote debugging,** which would let any local process control the browser, so we don't. Tab processes are listed and can be ended individually; the window points to Chrome's own Task Manager (Shift+Esc) for which-tab-is-which.
- **Window titles** come from every Linux backend (`Win::title`, `platform::window_titles()`; empty on macOS/Windows, which don't have the memory window). They're re-read every third refresh: on KDE each read runs a KWin script.
- **The list holds still while the pointer is in the window.** Processes come and go every second (tabs, short-lived helpers), and rows shifting under the pointer made a test click close the wrong app. Rows also keep their order between refreshes; CPU % needs two samples, so the first two refreshes always run.
- **Busy background programs:** every 10 s the daemon samples CPU time of detached groups' processes (`BusyTracker`); one that used ≥ 90% of a core over the whole chosen window (and, checked only then, has no window) opens `appkey memory --busy`, debounced like the memory warning.
- **Shortcut action:** `app = "appkey:memory"` (`config::MEMORY_ACTION`) opens the window, or focuses the one already open (found in `/proc` by its `memory` argument), from the daemon and from `appkey __hotkey` alike.
- **Closing is `SIGTERM`, then optionally `SIGKILL`,** a whole app or one process, only for our own uid, never for session/desktop processes (`memory::PROTECTED`) or pid 1. Each target is `(pid, start time)`; it's re-checked right before the signal, so a pid reused since the list was drawn is left alone. No elevation: other users' processes aren't listed at all.
- **Uninstall (`memory/uninstall.rs`) works out the install method from the app's main process,** never from what the user remembers: `.flatpak-info` in its `/proc/<pid>/root` (Flatpak; system vs user from `app-path`), the `APPIMAGE` variable in its environment (AppImage), an exe under `/snap/<name>/` (snap), under `JetBrains/Toolbox/apps` (Toolbox: not automatic), else `dpkg -S` on its exe (a `(deleted)` suffix after an upgrade is stripped; merged `/usr` tried both ways). Removal: `pkexec /usr/bin/snap remove`, `flatpak uninstall --user|--system` (Flatpak's own polkit helper), `pkexec /usr/bin/env DEBIAN_FRONTEND=noninteractive /usr/bin/apt-get remove -y` (not purge), `gio trash` for the AppImage and the `~/.local/share/applications` entries that launch it. apt is dry-run first (`apt-get -s`); the removal is refused when it touches desktop/system packages (`ESSENTIAL` prefixes) or exceeds 15 packages. Names are validated against each format before they reach an argv. The app is closed (SIGTERM, SIGKILL after 5 s) before the command runs: the clicked row, plus every process of the installation (snap: exe under `/snap/<name>/` or a `snap.<name>.` cgroup; Flatpak: its `app-flatpak-<id>-` scope). Tray helpers outside the row kept writing to `~/snap/<name>` and made `snap remove` fail at its last step, "directory not empty", after it had already removed the app from the menu (seen with Skype). If that still happens, the result offers to move exactly `~/snap/<name>` (which `snap remove` deletes anyway) to the Trash and run the removal again. Tested for real with a .deb (Geany) and an AppImage; snap and Flatpak only by unit tests (no snapd / Flatpak runtime in the test container).
- **SysRq needs root, so it goes through the OS's own prompt.** `pkexec` runs two fixed argvs, `/usr/bin/tee /etc/sysctl.d/90-appkey-sysrq.conf` (contents on stdin) and `/usr/sbin/sysctl -q -p` on that file. No shell, absolute paths, and appkey itself never runs as root. That can mean two password prompts; we accept that over running our own binary as root. The `90-` name sorts after Ubuntu's `10-magic-sysrq.conf`, so it wins at boot. The tab also shows the equivalent terminal command.
- **Why not more distros:** asked for Ubuntu/Mint. Nothing here is Ubuntu-specific except the default (176) we describe; widening means changing `is_supported_distro`.

## Security & permissions by OS

| OS | What's needed | How we handle it |
|---|---|---|
| macOS | Nothing. Carbon hotkeys and LaunchServices need no Accessibility, Input Monitoring or Automation permission. | If we ever need per-window control (AXUIElement), we must call `AXIsProcessTrustedWithOptions` with the prompt option and explain why first. Unsigned downloads are quarantined by Gatekeeper; signing/notarization is a release task. |
| Windows | Nothing. | UIPI: windows of elevated processes can't be fully controlled from a non-elevated appkey. We report it rather than auto-elevating. Unsigned exes trigger SmartScreen; signing is a release task. |
| Linux X11 | Nothing. X11 has no permission model. | n/a |
| Linux (Ubuntu / Mint), Memory tab | Changing `kernel.sysrq`, removing a snap or a .deb needs root. | `pkexec` with fixed argvs (absolute paths, validated names, no shell) shows the desktop's own password prompt; cancelling is reported, not retried. System Flatpaks use Flatpak's own polkit helper. The memory window only signals the user's own processes. |
| Linux Wayland | GNOME: the user keeps an extension enabled (visible and switchable in the Extensions app). KDE: the user approves shortcuts in KDE's dialog. Hyprland: nothing. | We never bypass the desktop: the extension only acts on appkey's D-Bus calls, the KDE dialog is the portal's own, and a cancelled approval is respected (reported, never overridden). The GNOME extension's D-Bus methods are reachable by any process of the same user, which can already control the session. |

## Linux backends

`platform/linux/mod.rs` picks a backend from `XDG_SESSION_TYPE` / `XDG_CURRENT_DESKTOP` / `HYPRLAND_INSTANCE_SIGNATURE`. Each implements `WindowSystem` (list windows most-recently-used first, activate, active window) and supplies a `Hotkeys`. Matching, MRU choice and launching are shared.

| Session | Windows | Shortcuts | User-visible setup |
|---|---|---|---|
| X11 | EWMH (`x11.rs`) | `XGrabKey` (global-hotkey) | none |
| Cinnamon (X11) | EWMH (`x11.rs`) | `XGrabKey`; Super combos as Cinnamon custom keybindings → `appkey __hotkey <id>` (`cinnamon.rs`) | none (entries visible under Keyboard → Custom Shortcuts) |
| GNOME Wayland | appkey's GNOME Shell extension (`gnome.rs`, `packaging/gnome-extension/`) | the extension (`grab_accelerator`) | log out/in once after first run |
| KDE Plasma Wayland | KWin scripting over D-Bus (`kde.rs`) | GlobalShortcuts portal (`portal.rs`) | approve KDE's dialog |
| Hyprland | IPC socket (`hyprland.rs`) | runtime `bind` → `appkey __hotkey <id>` | none |
| other Wayland (Sway, Cinnamon, COSMIC…) | not supported yet | | `appkey` explains |

Decisions:

- **GNOME needs an extension; there is no other sanctioned way.** It lists windows in Alt+Tab order (`get_tab_list`), activates with a real timestamp, and grabs keys. It's installed by the packages, or by appkey into `~/.local/share` for the tarball, and enabled via `gsettings`. GNOME only discovers new extensions at login, so the daemon keeps running, says "log out and back in", and retries every 5 s. GNOME disables extensions on the lock screen; the extension emits `Started` when re-enabled and the daemon re-grabs.
- **GNOME uses the extension for shortcuts too, not the portal.** We need the extension for windows anyway, and this avoids a second consent dialog.
- **GNOME quirks handled in the extension:** activation is applied twice if focus didn't move (seen right after windows open), and again after the Activities overview finishes hiding (which otherwise restores the old focus). Placeholder app ids (`window:N`) are ignored.
- **Cinnamon owns the Super key.** Muffin grabs the keyboard when Super goes down (for the menu) and replays the next key to the focused window, which bypasses every other client's passive grab. So `XGrabKey(Mod4+i)` never fires, and the settings window sees a bare `i` (the recorder asks the X server for the modifier state, `platform::logo_key_down`). Super shortcuts are therefore written as Cinnamon custom keybindings (`custom-list` + relocatable `custom-keybinding` entries, via `gsettings`), which run `appkey __hotkey <id>`. Ids must contain "custom" (`custom-appkey0`…): Cinnamon only unbinds such ids when the list changes, others keep their keys and old command until it restarts. Combos Cinnamon already uses are handled by the shared check below. Entries are replaced on every reload and removed on exit; we read-modify-write `custom-list`, keeping other ids.
- **Desktop-owned shortcuts are checked up front (Cinnamon, GNOME, KDE; `reserved.rs`).** The desktop grabs its own combos before any app, so registering them "succeeds" but never fires. KDE: every component's `allShortcutInfos` from kglobalaccel over D-Bus (KWin, Plasma, apps; active keys as Qt key codes, `xkb::from_qt`), skipping our own `appkey` component. Plasma 5 sends keys as `ai`, Plasma 6 as key sequences; `kde::shortcut_infos` accepts both. Not `kglobalshortcutsrc`: it doesn't list defaults. Cinnamon and GNOME keep them in `GSettings`: Cinnamon in `org.cinnamon.desktop.keybindings{,.wm,.media-keys}` plus its `custom-list`; GNOME in `org.gnome.desktop.wm.keybindings`, `org.gnome.shell.keybindings`, `org.gnome.mutter{,.wayland}.keybindings`, `org.gnome.settings-daemon.plugins.media-keys` plus its `custom-keybindings` paths, and Ubuntu Dock's `app-hotkey-N` when `hot-keys` is on. The daemon wraps the desktop's `Hotkeys` in `Reserved`, which re-reads them on every reload and reports matches as unavailable; the settings window shows the same list as non-blocking row warnings (`platform::desktop_shortcuts()`). Matching is by `HotKey` id after `xkb::from_gtk`. Values read with plain `gsettings` (no D-Bus needed to read).
- **KDE uses the portal for shortcuts.** It shows its own approval dialog, which is correct and expected. KDE remembers every id it was ever given and keeps grabbing its keys, so after each bind we `unregister` ids we no longer use. If the user cancels, KDE later registers the shortcut with *no keys* and doesn't ask again. We read `trigger_description` from the bind result and report that (pointing to System Settings) instead of claiming success.
- **Portal identity:** host apps must register an app id (`org.freedesktop.host.portal.Registry`) that matches an installed `.desktop` file; appkey writes `~/.local/share/applications/appkey.desktop` if no package installed one.
- **KWin scripting:** a generated JS file is loaded, run (`/Scripting/ScriptN.run`), reports back with `callDBus` to our connection, and is unloaded.
- **Hyprland binds exec a command,** so only our binary path (single-quoted) and a number ever reach the shell. Runtime binds vanish on config reload; we re-apply on `configreloaded`, and remove them on exit. The daemon handles SIGTERM/SIGINT/SIGHUP so this also happens on logout.

### Testing Wayland

`scripts/wayland/` holds a container image and an e2e script per desktop, run in CI (`linux-wayland-e2e`):

- **GNOME 50**: `gnome-shell --headless --virtual-monitor` with a system bus and a mock logind (python-dbusmock). `--unsafe-mode` lets the test check focus via `Eval`, independently of appkey. Keys are real input events via Mutter's RemoteDesktop API (`mutter-keys.py`; the session dies with the D-Bus connection that created it, hence a script).
- **KDE Plasma 6**: `kwin_wayland --virtual` (copied to drop file capabilities, which rootless containers refuse). The approval dialog is clicked through AT-SPI (`a11y.py`); shortcuts are fired with kglobalaccel's `invokeShortcut`.
- **Hyprland**: needs a DRM device. Load `vkms` on the host and pass `/dev/dri`. Hyprland ignores virtual keyboards for binds, so the test runs the bound command itself.

## Not yet done

- Start at login (LaunchAgent / `Run` registry key / XDG autostart), as an opt-in `appkey autostart on|off`
- Code signing: Windows (Authenticode) and macOS (Developer ID + notarization), so users don't see SmartScreen/Gatekeeper warnings
- AUR package for Arch
- More Wayland desktops: Sway/labwc/river (wlr-foreign-toplevel + config binds), Cinnamon on Wayland (default in Mint stays X11 for now), COSMIC

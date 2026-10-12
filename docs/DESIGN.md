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
- **App picker sources:** macOS scans `/Applications` and friends (bundle ids via `NSBundle`); Windows reads Start Menu `.lnk` targets (`platform/lnk.rs`, a tiny parser); Linux parses `.desktop` files (`platform/desktop_entry.rs`, `StartupWMClass` → `app`). Both parsers are platform-neutral and unit-tested on every host. "Pick a window" waits 3 s and reads the frontmost app. It's the cross-platform alternative to a crosshair picker, which Wayland and macOS wouldn't allow without extra permissions.
- **The tray is optional.** If it can't be created (no StatusNotifier host / D-Bus), shortcuts still work, and settings open with `appkey settings`.
- **Hand-rolled CLI.** Five subcommands don't justify `clap`'s size.

## Security & permissions by OS

| OS | What's needed | How we handle it |
|---|---|---|
| macOS | Nothing. Carbon hotkeys and LaunchServices need no Accessibility, Input Monitoring or Automation permission. | If we ever need per-window control (AXUIElement), we must call `AXIsProcessTrustedWithOptions` with the prompt option and explain why first. Unsigned downloads are quarantined by Gatekeeper; signing/notarization is a release task. |
| Windows | Nothing. | UIPI: windows of elevated processes can't be fully controlled from a non-elevated appkey. We report it rather than auto-elevating. Unsigned exes trigger SmartScreen; signing is a release task. |
| Linux X11 | Nothing. X11 has no permission model. | n/a |
| Linux Wayland | Not supported yet (see roadmap). The portal *will* show the user a consent dialog, which is the right model. | `ensure_supported` detects Wayland and explains the options. |

## Linux / Wayland roadmap

Target distros: Mint/Cinnamon (X11 by default → supported today), Ubuntu (GNOME on **Wayland** by default; 25.10+ has no Xorg GNOME session), Arch (varies), KDE (Wayland default).

Wayland splits the job into two problems:

1. **Global hotkeys:** `org.freedesktop.portal.GlobalShortcuts`. Supported by KDE Plasma 6, GNOME 48+, Hyprland. The user approves the shortcuts in a system dialog. Crate: `ashpd`.
2. **Focusing another app's window:** no standard protocol. Per compositor:
   - KDE: KWin scripting over D-Bus
   - Sway / Hyprland: IPC (`swaymsg`, `hyprctl dispatch focuswindow`)
   - GNOME: requires a small Shell extension (GNOME blocks `Shell.Eval`)
   - Cinnamon Wayland: experimental; revisit later

Plan: add a `platform/linux/wayland/` backend with a `Compositor` enum detected from `XDG_CURRENT_DESKTOP`, starting with KDE + Hyprland/Sway (no extension needed), then GNOME via an extension.

## Not yet done

- Start at login (LaunchAgent / `Run` registry key / XDG autostart), as an opt-in `appkey autostart on|off`
- Code signing: Windows (Authenticode) and macOS (Developer ID + notarization), so users don't see SmartScreen/Gatekeeper warnings
- AUR package for Arch
- Wayland (above)

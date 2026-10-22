# appkey

Tiny cross-platform (macOS, Windows, Linux/X11) tray app: a global shortcut opens an app, or focuses its most recently used window if it's already running. Design and rationale: @docs/DESIGN.md

## Commands

- Build / run: `cargo run -- <command>` (`run`, `check`, `list`, `open <app>`, `config`)
- Test: `cargo test`
- Lint (must be clean): `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt` (a hook runs it after every edit to a `.rs` file)
- Full verification across all three OSes: the `/verify` skill
- Size check: `cargo build --release && ls -lh target/release/appkey`
- Linux end-to-end on Xvfb + Openbox, both in CI and in `/verify`: `scripts/test-x11.sh <binary>` (focus behaviour), `scripts/test-settings-ui.sh <binary>` (record shortcut → save → daemon reload → shortcut works)
- Wayland e2e (containers; see `/verify` and docs/DESIGN.md "Testing Wayland"): `scripts/wayland/test-{gnome,kde,hyprland}.sh`
- See the settings window without screen-recording permission (debug builds): `APPKEY_SCREENSHOT=/tmp/s.png cargo run -- settings`, add `APPKEY_SCREENSHOT_SCENE=picker` for the app picker, `=memory` for the Memory tab (Linux). `APPKEY_SCREENSHOT=/tmp/m.png cargo run -- memory` for the warning window. Then Read the PNG. Do this after any UI change.

## Releases

Sole developer, no review process: commit and push straight to `main` (no feature branches or PRs). Every push to `main` ships a release, so run `/verify` first.

Every push to `main` runs `.github/workflows/release.yml`: CI → installers on native runners → GitHub Release `vMAJOR.MINOR.<run number>`. Packaging lives in `scripts/package-{linux,macos}.sh`, `scripts/package-windows.ps1` and `packaging/` (Inno Setup, Info.plist, .desktop, icon), plus `[package.metadata.deb]` / `[package.metadata.generate-rpm]` in Cargo.toml. Never commit a stamped version: `scripts/set-version.sh` is for CI only. `scripts/package-macos.sh` runs fully on this Mac. The Linux script runs in the `/verify` container (it builds for the container's own arch).

## Layout

- `src/main.rs`: CLI parsing (hand-rolled, no clap)
- `src/app.rs`: the daemon: tray (optional), hotkey registration, event loop (tao on macOS/Windows, plain channel on Linux), config watching, spawning the settings window
- `src/ui/`: settings window (`appkey settings`, egui, separate process). `keys.rs` = shortcut recording (unit-tested), `picker.rs` = app chooser, `font.rs` = system font, `devshot.rs` = debug-only screenshots
- `src/config.rs`: TOML config: path, parsing, validation, example. Most unit tests live here.
- `src/matcher.rs`: how a configured `app` name is compared to what the OS reports
- `src/platform/{macos,windows}.rs`, `src/platform/linux/`: one backend per OS, **same public functions in each** (listed in `platform/mod.rs`). `desktop_entry.rs` / `lnk.rs` are pure parsers compiled for tests on every host.
- `src/platform/linux/`: `mod.rs` picks a backend per session/desktop. `x11.rs`, `gnome.rs` (+ `packaging/gnome-extension/`), `kde.rs`, `hyprland.rs` implement `WindowSystem`. `cinnamon.rs` = Super shortcuts as Cinnamon custom keybindings (X11 windows otherwise). `portal.rs` = GlobalShortcuts portal, `xkb.rs` = key syntax conversions. See the table in docs/DESIGN.md.
- `src/memory/`: Ubuntu/Mint only: low-memory detection (`/proc/meminfo`, PSI), grouping and signalling the user's processes, Magic SysRq (`sysrq.rs`). Parsers compile on every host for tests. UI: `ui/memory.rs` (settings tab), `ui/pressure.rs` (`appkey memory [--alert]` window)
- `src/hotkeys.rs`: `Hotkeys` trait; global-hotkey on macOS/Windows/X11, desktop-specific ones on Wayland

## Rules

- **Small is a feature.** Don't add a dependency without saying why in the PR, and prefer crates already in `Cargo.lock`. No GTK on Linux (tray uses `ksni`). Don't re-enable egui's `default_fonts`. Check binary size impact for new deps (macOS arm64 release ≈ 3.0 MB now).
- **Platform parity.** A change to one backend's public API must be made in all three. Put shared logic in platform-neutral modules where it can be unit-tested on any host.
- **We develop on macOS.** Windows code is checked with `--target x86_64-pc-windows-msvc`, Linux code in a container (see `/verify`). CI runs all three natively. Never claim Linux/Windows *behaviour* works without testing it there. A compile check is not a runtime test.
- **Security:**
  - Launch commands run without a shell (argv list only).
  - The config is refused if other users can write to it.
  - Never bypass OS protections: no focus-stealing hacks, no `SendInput` tricks, no elevation, no injecting into other processes. If the OS needs the user to grant something, detect it, explain it, and let the OS show its own prompt.
  - The one thing that needs root (the SysRq setting, Memory tab) runs fixed argvs through `pkexec`, so the desktop shows its own password prompt. appkey's own binary never runs as root.
  - Every `unsafe` block needs a `// SAFETY:` comment.
- Errors: `anyhow` with context that tells the user what to do. Hotkey failures go to the tray status/tooltip, not just stderr. In a tray app nobody sees stderr.
- Keep `README.md` (user-facing) and `docs/DESIGN.md` (decisions, platform matrix) in sync with behaviour changes.

## Gotchas

- `rustup` comes from Homebrew (keg-only). If `cargo` isn't found, `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.
- `global-hotkey` and the tray must be created on the main thread *after* the tao loop starts (`StartCause::Init`).
- egui 0.36 API differs from older docs and examples: `App::ui(&mut self, ui, frame)`, `egui::Panel::top/bottom(..).show(ui, ..)`, `run_ui_native`. Read the crate source in `~/.cargo/registry` when unsure.
- Wayland gotchas (all hit and fixed once, keep them fixed): KDE keeps old portal shortcut ids grabbed (we unregister), and after a cancelled dialog it binds with no keys (we check `trigger_description`). GNOME restores focus after the overview hides (re-activate), and Mutter sometimes needs a second `activate`. Hyprland drops runtime binds on reload (re-apply). Portals need our `.desktop` file registered. Test env: `kwin_wayland` must be copied (file caps), GNOME Shell needs a system bus + mock logind, Hyprland needs `vkms` and ignores virtual keyboards for binds.
- Cinnamon swallows Super (menu): `XGrabKey` with Mod4 never fires and the settings window never sees the Super press. Super combos go through Cinnamon custom keybindings whose ids must contain "custom", or Cinnamon never unbinds them. Reproduce with a real `cinnamon --replace` on Xvfb (`apt install cinnamon`), not Openbox. Don't set `XDG_CONFIG_HOME` in that test: gsettings reads the dconf db from under it, so appkey would see (and write back) an empty `custom-list`.
- On Linux, winit panics (not errors) if `libxkbcommon-x11` is missing. The `.deb` depends on it, and e2e environments must install it.
- X11: always do a round-trip (`get_input_focus().reply()`) before dropping a connection that sent requests. Otherwise the server may discard them on disconnect, and focus fails at random. Send real server timestamps with `_NET_ACTIVE_WINDOW`.
- On Linux keep `x11rb` on the same minor version as `global-hotkey` uses (currently 0.13) to avoid two copies.
- `HotKey` ids are derived from mods+key, so identical combos written differently (`ctrl+a` vs `control+A`) collide. `config::parse` rejects that.

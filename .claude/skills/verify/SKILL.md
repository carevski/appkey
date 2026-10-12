---
name: verify
description: Verify appkey builds, lints and tests cleanly for macOS, Windows and Linux before declaring work done. Use after any code change, and before committing.
---

# Verify appkey on all three platforms

Run these from the repo root and fix everything they report. Don't skip a platform because the change "only touched" another one: shared modules affect all three.

If `cargo` isn't found: `export PATH="/opt/homebrew/opt/rustup/bin:$PATH"`.

1. **Host (macOS), native:**
   ```bash
   cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
   ```

2. **Windows, cross-checked** (type-checks and lints; can't run tests from macOS):
   ```bash
   rustup target add x86_64-pc-windows-msvc
   cargo clippy --all-targets --target x86_64-pc-windows-msvc -- -D warnings
   ```

3. **Linux, in a container** (real build + tests; needs no system libraries). Use Podman, or Docker if it's running:
   ```bash
   podman machine start 2>/dev/null; podman run --rm -v "$PWD":/src:ro -v appkey-linux-target:/target -v appkey-cargo-registry:/usr/local/cargo/registry -e CARGO_TARGET_DIR=/target -w /src docker.io/library/rust:1-slim sh -c 'rustup component add clippy >/dev/null && cargo clippy --all-targets --locked -- -D warnings && cargo test --locked'
   ```

   Then both end-to-end tests on a virtual desktop (focusing, and the settings window flow):
   ```bash
   podman run --rm -v "$PWD":/src -v appkey-linux-target-gnu:/src/target -v appkey-cargo-registry:/usr/local/cargo/registry -w /src docker.io/library/rust:1-slim bash -c 'apt-get -qq update && apt-get -qq install -y xvfb openbox xterm xdotool x11-utils fonts-dejavu-core fontconfig libgl1-mesa-dri libegl1 libxkbcommon-x11-0 libxcursor1 libxrandr2 libxi6 >/dev/null && cargo build -q --release && scripts/test-x11.sh target/release/appkey </dev/null && scripts/test-settings-ui.sh target/release/appkey </dev/null'
   ```

   Wayland desktops (when anything under `src/platform/linux/`, `src/hotkeys.rs`, `src/app.rs` or `packaging/gnome-extension/` changed). Build images once, then use the release binary from the volume:
   ```bash
   for d in gnome kde hyprland; do podman build -q -t appkey-$d -f scripts/wayland/Containerfile.$d scripts/wayland; done
   podman run --rm --user root -v appkey-linux-target-gnu:/t:ro -v "$PWD":/src:ro -w /src appkey-gnome scripts/wayland/test-gnome.sh /t/release/appkey
   podman run --rm -v appkey-linux-target-gnu:/t:ro -v "$PWD":/src:ro -w /src appkey-kde dbus-run-session scripts/wayland/test-kde.sh /t/release/appkey
   podman machine ssh 'sudo modprobe vkms && sudo chmod 666 /dev/dri/card*'   # Hyprland needs a DRM device
   podman run --rm --device /dev/dri -v appkey-linux-target-gnu:/t:ro -v "$PWD":/src:ro -w /src appkey-hyprland dbus-run-session scripts/wayland/test-hyprland.sh /t/release/appkey
   ```

4. **Workflows / scripts** (when `.github/` or `scripts/` changed):
   ```bash
   podman run --rm -v "$PWD":/repo:ro -w /repo docker.io/rhysd/actionlint:latest -no-color -oneline
   podman run --rm -v "$PWD":/mnt:ro docker.io/koalaman/shellcheck:stable -S warning scripts/*.sh scripts/wayland/*.sh
   ```

5. **UI changes:** look at them. `APPKEY_SCREENSHOT=/tmp/main.png cargo run -- settings` (and `APPKEY_SCREENSHOT_SCENE=picker`), then Read the PNGs.

6. **Size** (only when dependencies or the release profile changed): `cargo build --release && ls -lh target/release/appkey`. Report the before/after size.

Report which steps passed. Be explicit that steps 2–3 are compile/unit-test checks, not proof the hotkeys and focusing work on that OS. That needs a manual run there (or CI artifacts).

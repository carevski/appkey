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

   Then the X11 end-to-end test (real focusing on a virtual desktop). The container is ARM, so build for aarch64:
   ```bash
   podman run --rm -v "$PWD":/src -v appkey-linux-target:/src/target -v appkey-cargo-registry:/usr/local/cargo/registry -w /src docker.io/library/rust:1-slim bash -c 'rustup target add aarch64-unknown-linux-musl >/dev/null && cargo build -q --release --target aarch64-unknown-linux-musl && apt-get -qq update && apt-get -qq install -y xvfb openbox xterm xdotool x11-utils >/dev/null && scripts/test-x11.sh target/aarch64-unknown-linux-musl/release/appkey </dev/null'
   ```

4. **Workflows / scripts** (when `.github/` or `scripts/` changed):
   ```bash
   podman run --rm -v "$PWD":/repo:ro -w /repo docker.io/rhysd/actionlint:latest -no-color -oneline
   podman run --rm -v "$PWD":/mnt:ro docker.io/koalaman/shellcheck:stable -S warning scripts/*.sh
   ```

5. **Size** (only when dependencies or the release profile changed): `cargo build --release && ls -lh target/release/appkey`. Report the before/after size.

Report which steps passed. Be explicit that steps 2–3 are compile/unit-test checks, not proof the hotkeys and focusing work on that OS. That needs a manual run there (or CI artifacts).

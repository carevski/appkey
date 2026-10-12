#!/usr/bin/env bash
# End-to-end test of the Hyprland backend. Needs a DRM device: Hyprland has no
# headless-only mode, so load the virtual KMS driver (`modprobe vkms`) on the host
# and run inside scripts/wayland/Containerfile.hyprland with `--device /dev/dri`:
#   dbus-run-session scripts/wayland/test-hyprland.sh path/to/appkey
# Hyprland ignores virtual keyboards for binds, so instead of pressing keys we
# check the bind appkey installed and run exactly the command it executes.
set -uo pipefail
bin="$(realpath "${1:?usage: $0 path/to/appkey}")"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$(mktemp -d)}"; chmod 700 "$XDG_RUNTIME_DIR"
HOME="$(mktemp -d)"
export HOME LIBSEAT_BACKEND=noop XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=Hyprland

fail=0
report() {
    echo "$1 ${*:2}"
    if [[ "$1" == FAIL ]]; then
        fail=1
        [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::error title=Hyprland e2e::${*:2}"
    fi
}
wait_for() {
    local deadline=$((SECONDS + $1)); shift
    until "$@" >/dev/null 2>&1; do ((SECONDS >= deadline)) && return 1; sleep 0.2; done
}

mkdir -p "$HOME/.config/hypr"
printf 'misc {\n  disable_hyprland_logo = true\n}\n' >"$HOME/.config/hypr/hyprland.conf"
Hyprland >"$XDG_RUNTIME_DIR/hyprland.log" 2>&1 &
trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
socket() { ls "$XDG_RUNTIME_DIR"/hypr/*/.socket.sock; }
wait_for 20 socket || { report FAIL "Hyprland did not start"; tail -20 "$XDG_RUNTIME_DIR/hyprland.log"; exit 1; }
HYPRLAND_INSTANCE_SIGNATURE="$(basename "$(dirname "$(socket | head -1)")")"
export HYPRLAND_INSTANCE_SIGNATURE
for sock in "$XDG_RUNTIME_DIR"/wayland-*; do
    [[ "$sock" == *.lock ]] || { export WAYLAND_DISPLAY="${sock##*/}"; break; }
done

# Independent check: Hyprland's own idea of the focused window.
active() { hyprctl -j activewindow | sed -n 's/^ *"title": "\(.*\)",$/\1/p' | head -1; }
expect_active() {
    sleep 0.5
    local got; got=$(active)
    if [[ "$got" == "$2" ]]; then report ok "$1"; else report FAIL "$1: active is '$got', expected '$2'"; fi
}
window() { foot --app-id "$1" --title "$2" sleep 600 >/dev/null 2>&1 & sleep 1; }

window alpha alpha-1
window alpha alpha-2
window beta beta

checked=$("$bin" check 2>&1)
[[ "$checked" == *"Backend: Hyprland"* ]] && report ok "detects Hyprland" || report FAIL "backend not detected: $checked"
"$bin" open alpha >/dev/null; expect_active "focuses most recent of several windows" alpha-2
"$bin" open beta >/dev/null;  expect_active "switches to another app" beta
"$bin" open alpha >/dev/null; expect_active "follows most-recently-used order" alpha-2
hyprctl dispatch focuswindow title:alpha-1 >/dev/null; sleep 0.3
"$bin" open beta >/dev/null; "$bin" open alpha >/dev/null; expect_active "MRU tracks focus changes made outside appkey" alpha-1

# --- shortcuts
mkdir -p "$HOME/.config/appkey"
cfg="$HOME/.config/appkey/config.toml"
printf '[[shortcut]]\nkeys = "ctrl+alt+b"\napp = "beta"\n\n[[shortcut]]\nkeys = "ctrl+alt+g"\napp = "gamma"\nlaunch = ["foot", "--app-id", "gamma", "--title", "gamma", "sleep", "600"]\n' >"$cfg"
chmod 600 "$cfg"
"$bin" run >"$XDG_RUNTIME_DIR/daemon.log" 2>&1 &
wait_for 10 grep -q '2 shortcut(s) active' "$XDG_RUNTIME_DIR/daemon.log" \
    && report ok "shortcuts active" || { report FAIL "not active"; cat "$XDG_RUNTIME_DIR/daemon.log"; }

# The command Hyprland runs for a bind with these modifiers (modmask) and key.
bound_cmd() {
    hyprctl -j binds | python3 -c '
import json, sys
mods = {"SHIFT": 1, "CTRL": 4, "ALT": 8, "SUPER": 64}
want = sum(mods[m] for m in sys.argv[1].split()) if sys.argv[1] else 0
for b in json.load(sys.stdin):
    if b["dispatcher"] == "exec" and b["key"] == sys.argv[2] and b["modmask"] == want:
        print(b["arg"]); break' "$1" "$2"
}
press() { local cmd; cmd=$(bound_cmd "$1" "$2"); [[ -n "$cmd" ]] && sh -c "$cmd"; }
[[ -n "$(bound_cmd 'CTRL ALT' b)" ]] && report ok "bind for Ctrl+Alt+B installed" || report FAIL "no bind for Ctrl+Alt+B: $(hyprctl binds)"
"$bin" open alpha >/dev/null
press 'CTRL ALT' b; expect_active "shortcut focuses its app" beta
press 'CTRL ALT' g; expect_active "shortcut launches an app that isn't running" gamma

# Config reload in Hyprland wipes runtime binds; appkey must put them back.
hyprctl reload >/dev/null; sleep 1.5
[[ -n "$(bound_cmd 'CTRL ALT' b)" ]] && report ok "binds survive a Hyprland config reload" || report FAIL "binds lost after reload"

# Changing a shortcut removes the old bind.
sed -i 's/ctrl+alt+b/ctrl+alt+n/' "$cfg"
wait_for 10 bash -c "[[ -n \"\$(hyprctl -j binds | grep -c '\"key\": \"n\"')\" && \$(hyprctl -j binds | grep -c '\"key\": \"n\"') -gt 0 ]]"
sleep 0.5
if [[ -n "$(bound_cmd 'CTRL ALT' n)" && -z "$(bound_cmd 'CTRL ALT' b)" ]]; then report ok "changed shortcut replaces the old bind"
else report FAIL "binds after change: $(hyprctl -j binds | grep -E '"key"|"arg"')"; fi

# Quitting appkey removes its binds.
pkill -f "$bin run"; sleep 1
[[ -z "$(bound_cmd 'CTRL ALT' n)" ]] && report ok "binds removed when appkey quits" || report FAIL "binds left behind after quit"

exit $fail

#!/usr/bin/env bash
# End-to-end test of the GNOME (Wayland) backend in a headless GNOME Shell.
# Run as root inside scripts/wayland/Containerfile.gnome; it starts a system bus
# with a mock logind, then re-runs itself as the `tester` user in a D-Bus session:
#   scripts/wayland/test-gnome.sh path/to/appkey
# Covers the one-time setup (extension install + enable, "log in again"),
# focusing, and shortcuts pressed as real key events via Mutter's RemoteDesktop API.
set -uo pipefail
bin="$(realpath "${1:?usage: $0 path/to/appkey}")"
self="$(realpath "$0")"

if [[ "$(id -u)" == 0 ]]; then
    mkdir -p /run/dbus && dbus-daemon --system --fork
    python3 -m dbusmock --system --template logind >/tmp/logind.log 2>&1 &
    sleep 1
    exec runuser -u tester -- env GITHUB_ACTIONS="${GITHUB_ACTIONS:-}" dbus-run-session "$self" "$bin"
fi

export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$(mktemp -d)}"; chmod 700 "$XDG_RUNTIME_DIR"
export XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=GNOME WAYLAND_DISPLAY=wayland-0

fail=0
report() {
    echo "$1 ${*:2}"
    if [[ "$1" == FAIL ]]; then
        fail=1
        [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::error title=GNOME Wayland e2e::${*:2}"
    fi
}
wait_for() {
    local deadline=$((SECONDS + $1)); shift
    until "$@" >/dev/null 2>&1; do ((SECONDS >= deadline)) && return 1; sleep 0.3; done
}
eval_js() { gdbus call --session -d org.gnome.Shell -o /org/gnome/Shell -m org.gnome.Shell.Eval "$1"; }

shell_pid=""
start_shell() {
    # --unsafe-mode lets the test read state via Eval, independently of appkey.
    gnome-shell --headless --virtual-monitor 1280x800 --unsafe-mode --no-x11 >>"$XDG_RUNTIME_DIR/shell.log" 2>&1 &
    shell_pid=$!
    wait_for 60 eval_js 1 || { report FAIL "GNOME Shell did not start"; tail -20 "$XDG_RUNTIME_DIR/shell.log"; exit 1; }
}
stop_shell() { kill "$shell_pid"; wait "$shell_pid" 2>/dev/null; }
trap 'kill $(jobs -p) 2>/dev/null || true' EXIT

active() { eval_js 'global.display.focus_window?.get_title() ?? "none"' | sed -n "s/^(true, '\"\(.*\)\"')$/\1/p"; }
expect_active() {
    sleep 0.7
    local got; got=$(active)
    if [[ "$got" == "$2" ]]; then report ok "$1"; else report FAIL "$1: active is '$got', expected '$2'"; fi
}
window() { foot --app-id "$1" --title "$2" sleep 600 >/dev/null 2>&1 & sleep 1.5; }

# --- one-time setup
start_shell
out=$("$bin" check 2>&1)
[[ "$out" == *"Backend: Gnome"* ]] && report ok "detects GNOME Wayland" || report FAIL "backend: $out"
[[ "$out" == *"Log out and back in"* ]] && report ok "explains the one-time re-login" || report FAIL "no setup hint: $out"
ext="$HOME/.local/share/gnome-shell/extensions/appkey@carevski.github.io"
[[ -f "$ext/extension.js" ]] && report ok "installs its GNOME extension" || report FAIL "extension not installed"
[[ "$(gsettings get org.gnome.shell enabled-extensions)" == *appkey@carevski.github.io* ]] \
    && report ok "enables the extension" || report FAIL "not enabled: $(gsettings get org.gnome.shell enabled-extensions)"

mkdir -p "$HOME/.config/appkey"
cfg="$HOME/.config/appkey/config.toml"
printf '[[shortcut]]\nkeys = "ctrl+alt+b"\napp = "beta"\n\n[[shortcut]]\nkeys = "ctrl+alt+g"\napp = "gamma"\nlaunch = ["foot", "--app-id", "gamma", "--title", "gamma", "sleep", "600"]\n' >"$cfg"
chmod 600 "$cfg"
"$bin" run >"$XDG_RUNTIME_DIR/daemon.log" 2>&1 &
daemon=$!
wait_for 10 grep -q 'shortcuts unavailable' "$XDG_RUNTIME_DIR/daemon.log"
if kill -0 "$daemon" 2>/dev/null && grep -q 'Log out and back in' "$XDG_RUNTIME_DIR/daemon.log"; then
    report ok "daemon keeps running and explains the setup step"
else
    report FAIL "daemon before re-login: $(cat "$XDG_RUNTIME_DIR/daemon.log")"
fi

stop_shell   # "log out"
start_shell  # "log in"
ready() { "$bin" check 2>&1 | grep -q 'NOT READY' && return 1 || return 0; }
wait_for 10 ready && report ok "extension active after re-login" || report FAIL "still not ready: $("$bin" check 2>&1 | tail -2)"
wait_for 15 grep -q '2 shortcut(s) active' "$XDG_RUNTIME_DIR/daemon.log" \
    && report ok "running daemon picks up shortcuts once the helper is there" \
    || { report FAIL "daemon did not recover"; cat "$XDG_RUNTIME_DIR/daemon.log"; }

# --- focusing
window alpha alpha-1
window alpha alpha-2
window beta beta
"$bin" open alpha >/dev/null; expect_active "focuses most recent of several windows" alpha-2
"$bin" open beta >/dev/null;  expect_active "switches to another app" beta
"$bin" open alpha >/dev/null; expect_active "follows most-recently-used order" alpha-2
eval_js 'global.display.get_tab_list(0, null).find(w => w.get_title() === "alpha-1").activate(global.get_current_time())' >/dev/null
sleep 0.5; "$bin" open beta >/dev/null; "$bin" open alpha >/dev/null
expect_active "MRU tracks focus changes made outside appkey" alpha-1
eval_js 'global.display.get_tab_list(0, null).find(w => w.get_title() === "beta").minimize()' >/dev/null
"$bin" open beta >/dev/null; expect_active "restores a minimized window" beta

# --- shortcuts: real key events through Mutter's RemoteDesktop API
press() { python3 "$(dirname "$self")/mutter-keys.py" "ctrl+alt+$1"; }
"$bin" open alpha >/dev/null; sleep 0.5
press b; expect_active "Ctrl+Alt+B focuses its app" beta
press g; expect_active "Ctrl+Alt+G launches an app that isn't running" gamma

# Changing a shortcut releases the old keys.
sed -i 's/ctrl+alt+b/ctrl+alt+n/' "$cfg"
wait_for 10 bash -c "[[ \$(grep -c 'shortcut(s) active' '$XDG_RUNTIME_DIR/daemon.log') -ge 2 ]]"
"$bin" open alpha >/dev/null; sleep 0.5
press b; expect_active "old shortcut no longer fires after a change" alpha-1
press n; expect_active "changed shortcut works" beta

exit $fail

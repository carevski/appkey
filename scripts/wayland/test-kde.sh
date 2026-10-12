#!/usr/bin/env bash
# End-to-end test of the KDE Plasma (Wayland) backend in a headless KWin.
# Run inside scripts/wayland/Containerfile.kde, under dbus-run-session:
#   dbus-run-session scripts/wayland/test-kde.sh path/to/appkey
# Checks focusing (most recently used), the portal approval dialog, shortcuts
# firing (via kglobalaccel, as a key press would), and that a changed shortcut
# doesn't leave the old keys grabbed.
set -uo pipefail
bin="$(realpath "${1:?usage: $0 path/to/appkey}")"
here="$(dirname "$(realpath "$0")")"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-$(mktemp -d)}"; chmod 700 "$XDG_RUNTIME_DIR"
export HOME="$(mktemp -d)" XDG_SESSION_TYPE=wayland XDG_CURRENT_DESKTOP=KDE
export QT_FORCE_STDERR_LOGGING=1 QT_LOGGING_RULES="*.debug=false;js.debug=true"

fail=0
report() {
    echo "$1 ${*:2}"
    if [[ "$1" == FAIL ]]; then
        fail=1
        [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::error title=KDE Wayland e2e::${*:2}"
    fi
}
wait_for() {
    local deadline=$((SECONDS + $1)); shift
    until "$@" >/dev/null 2>&1; do ((SECONDS >= deadline)) && return 1; sleep 0.2; done
}
call() { gdbus call --session -d "$1" -o "$2" -m "$3" "${@:4}"; }

# kwin_wayland carries file capabilities that rootless containers refuse to exec.
cp "$(command -v kwin_wayland)" "$XDG_RUNTIME_DIR/kwin_wayland"
"$XDG_RUNTIME_DIR/kwin_wayland" --virtual --no-lockscreen --width 1280 --height 800 >"$XDG_RUNTIME_DIR/kwin.log" 2>&1 &
trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
wait_for 15 test -S "$XDG_RUNTIME_DIR/wayland-0" || { report FAIL "KWin did not start"; exit 1; }
export WAYLAND_DISPLAY=wayland-0 QT_QPA_PLATFORM=wayland QT_ACCESSIBILITY=1 QT_LINUX_ACCESSIBILITY_ALWAYS_ON=1
dbus-update-activation-environment WAYLAND_DISPLAY XDG_CURRENT_DESKTOP XDG_SESSION_TYPE QT_QPA_PLATFORM \
    XDG_RUNTIME_DIR QT_ACCESSIBILITY QT_LINUX_ACCESSIBILITY_ALWAYS_ON HOME
wait_for 15 call org.kde.KWin /Scripting org.freedesktop.DBus.Peer.Ping || { report FAIL "KWin D-Bus not up"; exit 1; }

# Independent check of the focused window (not using appkey's own code).
probe=0
active() {
    probe=$((probe + 1))
    printf 'print("ACTIVE=" + (workspace.activeWindow ? workspace.activeWindow.caption : "none"));' >"$XDG_RUNTIME_DIR/p$probe.js"
    local id; id=$(call org.kde.KWin /Scripting org.kde.kwin.Scripting.loadScript "$XDG_RUNTIME_DIR/p$probe.js" "p$probe" | tr -dc 0-9)
    call org.kde.KWin "/Scripting/Script$id" org.kde.kwin.Script.run >/dev/null
    sleep 0.3
    call org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript "p$probe" >/dev/null
    grep -o 'ACTIVE=.*' "$XDG_RUNTIME_DIR/kwin.log" | tail -1 | cut -d= -f2 | tr -d '"'
}
expect_active() {
    sleep 0.5
    local got; got=$(active)
    if [[ "$got" == "$2" ]]; then report ok "$1"; else report FAIL "$1: active is '$got', expected '$2'"; fi
}
window() { foot --app-id "$1" --title "$2" sleep 600 >/dev/null 2>&1 & sleep 1; }

window alpha alpha-1
window alpha alpha-2
window beta beta
sleep 1

"$bin" check 2>&1 | grep -q 'Backend: Kde' && report ok "detects KDE Wayland" || report FAIL "backend not detected: $("$bin" check 2>&1 | tail -2)"
"$bin" open alpha >/dev/null; expect_active "focuses most recent of several windows" alpha-2
"$bin" open beta >/dev/null;  expect_active "switches to another app" beta
"$bin" open alpha >/dev/null; expect_active "follows most-recently-used order" alpha-2

# --- shortcuts through the portal
mkdir -p "$HOME/.config/appkey"
cfg="$HOME/.config/appkey/config.toml"
printf '[[shortcut]]\nkeys = "ctrl+alt+b"\napp = "beta"\n\n[[shortcut]]\nkeys = "ctrl+alt+g"\napp = "gamma"\nlaunch = ["foot", "--app-id", "gamma", "--title", "gamma", "sleep", "600"]\n' >"$cfg"
chmod 600 "$cfg"
"$bin" run >"$XDG_RUNTIME_DIR/daemon.log" 2>&1 &
dialog() { python3 "$here/a11y.py" dump 2>/dev/null | grep -q "Global Shortcuts Requested"; }
if wait_for 20 dialog; then report ok "KDE asks the user to approve the shortcuts"
else report FAIL "no approval dialog"; cat "$XDG_RUNTIME_DIR/daemon.log"; fi
python3 "$here/a11y.py" click OK
active_count() { grep -q "$1 shortcut(s) active" "$XDG_RUNTIME_DIR/daemon.log"; }
wait_for 10 active_count 2 && report ok "shortcuts active after approval" || { report FAIL "not active"; cat "$XDG_RUNTIME_DIR/daemon.log"; }

comp=/component/appkey
id_of() { "$bin" __hotkey-id "$1"; }
press() { call org.kde.kglobalaccel $comp org.kde.kglobalaccel.Component.invokeShortcut "$(id_of "$1")" >/dev/null; }
"$bin" open alpha >/dev/null; sleep 0.5
press ctrl+alt+b; expect_active "shortcut focuses its app" beta
press ctrl+alt+g; expect_active "shortcut launches an app that isn't running" gamma

# --- change a shortcut: the old keys must be released
sed -i 's/ctrl+alt+b/ctrl+alt+n/' "$cfg"
wait_for 20 dialog && python3 "$here/a11y.py" click OK
wait_for 10 grep -c 'shortcut(s) active' "$XDG_RUNTIME_DIR/daemon.log"
sleep 2
names=$(call org.kde.kglobalaccel $comp org.kde.kglobalaccel.Component.shortcutNames)
if [[ "$names" == *"$(id_of ctrl+alt+n)"* && "$names" != *"$(id_of ctrl+alt+b)"* ]]; then
    report ok "changed shortcut replaces the old one in KDE"
else
    report FAIL "KDE shortcuts after change: $names"
fi
"$bin" open alpha >/dev/null; sleep 0.5
press ctrl+alt+n; expect_active "changed shortcut works" beta

exit $fail

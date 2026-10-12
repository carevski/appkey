#!/usr/bin/env bash
# End-to-end test of the Linux/X11 backend in a virtual desktop (Xvfb + Openbox).
# Checks: focus by WM_CLASS, most-recently-used choice, un-minimize, launch, no duplicates.
# Needs: xvfb openbox xterm xdotool.  Usage: scripts/test-x11.sh path/to/appkey
set -uo pipefail
bin="$(realpath "${1:?usage: $0 path/to/appkey}")"
XDG_CONFIG_HOME="$(mktemp -d)"
export DISPLAY=:99 XDG_SESSION_TYPE=x11 XDG_CONFIG_HOME
unset WAYLAND_DISPLAY

fail=0
report() { # report <ok|FAIL> <message>; failures also become GitHub annotations
    echo "$1 ${*:2}"
    if [[ "$1" == FAIL ]]; then
        fail=1
        [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::error title=X11 e2e::${*:2}"
    fi
}
# wait_for <seconds> <command...>: retry until the command succeeds.
wait_for() {
    local deadline=$((SECONDS + $1)); shift
    until "$@" >/dev/null 2>&1; do
        ((SECONDS >= deadline)) && return 1
        sleep 0.1
    done
}
active() { xdotool getactivewindow getwindowname 2>/dev/null; }
is_active() { [[ "$(active)" == "$1" ]]; }
window() { xdotool search --name "^$1\$" | head -1; }
managed() { local id; id=$(window "$1") && [[ -n "$id" ]] && xprop -root _NET_CLIENT_LIST | grep -qi "$(printf '0x%x' "$id")"; }

Xvfb :99 -screen 0 1024x768x24 >/dev/null 2>&1 &
trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
wait_for 10 xdotool getmouselocation || { report FAIL "Xvfb did not start"; exit 1; }
openbox >/dev/null 2>&1 &
# xprop exits 0 even when the property is missing, so look for the value.
wm_ready() { xprop -root _NET_SUPPORTING_WM_CHECK | grep -q 'window id'; }
wait_for 10 wm_ready || { report FAIL "openbox did not start"; exit 1; }

start() { # start <instance> <class> <title>
    # Run `sleep`, not a shell: shells often rewrite the title from their prompt.
    xterm -name "$1" -class "$2" -T "$3" -e sleep 600 >/dev/null 2>&1 &
    # Wait until the window manager has it in its client list (= managed, mapped).
    wait_for 15 managed "$3" || report FAIL "window $3 did not appear"
}
start alpha Alpha alpha-1
start alpha Alpha alpha-2
start beta Beta beta

check() { # check <description> <expected active window>
    if wait_for 5 is_active "$2"; then report ok "$1"
    else report FAIL "$1: active window is '$(active)', expected '$2'"; fi
}

"$bin" open alpha;  check "focuses most recent of several windows" alpha-2
xdotool windowactivate --sync "$(window alpha-1)"
wait_for 5 is_active alpha-1
"$bin" open beta;   check "switches to another app" beta
"$bin" open alpha;  check "follows most-recently-used order" alpha-1
xdotool windowminimize --sync "$(window beta)"
"$bin" open beta;   check "restores a minimized window" beta

before=$(pgrep -c xterm)
"$bin" open xterm   # no window has instance/class "xterm" yet → launches one
wait_for 15 is_active xterm || true
"$bin" open xterm   # now it exists → focuses, no second copy
sleep 1
after=$(pgrep -c xterm)
if ((after == before + 1)); then report ok "launches once, then focuses instead of duplicating"
else report FAIL "launch: $before → $after xterm processes (expected exactly one more)"; fi

exit $fail

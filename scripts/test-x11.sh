#!/usr/bin/env bash
# End-to-end test of the Linux/X11 backend in a virtual desktop (Xvfb + Openbox).
# Checks: focus by WM_CLASS, most-recently-used choice, un-minimize, launch, no duplicates.
# Needs: xvfb openbox xterm xdotool.  Usage: scripts/test-x11.sh path/to/appkey
set -euo pipefail
bin="$(realpath "${1:?usage: $0 path/to/appkey}")"
XDG_CONFIG_HOME="$(mktemp -d)"
export DISPLAY=:99 XDG_SESSION_TYPE=x11 XDG_CONFIG_HOME
unset WAYLAND_DISPLAY

Xvfb :99 -screen 0 1024x768x24 >/dev/null 2>&1 &
trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
sleep 1
openbox >/dev/null 2>&1 &
sleep 1

fail=0
active() { xdotool getactivewindow getwindowname; }
expect() {
    sleep 0.5
    local got; got=$(active)
    if [[ "$got" == "$2" ]]; then echo "ok   $1"; else echo "FAIL $1: active=$got, expected $2"; fail=1; fi
}

xterm -name alpha -class Alpha -T alpha-1 & sleep 1
xterm -name alpha -class Alpha -T alpha-2 & sleep 1
xterm -name beta -class Beta -T beta & sleep 1

"$bin" open alpha;                                         expect "focuses most recent of several windows" alpha-2
xdotool search --name '^alpha-1$' windowactivate --sync; sleep 0.5
"$bin" open beta;                                          expect "switches to another app" beta
"$bin" open alpha;                                         expect "follows most-recently-used order" alpha-1
xdotool search --name '^beta$' windowminimize; sleep 0.5
"$bin" open beta;                                          expect "restores a minimized window" beta

before=$(pgrep -c xterm)
"$bin" open xterm >/dev/null || true   # no window has instance/class "xterm" yet → launches
sleep 1
"$bin" open xterm; sleep 1
after=$(pgrep -c xterm)
if (( after == before + 1 )); then echo "ok   launches once, then focuses instead of duplicating"
else echo "FAIL launch: $before → $after xterm processes (expected +1)"; fail=1; fi

exit $fail

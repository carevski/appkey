#!/usr/bin/env bash
# End-to-end test of the settings window on a virtual X11 desktop:
# record a shortcut by pressing it → Save → the running daemon reloads →
# pressing the new shortcut launches the app. Also checks that entries for
# other OSes survive a save.
# Needs: xvfb openbox xterm xdotool x11-utils fonts-dejavu-core fontconfig, Mesa GL
# (libgl1-mesa-dri libegl1) and libxkbcommon-x11-0 libxcursor1 libxrandr2 libxi6.
# Usage: scripts/test-settings-ui.sh path/to/appkey
set -uo pipefail
bin="$(realpath "${1:?usage: $0 path/to/appkey}")"
XDG_CONFIG_HOME="$(mktemp -d)"
export DISPLAY=:98 XDG_SESSION_TYPE=x11 XDG_CONFIG_HOME
unset WAYLAND_DISPLAY
cfg="$XDG_CONFIG_HOME/appkey/config.toml"

fail=0
report() {
    echo "$1 ${*:2}"
    if [[ "$1" == FAIL ]]; then
        fail=1
        [[ -n "${GITHUB_ACTIONS:-}" ]] && echo "::error title=Settings UI e2e::${*:2}"
    fi
}
wait_for() {
    local deadline=$((SECONDS + $1)); shift
    until "$@" >/dev/null 2>&1; do
        ((SECONDS >= deadline)) && return 1
        sleep 0.1
    done
}

Xvfb :98 -screen 0 1280x800x24 >/dev/null 2>&1 &
trap 'kill $(jobs -p) 2>/dev/null || true' EXIT
wait_for 10 xdotool getmouselocation || { report FAIL "Xvfb did not start"; exit 1; }
openbox >/dev/null 2>&1 &
wm_ready() { xprop -root _NET_SUPPORTING_WM_CHECK | grep -q 'window id'; }
wait_for 10 wm_ready || { report FAIL "openbox did not start"; exit 1; }

mkdir -p "$(dirname "$cfg")"
cat >"$cfg" <<'EOF'
[[shortcut]]
keys = "ctrl+alt+t"
app = "Terminal"
os = ["macos"]

[[shortcut]]
keys = "ctrl+alt+t"
app = "xterm"
EOF
chmod 600 "$cfg"

"$bin" run >/tmp/appkey-daemon.log 2>&1 &
wait_for 10 grep -q 'shortcut(s) active' /tmp/appkey-daemon.log || report FAIL "daemon did not start"

"$bin" settings >/tmp/appkey-settings.log 2>&1 &
settings_window() { xdotool search --name '^appkey settings$'; }
wait_for 15 settings_window || { report FAIL "settings window did not open"; cat /tmp/appkey-settings.log; exit 1; }
win=$(settings_window | head -1)
geometry() { xdotool getwindowgeometry --shell "$win"; }
eval "$(geometry)" # WIDTH, HEIGHT
sized() { eval "$(geometry)"; ((WIDTH >= 500 && HEIGHT >= 300)); }
wait_for 10 sized || report FAIL "settings window has unexpected size ${WIDTH}x${HEIGHT}"
sleep 2 # first frames (software GL on CI is slow)

# The first row's shortcut button sits near the top-left; Save is anchored bottom-right.
record_and_save() {
    xdotool windowactivate --sync "$win"
    xdotool mousemove --window "$win" 100 102 click 1
    sleep 0.7
    xdotool key ctrl+shift+y
    sleep 0.7
    xdotool mousemove --window "$win" $((WIDTH - 26)) $((HEIGHT - 20)) click 1
    wait_for 3 grep -q 'keys = "ctrl+shift+y"' "$cfg"
}
saved=false
for _ in 1 2 3; do
    if record_and_save; then saved=true; break; fi
done
if $saved; then report ok "records a shortcut and saves it"
else report FAIL "config not updated (window ${WIDTH}x${HEIGHT}): $(tr '\n' ' ' <"$cfg")"; fi
if grep -q 'app = "Terminal"' "$cfg"; then report ok "keeps other-OS entries"
else report FAIL "macOS entry was lost on save"; fi
if [[ "$(stat -c %a "$cfg")" == 600 ]]; then report ok "saved file is private (600)"
else report FAIL "saved file mode is $(stat -c %a "$cfg")"; fi

reloaded() { (($(grep -c 'shortcut(s) active' /tmp/appkey-daemon.log) >= 2)); }
if wait_for 5 reloaded; then report ok "daemon reloads after save"
else report FAIL "daemon did not reload"; cat /tmp/appkey-daemon.log; fi

before=$(pgrep -c xterm || true)
xdotool key ctrl+shift+y
xterm_started() { (($(pgrep -c xterm || true) > before)); }
if wait_for 10 xterm_started; then report ok "new shortcut launches the app"
else report FAIL "pressing the new shortcut did nothing"; cat /tmp/appkey-daemon.log; fi

exit $fail

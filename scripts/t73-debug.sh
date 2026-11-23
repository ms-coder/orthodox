#!/usr/bin/env bash
# T7.3 debug: run ofm under Xvfb, send keys, capture screenshot + app output.
# Usage: scripts/t73-debug.sh "alt+F7" "Return"
set -uo pipefail
cd "$(dirname "$0")/.."

iso="$(mktemp -d)"
L="$iso/left"; R="$iso/right"
mkdir -p "$L/Documents" "$L/src" "$R"
for f in README.md main.rs notes.txt; do head -c 5000 /dev/zero | tr '\0' 'x' > "$L/$f"; done
head -c 3000 /dev/zero > "$R/data.bin"

export XDG_CONFIG_HOME="$iso/config" XDG_STATE_HOME="$iso/state" XDG_DATA_HOME="$iso/data"
mkdir -p "$XDG_CONFIG_HOME/ofm" "$XDG_STATE_HOME" "$XDG_DATA_HOME"
export GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1 RUST_LOG="ofm=debug"
export OFM_SHOT_OUT="$PWD/target/screenshots/t73-debug.jpg"
export T73_KEYS="$*"
export T73_LEFT="$L" T73_RIGHT="$R"

xvfb-run -a -s "-screen 0 1600x1000x24" bash -s <<'EOF'
"$PWD/target/debug/ofm" "$T73_LEFT" "$T73_RIGHT" > /tmp/t73-ofm.log 2>&1 &
pid=$!
sleep 3
for k in $T73_KEYS; do
  wid=$(xdotool search --onlyvisible --class ofm | tail -1)
  xdotool windowfocus "$wid" 2>/dev/null
  python3 -I -c '
import ctypes, sys
x = ctypes.CDLL("libX11.so.6"); t = ctypes.CDLL("libXtst.so.6")
x.XOpenDisplay.restype = ctypes.c_void_p
x.XStringToKeysym.restype = ctypes.c_ulong
x.XKeysymToKeycode.restype = ctypes.c_ubyte
x.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
t.XTestFakeKeyEvent.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_int, ctypes.c_ulong]
d = x.XOpenDisplay(None)
M = {"ctrl": "Control_L", "alt": "Alt_L", "shift": "Shift_L"}
chord = sys.argv[1]
codes = [x.XKeysymToKeycode(d, x.XStringToKeysym(M.get(p.lower(), p).encode())) for p in chord.split("+")]
for c in codes: t.XTestFakeKeyEvent(d, c, 1, 0)
for c in reversed(codes): t.XTestFakeKeyEvent(d, c, 0, 0)
x.XFlush(d); x.XSync(d, 0)
' "$k"
  sleep 1
done
sleep 2
{
  echo "windows: $(xdotool search --onlyvisible --class ofm 2>/dev/null | tr '\n' ' ')"
  kill -0 "$pid" 2>/dev/null && echo "alive: yes" || echo "alive: no"
} > /tmp/t73-state.log
import -window root "$OFM_SHOT_OUT"
kill "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
EOF

cat /tmp/t73-state.log 2>/dev/null
tail -30 /tmp/t73-ofm.log 2>/dev/null

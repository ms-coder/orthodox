#!/usr/bin/env bash
# Run OFM under Xvfb and save a screenshot, so agents can look at the UI.
# Usage: scripts/screenshot.sh NAME [-- ofm-args...]
# Output: target/screenshots/NAME.png   (open it with the Read tool)
# Env:    OFM_SCREENSHOT_DELAY  seconds to wait before capturing (default 3)
set -euo pipefail
cd "$(dirname "$0")/.."

name="${1:?usage: scripts/screenshot.sh NAME [-- ofm-args...]}"
shift
if [[ "${1:-}" == "--" ]]; then shift; fi

mkdir -p target/screenshots
export OFM_SHOT_OUT="$PWD/target/screenshots/$name.png"
export OFM_SHOT_DELAY="${OFM_SCREENSHOT_DELAY:-3}"

cargo build -p ofm-app --quiet
export OFM_SHOT_BIN="$PWD/target/debug/ofm"

export GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1
xvfb-run -a -s "-screen 0 1600x1000x24" bash -c '
  "$OFM_SHOT_BIN" "$@" &
  pid=$!
  sleep "$OFM_SHOT_DELAY"
  import -window root "$OFM_SHOT_OUT"
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
' ofm-shot "$@"

echo "$OFM_SHOT_OUT"

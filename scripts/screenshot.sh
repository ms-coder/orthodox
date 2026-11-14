#!/usr/bin/env bash
# Run OFM under Xvfb and save a screenshot, so agents can look at the UI.
# Usage: scripts/screenshot.sh NAME [-- ofm-args...]
# Output: target/screenshots/NAME.png   (open it with the Read tool)
# Env:    OFM_SCREENSHOT_DELAY  seconds to wait before capturing (default 3)
#         OFM_SHOT_THEME        theme id written to a temp config (far-blue | commander-light | adwaita)
#         OFM_SHOT_VIEW         panel view written to a temp config (brief | full | custom)
#         OFM_SHOT_FIXTURE=1    when no ofm-args are given, open two generated fixture directories
#         Any of the three also isolates XDG_CONFIG/STATE/DATA_HOME in a temp dir (never touches $HOME).
# Set:    scripts/screenshot.sh g1   -> target/screenshots/g1-{far,light,adwaita}-{full,brief}.png
set -euo pipefail
cd "$(dirname "$0")/.."

name="${1:?usage: scripts/screenshot.sh NAME [-- ofm-args...]}"
shift

if [[ "$name" == "g1" ]]; then
  for t in far:far-blue light:commander-light adwaita:adwaita; do
    for v in full brief; do
      OFM_SHOT_THEME="${t#*:}" OFM_SHOT_VIEW="$v" OFM_SHOT_FIXTURE=1 "$0" "g1-${t%%:*}-$v"
    done
  done
  exit 0
fi
if [[ "${1:-}" == "--" ]]; then shift; fi

mkdir -p target/screenshots
export OFM_SHOT_OUT="$PWD/target/screenshots/$name.png"
export OFM_SHOT_DELAY="${OFM_SCREENSHOT_DELAY:-3}"

if [[ -n "${OFM_SHOT_THEME:-}${OFM_SHOT_VIEW:-}${OFM_SHOT_FIXTURE:-}" ]]; then
  iso="$(mktemp -d)"
  trap 'rm -rf "$iso"' EXIT
  export XDG_CONFIG_HOME="$iso/config" XDG_STATE_HOME="$iso/state" XDG_DATA_HOME="$iso/data"
  mkdir -p "$XDG_CONFIG_HOME/ofm" "$XDG_STATE_HOME" "$XDG_DATA_HOME"
  {
    [[ -n "${OFM_SHOT_THEME:-}" ]] && printf '[theme]\nname = "%s"\n' "$OFM_SHOT_THEME"
    [[ -n "${OFM_SHOT_VIEW:-}" ]] && printf '[panels]\ndefault_view = "%s"\n' "$OFM_SHOT_VIEW"
    true
  } > "$XDG_CONFIG_HOME/ofm/config.toml"
  if [[ "${OFM_SHOT_FIXTURE:-}" == "1" && $# -eq 0 ]]; then
    L="$iso/left"; R="$iso/right"
    mkdir -p "$L/Documents" "$L/src" "$L/.cache" "$L/a directory with a rather long name" "$R/build" "$R/data"
    for f in README.md main.rs Cargo.toml notes.txt archive.tar.gz photo.jpg "very_long_file_name_that_will_not_fit_in_a_brief_column.txt" .hidden; do
      head -c $((RANDOM % 90000 + 10)) /dev/zero | tr '\0' 'x' > "$L/$f"
    done
    ln -s README.md "$L/link-to-readme"
    chmod +x "$L/main.rs"
    for i in $(seq 1 40); do head -c $((i * 3000)) /dev/zero > "$R/file_$i.dat"; touch -d "2026-0$((i % 9 + 1))-1$((i % 9)) 1$((i % 10)):30" "$R/file_$i.dat"; done
    set -- "$L" "$R"
  fi
fi

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

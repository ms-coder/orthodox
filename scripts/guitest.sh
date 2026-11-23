#!/usr/bin/env bash
# Run ONE ofm-app GUI test target under Xvfb, single-threaded (agents' fast loop;
# the full serialised run is scripts/check.sh). Usage: scripts/guitest.sh <target> [args...]
set -euo pipefail
cd "$(dirname "$0")/.."

target="${1:?usage: scripts/guitest.sh <test-target> [args...]}"
shift

GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1 \
  xvfb-run -a -s "-screen 0 1600x1000x24" \
  cargo test -p ofm-app --test "$target" -- --test-threads=1 "$@"

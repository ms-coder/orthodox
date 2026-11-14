#!/usr/bin/env bash
# The single "is everything green?" command. Must pass before every commit and after every merge.
# Set OFM_SKIP_UI=1 only on machines without Xvfb (never in agent runs).
set -euo pipefail
cd "$(dirname "$0")/.."

echo "== cargo fmt"
cargo fmt --all -- --check

echo "== cargo clippy"
cargo clippy --workspace --all-targets -- -D warnings

echo "== tests (all crates except ofm-app)"
cargo test --workspace --exclude ofm-app

echo "== tests (ofm-app, under Xvfb)"
if [[ "${OFM_SKIP_UI:-0}" == "1" ]]; then
  echo "OFM_SKIP_UI=1 -> skipping UI tests"
elif command -v xvfb-run >/dev/null 2>&1; then
  # GTK must be initialised on one thread only -> run UI tests single-threaded.
  GDK_BACKEND=x11 GSK_RENDERER=cairo NO_AT_BRIDGE=1 \
    xvfb-run -a -s "-screen 0 1600x1000x24" \
    cargo test -p ofm-app -- --test-threads=1
else
  echo "xvfb-run not found: install it (scripts/bootstrap-ubuntu.sh) or set OFM_SKIP_UI=1" >&2
  exit 1
fi

echo "== all checks passed"

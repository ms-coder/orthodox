#!/usr/bin/env bash
# Run ./scripts/check.sh under a machine-wide lock so concurrent agents never run the
# (load-sensitive) real-key e2e suite at the same time. Use this instead of check.sh in parallel work.
set -euo pipefail
cd "$(dirname "$0")/.."
exec flock /tmp/ofm-check.lock ./scripts/check.sh "$@"

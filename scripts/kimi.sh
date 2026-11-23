#!/usr/bin/env bash
# Run a headless Claude Code session on Kimi Code (K2.8 Preview = model id kimi-for-coding).
# Usage: KIMI_API_KEY=... ./scripts/kimi.sh <dir> "<prompt>"   (dir = a git worktree for the task)
set -euo pipefail
dir=${1:?worktree dir}; prompt=${2:?prompt}
: "${KIMI_API_KEY:?set KIMI_API_KEY (Kimi Code key)}"
cd "$dir"
# Claude Code does not know these model ids and assumes a 1M window: tell it the real one so it
# compacts before overflowing (k3-256k = 262144 tokens).
[ "${KIMI_MODEL:-k3-256k}" = "k3-256k" ] && export CLAUDE_CODE_AUTO_COMPACT_WINDOW=${CLAUDE_CODE_AUTO_COMPACT_WINDOW:-200000}
ANTHROPIC_BASE_URL=https://api.kimi.com/coding/ \
ANTHROPIC_AUTH_TOKEN="$KIMI_API_KEY" \
ANTHROPIC_DEFAULT_SONNET_MODEL="${KIMI_MODEL:-k3-256k}" ANTHROPIC_DEFAULT_OPUS_MODEL="${KIMI_MODEL:-k3-256k}" ANTHROPIC_DEFAULT_HAIKU_MODEL="${KIMI_MODEL:-k3-256k}" \
ANTHROPIC_MODEL="${KIMI_MODEL:-k3-256k}" \
  claude -p "$prompt" --permission-mode acceptEdits --max-turns "${MAX_TURNS:-150}"

#!/usr/bin/env bash
# Run a headless Claude Code session on GLM (Z.ai Coding Plan) so work uses the GLM quota.
# Usage: ZAI_API_KEY=... ./scripts/glm.sh <dir> "<prompt>"   (dir = a git worktree for the task)
set -euo pipefail
dir=${1:?worktree dir}; prompt=${2:?prompt}
: "${ZAI_API_KEY:?set ZAI_API_KEY (Z.ai API key)}"
cd "$dir"
ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic \
ANTHROPIC_AUTH_TOKEN="$ZAI_API_KEY" \
ANTHROPIC_DEFAULT_SONNET_MODEL=glm-5.2 ANTHROPIC_DEFAULT_OPUS_MODEL=glm-5.2 ANTHROPIC_DEFAULT_HAIKU_MODEL=glm-5.2 \
  claude -p "$prompt" --permission-mode acceptEdits --max-turns 60

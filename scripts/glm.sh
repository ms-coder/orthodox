#!/usr/bin/env bash
# Run a headless Claude Code session on GLM (Z.ai Coding Plan) so work uses the GLM quota.
# Usage: ZAI_API_KEY=... ./scripts/glm.sh <dir> "<prompt>"   (dir = a git worktree for the task)
set -euo pipefail
dir=${1:?worktree dir}; prompt=${2:?prompt}
: "${ZAI_API_KEY:?set ZAI_API_KEY (Z.ai API key)}"
cd "$dir"
# In Claude Code the strong model is selected as `glm-5.4[1m]` (the suffix declares the 1M context window).
CC_MODEL="${GLM_MODEL:-glm-5.4}"; [ "$CC_MODEL" = "glm-5.4" ] && CC_MODEL="glm-5.4[1m]"
# Igor's rule: only glm-5.4 (strong) or glm-5.3-flash (small); never glm-5.3 or older.
case "${GLM_MODEL:-glm-5.4}" in glm-5.4|glm-5.3-flash) ;; *) echo "glm.sh: refusing model ${GLM_MODEL} (allowed: glm-5.4, glm-5.3-flash)" >&2; exit 2 ;; esac
ANTHROPIC_BASE_URL=https://api.z.ai/api/anthropic \
ANTHROPIC_AUTH_TOKEN="$ZAI_API_KEY" \
ANTHROPIC_DEFAULT_SONNET_MODEL="$CC_MODEL" ANTHROPIC_DEFAULT_OPUS_MODEL="$CC_MODEL" ANTHROPIC_DEFAULT_HAIKU_MODEL="$CC_MODEL" \
  claude -p "$prompt" --permission-mode acceptEdits --max-turns "${MAX_TURNS:-150}"

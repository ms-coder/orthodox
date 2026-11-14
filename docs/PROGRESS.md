# OFM — Progress

The lead keeps this file current after every task. A fresh session must be able to resume from it.

## Current state

- Milestone: M1 — in progress
- Waiting for: parallel group 1.A (T1.1, T1.4 on GLM via scripts/glm.sh in worktrees ../orthodox-wt-T1.1, ../orthodox-wt-T1.4; T1.2, T1.3 on Claude core-engineer agents)
- Next step: merge each branch with --no-ff, check.sh, then T1.5 review (reviewer), tag m1-done

## Milestones

| M | Name | Status | Tag | Notes |
|---|---|---|---|---|
| M0 | Bootstrap | done | m0-done | |
| M1 | Core foundations | in progress | | |
| M2 | UI shell (G1) | not started | | |
| M3 | File operations (G2) | not started | | |
| M4 | Command line & terminal | not started | | |
| M5 | Viewer & quick view | not started | | |
| M6 | Archives | not started | | |
| M7 | Search & panelize | not started | | |
| M8 | Power features & desktop (G3) | not started | | |
| M9 | Hardening | not started | | |
| M10 | Release (G4) | not started | | |

Status values: `not started` · `in progress` · `review` · `waiting for Igor` · `done`

## Tasks

| Task | Agent | Status | Branch | Commit | Notes |
|---|---|---|---|---|---|
| T1.0 | architect | done | main | 9bc5168 | stubs, ADR-0002..0005, COMMANDS.md (128 commands) |
| T1.1 | glm | in progress | m1/T1.1-panel-model | | |
| T1.2 | core-engineer | in progress | m1/T1.2-local-vfs | | |
| T1.3 | core-engineer | in progress | m1/T1.3-config | | also owns ofm-core/src/keychord.rs |
| T1.4 | glm | in progress | m1/T1.4-testkit | | |
| T0.2 | glm | done | m0/T0.2-dry-run | merged+reverted | Dry run done via scripts/glm.sh (GLM works, ~1 min). Claude agents not dry-run to save quota |
| T0.1 | lead | done | main | see git log | check.sh green; screenshot m0 OK; app ID io.github.mscoder.Ofm; gtk4 0.11 / libadwaita 0.9 |

(The lead adds a row when a task starts.)

## Gate feedback log

| Gate | Date | Igor's feedback | Resulting tasks |
|---|---|---|---|

## Decisions and deviations

| Date | What | Why | ADR |
|---|---|---|---|
| 2026-11-14 | All agents (architect, reviewer, fileops) on Sonnet, not Opus | Igor is on Pro plan with limited weekly quota | - |
| 2026-11-14 | GLM offload via `scripts/glm.sh` for tests, packaging, docs, ofm-viewer, ofm-search | Separate Z.ai Coding Plan quota | - |

# OFM — Progress

The lead keeps this file current after every task. A fresh session must be able to resume from it.

## Current state

- Milestone: M1 — done (tag m1-done); M2 (UI shell, gate G1) not started
- Waiting for: nothing — next is M2 per docs/PLAN.md (T2.0 architect stubs first)
- Next step: read M2 in docs/PLAN.md, commit interface stubs to main, then spawn ui-engineer tasks. Handle the "Deferred review findings" assigned to M2.

## Milestones

| M | Name | Status | Tag | Notes |
|---|---|---|---|---|
| M0 | Bootstrap | done | m0-done | |
| M1 | Core foundations | done | m1-done | |
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
| T1.5 | reviewer | done | m1/review-fixes | 555f57b | findings fixed; rest deferred below |
| T1.0 | architect | done | main | 9bc5168 | stubs, ADR-0002..0005, COMMANDS.md (128 commands) |
| T1.1 | glm | done | m1/T1.1-panel-model | a6f322a | |
| T1.2 | core-engineer | done | m1/T1.2-local-vfs | e7a7785 | |
| T1.3 | core-engineer | done | m1/T1.3-config | 895d2d4 | also owns ofm-core/src/keychord.rs |
| T1.4 | glm | done | m1/T1.4-testkit | 9a0024b | |
| T0.2 | glm | done | m0/T0.2-dry-run | merged+reverted | Dry run done via scripts/glm.sh (GLM works, ~1 min). Claude agents not dry-run to save quota |
| T0.1 | lead | done | main | see git log | check.sh green; screenshot m0 OK; app ID io.github.mscoder.Ofm; gtk4 0.11 / libadwaita 0.9 |

(The lead adds a row when a task starts.)

## Deferred review findings (M1 review)

| Item | Where to handle |
|---|---|
| Pending cursor after `begin_load` on large dirs; stale `cursor_name` jumps back (model.rs) | M2 (ui-engineer / T2.x panel wiring) |
| `Key::Char('+')` unparseable: GDK mapping must produce `Gray+`; validate `Session.active_panel/active_tab` on load | M2 T2.0/UI |
| `FsFile::try_clone_from/copy_range_from` need raw fd / as_any so FaultyFs can wrap | M3 T3.0 (architect) |
| Keymap duplicate-chord "last wins" is key-sorted; unknown top-level sections silently ignored | M8 polish |
| Missing `themes/` dir never watched; `atomic_write` follows symlinks | M9 hardening |
| Ext sort 100k = ~55 ms on busy dev box (target 50) | re-measure in M9 |

## Gate feedback log

| Gate | Date | Igor's feedback | Resulting tasks |
|---|---|---|---|

## Decisions and deviations

| Date | What | Why | ADR |
|---|---|---|---|
| 2026-11-14 | All agents (architect, reviewer, fileops) on Sonnet, not Opus | Igor is on Pro plan with limited weekly quota | - |
| 2026-11-14 | GLM offload via `scripts/glm.sh` for tests, packaging, docs, ofm-viewer, ofm-search | Separate Z.ai Coding Plan quota | - |

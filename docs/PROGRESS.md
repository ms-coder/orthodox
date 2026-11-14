# OFM — Progress

The lead keeps this file current after every task. A fresh session must be able to resume from it.

## Current state

- Milestone: M2 (UI shell, gate G1) — in progress; M1 done (m1-done)
- T2.0, T2.1, T2.3 merged on main (check.sh green). Waiting for T2.2 (chrome/themes/session; must merge main and wire items from T2.1/T2.3). Then T2.4 (smoke tests; xdotool now installed — add a real key-event test; un-ignore `file_list_brief_mode_columns` and `navigation_commands` placeholders in tests/shell_skeleton.rs). File ownership: crates/ofm-app/OWNERSHIP.md
- After 2.A: merge each branch --no-ff, check.sh, then STOP at gate G1 with a "please try this" checklist for Igor

## Milestones

| M | Name | Status | Tag | Notes |
|---|---|---|---|---|
| M0 | Bootstrap | done | m0-done | |
| M1 | Core foundations | done | m1-done | |
| M2 | UI shell (G1) | in progress | | |
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
| T2.0 | architect | done | main | see git log | ofm-app skeleton, hooks.rs, OWNERSHIP.md; fixed Gray+ and pending-cursor findings |
| T2.1 | ui-engineer | done | m2/T2.1-filelist | b9f9618 | merged; 100k entries ~1 ms/frame |
| T2.2 | ui-engineer | in progress | m2/T2.2-chrome-themes | | wiring: install_mouse, file_list::configure, session capture/restore |
| T2.3 | ui-engineer | done | m2/T2.3-navigation | bd3846c | merged; also registered dialog.ok/cancel (navigation/modal.rs) |
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
| ~~Pending cursor after `begin_load`~~ fixed in T2.0 (`LoadOpts::cursor_name`) | done |
| ~~`Key::Char('+')` / `Gray+`~~ fixed in T2.0. Validate `Session.active_panel/active_tab` on load | M2 T2.2 (session restore) |
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

## Open notes from T2.0

- No directory watcher in `PanelController` yet; T2.3 needs a controller hook (lead to add) if it wants live refresh.
- `base = "adwaita"` themes: T2.2 must read real colours from `adw::StyleManager` (SystemColors is fallback only).
- Non-Latin layouts: letter shortcuts assume layout group 0 is Latin — unverified.
- Real key-event test needs xdotool (not installed); T2.4 should add one if available.
- T2.3: `panel.open` on regular file launches default app via gio (archive detection waits for M6); mask dialog 'Files only' defaults on; filter mode has no persistent indicator after Enter (polish, M8).
- T2.1: owner/group columns show numeric uid/gid (no passwd lookups on main thread); `PanelController::pending_cursor` is private.

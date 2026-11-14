# OFM — Progress

The lead keeps this file current after every task. A fresh session must be able to resume from it.

## Current state

- Milestone: M2 (UI shell) — **waiting for Igor at gate G1**. All of T2.0–T2.4 merged, check.sh green (~77 s).
- Next step: read Igor's G1 feedback into the "Gate feedback log", create fix tasks if needed, tag m2-done, then M3 (T3.0 architect stubs for the copy engine; fileops-engineer; Opus review of M3 at G2 only by explicit decision).

### G1: please try this

Run `cargo run -p ofm-app` (or `cargo run -p ofm-app -- DIR_LEFT DIR_RIGHT`). Nothing modifies files yet (file operations come in M3), so it is safe to browse anywhere.

1. **Navigation:** Tab switches panels; arrows, PgUp/PgDn, Home/End move; Enter enters a dir; Backspace goes up and puts the cursor on the dir you left; Alt+F12 history list; Ctrl+D hotlist; Alt+F1/F2 drive menu.
2. **Views and sorting:** Ctrl+1/2/3 = Brief/Full/Custom; Ctrl+F3..F6 sort; click a column header to sort (click twice reverses); Ctrl+H toggles hidden files. Try a directory with 100k+ files (e.g. generate one in /tmp) and scroll hard.
3. **Mouse:** click, double-click, Ctrl/Shift-click, wheel, right-click (a context-menu signal only, no menu yet).
4. **Selection and search:** Insert/Space mark, `+`/`-`/`*` (numpad) mask dialog, quick search (far: Alt+letters; tc: plain letters), Esc clears.
5. **Tabs:** Ctrl+T new, Ctrl+W close, lock a tab; quit and restart: tabs, sort, view, window size and split ratio should come back.
6. **Chrome and themes:** F9 menu bar (menu items for commands not implemented yet do nothing), F-key bar with Shift/Ctrl/Alt variants, `app.theme_next` cycles far-blue / commander-light / adwaita instantly. Screenshots: `./scripts/screenshot.sh g1` writes `target/screenshots/g1-*.png`.
7. **Layouts you care about:** try your usual keyboard layout (and a non-Latin one): letter shortcuts on non-Latin layouts are unverified.

Known issues to judge (tell me if any is a blocker):
- Commander Light: white text on the bright orange active path bar is low contrast; Adwaita brief mode is hard to scan with the same orange accent.
- Brief mode draws a header (with sort arrow) for every column, even empty ones.
- Menu entries and F-keys for copy/move/delete/mkdir/view/edit do nothing until M3/M5.
- No live directory refresh (no watcher wired into the panels yet): use refresh (Ctrl+R).
- Real-key testing note: stock xdotool sends F5/F9/F10 as Alt+F5/F9/F10 under Xvfb; `tests/real_binary.rs` uses an XTest helper for that. My earlier remark that F9 opened the menu in a manual xdotool run is therefore unverified; the test suite does cover it.

## Milestones

| M | Name | Status | Tag | Notes |
|---|---|---|---|---|
| M0 | Bootstrap | done | m0-done | |
| M1 | Core foundations | done | m1-done | |
| M2 | UI shell (G1) | waiting for Igor | | |
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
| T2.2 | ui-engineer | done | m2/T2.2-chrome-themes | 57652db | merged; session/themes/F9 menu/F-key bar |
| T2.3 | ui-engineer | done | m2/T2.3-navigation | bd3846c | merged; also registered dialog.ok/cancel (navigation/modal.rs) |
| T2.4 | test-engineer | done | m2/T2.4-smoke-tests | ac1e85b | merged; real-binary tests (tests/real_binary.rs), g1 screenshot set via scripts/screenshot.sh g1 |
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
- Lead fix after T2.2 merge: `app::run` dropped its `Rc<AppContext>` (UI closures hold only Weak) so commands/keys died in the real binary; tests missed it because they hold the Rc. Now `app.rs` owns `(ctx, window)`; the thread_local workaround in main_window.rs was removed. Verified with real xdotool keys (Tab, Down, F9). T2.4 must include a test on the real binary path.
- T2.2: `app.theme_next` rewrites config.toml (comments in hand-edited config are lost) — M8/M9 polish. `cli_overrides_paths()` checks argv[1]; cleaner as a flag in AppInit.
- T2.2: F9 menu key handling relies on GTK running later-added controllers first (verified with probe + real F9 open).
- T2.4: tests launching `ofm` must set DBUS_SESSION_BUS_ADDRESS=unix:path=/nonexistent (GApplication single-instance on the session bus) and use temp XDG dirs; select the window with `xdotool search --class ofm`. Open question: add `screenshot.sh g1` (+30 s) to check.sh? (default: no).

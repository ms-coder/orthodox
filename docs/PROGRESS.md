# OFM — Progress

The lead keeps this file current after every task. A fresh session must be able to resume from it.

## Current state

- Milestone: M3 (File operations, gate G2) — in progress; M2 done (tag m2-done)
- T3.0 done and committed (API in docs/adr/0006-jobs-api.md; ARCHITECTURE.md §2 JobEvent/Question shapes are stale, ADR-0006 is authoritative). Now group 3.A: T3.1 engine+copy/move (fileops-engineer), T3.2 other ops (core-engineer), T3.3 file-op UI (ui-engineer), T3.4 torture suite (test-engineer; GLM only if ZAI_API_KEY set). Then T3.5 deep review (reviewer), tag m3-done, STOP at G2.
- T3.0 must also: make `FsFile::try_clone_from/copy_range_from` wrappable by FaultyFs (deferred M1 finding); add a directory-watcher hook to `PanelController` for T3.3 auto-refresh (panel/ is frozen core, architect may edit).

## Milestones

| M | Name | Status | Tag | Notes |
|---|---|---|---|---|
| M0 | Bootstrap | done | m0-done | |
| M1 | Core foundations | done | m1-done | |
| M2 | UI shell (G1) | done | m2-done | | |
| M3 | File operations (G2) | in progress | | |
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
| T3.4 | test-engineer (GLM died on 429, finished by Claude) | done | m3/T3.4-torture | 0a412d8 | merged; 94 torture tests, no engine bugs found; mutation-checked by lead (3 injected bugs caught by 4/25/6 tests) |
| T3.3v | test-engineer | done | m3/T3.3v-e2e-fileops | 3665b1d | merged; real-binary file-ops e2e (8 pass, 2 slow via OFM_E2E_SLOW=1), pinned 8 bug tests |
| T3.3 | ui-engineer | done | m3/T3.3-fileops-ui | e4bfafd | merged; verified only with synthetic events; real-engine e2e = T3.3v (test-engineer, branch m3/T3.3v-e2e-fileops) |
| T3.1 | fileops-engineer (Opus) | done | m3/T3.1-engine-copy-move | feb7548 | merged; 51 integration tests incl. cancel at 9 offsets, ENOSPC/EIO/short write, real EXDEV /tmp->/dev/shm; self-checked T3.5 list 11/11 |
| T3.2 | core-engineer | done | m3/T3.2-other-ops | 6b8f43d | merged; own freedesktop trash impl (no trash crate), chrono added |
| T3.0 | architect | done | main | see git log | jobs API frozen, RealFs/FaultyFs implemented, PanelController watcher, JobsController stub |
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
| Stray dots in empty FileList area on Igor's Wayland session (not reproducible on Xvfb) | M9 / when Igor reports renderer result |
| Light theme: white text on orange active path bar low contrast; Brief mode draws header for empty columns | M8 polish |
| FileOps dialogs render in default libadwaita colours in all 3 themes (`colors.dialog` not applied to adw::Dialog; same for mask dialog); jobs view showed 2 white specks in empty area (cf. G1 dots report) | M8 polish / M9 |
| Group field in Properties editable only as root (no euid/group-membership API in ofm-app); dir-size result lost if watcher re-stats the entry; progress row lives in `fkeybar_slot` not per-panel status | M8 polish |
| Ext sort 100k = ~55 ms on busy dev box (target 50) | re-measure in M9 |

## Gate feedback log

| Gate | Date | Igor's feedback | Resulting tasks |
|---|---|---|---|
| G1 | 2026-11-15 | "file manager looks great, really cool, continue with other milestones". Earlier report: small stray dots left of the list in empty area (Wayland session). | Dots not reproducible on Xvfb (cairo+ngl, scale 1/2, text scale 1.25; 24 captures clean); code review found no draw path. Asked Igor to try GSK_RENDERER=cairo / GDK_BACKEND=x11 — no answer yet. Deferred to M9 hardening unless it reappears. |

## Decisions and deviations

| Date | What | Why | ADR |
|---|---|---|---|
| 2026-11-14 | All agents (architect, reviewer, fileops) on Sonnet, not Opus (superseded 2026-11-15 for M3, see next row) | Igor is on Pro plan with limited weekly quota | - |
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
| 2026-11-15 | Opus allowed for M3 by Igor (Claude quota resets soon): T3.1 copy/move engine (fileops-engineer) and T3.5 deep review (reviewer) on Opus; rest Sonnet | data-safety critical | - |
| 2026-11-15 | GLM available: ZAI_API_KEY is in ~/.bashrc, not in the session env; run `bash -ic './scripts/glm.sh <dir> "<prompt>"'` | Igor exported it after session start | - |
- T3.2 open: `Outcome::Size` has no Report so size-walk warnings (unreadable dirs) never reach the UI (needs API change; decide at T3.3/T3.5); mkdir `note_created` records the full requested path, not the topmost created component (cursor focus in UI); `SizeTotals.disk_bytes` includes dirs' own blocks (du-like); empty/`/` Delete list -> `Invalid`.
- T3.1 decisions: missing destination dir is created like `mkdir -p` (accepted, mc-like; T3.3 dialog should show the resolved target); same-file conflict answered "rename existing" acts like "rename new" (accepted); directory merge keeps the existing dir's mode/times (accepted); mtime verify tolerance 2 s for FAT (accepted); metadata failures (mode/times/xattr/owner) are warnings once per kind+errno.
- T3.1 deferred: ACLs (`system.*` xattrs) are not copied (SPEC F17 lists xattrs, ownership — reviewer T3.5 to decide if ACLs are in scope; else M9); a retried file counts twice in `bytes_done` (progress cosmetics, M9); `plan::unique_name` probe loop has no CancelToken checkpoint (bounded; reviewer to confirm).
- T3.4 note from T3.1: `FaultyFs` counts `copy_range_from` probes even when the read/write tier is used, so write faults must target the operation the active tier uses.
- 2026-11-15: GLM T3.4 run died on Z.ai 429 "Usage limit reached for 5 hour" (resets 20:27 same day) after writing 7 uncommitted test files in ../orthodox-wt-T3.4. T3.4 re-assigned to a Claude test-engineer working in that worktree (merge main, un-ignore, run against real engine). Lesson: check glm log for 429 before assuming a GLM job is running; GLM jobs started via `bash -ic` run as `claude -p` (pgrep -af "claude -p").
- T3.3v finding (blocker for G2): real key events never reach the dispatcher while a dialog is open (Enter/F2/Ctrl+A/Del/Ctrl+J); synthetic tests missed it. Escalated to architect (Opus) with the two small dialog bugs (Alt+A mnemonic clash in conflict dialog, rename selects whole name instead of stem). `ofm_testkit::temp::TempDir::drop` silently fails on trees with read-only dirs (leaks temp dirs) — M9.
- Dialog key bug FIXED (architect/Opus): libadwaita 1.10 calls gtk_widget_set_limit_events on every adw::Dialog, so the main-window dispatcher never saw keys inside dialogs. `modal::present` now attaches the dispatch controller (WindowKind::Dialog) to every dialog (OWNERSHIP.md "Keys in dialogs"). Also: conflict-dialog Abort is Alt+B; rename preselects only the stem. All 18 real-binary file-ops e2e pass (slow ones via OFM_E2E_SLOW=1, ~144 s).
- Data-safety fix in quit path (jobs/quit.rs): quitting with running jobs now cancels, then waits on the main loop (max 15 s) until workers removed their .ofm-part temp files, then closes (previously the process could exit first and leave temp files). Reviewer T3.5 must look at it. If a job does not stop in 15 s the window closes anyway and a warning is logged.
- `[global]` chords (F9, Ctrl+O, F11) also work inside dialogs (ADR-0005 design). Main window initial focus is on a GtkToggleButton (harmless).

## T3.5 review (Opus reviewer) — verdict BLOCK (2026-11-15)

Blocker: B1 job can overwrite an item it created itself (duplicate target names: mask `*.bak` on a.txt+a.md; two same-named files from a list panel; FAT case-insensitive names IMG.JPG/img.jpg with "newer only") -> first source lost, job reports Ok. copy.rs:121-137, plan.rs:87-106.
Major: M1 ACLs not copied + mode copied as-is -> private 0600 file with ACL entry becomes 0660 (copy/meta.rs); M2 non-UTF-8 names lossy through copy dialog entries -> renamed with U+FFFD (dialogs/copy.rs); M3 RENAME_NOREPLACE unsupported on NFS/FUSE (ntfs-3g, sshfs, gvfs) -> endless Retry/Skip (fs/real.rs, file.rs, move_.rs) needs link()+unlink fallback; M4 close() result of temp unchecked, cross-device move may unlink source after deferred write error (file.rs:323); M5 overwrite publishes unsynced file when sync off (file.rs:297) -> always fsync when replacing.
Minor: m1 delete/chmod path-based TOCTOU on dir->symlink swap (needs openat/unlinkat Fs revision -> M9, architect); m2 trashinfo ENAMETOOLONG for >245-byte names mapped to Unavailable; m3 "rename existing" renames the source's own parent; m4 recursive chmod clearing u+rx locks job out (needs post-order); m5 open_read without O_NONBLOCK (FIFO swap hang); m6 copy_file_range/reflink fallback errno list lacks EPERM/ETXTBSY/EIO; m7 stale .ofm-part cleanup "on next run" (ADR-0004) not implemented.
Checklist: all PASS except NOREPLACE (FAIL in effect: B1, M3). Hunt items without defect: dialog defaults safe; apply-to-all per job; cancel at publish/unlink never loses; set-id bits cleared; special files skipped. Note: Shift+Del permanent-delete dialog focuses "Delete permanently" (Enter deletes) — decision pending.
Fix plan T3.6: fileops-engineer(Opus): B1 M1 M3 M4 M5 m3 m5 m6; core-engineer: m2 m4; ui-engineer: M2. m1 and m7 -> M9. Then re-review (narrow) before tag m3-done.

## Model policy update (2026-11-15, Igor): GLM 5.3 is back — use GLM as much as possible; Sonnet only for very difficult tasks; Opus only for extremely important AND difficult tasks (don't burn the weekly Claude limit in one session). scripts/glm.sh now uses glm-5.3. Claude agents hit a 5-hour limit once already (T3.6a/b/c died on 429; their partial work was kept).
T3.6 fix status: T3.6a (Opus: B1 M3 M4 M5 m3 m5 m6, copy engine) branch m3/T3.6a-engine-fixes; T3.6b (GLM: trash long names, chmod post-order) worktree .claude/worktrees/agent-a98b290549b496009 branch m3/T3.6b-ops-fixes; T3.6c (GLM: non-UTF-8 dialogs, Shift+Del focus) worktree .claude/worktrees/agent-a4ae05127d1b77e8d branch m3/T3.6c-nonutf8-dialog; T3.6d (GLM: ACL copy M1, copy/meta.rs only) ../orthodox-wt-T3.6d branch m3/T3.6d-acl. GLM logs: scratchpad/glm-{b,c,d}.log. Next: merge all four, check.sh, narrow Opus re-review (or GLM review + Opus only if doubts), tag m3-done, G2.
T3.6 merged (2026-11-15): T3.6b (GLM, 9ded84a), T3.6a (Opus, eb8c994: B1 M3 M4 M5 m3 m5 m6; adds FsFile::close + FaultOp::FileClose + one `unsafe` try_close in fs/real.rs, documented), T3.6d (GLM, ACL copy), T3.6c (GLM, non-UTF-8 dialogs + Shift+Del default focus Cancel for multi-item/dirs). check.sh green 4x on main; ACL tests verified to really run (setfacl present, /dev/shm supports ACLs). One earlier full run had 2 real_binary_fileops 'ofm exited during start-up' failures (not reproduced in 4 runs) — watch for flakes. Open: own-output collisions reuse ConflictKind::SameFile so the dialog caption says 'same file' (add dedicated variant later, needs ofm-app change); FAT lstat+rename fallback keeps a tiny documented race. Next: narrow Opus re-review running; then m3-done and G2.

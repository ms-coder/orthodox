# OFM — Progress

The lead keeps this file current after every task. A fresh session must be able to resume from it.

## Current state

- Milestone: M5 done (tag m5-done); next M6 archives (not started - Igor stopped the session here to continue later). M4 done (m4-done). M3 (File operations) — **waiting for Igor at gate G2** (tag m3-done). All of T3.0–T3.7 merged, check.sh green. Reviews: T3.5 BLOCK -> T3.6 fixes -> re-review SIGN OFF WITH CONDITIONS -> T3.7 (N1 default ACL, N2 FAT unstable inodes, N3 chmod restore) merged and verified by the lead (reviewer's N1 repro now clean).
- Next step: read Igor's G2 feedback into the "Gate feedback log", fix what he finds, then M4 (command line & terminal; T4.0 architect design first). GLM quota resets 2026-11-16 09:31 (server time); until then use Sonnet sparingly (Igor: save tokens), no Opus unless extremely important.

### G2: trust the copy engine — please try this (ONLY ON COPIES OF YOUR DATA)

Rule zero: never point OFM at your only copy of anything. Make a scratch area first, e.g.
`mkdir -p ~/ofm-g2/{src,dst}` and put a COPY of a photo folder (a few GB, thousands of files, some odd names) into `~/ofm-g2/src` with `cp -a` or rsync. Keep the original untouched somewhere else. Run the app with `cargo run -p ofm-app -- ~/ofm-g2/src ~/ofm-g2/dst`.

Comparison one-liners (run in a normal terminal; both print nothing when identical):
- `diff -rq ~/ofm-g2/src ~/ofm-g2/dst/<folder>`  (content + names)
- `rsync -rlcnH --delete --itemize-changes ~/ofm-g2/src/ ~/ofm-g2/dst/<folder>/`  (checksums, dry run; lists any difference)
- `find ~/ofm-g2 -name '.*ofm-part*'`  (must print nothing: no leftover temp files)

1. **Copy within the SSD:** mark the photo folder in the left panel, F5, Enter. Wait for the jobs bar to finish. Compare with the one-liners; sources must be unchanged.
2. **Copy to the USB stick** (cross-device; first run `findmnt -no FSTYPE,TARGET /media/$USER/*` and tell me the file system: ext4, FAT/exFAT or NTFS): same as 1, destination = a new folder on the stick. Compare.
3. **Cancel in the middle:** start another big copy, open the jobs view (Ctrl+J), cancel the job (Del). Then check: no file under a final name is truncated (rsync -rcn lists only MISSING files, never differing ones), and no `.ofm-part` files remain.
4. **Conflicts:** copy the same folder again into the same destination. In the conflict dialog try Skip, then repeat and choose "Overwrite if newer" with apply-to-all (Ctrl+A). Before the second run `touch` a few files in the source so they are newer. Only the newer ones may change.
5. **Move:** F6 a copy-folder from src to the stick and back. After each move check source gone / destination complete (rsync one-liner against your other copy).
6. **Delete to trash and restore:** F8 on a few files in the scratch area; open GNOME Files > Trash and restore them; they must come back with the right content. (Shift+F8 deletes permanently: only in the scratch area.)
7. **Odd names:** put files named like `-rf`, `a b`, `new
line`, `ü ß`, a very long name, and a non-UTF-8 name (`printf 'x\xff.txt'` via touch "$(printf 'x\xff.txt')") in the source and copy/move them with F5/F6, Enter in the dialog: names must stay byte-identical.
8. **Quit during a job:** start a big copy and press F10: it asks, then cancels and waits for cleanup (up to 15 s); no temp files left.
Tell me anything odd: slowness, wrong dialog wording (own-output collisions say 'same file'), dots/artifacts, crashes. Include the stick's file system type.
## Milestones

| M | Name | Status | Tag | Notes |
|---|---|---|---|---|
| M0 | Bootstrap | done | m0-done | |
| M1 | Core foundations | done | m1-done | |
| M2 | UI shell (G1) | done | m2-done | | |
| M3 | File operations (G2) | waiting for Igor | m3-done | | |
| M4 | Command line & terminal | done | m4-done | review BLOCK -> T4.5 fixes, verified by lead |
| M5 | Viewer & quick view | done | m5-done | review SIGN OFF WITH CONDITIONS -> T5.5a/b fixes, verified by lead |
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

## T3.6 re-review (Opus) — SIGN OFF WITH CONDITIONS (2026-11-15)
All of B1, M2-M5, m2-m6 fixed; M1 partial. Conditions before m3-done: N1 (MAJOR) copying a default ACL lets copies inherit permissions the source never had (D with `setfacl -d -m u:nobody:rwx`, file 0640 w/o ACL -> copy has nobody:rwx, effective r--; needs additive Fs::remove_xattr, compare ACL bytes in acl_safe_perm); N2 (MAJOR, untested by reviewer) vfat/exFAT inode numbers are unstable (iunique after eviction), so own-output protection by (dev,ino) can fail on a big photo import to a stick -> also record published paths per parent dir and compare case-insensitively. Both + N3 (MINOR: chmod read_dir `?` skips restore_mode on abort/cancel; temp u+rx opened even when not owner) launched on GLM: branches m3/T3.7a-default-acl, m3/T3.7b-fat-ino, m3/T3.7c-chmod-restore in worktrees ../orthodox-wt-T3.7{a,b,c}.
Deferred to M9: N4 NFS retransmitted link() EEXIST can leave moved item at source (compare lstat from/to inode in noreplace.rs); N5 trash renameat2(NOREPLACE) outside Fs, EINVAL on NFS/FUSE -> every trash item asks retry/skip (route through noreplace fallback); N6 link dialog link_origin for non-UTF-8 folder after user deletes the name (U+FFFD, ENOENT, no data loss); EINTR in checked close counts as success (nit).

2026-11-16: GLM quota out again (429, resets 2026-11-16 09:31 server time) — 7b and 7c died; Igor asked to continue on Sonnet and save tokens. Plan: ONE Sonnet fileops-engineer finishes T3.7b + T3.7c in their existing worktrees (7c had fix+test uncommitted; 7b only tests). T3.7a (N1 default ACL) still running on GLM in ../orthodox-wt-T3.7a (6 files dirty); if it dies, finish with the same Sonnet agent. Token-saving rules: short prompts, one agent per batch, no Opus, lead verifies by running check.sh + reviewer repros instead of new reviews.
- G2 known wording issue: a collision with the job's own output reuses ConflictKind::SameFile, so the dialog says 'source and destination are the same file'.
- G2 setup done by lead (2026-11-15): synthetic data in ~/ofm-g2/src (4.7 GB, 3021 files: 3000 photo-like files, 3x300 MB videos, 1 GiB sparse file, odd-names/ with newline/non-UTF-8/long/leading-dash names, symlinks, hardlink pair, empty file+dir, case-collision/ IMG.JPG vs img.jpg), pristine reference in ~/ofm-g2-reference (rsync -aHS). Verify src unchanged: `rsync -rlcnH --delete --itemize-changes ~/ofm-g2-reference/ ~/ofm-g2/src/` (use -l so symlinks are compared). case-collision/ is the FAT/exFAT stick test: the engine must ask/rename, never silently overwrite.
- G2 finding 1 (Igor): jobs view Cancel / Pause / Del did nothing (Close worked). Root cause: the view selected its oldest row; with finished jobs listed above the running one, actions hit a finished job. Fix (lead, jobs/view.rs): `pick_target` = selected job if unfinished, else running > paused > queued; reselect on add/finish/clear and on open. Regression: unit tests in view.rs + e2e `del_cancels_the_running_job_when_finished_jobs_are_listed_above_it` (slow, OFM_E2E_SLOW=1; fails on old code). Lesson: e2e tests need multi-job scenarios and mouse clicks; UX gap left: no 'Cancelling…' feedback until the worker stops.
- G2 finding 2 (Igor, Wayland/GNOME): 'Alt menu stays after Alt tap' and 'Alt+F2 shows a user-command popup instead of the drive menu'. Not reproducible on X11/Xvfb (Alt tap does nothing). Cause of the popup: GNOME binds <Alt>F2 to panel-run-dialog ('Enter a Command'), and Alt+F7/F8/F10 to move/resize/maximize, so OFM never sees them. Fix (untested on a real Wayland session, needs Igor): new `[general] inhibit_system_shortcuts = true`; main window calls gdk Toplevel::inhibit_system_shortcuts when realized (mutter may ask once; Super+Esc restores shortcuts; set to false if Alt+Tab etc. get in the way). If the 'lingering menu' persists after this, ask Igor for a screenshot (is it the GNOME run box or an OFM menu?).
- G2 finding 3 (Igor): F10 then Quit exits the app — read as step 7 working; still to confirm no `.ofm-part` temp files and that it waited for cleanup.
- Latent M1 bug found by proptest during G2 work (not related to G2 changes): extension sort fast path (ofm-core panel/sort_ids.rs sort_group_ext) treated extensions that agree in the first 8 encoded bytes as equal (the `exact` flag covered 16 bytes but only 8 are stored) -> wrong order for e.g. "A.0__2" vs "_.0__2_". Fixed (exact only if nothing is encoded below the top 8 bytes) + deterministic test; 30k-case proptest clean. Process slip: I committed dc5b330 before check.sh finished (it was failing because of this); fixed in the next commit. Commits are now gated on check.sh.


## 2026-11-16 — M4 started (on GLM, per user: save Claude tokens)
- T4.0 (GLM) merged: ADR-0003 frozen, `ofm-core::cmdline` helpers (quote/osc7/cd/complete + proptests), `terminal/` and `cmdline/` skeletons wired through hooks, `vte4 0.10` (v0_76). check.sh green. GLM also bound `Tab` -> `cmdline.complete` in the `[cmdline]` section of all 3 presets (reviewed, ok: section only active with text).
- Lead follow-ups done (54ecbcd): `AppContext::terminal_focused` (dispatch uses only `global` chords while VTE has focus), `[terminal] return_to_panels`.
- T4.1 terminal and T4.2 command line running in parallel on GLM (worktrees orthodox-wt-T4.1/T4.2). Then T4.3 tests, T4.4 lean review, tag m4-done.
- Still open from G2: USB-stick results, retest of jobs-view cancel, Alt+F2 shortcut inhibition on real Wayland, F10-quit leaves no `.ofm-part`.
- 2026-11-17: GLM weekly limit exhausted (resets 2026-11-21 16:18). Kimi Code (K2.8 Preview, `kimi-for-coding`, `scripts/kimi.sh`, key `KIMI_API_KEY` in ~/.bashrc) is now the offload target. T4.1/T4.2 continue on Kimi from GLM's uncommitted partial work; T5.0+T5.1 (ofm-viewer backend + ADR-0007) runs on Kimi in parallel.

## 2026-11-16 — M4 integration state (resumed session)
- T4.1 (terminal) and T4.2 (cmdline) merged; T5.0+T5.1 (viewer backend) merged. Lead fix a3635cf: terminal UI looked for the cmdline entry only among direct children of `cmdline_slot`; now searches the subtree. check.sh green (incl. `terminal_real` 4 e2e: Ctrl+O toggle, panel->shell cwd, shell cd->panel, typed command).
- T4.3 considered covered by existing tests (cmdline proptests in ofm-core, `cmdline_real` incl. history persistence over quit/restart, `terminal_real`). No separate task.
- T4.4 lean Sonnet review running; on SIGN OFF fix findings, tag `m4-done`, then M5 UI (T5.2+). Leftover worktrees ../orthodox-wt-T4.1, T4.2, T5.1 and their merged branches can be removed after the review.

## 2026-11-16 — M4 closed
- T4.4 review (Sonnet): BLOCK — fish injection via POSIX quoting in cd sync (reproduced by lead: `touch` ran), control bytes in fed line, plus 5 should-fix. T4.5 (ui-engineer, Sonnet) fixed all with real bash/zsh/fish round-trip tests (`crates/ofm-core/tests/quote_real_shells.rs`); merged, check.sh green. No second review (token policy); lead re-verified with the tests.
- Flake found while merging: `panel_watch` aborted ~1 in 7 runs (glib ThreadGuard: watcher thread woke a task after the test's private MainContext was dropped). Test-only; fixed by leaking one context ref in `with_loop`; 30/30 clean. App uses the default context, unaffected.
- Open from M4 review (-> M9 unless noted): (a) verify VTE emits `current-directory-uri-changed` for two identical consecutive URIs (else shell stays Busy after `ls` in same dir and sync stalls) — worth a quick manual check at G3; (b) `send_line` (Enter in cmdline) is not guarded against a half-typed terminal line; (c) when a panel move is blocked by a dirty terminal line it is not re-synced until the next panel change; (d) wrapper-dir cleanup is on a detached thread and can be cut short at process exit (files in $XDG_RUNTIME_DIR, cleared at logout); (e) program output can fake OSC 7 (documented in ADR-0003).

## 2026-11-17 — M5 started
- T5.2a (viewer window UI, ui-engineer Sonnet, branch m5/T5.2a-viewer-ui) and T5.2b (quick view + F4 editor, ui-engineer Sonnet, branch m5/T5.2b-quickview-editor) running in parallel in agent worktrees; disjoint dirs (`viewer/` vs `quickview/`+`desktop/editor.rs`), tiny shared wiring edits -> lead resolves merge conflicts. Next: merge both, check.sh, T5.3 tests (Kimi/GLM if quota allows), T5.4 lean review, tag m5-done.

## 2026-11-17 — M5 merged, review running
- Merged (check.sh green after each): T5.2b quick view + F4/Shift+F4 editor (ef9c41c), T5.2a viewer window (d4847a6; conflicts only in hooks.rs/Cargo.toml, resolved by keeping both), T5.3a viewer backend edge-case tests (Kimi, ~1000 lines in ofm-viewer/tests).
- Kimi note: a `bash -ic ... &` launch looked "finished with no output" but the `claude -p` kept running detached; relaunching created 3 concurrent writers on one worktree (lead killed 2). Rule: after launching an offload job, check `pgrep -af "claude -p"` before relaunching; give the prompt via a file, `< /dev/null`.
- Open from builders (-> M8/M9 unless the T5.4 review raises them): `ofm_viewer::LineIndex` miscounts UTF-16 and holds a Vec of all line starts (app uses its own viewer/scan.rs) -> fix or remove; `TextPreviewProvider` merges lone-CR lines (ignored test BUG in ofm-viewer/tests/edge_cases.rs); quick view keeps the preview side's stale path header/status line ("Up one level") and shows 1-px specks in Adwaita/Commander-light dir previews; viewer has no file name in status line, no text selection/copy, case folding is ASCII-only (ADR-0007); `real_support::Shot.px` is 16-bit but treated as 8-bit; one flake of real_binary::keys_enter_dir_and_go_up ("ofm exited during start-up", cf. earlier fileops flake) - watch; OWNERSHIP.md has no M5 section (add viewer/quickview/desktop paths + commands).
- Next: T5.4 review (lean Sonnet) -> fix findings -> tag m5-done -> M6 archives.

## 2026-11-17 — T5.4 review: SIGN OFF WITH CONDITIONS
- #1 SIGBUS reachable: `Document::read_at` copies from the mmap after a stat; truncate in between kills the process (reviewer reproduced in 20 s with a truncate loop; Kimi's tests truncate before reading so missed it). Fix: always pread. #2 UTF-16 search returns odd-offset (misaligned) matches. #3 quick view image: no pixel cap (decompression bomb). #4 terminal-editor launch: appends to a half-typed terminal line (`rm -rf ` + F4), plain shells never become Idle so F4 always says busy, `emacs` in default terminal_editors hides panels for GUI emacs. Also: lone-CR preview, FIFO swap hang in Document::open (O_NONBLOCK + fstat), LineIndex UTF-16.
- Running: T5.5a (Kimi, ofm-viewer: #1 #2 lone-CR FIFO LineIndex) branch m5/T5.5a-viewer-fixes in ../orthodox-wt-T5.5a; T5.5b (ui-engineer Sonnet: #3 #4 + stale preview header) branch m5/T5.5b-ui-fixes. Then merge, check, tag m5-done.
- Backlog (M9): dir summary walk does not stop at mount points (cursor on `/` walks /proc,/sys until cursor moves); walk thread not cancelled on window close; harness should dump OFM_E2E_LOG + exit status/signal when "ofm exited during start-up" fires (flake evidence); cosmetic 1-px specks.

## 2026-11-17 — M5 closed (session stopped here by Igor; resume with M6)
- T5.5a (Kimi, ofm-viewer): reads are pread only (no mmap) so a concurrent truncate cannot SIGBUS; stress tests `concurrent_truncate_and_{read_at,search}_never_faults`; UTF-16 search is unit-aligned; Document::open is O_NONBLOCK+fstat (FIFOs/devices/dirs rejected); lone-CR preview fixed (test un-ignored); LineIndex documented/fixed. T5.5b (Sonnet): quick-view pixel cap (100 MP / 30000 px, header via Pixbuf::file_info) and size from the opened fd; F4 terminal editors now run via `TerminalHandle::run_program` (own VTE child, exact argv, works for all shells, immune to half-typed lines) — ADR-0003 updated; `emacs` removed from default terminal_editors. check.sh green on merged main.
- Flake notes: `real_binary_fileops::shift_f6_renames_in_place` timed out once (15 s) while another agent ran a full test suite concurrently; 4/4 pass alone, full check green when idle. E2E tests are load-sensitive: do not run two check.sh at once (agents running check.sh in parallel with the lead can cause these).
- Open (M8/M9): quick view preview side keeps stale status line ("Up one level"); a "Quick view" header attempt made `quick_view_follows_the_cursor_and_restores_the_panel` fail with a 4-px diff after the 2nd Ctrl+Q (reverted; look at that screenshot diff); 1-px specks in Adwaita/Commander-light dir previews; dir-summary walk crosses mount points; viewer: no file name in status line, no selection/copy, ASCII-only case folding; harness should log OFM_E2E_LOG + exit status on "ofm exited during start-up".
- Igor can try M4+M5 features now (cargo run -p ofm-app): Ctrl+O/F12 terminal, F11 command line, F3 viewer (F4 hex, F2 wrap, F7 search, F8 encoding, Alt+F8 goto), Ctrl+Q quick view, F4/Shift+F4 editor. Unverified by hand: VTE duplicate-URI cwd sync after `ls` twice in the same dir. Still open from G2: USB stick results, jobs-view cancel retest, Alt+F2 on Wayland, F10-quit leaves no .ofm-part.

## 2026-11-22 — "work hard today" plan (Igor: all Claude models except Fable, little Opus, Kimi/GLM too, more agents at once; stop at G3)
- Machine: 8 cores, 7 GB RAM (Chrome eats most) -> (SUPERSEDED, see lesson below: shared CARGO_TARGET_DIR was tried and dropped) offload jobs run crate-level tests only (lead runs full check at merge), Claude agents use `./scripts/locked-check.sh` (flock, serialises e2e suites; concurrent check.sh runs caused e2e flakes). Max ~2 heavy GUI agents at once.
- Order: M8 cores (no stubs needed) + T6.0/T7.0 API freeze + T8.4 in parallel -> M6 (T6.1 Kimi, T6.2 fileops Sonnet, T6.3 UI) and M7 (T7.1/T7.2 offload, T7.3 UI) -> M8 UI follow-ups (compare/menu/batch-rename + content-compare job) -> reviews -> G3 checklist -> STOP (human gate).
- Running (wave 1): T8.1a compare engine (Kimi, m8/T8.1a-compare), T8.2a user-menu config+expansion (GLM, m8/T8.2a-menu), T8.3a batch-rename engine (Kimi, m8/T8.3a-batch-rename), T6.0+T7.0 API stubs + ADR-0008/0009 (architect Sonnet, m6/T6.0-T7.0-api), T8.4 desktop integration F31/F33 (ui-engineer Sonnet, m8/T8.4-desktop).
- 2026-11-22 wave 2 (more agents at Igor's request): merged T8.1a compare engine (Kimi) and T8.3a batch-rename engine (Kimi). Running: T8.2a menu (GLM), T6.0+T7.0 architect, T8.4 desktop (UI), T8.1b content-compare job (fileops), T8.3b rename-batch job (fileops), T8.1c compare UI (UI), T8.7 M8 UI polish (UI), T8.8 config polish (Kimi, m8/T8.8-config-polish in ../orthodox-wt-T8.8). Not started on purpose: M9/M10 work (gated by G3 feedback).

- LESSON 2026-11-22 (shared cargo target dir): do NOT share one CARGO_TARGET_DIR between git worktrees running in parallel. Cargo hashes path-dependency artifacts relative to the workspace root, so worktrees with different source collide on the same artifact names; combined with `target/debug/deps` and `.fingerprint` vanishing repeatedly (culprit not found) it produced bogus E0432/E0433 errors ("no `menu_expand` in `ofm_core`") and untrustworthy green runs. Each worktree uses its own ./target (agents were told to `unset CARGO_TARGET_DIR`). Full checks stay serialised via scripts/locked-check.sh. Kimi/GLM sandboxes ignored the export anyway.
- 2026-11-22 merged so far: T8.1a, T8.3a, T8.2a (engines), T8.1b (content-compare job), T8.4 (desktop: Enter-open, clipboard, DnD, session fixes), T8.8 (config polish; theme persist now in-place edit). Running: T6.0+T7.0 architect, T8.3b rename job, T8.1c compare UI, T8.2b menu UI, T8.7 UI polish.
- Igor's clipboard: T8.4 agent's first two clipboard test runs used the real Wayland session (before GDK_BACKEND=x11) and may have overwritten the clipboard / flashed windows. Told Igor.

## 2026-11-22 (later) — interruption and recovery
- ~01:00 all five Claude agents died on the 5-hour limit (reset 02:40; I only noticed at 09:15 when Igor said "continue"; lesson: the session sat idle ~8 h — when limits hit, check the clock and resume/redo immediately). Their work was saved: T8.3b, T8.7 (all 8 items), T8.1c, T8.2b were committed, so the lead merged them (conflicts only in ofm-jobs spec/runner/lib — T8.3b deliberately removed the old stub `JobSpec::Rename`, kept `CompareContent` + `RenameBatch` + `RenameRollback` — and ofm-app hooks.rs) and ran one combined `locked-check.sh`.
- An orphaned check from a dead agent (the architect's worktree) held /tmp/ofm-check.lock for hours and blocked every other agent's check; lead killed it. Rule: after agents die, `ps` for orphaned `flock`/`check.sh` chains before anything else.
- T6.0+T7.0 (API freeze) was unfinished (wip commit + failing test/clippy): handed to Kimi (worktree .claude/worktrees/agent-a45315042fadfcd89, branch m6/T6.0-T7.0-api). T8.3c batch-rename UI started on Sonnet.
- 2026-11-22 09:50 wave 3: T8.3b/T8.7/T8.1c/T8.2b merged, T6.0+T7.0 interface freeze merged (e6b7c30; conflicts in ofm-jobs lib/ops/runner resolved: freeze's top-level `extract.rs` + `ExtractOpts`, T8.3b's `RenameBatch`/`RenameRollback`, `CompareContent`; old stub `Rename`/`ops/extract` gone). Combined check #1 failed in `quickview_editor_real::quick_view_follows_the_cursor...` with "ofm exited during start-up: exit status: 1 (not a signal)" (passes 3/3 alone; app exiting 1 = GDK lost the X server, suspect an agent's broad pkill of Xvfb/ofm; all agents now told never to pkill/kill foreign processes). Cargo stops at the first failing test binary, so that run did NOT execute the later suites -> full re-check #3 running on main (log scratchpad/check-main3.log).
- Running: T6.1 archive provider (Kimi, ../orthodox-wt-T6.1-archive-provider), T7.1 search engine (GLM, ../orthodox-wt-T7.1-search-engine), T7.2 list+panelize (Kimi, ../orthodox-wt-T7.2-list-panelize), T6.2 extract job (fileops Sonnet), T6.3 archive UI (UI Sonnet), T7.3 find/panelize UI (UI Sonnet), T8.3c batch-rename UI (UI Sonnet). Next: merge as they finish, T6.4/T7.4 tests, reviews (Opus once for extract), M8 reviews + G3 checklist, STOP at G3.

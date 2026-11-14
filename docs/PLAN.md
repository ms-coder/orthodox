# OFM v1.0 — Implementation Plan

11 milestones (M0–M10), about 50 tasks, 4 human gates. Each task names its agent, the spec IDs it
covers, what it owns, what it depends on, and when it is done. The lead (main session) follows the
orchestration protocol below.

## Orchestration protocol (the lead follows this for every milestone)

1. **Prepare.** Re-read the milestone section and `docs/PROGRESS.md`. Mark the milestone `in_progress`.
2. **Stubs first.** Run the milestone's `T*.0` task (architect, foreground, on `main`): interfaces,
   trait/type stubs with `todo!()` bodies, module skeletons, ADRs. The lead runs `./scripts/check.sh`
   (stubs must compile; tests may be `#[ignore]`d) and **commits to `main`** — worktree agents branch
   from `main`, so anything they depend on must be committed first.
3. **Fan out.** Spawn the tasks of the next parallel group as background agents (max 4 at once), each
   with `isolation: worktree` (already set in the agent files). Each delegation message contains:
   - Task ID + title, spec IDs, acceptance criteria (copy from this file)
   - Owned paths (may edit) and forbidden paths (must not edit)
   - Branch name: `m<N>/<task-id>-<slug>`
   - "Run `./scripts/check.sh` before your final commit. Report: branch, ≤ 15-line summary,
     test results, deviations from the plan, open questions."
4. **Merge.** When an agent reports: review its summary, `git merge --no-ff <branch>` into `main`,
   run `./scripts/check.sh`. Fix trivial integration issues yourself (wiring, `mod` lines, registry
   entries). If something non-trivial breaks, send it back to the same agent type with the failing output.
5. **Review.** After all tasks of the milestone are merged, spawn `reviewer` on the milestone diff
   (`git diff m<N-1>-done..main`). Route each finding (blocker / should-fix) to the owning agent type as
   a fix task. Nits are optional.
6. **Close.** Update `docs/PROGRESS.md` (status, commits, notes), commit, tag `m<N>-done`.
   If the milestone ends with a gate: stop and ask Igor (see "Human gates").

**Escalation.** A task that fails acceptance twice goes to `architect` for diagnosis (root cause +
plan), then back to an implementation agent. Still failing → stop and ask Igor.
**Never** to get green: delete or weaken tests, add blanket `#[allow]`, `--no-verify`, or skip
the reviewer on `ofm-jobs`.
**Context hygiene.** The lead keeps summaries, not code. After each milestone, it's fine to `/compact`;
`docs/PROGRESS.md` must make a fresh session able to continue.

## Agent roster

| Agent | Model / effort | Typical tasks |
|---|---|---|
| architect | opus / high | `T*.0` stubs + ADRs, escalations |
| core-engineer | sonnet / high | ofm-core, ofm-vfs, ofm-config, ofm-viewer, ofm-search |
| fileops-engineer | opus / high | ofm-jobs engine + copy/move/extract |
| ui-engineer | sonnet / high | ofm-app |
| test-engineer | sonnet / medium | ofm-testkit, suites, benchmarks, screenshots |
| reviewer | opus / high | milestone reviews (read-only) |
| release-engineer | sonnet / medium | packaging, docs, release |

## Milestone overview

| M | Name | Result | Gate |
|---|---|---|---|
| M0 | Bootstrap | Workspace builds, empty window, check + screenshot scripts work | — |
| M1 | Core foundations | Panel model, local listing, config/keymap/themes, test kit | — |
| M2 | UI shell | Two working panels, navigation, tabs, themes, F-key bar, menus | **G1** |
| M3 | File operations | Job engine, copy/move/delete/mkdir/link/attrs, dialogs, torture suite | **G2** |
| M4 | Command line & terminal | Command line, VTE shell, Ctrl+O, cwd sync | — |
| M5 | Viewer & quick view | F3 viewer, Ctrl+Q quick view, F4 external editor | — |
| M6 | Archives | Browse zip/tar/7z read-only, extract via F5 | — |
| M7 | Search & panelize | Alt+F7 find, streaming results, panelize | — |
| M8 | Power features & desktop | Compare dirs, user menu, batch rename, open/clipboard/DnD | **G3** |
| M9 | Hardening | Performance, fuzzing, accessibility, session restore, spec coverage | — |
| M10 | Release | .deb, docs, RC, fixes, v1.0.0 tag | **G4** |

(Archives come before search so "search inside archives" can use the archive provider directly.)

---

## M0 — Bootstrap (lead, sequential)

**T0.1 Workspace skeleton** — lead
- Apply project name/app ID from KICKOFF (rename if not OFM).
- Root `Cargo.toml` workspace (resolver 3, edition 2024, shared `[workspace.dependencies]`,
  `[workspace.lints]` with `clippy::unwrap_used = "warn"` for non-test code via
  `#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]` in each lib).
- Crates from CLAUDE.md with empty `lib.rs`; `ofm-testkit` (publish = false).
- `ofm-app`: `adw::Application` with app ID, one `adw::ApplicationWindow` titled "OFM", 1200×800.
- `crates/ofm-app/clippy.toml` with `disallowed-methods` for `std::fs::*` (read_dir, metadata,
  symlink_metadata, read, read_to_string, write, copy, rename, remove_file, remove_dir_all, create_dir_all)
  and `std::fs::File::open` / `create`.
- `rust-toolchain.toml` (stable, rustfmt, clippy), `.gitignore` (target, *.ofm-part-*), `docs/adr/` index.
- Done when: `./scripts/check.sh` green; `./scripts/screenshot.sh m0` produces a PNG showing the window
  (lead opens it with Read and confirms).

**T0.2 Agents dry run** — lead
- Spawn `test-engineer` with a trivial task ("add a unit test to ofm-core that asserts 1+1=2 on branch
  m0/T0.2-dry-run, run check, report") to verify worktree isolation, permissions and reporting work.
  Merge, then remove the dummy test.
- Done when: merged and reverted cleanly; tag `m0-done`.

---

## M1 — Core foundations

**T1.0 Core interfaces & ADRs** — architect (foreground, on main)
- Stubs per ARCHITECTURE §2 for ofm-core (Location, Entry, EntryKind, CancelToken, KeyChord, CommandId +
  full command list from SPEC, PanelModel API, ModelChange, mask API), ofm-vfs (Vfs trait, ListEvent,
  VfsError, VfsRegistry, Caps), ofm-config (Config, Keymap, Theme structs + load/save signatures),
  ofm-jobs (JobSpec, JobEvent, JobEngine API, Fs trait).
- ADR-0002 FileList as a custom-drawn widget; ADR-0003 shell/terminal model; ADR-0004 copy algorithm;
  ADR-0005 config/theme/keymap file formats (with full example files in `crates/ofm-config/defaults/`).
- `docs/COMMANDS.md`: table of every CommandId, context, title, Far/TC/mc default keys.
- Done when: workspace compiles; ADRs written; lead commits to main.

Parallel group 1.A (after T1.0):

**T1.1 PanelModel** — core-engineer · F02 F03 F06 F10 (logic)
- Owns: `crates/ofm-core/src/panel/**`, `crates/ofm-core/src/mask.rs`.
- Sorting (name natural / ext / size / mtime, reverse, dirs first, `..` always first), hidden toggle,
  quick search (prefix then substring, case-insensitive, next/prev match), filter-as-you-type,
  cursor movement API (up/down/page/home/end/column-wise for Brief), selection by name surviving reload,
  select/deselect by mask, invert, restore last selection, incremental `apply_batch` returning `ModelChange`.
- Done when: proptests (sort is a total order, selection invariants, cursor stays valid after any change),
  sorting 100k entries < 50 ms (criterion bench), check green.

**T1.2 Local VFS + watcher** — core-engineer · F02 F04 F08 (data)
- Owns: `crates/ofm-vfs/src/local/**`, `crates/ofm-vfs/src/watch.rs`, `crates/ofm-vfs/src/pool.rs`.
- Streaming listing (batches 512 / 16 ms), statx with fallback, symlink target kind, user/group name
  cache, free space, permission-denied directory → error event, I/O pool (4 threads), watcher with
  100 ms debounce.
- Done when: 100k-entry listing first batch < 50 ms, full < 1 s on SSD (bench); tests for symlinks,
  broken links, unreadable dir, non-UTF-8 names; check green.

**T1.3 Config, keymaps, themes** — core-engineer · F29 F30 (data) F33 (storage)
- Owns: `crates/ofm-config/**`.
- Loading with defaults + user overrides, atomic save, warnings on invalid files; keychord parser
  (incl. Gray+/Gray−/Gray*, KP keys, F1–F24); presets far/tc/mc complete for every CommandId in
  `docs/COMMANDS.md`; conflict detection; theme schema + Far Blue, Commander Light, Adwaita files per
  SPEC; highlight rule matcher; session/history storage types; live-reload hook (notify).
- Done when: every CommandId has a binding in the far preset (test enforces it); round-trip tests;
  invalid file tests; check green.

**T1.4 Test kit** — test-engineer
- Owns: `crates/ofm-testkit/**`, `scripts/make-test-dirs.sh` (may extend).
- Tree builder DSL (files with content/size/sparse, dirs, symlinks, modes, xattrs, mtimes, invalid
  UTF-8 names), `assert_trees_equal(a, b, Compare { content, mode, mtime, xattrs })`, helpers to find two
  different filesystems for cross-device tests, `FaultPlan` type for ofm-jobs fault injection.
- Done when: documented with examples; self-tests; check green.

**T1.5 Review M1** — reviewer. Fixes routed. Tag `m1-done`.

---

## M2 — UI shell → Gate G1

**T2.0 App skeleton** — architect (foreground, on main)
- `ofm-app` modules per ARCHITECTURE §2: `app.rs`, `dispatch.rs` (key capture → command execution),
  `panel/controller.rs`, `theme/` (palette + CSS generation API), `widgets/file_list/` stub that draws
  plain rows and scrolls, `window/` with `MainWindow` layout regions and extension hooks
  (`install_*` functions) so parallel UI tasks don't edit the same files, event wiring pattern.
- Done when: app shows two panels listing real directories (plain rendering), Tab switches, arrows move;
  check green; lead commits.

Parallel group 2.A:

**T2.1 FileList widget** — ui-engineer · F02 F03 F32 (mouse)
- Owns: `crates/ofm-app/src/widgets/file_list/**`.
- Brief (column-major), Full, Custom columns; header with sort indicator (click to sort); cursor and
  marked rendering, highlight colours, frames (box-drawing look when theme `retro.frames = true`);
  keyboard navigation commands; mouse: click, double-click, Shift/Ctrl click, right-click context
  menu signal, wheel, drag source hook; accessible role + label; HiDPI.
- Done when: 100k entries scroll without visible stutter (manual + frame-time log), screenshots for
  all 3 themes look right (open them), check green.

**T2.2 Window chrome & themes** — ui-engineer · F01 F04 F30 F32 F33 (partial)
- Owns: `crates/ofm-app/src/window/**`, `crates/ofm-app/src/theme/**`, `crates/ofm-app/src/chrome/**`.
- Paned split (ratio persisted), one-panel mode, active panel highlight, status line, F-key bar with
  modifier variants + clicks, F9 menu bar built from the command registry, runtime theme switching and
  generated CSS for standard widgets, session save/restore (paths, view modes, sort, window size).
- Done when: theme switch is instant; restart restores state; screenshots per theme; check green.

**T2.3 Navigation** — ui-engineer · F05 F06 F07 F08 F09 F10
- Owns: `crates/ofm-app/src/navigation/**`, `crates/ofm-app/src/dialogs/{mask,hotlist}.rs`.
- Tabs per panel (new/close/next/prev/lock, restored), quick-search popover + filter mode (typing mode
  from config), history back/forward + Alt+F12 list, hotlist Ctrl+D + editor, mount menu Alt+F1/F2
  (gio::VolumeMonitor, mount via gtk::MountOperation, GVfs mounts, Trash), go up (cursor on previous
  dir), root, swap panels, same dir, refresh, selection commands + mask dialog with history.
- Done when: all listed commands work from keys in the far and tc presets; check green.

**T2.4 UI smoke tests & screenshots** — test-engineer
- Owns: `crates/ofm-app/tests/**`, `scripts/screenshot.sh` (may extend).
- Xvfb smoke tests driving commands on fixture dirs (navigate, sort, tabs, quick search, selection);
  screenshot set `target/screenshots/g1-{far,light,adwaita}-{full,brief}.png`.
- Done when: tests run in `check.sh`; screenshots produced.

**T2.5 Review M2** — reviewer. Fixes. Tag `m2-done`.

**Gate G1 — UI feel.** Lead writes to Igor:
- `cargo run -p ofm-app --release`; try navigation in Far keys, Brief/Full (Ctrl+1/2), sorting,
  tabs, quick search, mount menu, selection with Gray+/Gray−.
- Switch themes; compare Far Blue with Far/far2l and Commander Light with Double Commander; tell me
  what to tune (colours, fonts, spacing).
- `./scripts/make-test-dirs.sh /tmp/ofm-big` and open `/tmp/ofm-big/100k`: is scrolling smooth?
Wait for feedback; turn it into fix tasks (ui-engineer) before M3.

---

## M3 — File operations → Gate G2

**T3.0 Jobs API freeze** — architect (foreground, on main)
- Finalize `ofm-jobs` public API, `Fs` trait + `FaultyFs`, `Question`/`Answer` types (conflict choices,
  error choices, "apply to all"), `Progress`, job titles; UI-facing `JobsController` stub in `ofm-app`.
- Done when: compiles; lead commits.

Parallel group 3.A:

**T3.1 Engine + copy/move** — fileops-engineer · F13 F15 F16 F17
- Owns: `crates/ofm-jobs/src/{engine,plan,copy,move_,fs,progress}.rs` (+ submodules).
- Engine (queued lane + parallel jobs, pause/resume, cancel, question protocol, panic capture,
  progress ≤ 10 Hz), planning (scan, dest-inside-source, same-file, totals), copy algorithm exactly per
  ADR-0004, rename masks, copy-into-same-dir rename, move same-fs via `renameat2(NOREPLACE)`, cross-device
  move (copy + fsync + verify + delete source per item).
- Done when: T3.4 torture suite green for copy/move; review checklist (below) self-checked; check green.

**T3.2 Other operations** — core-engineer · F14 F18 F19 (engine side)
- Owns: `crates/ofm-jobs/src/ops/{delete,mkdir,link,attrs,size}.rs`.
- Trash (incl. other mounts; ask when unavailable), permanent delete (no symlink following,
  per-entry errors), mkdir -p, symlink (absolute/relative computation) and hardlink, chmod/chown
  (recursive option, X bit for dirs only), size calculation (dedupe hardlinks by dev+ino, cancellable).
- Done when: tests for each op incl. errors; check green.

**T3.3 File-operation UI** — ui-engineer · F13 F14 F15 F16 F18 F19 (UI)
- Owns: `crates/ofm-app/src/jobs/**`, `crates/ofm-app/src/dialogs/{copy,conflict,delete,mkdir,link,properties}.rs`.
- Copy/move dialog (editable target defaulting to other panel, mask, queue/parallel, options), conflict
  dialog (both sides' size/date, choices + apply-to-all), delete confirmation (trash vs permanent),
  mkdir, link, properties (chmod octal + checkboxes, owner/group, size calc), jobs view (Ctrl+J) with
  pause/resume/cancel, progress in status line, quit-with-running-jobs prompt, panels auto-refresh
  after ops (watcher) with cursor on the new item.
- Done when: flows work against the merged engine; smoke tests for dialogs; check green.

**T3.4 Torture suite** — test-engineer · F17
- Owns: `crates/ofm-jobs/tests/**`.
- All cases from ARCHITECTURE §5 for copy, move, delete, plus: cancel at random byte offsets (no
  final-name file left, temp removed), `FaultyFs` ENOSPC/EIO/short write at N-th call (dest not
  half-written, source intact on move), overwrite decisions (only chosen files replaced), names with
  newlines/backslashes/leading dashes, 2 GB sparse file stays sparse (allocated blocks check).
  Write first against stubs (failing), then iterate with T3.1/T3.2 as they merge.
- Done when: suite green on main after T3.1 and T3.2; runtime < 3 min.

**T3.5 Deep review of ofm-jobs** — reviewer (must sign off; blocker findings must be fixed)
Checklist: temp-then-rename everywhere; `O_EXCL` + `O_NOFOLLOW`; NOREPLACE unless overwrite chosen;
fsync before source unlink on move; temp cleanup on every error path; no symlink following in delete;
CancelToken checked in all loops; no unwrap; errors carry paths; progress never blocks the worker;
`Fs` used for every syscall (testability).
Tag `m3-done`.

**Gate G2 — trust the copy engine.** Lead writes to Igor:
- On a copy of real data: copy/move a large photo folder within the SSD and to a USB stick (cross-device),
  cancel in the middle, overwrite some files with "newer only", delete to trash and restore from GNOME
  Files, move something back.
- Compare results (lead provides a one-liner: `diff -r` or `rsync -rcn --delete a/ b/`).
Wait for feedback.

---

## M4 — Command line & terminal

**T4.0 Shell integration design** — architect (foreground)
- Finalize ADR-0003: rc injection for bash/zsh/fish emitting OSC 7, idle/busy detection (busy from
  Enter until next OSC 7), cd sync rules, behaviour when the shell exits (respawn), Tab rule
  (non-empty command line → completion, empty → switch panel), `typing` modes.
- Stubs for `cmdline/` and `terminal/` modules; pure helpers in `ofm-core::cmdline`.

Parallel group 4.A:

**T4.1 Terminal** — ui-engineer · F12
- Owns: `crates/ofm-app/src/terminal/**`, `crates/ofm-app/resources/shell/**`.
- VTE widget, shell spawn with injected rc, Ctrl+O toggle (panels hidden ↔ shown), OSC 7 → panel dir,
  panel dir → ` cd -- '…'` when idle, respawn on exit, theme colours for the terminal palette,
  terminal editors launched from F4 run here.
- Done when: works with bash, zsh, fish (whichever are installed; others documented); check green.

**T4.2 Command line** — ui-engineer · F11
- Owns: `crates/ofm-app/src/cmdline/**`.
- Entry with prompt (`~/path$`), persistent history (Ctrl+E/Ctrl+X, Alt+F8 list), path completion,
  insert name / full path / left / right panel path, Enter sends to shell and shows terminal,
  printable-key routing per `typing` mode.
- Done when: all F11 keys work; check green.

**T4.3 Tests** — test-engineer: `ofm-core::cmdline` unit/proptests (quoting incl. `'`, OSC 7 URIs
with percent-encoding, cd parsing), history persistence, Xvfb smoke test: type `cd /tmp` → panel follows.

**T4.4 Review M4** — reviewer. Tag `m4-done`.

---

## M5 — Viewer, quick view, external editor

**T5.0 Viewer API** — architect (short): `ofm-viewer` public API + `PreviewProvider` trait stub.

Parallel group 5.A:

**T5.1 Viewer backend** — core-engineer · F21 (backend)
- Owns: `crates/ofm-viewer/**`.
- Document (mmap + fallback), lazy LineIndex in worker, encodings + detection, wrap helper (by display
  columns), hex rows, search (text/hex/regex, forward/backward, chunk boundaries), go to line/offset.
- Done when: 10 GB sparse file opens in < 100 ms (first screen), bench for 1 GB indexing, encoding
  fixture tests; check green.

**T5.2 Viewer UI + quick view + F4** — ui-engineer · F21 (UI) F22 F23
- Owns: `crates/ofm-app/src/{viewer,quickview}/**`, `crates/ofm-app/src/desktop/editor.rs`.
- Viewer window with custom widget (text/hex, wrap toggle, encoding menu, search dialog, status line,
  keys per SPEC), Ctrl+Q quick view in the other panel (text via ofm-viewer, images decoded in a worker
  via gdk-pixbuf then uploaded as texture, directory summary via CalcSize job), F4 external editor per
  mask (GUI editors detached, terminal editors in VTE), Shift+F4 new file.
- Done when: works on fixtures; screenshots; check green.

**T5.3 Tests** — test-engineer: viewer edge cases, quick-view debounce test, screenshots.
**T5.4 Review M5** — reviewer. Tag `m5-done`.

---

## M6 — Archives (read-only)

**T6.0 Archive API** — architect (short): `ArchiveIndex`, `ArchiveReader`, `PasswordProvider` stubs.

Parallel group 6.A:

**T6.1 Archive provider** — core-engineer · F26
- Owns: `crates/ofm-vfs/src/archive/**`.
- Magic detection; zip (incl. AES/ZipCrypto with password), tar + gz/bz2/xz/zst, 7z (incl. encrypted
  headers → password); index build in worker; zip-slip rejection; corrupt archive → error event.
- Done when: listing tests for every format + malformed inputs; check green.

**T6.2 Extract job** — fileops-engineer · F26
- Owns: `crates/ofm-jobs/src/extract.rs`.
- Extract selected entries/dirs via `ArchiveReader` using the same temp-then-rename + conflict protocol
  as copy; restore mode/mtime; refuse unsafe paths.
- Done when: extraction tests incl. conflicts + cancel; check green.

**T6.3 Archive UI** — ui-engineer · F26
- Owns: `crates/ofm-app/src/panel/archive.rs`, `crates/ofm-app/src/dialogs/password.rs`.
- Enter / Ctrl+PgDn into archives, Backspace at archive root returns with cursor on the archive file,
  F5 from archive panel = extract to other panel, write ops disabled with a clear message, F3 on an
  archive entry extracts to a temp file and opens the viewer, password dialog.
- Done when: smoke tests; check green.

**T6.4 Tests** — test-engineer: fixtures generated in tests (zip/tar via crates; small checked-in 7z
fixtures ≤ 50 KB allowed under `tests/fixtures/`), malformed archives, proptest random bytes → no panic.
**T6.5 Review M6** — reviewer. Tag `m6-done`.

---

## M7 — Search & panelize

**T7.0 Search API** — architect (short): `ofm-search` API, `List` provider API.

Parallel group 7.A:

**T7.1 Search engine** — core-engineer · F24
- Owns: `crates/ofm-search/**`.
- Masks, content (text/regex, case, encoding), size/date filters, follow symlinks, same filesystem,
  inside archives (via ofm-vfs archive), streaming batches, cancel.
- Done when: tests on fixtures; 100k-file tree name search < 2 s; check green.

**T7.2 List provider + command panelize** — core-engineer · F25
- Owns: `crates/ofm-vfs/src/list/**`, `crates/ofm-core/src/panelize.rs`.
- Lists with real paths; "panelize command output" (run a shell command in a worker, parse lines as
  paths relative to the panel dir, ignore non-existing); file ops map to real paths.

**T7.3 Find dialog + panelize UI** — ui-engineer · F24 F25
- Owns: `crates/ofm-app/src/search/**`, `crates/ofm-app/src/dialogs/find.rs`.
- Alt+F7 dialog (all options, history), streaming results list with count, F3/F4 on results, "go to"
  (opens the dir with cursor on the file), "panelize" (F10) into the active panel, panelize-command dialog.

**T7.4 Tests** — test-engineer. **T7.5 Review M7** — reviewer. Tag `m7-done`.

---

## M8 — Power features & desktop integration → Gate G3

Parallel group 8.A (no `T8.0` needed unless the lead sees overlap; then a short architect stub task):

**T8.1 Compare directories** — core-engineer (engine in `crates/ofm-core/src/compare.rs`, content
compare as a job in `crates/ofm-jobs/src/compare.rs`) + ui-engineer follow-up for marking and the
Shift+F2 command · F27.

**T8.2 User menu** — core-engineer (`crates/ofm-config/src/menu.rs`, placeholder expansion with shell
quoting in `crates/ofm-core/src/menu_expand.rs`) + ui-engineer follow-up (F2 menu popover, edit menu
entry in the configured editor, per-directory `.ofm-menu.toml` with a trust prompt the first time) · F28.
Ship default entries: "tar.gz selection", "rsync selection to other panel", "open terminal here".

**T8.3 Batch rename via editor** — core-engineer (`crates/ofm-core/src/batch_rename.rs`: diff of name
lists, conflict/cycle detection with temp names, undo log) + ui-engineer follow-up (temp file → editor,
"Apply" dialog for detached GUI editors, preview dialog, run as `Rename` job, undo command) · F20.

**T8.4 Desktop integration** — ui-engineer · F31 F33
- Owns: `crates/ofm-app/src/desktop/**` (except editor.rs).
- Enter opens with default app via `gtk::FileLauncher`; executables and scripts with shebang run in
  the terminal; clipboard copy/cut/paste compatible with GNOME Files; drag & drop in/out (drop = copy,
  Shift = move); session restore completeness (tabs, cursor positions).

**T8.5 Tests** — test-engineer for all of the above (pure logic heavily; clipboard via Xvfb).
**T8.6 Review M8** — reviewer. Tag `m8-done`.

**Gate G3 — feature complete.** Lead writes a full walkthrough checklist covering F01–F33
(one line per feature: what to press, what should happen) and asks Igor to go through it for a day
of normal use. Turn feedback into fix tasks before M9.

---

## M9 — Hardening

**T9.1 Performance** — test-engineer: measure every quality-bar metric from SPEC (release build),
write `docs/PERF.md` with numbers and flamegraph notes; file fix tasks for misses.
**T9.2 Performance fixes** — core-engineer / ui-engineer as filed.
**T9.3 Robustness** — test-engineer: 10-minute proptest/fuzz runs on archive listing, viewer decoding,
keymap/theme/menu parsing, mask parsing; fix tasks for crashes.
**T9.4 Accessibility & polish** — ui-engineer: accessible names for all controls, cursor announcements,
high-contrast + large-text check, keyboard-only audit of every dialog (focus order, Esc, Enter),
consistent error messages, `tr()` everywhere.
**T9.5 Spec coverage** — reviewer: `docs/SPEC-COVERAGE.md` matrix: for each F-ID → implemented (file),
tested (test name), verified at gate (yes/no). Every gap becomes a task. Tag `m9-done`.

---

## M10 — Release → Gate G4

**T10.1 Packaging** — release-engineer
- `cargo-deb` config: binary, desktop file, icon (simple two-panel SVG + symbolic), AppStream metainfo
  (validated with `appstreamcli validate` if installed), man page `ofm.1`, license files.
- `ofm --version`, `ofm --dump-keys <preset>` (Markdown key reference generated from the keymap).
**T10.2 Docs** — release-engineer: `docs/USER-GUIDE.md` (concepts, keys per preset from `--dump-keys`,
themes, user menu, config files), repository `README.md`, `CHANGELOG.md`.
**T10.3 Release candidate** — lead: version `1.0.0-rc.1`, `cargo build --release --locked`, build the
.deb, install it into a scratch prefix or check the package contents, Xvfb smoke test of the installed
binary, tag `v1.0.0-rc.1`.

**Gate G4 — release candidate.** Igor installs the .deb and uses OFM for a few days. Bugs → fix tasks
(normal protocol) → `1.0.0-rc.N` as needed → final: version `1.0.0`, tag `v1.0.0`, release notes.

---

## Report template (agents → lead)

```
Task: T3.2  Branch: m3/T3.2-other-ops  Commit: abc1234
Summary:
- …
Tests: ./scripts/check.sh → OK (412 passed, 3 ignored: cross-device not available)
Deviations: none | …
Open questions: none | …
```

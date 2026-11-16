# ofm-app: file ownership for parallel tasks (M2, M3)

Rule: a task edits **only** the paths listed for it. Everything under "Frozen core" is changed by
the architect/lead only; if you need an API change there, report it instead of editing.
Cross-module wiring goes through the hooks in `src/hooks.rs` (already calling every module), so
nobody needs to add a `mod` line or a call in a shared file.

| Owner | Paths |
|---|---|
| **T2.1** FileList | `src/widgets/file_list/**` |
| **T2.2** chrome/themes | `src/window/**`, `src/theme/**`, `src/chrome/**` |
| **T2.3** navigation | `src/navigation/**`, `src/dialogs/mask.rs`, `src/dialogs/hotlist.rs` |
| **T2.4** tests | `tests/**` (new files; `tests/shell_skeleton.rs` is the T2.0 baseline, extend it), `scripts/screenshot.sh` |
| Frozen core (T2.0) | `src/{lib,main,app,context,dispatch,keys,hooks,signal,wiring,i18n}.rs`, `src/panel/**`, `src/widgets/mod.rs`, `src/dialogs/mod.rs`, `Cargo.toml` |

Adding a dependency: ask the lead (Cargo.toml is shared). Adding a new file inside your own
directory needs a `mod` line in your own `mod.rs` only.

## Hook points (src/hooks.rs) and who fills them

| Hook | Called | T2.1 | T2.2 | T2.3 |
|---|---|---|---|---|
| `install_commands(&mut CommandRegistry)` | startup, before context state | `widgets::file_list::install_commands` | `window::`, `chrome::install_commands` | `navigation::`, `dialogs::install_commands` (-> `mask`, `hotlist`) |
| `install_state(&Rc<AppContext>)` | after dispatcher is set | - | `window::`, `chrome::install_state` | `navigation::`, `dialogs::install_state` |
| `install_panel_view(&ctx, &PanelView)` | once per side, after the view is built | - | `window::install_panel_view` (panel_view.rs) | `navigation::install_panel_view` (tab bar into `view.tab_bar_slot`, popovers on `view.overlay`) |
| `install_ui(&ctx, &MainWindow)` | once, window complete, before present | - | `window::install_ui` (main_window.rs), `chrome::install_ui` (fill `menubar_slot`, `status_slot`, `fkeybar_slot`) | `navigation::install_ui` |

State of a module lives in `ctx.ext` (`Extensions`, type-keyed): insert in `install_state`, read
with `ctx.ext.get::<T>()`. Do not add fields to `AppContext`.

## Command ownership (a command is registered by exactly one module; a test fails on duplicates)

- **T2.1**: `panel.cursor_*`, `panel.page_*`, `panel.home/end` (stub exists), `panel.view_brief/full/custom`,
  `panel.sort_*`, `panel.toggle_hidden`.
- **T2.2**: `app.*` (quit exists; menu, theme_next, reload_config, preferences stub), `window.fullscreen`,
  `panel.switch` (exists), `panel.toggle_other`, one-panel mode, split ratio.
- **T2.3**: `panel.go_up/go_root/go_home/goto_path/open/enter_dir/refresh/swap/same_dir`, `panel.history_*`,
  `panel.hotlist*` (dialog in `dialogs/hotlist.rs`), `panel.drive_menu_*`, `panel.quicksearch*`, `panel.filter`,
  `panel.cancel`, `tab.*`, `select.*` (mask dialog in `dialogs/mask.rs`).
- Unowned in M2 (stay `NotImplemented`): everything in `file.*`, `view.*`, `cmdline.*`, `search.*`, ...

## Patterns every task must follow

- **Keys**: never handle keys in widgets (except text entry). Add the command + keymap entry; the
  capture-phase controller from `dispatch::install_key_controller` does the rest. Show every
  dialog with `navigation::modal::present` (never `adw::Dialog::present` directly): it enters the
  modal state (keys resolve in the `dialog` keymap section, then `global`) and attaches the same
  dispatcher controller to the dialog itself (`WindowKind::Dialog`), see "Keys in dialogs" below.
- **Background work**: `wiring::run_on_pool(&ctx.services.pool, |tx| { blocking I/O; tx.send_blocking(..) })`
  then `wiring::attach(rx, |msg| ControlFlow::Continue(()), || {})`. Drop results of stale
  generations (see `PanelController::load`). No `std::fs`/`stat` on the main thread (clippy.toml enforces).
- **Observers**: `Signal<T>` (`PanelController::changed`, `PanelHost::{active_changed, tabs_changed}`,
  `AppContext::active_changed`, `ThemeHost::changed`). Use `Weak` in closures that capture widgets or ctx.
- **Strings**: `i18n::tr("...")` for user-facing text. **Paths**: `PathBuf`/`OsString`, lossy only for display.
- **Tests**: UI tests drive `ctx.execute(cmd::...)` (see `tests/shell_skeleton.rs`); GTK tests skip
  themselves without a display; unimplemented ones are `#[ignore = "T..."]`.

## What T2.0 left to you (TODO list)

- **T2.1**: the stub draws one column without header/frames, no mouse, no `gtk::Scrollable`, creates
  Pango layouts per row per frame (cache them), geometry reports `columns = 1`. `FileList::{bind,
  set_palette, set_active}` are the stable surface used by `window/panel_view.rs`.
- **T2.2**: `SystemColors` is `fallback()` only: fill it from `adw::StyleManager` (accent, dark mode,
  view/header colours) and re-run on `notify::dark`/accent changes via `ThemeHost::set_system_colors`.
  Session save/restore is not wired: **validate `Session.active_panel` (clamp to 0/1) and
  `active_tab` against the tab count on load** (M1 deferred finding). Window size/maximized, split
  ratio. Theme switching command + `Theme::discover` list are available in `AppContext::paths`.
  The F-key bar etc. fill empty slots; `ThemeHost::apply_css` is called once at startup.
- **T2.3**: directory watching (`ofm_vfs::watch::watch_dir`, 100 ms debounce, `WatchEvent::{Changed,
  Rescan, DirGone}`) is not wired to `PanelController` (nothing in `panel/` may be edited by you:
  start the watcher in `navigation/` per tab and apply `PanelModel::apply_update` through
  `PanelController::mutate_view`; ask the lead if you need a controller hook). Pending-cursor
  handling (M1 deferred finding "cursor after begin_load") is done in `PanelController::load` via
  `LoadOpts::cursor_name`: use it for go-up. Mouse/active-panel-on-click is T2.1 -> T2.2.
- **Not verified in T2.0**: real key events (xdotool is not installed here); `keys.rs` is unit-tested
  on GDK key values, and the dispatch path is covered through `AppContext::execute`. T2.4: if
  `xdotool` becomes available, add a real key-event smoke test.

## Deferred review findings (M1) status

| Finding | Status |
|---|---|
| `Key::Char('+')` unparseable / GDK mapping for `Gray+` | done in `keys.rs` (KP_Add -> `Gray+`; Shift re-translated to base key, so `Shift+=`) |
| Pending cursor after `begin_load` on large dirs | done in `PanelController` (`pending_cursor`, cleared on user move) |
| Validate `Session.active_panel/active_tab` on load | open -> T2.2 (session restore) |

---

# M3 (file operations): ownership and hooks

T3.0 added the skeleton below. Same rules as above: edit only your own paths; cross-module wiring
only through `src/hooks.rs` (already calls `jobs::install_commands/install_state/install_ui`).

| Owner | Paths |
|---|---|
| **T3.3** file-op UI | `src/jobs/**`, `src/dialogs/{copy,conflict,delete,mkdir,link,properties}.rs`, new files `tests/jobs_*.rs` |
| **T3.1/T3.2/T3.4** | no `ofm-app` files (`crates/ofm-jobs/**` only, see below) |
| Frozen core (T3.0) | everything in "Frozen core" above (now incl. `src/panel/**` with the watcher), `src/dialogs/mod.rs`, `src/jobs/mod.rs` hook signatures, `Cargo.toml` (ask the lead) |

`ofm-jobs` ownership (see `crates/ofm-jobs/src/lib.rs`): T3.1 `engine, plan, copy, move_, progress,
fs/real`; T3.2 `ops/{delete,mkdir,link,attrs,size,trash}` (+ the `trash` dependency); T3.4 `tests/**`;
frozen (architect): `lib, spec, event, error, ctx, runner, fs/mod, fs/faulty, ops/mod`. API contract:
`docs/adr/0006-jobs-api.md`.

## Hook table (M3)

| Hook | T3.3 fills |
|---|---|
| `install_commands` | `jobs::install_commands`: the commands below, each registered exactly once |
| `install_state` | `jobs::install_state` (done: `JobsController` in `ctx.ext`; fetch with `jobs::controller(&ctx)`) |
| `install_ui` | `jobs::install_ui`: jobs view (Ctrl+J), status-line progress, quit-with-running-jobs prompt |
| `install_panel_view` | not used |

## Command ownership (M3)

- **T3.3**: `file.copy`, `file.move`, `file.rename`, `file.mkdir`, `file.delete`, `file.delete_permanent`,
  `file.link`, `file.properties`, `file.calc_size`, `app.jobs`, `jobs.pause_resume`, `jobs.cancel`,
  `jobs.clear_finished`.
- Still unowned (`NotImplemented`): `file.new_file`, `file.batch_rename*` (M8), `file.clipboard_*`,
  `file.copy_path`, `file.open_with` (desktop integration milestone), `file.view/edit` (M5).

## `JobsController` (src/jobs/controller.rs, T3.3 implements the two `todo!("T3.3")` bodies)

- `submit(&Rc<Self>, &Rc<AppContext>, JobSpec, RunMode) -> JobId`: lazily creates `JobEngine::new()`,
  attaches the pump (`wiring::attach(engine.events(), ..)` with Weak refs) once, submits.
- `handle_event(&Rc<Self>, &Rc<AppContext>, JobEvent)`: the pump body; also the test entry point:
  tests feed synthetic `JobEvent`s (build `Ask` with `ofm_core::Responder::channel()`), no engine
  or threads needed. Updates the job table, emits `JobsEvent`, opens `dialogs::conflict::present` for
  `Ask`, refreshes/focuses panels on `Finished` (`Report::created`).
- `set_engine(JobEngine)` for tests (`JobEngine::with_fs(Arc<FaultyFs>)`; needs `ofm-jobs` with the
  `testing` feature in `[dev-dependencies]`: ask the lead to add it).
- `pause/resume/cancel/cancel_all/jobs/has_unfinished/clear_finished` work today.
- The UI must honour `ConflictKind::allows` (disable forbidden choices) and answer every `Ask` exactly
  once (closing a dialog = drop the responder = abort the job).

## Directory watching in `PanelController` (frozen core, done in T3.0)

`PanelController` subscribes to `ofm_vfs::watch` for its local directory and applies changes through
`PanelModel::apply_update` (emits `PanelEvent::Model`). Nobody else starts watchers.
- Lifecycle: started by `load` (before the listing is queued) when the directory changes, kept across
  reloads of the same directory, stopped on navigation to a non-local location and on `shutdown`
  (tab close). Starting runs on the I/O pool (no syscalls on the main thread).
- Events during a listing are buffered and flushed at `Done`; `Changed` names are re-stat-ed on the
  pool, one batch in flight at a time, stale results (load generation changed) are dropped; `Rescan`
  -> `reload()`; `DirGone` -> navigates to the nearest existing ancestor.
- After an operation: `focus_name(name)` (cursor on `name` as soon as it is listed, waits max 5 s,
  cancelled by a user cursor move) for local panels; `reload_and_focus(name)` for locations the
  watcher does not cover (archive/list). `watching()` tells whether a watch is established.
- Known limitation: while the job still writes, `*.ofm-part-*` temp names are visible in the panel
  (ADR-0004 consequence); they vanish on the rename. Do not special-case them in the model.
- Tests: `tests/panel_watch.rs` (headless; private glib `MainContext` per test).

## Deferred M1 finding closed by T3.0

`FsFile::try_clone_from`/`copy_range_from` now go through `FsFile::as_fd`, so `FaultyFs` intercepts them.

## Keys in dialogs (dispatcher on every dialog)

libadwaita >= 1.7 (GTK >= 4.18) marks a presented `adw::Dialog` with `gtk_widget_set_limit_events`:
key events targeted inside the dialog are **not** propagated to the main window, so the window's
capture controller never sees them. Therefore `modal::present` calls
`dispatch::install_key_controller(dialog, ctx, WindowKind::Dialog)` for every dialog.
- Resolution: `dialog` section, then `global` (Ctrl+J = `app.jobs` closes the jobs view).
- Text entries: `dispatch::entry_wants_key` lets plain typing/editing keys (letters, Space,
  Backspace, Del, Home/End, Left/Right, Ctrl+A/C/V/X/Z/Y) reach a focused entry; Enter, Esc, F-keys
  and other chords go to the keymap, so Enter in an entry is `dialog.ok`.
- A dialog's `on_ok` (from `modal::present`) decides what Enter does; dialogs with buttons call
  `jobs::ui::activate_focused_button` first, so Enter on a focused Cancel button is Cancel.
- Alt+mnemonics are not bound in `dialog`/`global`, so they stay native GTK behaviour. Keep the
  mnemonics of one dialog unique.
- No double dispatch: with an older libadwaita (no event limit, e.g. Ubuntu 24.04) the window
  controller runs first (capture is top-down), resolves in the same `dialog` section and stops the
  event; with the limit only the dialog's controller runs. `install_key_controller` is idempotent
  per widget (named controller `ofm-dispatch`).
- Tests: `tests/real_binary_fileops.rs` presses real keys inside dialogs (Enter, F2, Ctrl+A, Del,
  Ctrl+J); synthetic tests that call `ctx.execute` cannot catch a broken key path.

---

# M4 (command line & terminal): ownership and hooks

T4.0 froze the skeletons and interfaces below (ADR-0003 holds the full rules). Same rules as the
M2/M3 sections: edit only your own paths; cross-module wiring only through `src/hooks.rs`
(already calling `terminal::*` and `cmdline::*` for all four hooks — no shared file needs
editing). Pure logic lives in `ofm-core::cmdline` (also frozen; T4.3 extends its tests only).

| Owner | Paths |
|---|---|
| **T4.1** terminal | `src/terminal/**`, `crates/ofm-app/resources/shell/**` (rc wrappers) |
| **T4.2** command line | `src/cmdline/**` |
| **T4.3** tests | new `tests/cmdline_*.rs`, `tests/terminal_*.rs`; extend `crates/ofm-core/src/cmdline/proptests.rs` |
| Frozen (lead) | everything above, `src/{lib,hooks}.rs`, `Cargo.toml`, `crates/ofm-config/**`, keymap presets |

## Hook table (M4)

| Hook | T4.1 (`terminal`) | T4.2 (`cmdline`) |
|---|---|---|
| `install_commands` | `app.terminal_toggle` | the ten `cmdline.*` ids (currently debug-log stubs) |
| `install_state` | `ShellHost` + `TerminalHandle` in `ctx.ext` (done) | `CmdlineState` in `ctx.ext` (done) |
| `install_panel_view` | per-tab `PanelEvent::Location` → cd sync | reserved |
| `install_ui` | VTE widget in `win.terminal_slot`, shell spawn, OSC 7 wiring, respawn | entry in `win.cmdline_slot`, history, typing-mode routing |

## Command ownership (M4)

- **T4.1**: `app.terminal_toggle`.
- **T4.2**: `cmdline.history_prev`, `cmdline.history_next`, `cmdline.history_list`,
  `cmdline.insert_name`, `cmdline.insert_fullpath`, `cmdline.insert_left_path`,
  `cmdline.insert_right_path`, `cmdline.execute`, `cmdline.clear`, `cmdline.complete`.

## The T4.1 ↔ T4.2 interface (frozen; defined in `src/terminal/mod.rs`)

- `terminal::handle(ctx) -> Option<Rc<TerminalHandle>>`: `send_line(&[u8])` (appends `\n` if
  missing; flips the shell to Busy), `show()` / `hide()` (Ctrl+O view), `is_visible()`,
  `shell_state()`. The command line fetches it lazily inside its handlers — no `install_state`
  ordering dependency.
- `terminal::shell_host(ctx) -> Option<Rc<ShellHost>>`: `ShellState` (`Idle | Busy | Exited`) +
  `changed: Signal<ShellState>`; the command line shows a busy hint from it.
- The command line keeps `AppContext::set_cmdline_has_text` updated; the terminal reads
  `cmdline_has_text()` before sending a sync `cd`.
- Never spawn shells or touch VTE from `cmdline/`; never edit widgets in `cmdline_slot` from
  `terminal/` (chrome already put the slot CSS there).

## M4 lead follow-ups (DONE on main, applied after T4.0)

1. `AppContext::set_terminal_focused(bool)` / `terminal_focused()`; `dispatch::resolve` consults only
   the `global` keymap section while it is true. **T4.1 sets it** from a focus controller on the VTE
   widget (focus-enter -> true, focus-leave -> false).
2. `[terminal] return_to_panels` (bool, default `false`) in `ofm-config` + `defaults/config.toml`.

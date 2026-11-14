# ofm-app: file ownership for parallel tasks (M2)

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
  window-level capture controller in `dispatch.rs` does the rest. While a modal dialog is open hold
  `let _g = ctx.enter_modal();` so keys resolve in the `dialog` keymap section.
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

//! The command line under the panels (F11, ADR-0003). Owner: **T4.2** (`cmdline/**`); T4.0
//! froze the hook signatures and the pure helpers in [`ofm_core::cmdline`] — T4.2 implements
//! the UI without changing them (report to the lead if something is missing).
//!
//! Behaviour summary (rules and rationale in ADR-0003):
//! - A GTK entry in `win.cmdline_slot` with a `~/path$`-style prompt of the active panel's
//!   directory; focus follows the `typing` keymap mode (below).
//! - Every text change updates `AppContext::set_cmdline_has_text` — that flag switches key
//!   resolution to the `cmdline` keymap section (Enter/Esc/Tab/Ctrl+Space behave differently
//!   with text in the line; an empty line falls through to `panels`, so Tab switches panels
//!   and Enter opens the entry under the cursor).
//! - Enter: [`ofm_core::cmdline::parse_cd_target`] — a plain resolvable `cd` navigates the
//!   panel only (no shell round-trip); anything else goes to
//!   `terminal::handle(ctx).send_line(quoted + b"\n")` followed by `show()` (the terminal is
//!   revealed while the command runs; when the panels come back is a setting, default: on
//!   Ctrl+O). The line is pushed into the history ring.
//! - History: Ctrl+E / Ctrl+Shift+E step, Alt+F8 opens the list; persisted through
//!   `navigation::store::HistoryStore` (`history.toml`); [`ofm_core::cmdline::HistoryRing`]
//!   is the in-memory model.
//! - Completion (`cmdline.complete`, Ctrl+Space and Tab with a non-empty line): split the line
//!   at the last `/` of the word under the cursor, list that directory **on the I/O pool**
//!   (`wiring::run_on_pool`, invariant 2 — may be a different directory than the panel's),
//!   then [`ofm_core::cmdline::complete_in`] over the names; a unique match gets a trailing
//!   `/` for directories, ambiguous matches extend by the common prefix and list the rest.
//! - Insert commands (Ctrl+Enter name, Ctrl+F full path, Ctrl+[ / Ctrl+] panel paths) insert
//!   [`ofm_core::cmdline::shell_quote`] text at the cursor.
//! - `typing` modes (ADR-0005): `command_line` (Far/mc) — a bubble-phase key controller (added
//!   **after** `navigation::quicksearch`'s, which owns Alt+letter in this mode) routes plain
//!   printable keys that no command claimed into the entry; `quick_search` (TC) — plain keys
//!   stay with quick search, the entry is reached with the mouse or the history keys. Once
//!   the entry has focus, the text-entry exception in `dispatch::entry_wants_key` applies.
//! - The shell's phase is read from `terminal::shell_host(ctx)` (e.g. to show a busy hint);
//!   never spawn shells from here.

use std::rc::Rc;

use ofm_core::cmd;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::{MainWindow, PanelView};

/// State of the command line, stored in `ctx.ext` by [`install_state`]. T4.2 adds its fields
/// (the entry, the [`ofm_core::cmdline::HistoryRing`], the completion list popover); this
/// placeholder keeps the `ctx.ext` type stable from day one.
#[derive(Default)]
pub struct CmdlineState {
    /// T4.2: widgets and history go here.
    _reserved: (),
}

impl CmdlineState {
    /// Empty state (the real one is built in `install_ui`, when the window exists).
    pub fn new() -> Self {
        Self::default()
    }
}

/// The window's [`CmdlineState`] (`None` only before [`install_state`] ran).
pub fn state(ctx: &AppContext) -> Option<Rc<CmdlineState>> {
    ctx.ext.get::<CmdlineState>()
}

/// Register the `cmdline.*` commands (F11). Stubs until T4.2; following the project convention
/// that unimplemented commands log at debug level. Owned here: `cmdline.history_prev`,
/// `cmdline.history_next`, `cmdline.history_list`, `cmdline.insert_name`,
/// `cmdline.insert_fullpath`, `cmdline.insert_left_path`, `cmdline.insert_right_path`,
/// `cmdline.execute`, `cmdline.clear`, `cmdline.complete`.
pub fn install_commands(reg: &mut CommandRegistry) {
    for id in [
        cmd::CMDLINE_HISTORY_PREV,
        cmd::CMDLINE_HISTORY_NEXT,
        cmd::CMDLINE_HISTORY_LIST,
        cmd::CMDLINE_INSERT_NAME,
        cmd::CMDLINE_INSERT_FULLPATH,
        cmd::CMDLINE_INSERT_LEFT_PATH,
        cmd::CMDLINE_INSERT_RIGHT_PATH,
        cmd::CMDLINE_EXECUTE,
        cmd::CMDLINE_CLEAR,
        cmd::CMDLINE_COMPLETE,
    ] {
        reg.register(id, move |_ctx| {
            tracing::debug!("{id}: not implemented yet (T4.2)");
        });
    }
}

/// Create the [`CmdlineState`] in `ctx.ext` (the entry itself is built in `install_ui`).
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(CmdlineState::new()));
}

/// Reserved for T4.2 (e.g. reacting to panel changes for the prompt and completion context);
/// the panel model is read through `ctx.active_controller()` directly.
pub fn install_panel_view(_ctx: &Rc<AppContext>, _view: &PanelView) {}

/// Build the entry into `win.cmdline_slot`, keep `set_cmdline_has_text` in sync, load/save the
/// history and install the `command_line` typing-mode key routing. Reserved for T4.2.
pub fn install_ui(_ctx: &Rc<AppContext>, _win: &MainWindow) {}

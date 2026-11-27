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
//! - History: Ctrl+E / Ctrl+Shift+E step (the draft text survives the excursion), Alt+F8 opens
//!   the list; persisted through `navigation::store::HistoryStore` (`history.toml`);
//!   [`ofm_core::cmdline::HistoryRing`] is the in-memory model.
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
//!   stay with quick search, the entry is reached with the mouse or the history keys. Once the
//!   entry has focus, the text-entry exception in `dispatch::entry_wants_key` applies.
//! - The shell's phase is read from `terminal::shell_host(ctx)` (a subtle busy marker in the
//!   prompt row); never spawn shells from here.
//!
//! Module map: [`prompt`] / [`exec`] / [`complete`] / [`history_nav`] / [`typing`] hold the
//! pure, unit-tested decisions; [`ui`] builds the widgets and the command bodies.

mod complete;
mod exec;
mod history_nav;
mod prompt;
mod return_panel;
mod typing;
mod ui;

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};

use ofm_core::cmd;
use ofm_core::cmdline::HistoryRing;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::navigation::store::HistoryStore;
use crate::panel::PanelController;
use crate::signal::SignalId;
use crate::window::{MainWindow, PanelView};

/// State of the command line, stored in `ctx.ext` by [`install_state`]: the widgets (after
/// [`install_ui`]) plus the in-memory history and its stepping position.
pub struct CmdlineState {
    /// Widgets once [`install_ui`] built them; commands are no-ops before that.
    pub(crate) widgets: RefCell<Option<Rc<ui::Widgets>>>,
    /// In-memory command history, newest first (loaded from `history.toml` in [`install_ui`]).
    pub(crate) ring: RefCell<HistoryRing>,
    /// Ctrl+E / Ctrl+Shift+E stepping (the draft text survives the excursion).
    pub(crate) nav: RefCell<history_nav::HistoryNav>,
    /// Completion generation counter: a pool result is applied only while it still matches.
    pub(crate) generation: Cell<u64>,
    /// The history store, filled once in [`install_ui`] (start-up read).
    pub(crate) store: OnceCell<Rc<HistoryStore>>,
    /// The prompt's `PanelEvent::Location` subscription; reconnected when the active tab moves.
    pub(crate) loc_conn: RefCell<Option<(Weak<PanelController>, SignalId)>>,
}

impl Default for CmdlineState {
    fn default() -> Self {
        Self::new()
    }
}

impl CmdlineState {
    /// Empty state (the widgets are built in [`install_ui`], when the window exists).
    pub fn new() -> Self {
        CmdlineState {
            widgets: RefCell::new(None),
            ring: RefCell::new(HistoryRing::default()),
            nav: RefCell::new(history_nav::HistoryNav::new()),
            generation: Cell::new(0),
            store: OnceCell::new(),
            loc_conn: RefCell::new(None),
        }
    }
}

/// The window's [`CmdlineState`] (`None` only before [`install_state`] ran).
pub fn state(ctx: &AppContext) -> Option<Rc<CmdlineState>> {
    ctx.ext.get::<CmdlineState>()
}

/// Register the `cmdline.*` commands (F11). Owned here: `cmdline.history_prev`,
/// `cmdline.history_next`, `cmdline.history_list`, `cmdline.insert_name`,
/// `cmdline.insert_fullpath`, `cmdline.insert_left_path`, `cmdline.insert_right_path`,
/// `cmdline.execute`, `cmdline.clear`, `cmdline.complete`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::CMDLINE_HISTORY_PREV, ui::history_prev);
    reg.register(cmd::CMDLINE_HISTORY_NEXT, ui::history_next);
    reg.register(cmd::CMDLINE_HISTORY_LIST, ui::history_list);
    reg.register(cmd::CMDLINE_INSERT_NAME, ui::insert_name);
    reg.register(cmd::CMDLINE_INSERT_FULLPATH, ui::insert_fullpath);
    reg.register(cmd::CMDLINE_INSERT_LEFT_PATH, |ctx| {
        ui::insert_panel_path(ctx, crate::panel::LEFT)
    });
    reg.register(cmd::CMDLINE_INSERT_RIGHT_PATH, |ctx| {
        ui::insert_panel_path(ctx, crate::panel::RIGHT)
    });
    reg.register(cmd::CMDLINE_EXECUTE, ui::execute);
    reg.register(cmd::CMDLINE_CLEAR, ui::clear);
    reg.register(cmd::CMDLINE_COMPLETE, ui::complete);
}

/// Create the [`CmdlineState`] in `ctx.ext` (the entry itself is built in `install_ui`).
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(CmdlineState::new()));
}

/// Per-panel wiring: tab switches re-point the prompt's Location subscription at the new
/// active tab; the panel model is read through `ctx.active_controller()` directly.
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    ui::install_panel_view(ctx, view);
}

/// Build the entry into `win.cmdline_slot`, keep `set_cmdline_has_text` in sync, load/save the
/// history and install the `command_line` typing-mode key routing.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    ui::install(ctx, win);
}

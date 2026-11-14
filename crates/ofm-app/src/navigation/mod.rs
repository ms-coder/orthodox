//! Panel navigation features. Owner: T2.3 (everything under `navigation/`, plus
//! `dialogs/mask.rs` and `dialogs/hotlist.rs`).
//!
//! - `goto`: go up/root/home/path, open/enter, refresh, swap, same dir; every navigation goes
//!   through [`goto::navigate`] (tab-lock rule, history, "cursor on the directory we came from").
//! - `tabs`: `tab.*` commands and the tab bar; `session`: tabs <-> session file data.
//! - `quicksearch`: quick-search / filter bar and typing mode.
//! - `history`: back/forward and the Alt+F12 list; `mount`: Alt+F1/F2 drive menu.
//! - `selection`: `select.*` (the mask dialog is `dialogs::mask`; the hotlist `dialogs::hotlist`).
//! - Plumbing: `modal` (dialog stack + `dialog.ok`/`dialog.cancel`), `chooser` (pick-one list
//!   dialog), `store` (async-persisted `history.toml`), `style` (CSS from the theme palette).
//!
//! Refresh is manual (`PanelController::reload`): there is no directory watcher yet.

pub mod chooser;
pub mod goto;
pub mod history;
pub mod modal;
pub mod mount;
pub mod quicksearch;
pub mod selection;
pub mod session;
pub mod store;
pub mod style;
pub mod tabs;

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::{MainWindow, PanelView};

/// Register the commands listed in OWNERSHIP.md for T2.3 (the dialog-opening ones are in
/// `dialogs::install_commands`).
pub fn install_commands(reg: &mut CommandRegistry) {
    modal::install_commands(reg);
    goto::install_commands(reg);
    history::install_commands(reg);
    mount::install_commands(reg);
    quicksearch::install_commands(reg);
    selection::install_commands(reg);
    tabs::install_commands(reg);
}

/// Create navigation state in `ctx.ext`.
pub fn install_state(ctx: &Rc<AppContext>) {
    modal::install_state(ctx);
    store::HistoryStore::ensure(ctx);
    style::NavStyle::install(ctx);
    quicksearch::install_state(ctx);
    tabs::install_state(ctx);
}

/// Decorate one panel view: tab bar into `view.tab_bar_slot`, quick-search bar on `view.overlay`.
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    tabs::install_panel_view(ctx, view);
    quicksearch::install_panel_view(ctx, view);
}

/// Window-level typing handler for quick search.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    quicksearch::install_ui(ctx, win);
}

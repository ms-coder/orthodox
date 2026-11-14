//! Panel navigation features. Owner: T2.3 (everything under `navigation/`, plus
//! `dialogs/mask.rs` and `dialogs/hotlist.rs`). Empty skeleton from T2.0; suggested split:
//! `tabs.rs` (F06; tab bar widget + `tab.*` commands), `history.rs` (F07; back/forward + Alt+F12
//! popup), `quicksearch.rs` (F08; popover + filter mode), `selection.rs` (F09; `select.*`),
//! `mount.rs` (F10; Alt+F1/F2 drive menu via gio::VolumeMonitor), `goto.rs` (go up/root/home,
//! enter, open, swap, same dir, refresh).
//!
//! Building blocks already provided by T2.0:
//! - `PanelHost::{add_tab, close_tab, set_active, tabs, active}` and `PanelController::{load,
//!   reload, cursor_move, mutate, mutate_view, history}` with `LoadOpts { cursor_name,
//!   keep_selection, skip_history }` (go-up with the cursor on the directory we came from).
//! - `PanelModel` selection/quick-search/filter methods (ofm-core).
//! - `AppContext::ext` for state, `AppContext::enter_modal()` while a dialog is open.
//!
//! Do not edit `panel/`, `context.rs` or `dispatch.rs`; if you need an API change, report it.

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::window::{MainWindow, PanelView};

/// Register the commands listed in OWNERSHIP.md for T2.3.
pub fn install_commands(_reg: &mut CommandRegistry) {}

/// Create navigation state (hotlist, filters, ...) in `ctx.ext`.
pub fn install_state(_ctx: &Rc<AppContext>) {}

/// Decorate one panel view: tab bar into `view.tab_bar_slot`, quick-search popover on
/// `view.overlay`.
pub fn install_panel_view(_ctx: &Rc<AppContext>, _view: &PanelView) {}

/// Window-level UI of navigation (none expected).
pub fn install_ui(_ctx: &Rc<AppContext>, _win: &MainWindow) {}

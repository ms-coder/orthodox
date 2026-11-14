//! Dialogs. Each file has one owner (see OWNERSHIP.md): `mask.rs` and `hotlist.rs` belong to T2.3
//! now; later milestones add `copy_move.rs`, `conflict.rs`, `delete.rs`, `mkdir.rs`, ... as new
//! files and one `mod` line plus one call in each hook below (those two lines are the only
//! shared edits, made by the lead when the milestone starts).
//!
//! A dialog is a plain function/struct that builds an `adw::Dialog` (or window), holds
//! `ctx.enter_modal()` while shown, and runs commands via `ctx.execute`.

pub mod hotlist;
pub mod mask;

use std::rc::Rc;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;

/// Register the commands that open dialogs (`select.by_mask`, `panel.hotlist`, ...).
pub fn install_commands(reg: &mut CommandRegistry) {
    mask::install_commands(reg);
    hotlist::install_commands(reg);
}

/// Create dialog state (mask history, hotlist) in `ctx.ext`.
pub fn install_state(ctx: &Rc<AppContext>) {
    mask::install_state(ctx);
    hotlist::install_state(ctx);
}

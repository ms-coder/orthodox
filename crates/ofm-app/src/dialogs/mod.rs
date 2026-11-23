//! Dialogs. Each file has one owner (see OWNERSHIP.md): `mask.rs` and `hotlist.rs` belong to T2.3;
//! `copy.rs`, `conflict.rs`, `delete.rs`, `mkdir.rs`, `link.rs`, `properties.rs` to T3.3 (M3 file
//! operations; they register nothing here: their commands are installed by `jobs::install_commands`).
//! Later milestones add files plus one `mod` line (made by the architect when the milestone starts).
//!
//! A dialog is a plain function/struct that builds an `adw::Dialog` (or window), holds
//! `ctx.enter_modal()` while shown, and runs commands via `ctx.execute`.

pub mod batchrename_choice;
pub mod batchrename_preview;
pub mod batchrename_prompt;
pub mod compare;
pub mod conflict;
pub mod copy;
pub mod delete;
pub mod hotlist;
pub mod link;
pub mod mask;
pub mod mkdir;
pub mod password;
pub mod properties;
pub mod usermenu_input;
pub mod usermenu_menu;
pub mod usermenu_trust;

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

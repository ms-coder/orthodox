//! Window-level commands (T2.2 owns this file): `app.quit`, `window.fullscreen`, `panel.switch`,
//! `panel.toggle_other`, and the `dialog.ok` / `dialog.cancel` routing for modal surfaces.

use std::rc::Rc;

use gtk::prelude::*;
use ofm_core::cmd;

use super::{layout, modal_keys, session};
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::theme;

/// Register the window commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    // Closing goes through the close request, which saves the session first.
    reg.register(cmd::APP_QUIT, |ctx| {
        if let Some(w) = ctx.window() {
            w.window.close();
        }
    });
    reg.register(cmd::PANEL_SWITCH, |ctx| {
        ctx.set_active_side(ctx.other_side());
    });
    reg.register(cmd::PANEL_TOGGLE_OTHER, layout::toggle_one_panel);
    reg.register(cmd::WINDOW_FULLSCREEN, |ctx| {
        if let Some(w) = ctx.window() {
            w.window.set_fullscreened(!w.window.is_fullscreen());
        }
    });
    modal_keys::install_commands(reg);
}

/// Create window state: modal-key stack, session (restores the panels), layout, theme list.
pub fn install_state(ctx: &Rc<AppContext>) {
    modal_keys::install_state(ctx);
    session::install_state(ctx);
    theme::registry::install_state(ctx);
}

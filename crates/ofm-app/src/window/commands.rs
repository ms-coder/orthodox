//! Window-level commands (T2.2 owns this file): `app.quit`, `window.fullscreen`, `panel.switch`,
//! `panel.toggle_other`. (`panel.swap` and `dialog.ok`/`dialog.cancel` belong to navigation.)

use std::rc::Rc;

use gtk::prelude::*;
use ofm_core::cmd;

use super::{layout, session};
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::theme;
use crate::widgets::file_list;

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
}

/// Create window state: session (restores the panels), layout, theme list.
pub fn install_state(ctx: &Rc<AppContext>) {
    file_list::configure(&ctx.config.borrow().panels);
    session::install_state(ctx);
    theme::registry::install_state(ctx);
}

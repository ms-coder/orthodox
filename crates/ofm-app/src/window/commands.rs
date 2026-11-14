//! Window-level commands (T2.2 owns this file). T2.0 ships `app.quit` and `panel.switch`.

use std::rc::Rc;

use gtk::prelude::*;
use ofm_core::cmd;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;

/// Register `app.quit`, `window.fullscreen`, `panel.switch`, `panel.toggle_other`, `panel.swap`
/// (T2.2 adds the latter three and the rest of `app.*`).
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::APP_QUIT, |ctx| {
        if let Some(w) = ctx.window() {
            w.window.close();
        }
    });
    reg.register(cmd::PANEL_SWITCH, |ctx| {
        ctx.set_active_side(ctx.other_side());
    });
}

/// Create window state (split ratio, one-panel mode) in `ctx.ext`.
pub fn install_state(_ctx: &Rc<AppContext>) {}

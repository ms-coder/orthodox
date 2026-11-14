//! Commands acting on the list of the active panel (T2.1 owns this file).
//! T2.0 registers the cursor commands; T2.1 adds view-mode, sort and hidden-toggle commands
//! (`panel.view_*`, `panel.sort_*`, `panel.toggle_hidden`, `select.*` stay with T2.3).

use ofm_core::cmd;
use ofm_core::panel::CursorMove;

use crate::dispatch::CommandRegistry;

/// Hook called once at startup from `hooks::install_commands`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_CURSOR_UP, |ctx| {
        ctx.active_controller().cursor_move(CursorMove::By(-1));
    });
    reg.register(cmd::PANEL_CURSOR_DOWN, |ctx| {
        ctx.active_controller().cursor_move(CursorMove::By(1));
    });
    reg.register(cmd::PANEL_PAGE_UP, |ctx| {
        let c = ctx.active_controller();
        let rows = c.geometry().rows_per_page;
        c.cursor_move(CursorMove::Page { rows, pages: -1 });
    });
    reg.register(cmd::PANEL_PAGE_DOWN, |ctx| {
        let c = ctx.active_controller();
        let rows = c.geometry().rows_per_page;
        c.cursor_move(CursorMove::Page { rows, pages: 1 });
    });
    reg.register(cmd::PANEL_HOME, |ctx| {
        ctx.active_controller().cursor_move(CursorMove::Home);
    });
    reg.register(cmd::PANEL_END, |ctx| {
        ctx.active_controller().cursor_move(CursorMove::End);
    });
    // Left/Right move by columns; in a single-column layout they do nothing.
    reg.register(cmd::PANEL_CURSOR_LEFT, |ctx| column_move(ctx, -1));
    reg.register(cmd::PANEL_CURSOR_RIGHT, |ctx| column_move(ctx, 1));
}

fn column_move(ctx: &std::rc::Rc<crate::context::AppContext>, columns: i32) {
    let c = ctx.active_controller();
    let g = c.geometry();
    if g.columns > 1 {
        c.cursor_move(CursorMove::Column {
            rows_per_column: g.rows_per_page,
            columns,
        });
    }
}

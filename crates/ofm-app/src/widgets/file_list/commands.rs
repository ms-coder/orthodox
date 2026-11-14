//! Commands acting on the list of the active panel (T2.1 owns this file).
//! T2.0 registers the cursor commands; T2.1 adds view-mode, sort and hidden-toggle commands
//! (`panel.view_*`, `panel.sort_*`, `panel.toggle_hidden`, `select.*` stay with T2.3).

use ofm_core::cmd;
use ofm_core::panel::{CursorMove, SortKey};

use super::settings::{ViewMode, set_view_mode};
use crate::dispatch::CommandRegistry;
use crate::panel::PanelController;

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
        c.cursor_move(CursorMove::Page {
            rows: page_rows(&c),
            pages: -1,
        });
    });
    reg.register(cmd::PANEL_PAGE_DOWN, |ctx| {
        let c = ctx.active_controller();
        c.cursor_move(CursorMove::Page {
            rows: page_rows(&c),
            pages: 1,
        });
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

    // View modes (F02).
    reg.register(cmd::PANEL_VIEW_BRIEF, |ctx| {
        set_view_mode(&ctx.active_controller(), ViewMode::Brief);
    });
    reg.register(cmd::PANEL_VIEW_FULL, |ctx| {
        set_view_mode(&ctx.active_controller(), ViewMode::Full);
    });
    reg.register(cmd::PANEL_VIEW_CUSTOM, |ctx| {
        set_view_mode(&ctx.active_controller(), ViewMode::Custom);
    });

    // Sorting (F03): the same key twice reverses.
    reg.register(cmd::PANEL_SORT_NAME, |ctx| {
        toggle_sort(&ctx.active_controller(), SortKey::Name);
    });
    reg.register(cmd::PANEL_SORT_EXT, |ctx| {
        toggle_sort(&ctx.active_controller(), SortKey::Ext);
    });
    reg.register(cmd::PANEL_SORT_MTIME, |ctx| {
        toggle_sort(&ctx.active_controller(), SortKey::Mtime);
    });
    reg.register(cmd::PANEL_SORT_SIZE, |ctx| {
        toggle_sort(&ctx.active_controller(), SortKey::Size);
    });
    reg.register(cmd::PANEL_SORT_REVERSE, |ctx| {
        ctx.active_controller().mutate_view(|m| {
            let mut spec = m.sort_spec();
            spec.reverse = !spec.reverse;
            m.set_sort(spec)
        });
    });
    reg.register(cmd::PANEL_TOGGLE_HIDDEN, |ctx| {
        ctx.active_controller()
            .mutate_view(|m| m.set_show_hidden(!m.show_hidden()));
    });
}

/// Sort by `key`; choosing the key that is already active reverses the order. Shared by the sort
/// commands and the column-header click.
pub(super) fn toggle_sort(ctrl: &PanelController, key: SortKey) {
    ctrl.mutate_view(|m| {
        let mut spec = m.sort_spec();
        if spec.key == key {
            spec.reverse = !spec.reverse;
        } else {
            spec.key = key;
            spec.reverse = false;
        }
        m.set_sort(spec)
    });
}

/// Entries per page (all columns of the grid).
fn page_rows(c: &PanelController) -> u32 {
    let g = c.geometry();
    g.rows_per_page.saturating_mul(g.columns.max(1))
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

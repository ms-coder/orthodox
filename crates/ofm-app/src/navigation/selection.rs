//! Selection commands (F10). The mask dialogs live in `dialogs/mask.rs`.
//!
//! `select.extend_*` (Shift+arrows) select the entry under the cursor *and everything the cursor
//! passes over*, then land on the destination; the destination itself is not selected, so
//! Shift+Down, Shift+Down selects exactly the two rows that were left.

use std::rc::Rc;

use ofm_core::cmd;
use ofm_core::panel::CursorMove;

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;

/// Register the `select.*` commands except the two mask dialogs.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::SELECT_TOGGLE, |ctx| {
        let c = ctx.active_controller();
        c.mutate(|m| {
            if let Some(i) = m.cursor() {
                m.toggle_select(i);
            }
        });
        c.cursor_move(CursorMove::By(1));
    });
    reg.register(cmd::SELECT_EXTEND_UP, |ctx| {
        extend(ctx, |_| CursorMove::By(-1))
    });
    reg.register(cmd::SELECT_EXTEND_DOWN, |ctx| {
        extend(ctx, |_| CursorMove::By(1))
    });
    reg.register(cmd::SELECT_EXTEND_PAGE_UP, |ctx| {
        extend(ctx, |rows| CursorMove::Page { rows, pages: -1 });
    });
    reg.register(cmd::SELECT_EXTEND_PAGE_DOWN, |ctx| {
        extend(ctx, |rows| CursorMove::Page { rows, pages: 1 });
    });
    reg.register(cmd::SELECT_EXTEND_HOME, |ctx| {
        extend(ctx, |_| CursorMove::Home)
    });
    reg.register(cmd::SELECT_EXTEND_END, |ctx| {
        extend(ctx, |_| CursorMove::End)
    });
    reg.register(cmd::SELECT_INVERT, |ctx| {
        ctx.active_controller().mutate(|m| m.invert_selection());
    });
    reg.register(cmd::SELECT_ALL, |ctx| {
        ctx.active_controller().mutate(|m| m.select_all(true));
    });
    reg.register(cmd::SELECT_NONE, |ctx| {
        ctx.active_controller().mutate(|m| m.select_all(false));
    });
    reg.register(cmd::SELECT_RESTORE, |ctx| {
        ctx.active_controller().mutate(|m| m.restore_selection());
    });
}

fn extend(ctx: &Rc<AppContext>, mv: impl FnOnce(u32) -> CursorMove) {
    let c = ctx.active_controller();
    let from = c.model().cursor();
    c.cursor_move(mv(c.geometry().rows_per_page));
    let to = c.model().cursor();
    let (Some(a), Some(b)) = (from, to) else {
        return;
    };
    c.mutate(|m| {
        if b > a {
            m.select_range(a, b - 1, true);
        } else if b < a {
            m.select_range(b + 1, a, true);
        } else {
            // The cursor could not move (list end): still take the row it stands on.
            m.select_range(a, a, true);
        }
    });
}

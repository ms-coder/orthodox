//! Directory history (F07): `panel.history_back/forward` and the Alt+F12 list.
//! The per-tab list itself is `panel::NavHistory`; this module only drives it.
//! A locked tab ignores history navigation (it never leaves its directory).

use std::rc::Rc;

use ofm_core::{Location, cmd};

use super::chooser::{self, ChoiceRow, ChooserSpec};
use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::panel::{LoadOpts, PanelController};

/// Register the history commands.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_HISTORY_BACK, |ctx| {
        let c = ctx.active_controller();
        if c.locked() {
            return;
        }
        let target = c.history.borrow_mut().back();
        go(&c, target);
    });
    reg.register(cmd::PANEL_HISTORY_FORWARD, |ctx| {
        let c = ctx.active_controller();
        if c.locked() {
            return;
        }
        let target = c.history.borrow_mut().forward();
        go(&c, target);
    });
    reg.register(cmd::PANEL_HISTORY_LIST, list);
}

/// Load a location picked from the history (the history position was already moved).
fn go(ctrl: &Rc<PanelController>, target: Option<Location>) {
    if let Some(loc) = target {
        ctrl.load(
            loc,
            LoadOpts {
                keep_selection: false,
                cursor_name: None,
                skip_history: true,
            },
        );
    }
}

fn list(ctx: &Rc<AppContext>) {
    let ctrl = ctx.active_controller();
    // Newest first, like Far's Alt+F12.
    let entries: Vec<Location> = ctrl
        .history
        .borrow()
        .entries()
        .iter()
        .rev()
        .cloned()
        .collect();
    let current = ctrl.history.borrow().position();
    let n = entries.len();
    let rows = entries
        .iter()
        .map(|l| ChoiceRow {
            title: l.display_lossy(),
            subtitle: None,
        })
        .collect();
    let on_pick = {
        let ctrl = Rc::clone(&ctrl);
        Rc::new(move |_: &Rc<AppContext>, row: usize| {
            if ctrl.locked() {
                return;
            }
            let index = n - 1 - row.min(n - 1);
            let target = ctrl.history.borrow_mut().jump(index);
            go(&ctrl, target);
        })
    };
    chooser::open(
        ctx,
        ChooserSpec {
            title: "Directory history",
            rows,
            selected: n.saturating_sub(1 + current),
            empty_text: "No history yet",
            on_pick,
            buttons: Vec::new(),
        },
    );
}

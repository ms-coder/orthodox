//! Dialog-local commands: `dialog.queue` (F2), `dialog.apply_all` (Ctrl+A), `jobs.pause_resume`
//! (Space), `jobs.cancel` (Del) and `jobs.clear_finished` (Ctrl+L). The keymap binds them in the
//! `dialog` section, so while any modal dialog is open they arrive here; the topmost dialog that
//! registered a handler (via [`bind`]) gets them. Dialogs that do not care ignore them, except
//! that Space activates the focused button/check box (the key would otherwise be lost because
//! the window-level key controller consumes it, ADR-0005).

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use crate::context::AppContext;
use crate::navigation::modal::ModalStack;

/// A dialog-local action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// `dialog.queue`
    Queue,
    /// `dialog.apply_all`
    ApplyAll,
    /// `jobs.pause_resume`
    PauseResume,
    /// `jobs.cancel`
    Cancel,
    /// `jobs.clear_finished`
    ClearFinished,
}

type Handler = Rc<dyn Fn(Action) -> bool>;

struct Bound {
    id: u64,
    depth: usize,
    handler: Handler,
}

/// Registry of per-dialog action handlers (state in `ctx.ext`).
#[derive(Default)]
pub struct DialogActions {
    stack: RefCell<Vec<Bound>>,
    next: Cell<u64>,
}

/// Create the registry.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(DialogActions::default()));
}

/// Attach `handler` to `dialog`, which must already have been shown with `modal::present`. The
/// handler returns whether it used the action (otherwise [`fallback`] runs).
/// Removed again when the dialog closes.
pub fn bind(
    ctx: &Rc<AppContext>,
    dialog: &adw::Dialog,
    handler: impl Fn(Action) -> bool + 'static,
) {
    let (Some(reg), Some(modal)) = (ctx.ext.get::<DialogActions>(), ctx.ext.get::<ModalStack>())
    else {
        return;
    };
    let id = reg.next.get();
    reg.next.set(id + 1);
    reg.stack.borrow_mut().push(Bound {
        id,
        depth: modal.depth(),
        handler: Rc::new(handler),
    });
    let weak = Rc::downgrade(&reg);
    dialog.connect_closed(move |_| {
        if let Some(reg) = weak.upgrade() {
            reg.stack.borrow_mut().retain(|b| b.id != id);
        }
    });
}

/// Run `action` for the topmost dialog. Returns whether a bound dialog handled it.
pub fn dispatch(ctx: &Rc<AppContext>, action: Action) -> bool {
    let (Some(reg), Some(modal)) = (ctx.ext.get::<DialogActions>(), ctx.ext.get::<ModalStack>())
    else {
        return false;
    };
    let depth = modal.depth();
    let handler = reg
        .stack
        .borrow()
        .iter()
        .rev()
        .find(|b| b.depth == depth)
        .map(|b| Rc::clone(&b.handler));
    handler.is_some_and(|h| h(action))
}

/// The key bound to a dialog action reached a dialog that does not use it. Space must still
/// press the focused button / toggle the focused check box.
pub fn fallback(ctx: &Rc<AppContext>, action: Action) {
    if action != Action::PauseResume {
        return;
    }
    let Some(win) = ctx.window() else { return };
    let Some(focus) = gtk::prelude::RootExt::focus(&win.window) else {
        return;
    };
    if let Some(button) = focus.downcast_ref::<gtk::Button>() {
        button.emit_clicked();
    } else if focus.is::<gtk::CheckButton>() {
        focus.activate();
    }
}

//! Modal dialog plumbing shared by every dialog of this module (and, later, by other dialogs).
//!
//! A dialog is an `adw::Dialog` shown over the main window. [`present`] enters the modal state
//! (so keys resolve in the `dialog` keymap section, ADR-0005), and the commands `dialog.ok` /
//! `dialog.cancel` registered here act on the topmost dialog: `ok` runs the dialog's accept
//! callback (the dialog closes when it returns `true`), `cancel` closes it.
//!
//! [`present`] also attaches the dispatcher's key controller to the dialog (libadwaita does not
//! propagate key events from a dialog to the window), so every key of every dialog goes through
//! the keymap. Dialogs must therefore be shown with [`present`], never with `adw::Dialog::present`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use ofm_core::cmd;

use crate::context::{AppContext, ModalGuard};
use crate::dispatch::{self, CommandRegistry, WindowKind};

type OkFn = Rc<dyn Fn() -> bool>;

struct Shown {
    id: u64,
    dialog: adw::Dialog,
    on_ok: OkFn,
    _guard: ModalGuard,
}

/// Stack of open modal dialogs (state in `ctx.ext`).
#[derive(Default)]
pub struct ModalStack {
    stack: RefCell<Vec<Shown>>,
    next_id: Cell<u64>,
}

impl ModalStack {
    fn top(&self) -> Option<(adw::Dialog, OkFn)> {
        self.stack
            .borrow()
            .last()
            .map(|s| (s.dialog.clone(), Rc::clone(&s.on_ok)))
    }

    fn remove(&self, id: u64) {
        let gone = {
            let mut stack = self.stack.borrow_mut();
            stack
                .iter()
                .position(|s| s.id == id)
                .map(|i| stack.remove(i))
        };
        drop(gone); // releases the modal guard outside of the borrow
    }

    /// Number of open dialogs.
    pub fn depth(&self) -> usize {
        self.stack.borrow().len()
    }
}

/// Register `dialog.ok` and `dialog.cancel`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::DIALOG_OK, |ctx| {
        let top = ctx.ext.get::<ModalStack>().and_then(|s| s.top());
        if let Some((dialog, on_ok)) = top
            && on_ok()
        {
            dialog.close();
        }
    });
    reg.register(cmd::DIALOG_CANCEL, |ctx| {
        if let Some((dialog, _)) = ctx.ext.get::<ModalStack>().and_then(|s| s.top()) {
            dialog.close();
        }
    });
}

/// Create the dialog stack.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(ModalStack::default()));
}

/// Show `dialog` over the main window and enter the modal state until it closes. `on_ok` runs for
/// `dialog.ok` (Enter); return `true` to close the dialog. Returns `false` (and shows nothing)
/// when there is no main window yet.
pub fn present(
    ctx: &Rc<AppContext>,
    dialog: &adw::Dialog,
    on_ok: impl Fn() -> bool + 'static,
) -> bool {
    let Some(win) = ctx.window() else {
        tracing::warn!("cannot show a dialog without a main window");
        return false;
    };
    present_over(ctx, dialog, &win.window, on_ok)
}

/// Like [`present`], over another toplevel `parent` (the viewer window). Returns `false` when the
/// dialog state is not installed.
pub fn present_over(
    ctx: &Rc<AppContext>,
    dialog: &adw::Dialog,
    parent: &impl IsA<gtk::Widget>,
    on_ok: impl Fn() -> bool + 'static,
) -> bool {
    let Some(stack) = ctx.ext.get::<ModalStack>() else {
        tracing::warn!("cannot show a dialog: no modal stack");
        return false;
    };
    let id = stack.next_id.get();
    stack.next_id.set(id + 1);
    stack.stack.borrow_mut().push(Shown {
        id,
        dialog: dialog.clone(),
        on_ok: Rc::new(on_ok),
        _guard: ctx.enter_modal(),
    });
    let weak_stack = Rc::downgrade(&stack);
    dialog.connect_closed(move |_| {
        if let Some(stack) = weak_stack.upgrade() {
            stack.remove(id);
        }
    });
    // Every dialog resolves its keys through the keymap's `dialog` section itself: libadwaita
    // stops key events at the dialog, so the main window's controller does not see them
    // (see `dispatch::install_key_controller`).
    dispatch::install_key_controller(dialog, ctx, WindowKind::Dialog);
    // Hook for the theme's dialog colours (`theme::css`).
    dialog.add_css_class("ofm-dialog");
    dialog.present(Some(parent));
    true
}

/// Dialog content with a header bar (shows the dialog title and a close button).
pub fn with_header(content: &impl IsA<gtk::Widget>) -> adw::ToolbarView {
    let view = adw::ToolbarView::new();
    view.add_top_bar(&adw::HeaderBar::new());
    view.set_content(Some(content));
    view
}

/// Vertical box with the standard dialog padding.
pub fn padded_column(spacing: i32) -> gtk::Box {
    let column = gtk::Box::new(gtk::Orientation::Vertical, spacing);
    column.set_margin_top(12);
    column.set_margin_bottom(12);
    column.set_margin_start(12);
    column.set_margin_end(12);
    column
}

//! `dialog.ok` / `dialog.cancel` routing for whatever modal surface is on top (menu drop-down,
//! dialogs of later tasks).
//!
//! While a modal is open the dispatcher resolves keys in the `dialog` keymap section, where Enter
//! and Esc are bound to `dialog.ok` / `dialog.cancel`, and it *consumes* a key as soon as it
//! resolves to a command. So a modal surface needs somebody to handle those two commands: it calls
//! [`push`], keeps the returned guard while it is shown, and its handlers run when the user presses
//! the keys (or something executes the commands). The guard also holds `AppContext::enter_modal`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ofm_core::cmd;

use crate::context::{AppContext, ModalGuard};
use crate::dispatch::CommandRegistry;

/// What a modal surface does for the two dialog commands.
pub struct ModalHandler {
    /// Enter / `dialog.ok`.
    pub ok: Box<dyn Fn()>,
    /// Esc / `dialog.cancel`.
    pub cancel: Box<dyn Fn()>,
}

type Entry = (u64, Rc<ModalHandler>);

/// Stack of open modal surfaces (state in `ctx.ext`).
#[derive(Default)]
pub struct ModalKeys {
    stack: RefCell<Vec<Entry>>,
    next: Cell<u64>,
}

impl ModalKeys {
    fn top(&self) -> Option<Rc<ModalHandler>> {
        self.stack.borrow().last().map(|(_, h)| Rc::clone(h))
    }
}

/// Keeps a modal surface registered; dropping it unregisters it and leaves modal mode.
pub struct ModalKeysGuard {
    keys: Rc<ModalKeys>,
    id: u64,
    _modal: ModalGuard,
}

impl Drop for ModalKeysGuard {
    fn drop(&mut self) {
        self.keys.stack.borrow_mut().retain(|(i, _)| *i != self.id);
    }
}

/// Enter modal mode and make `handler` the target of Enter/Esc until the guard is dropped.
pub fn push(ctx: &Rc<AppContext>, handler: ModalHandler) -> ModalKeysGuard {
    let keys = ctx.ext.get::<ModalKeys>().unwrap_or_else(|| {
        let k = Rc::new(ModalKeys::default());
        ctx.ext.insert(Rc::clone(&k));
        k
    });
    let id = keys.next.get() + 1;
    keys.next.set(id);
    keys.stack.borrow_mut().push((id, Rc::new(handler)));
    ModalKeysGuard {
        keys,
        id,
        _modal: ctx.enter_modal(),
    }
}

/// Register `dialog.ok` and `dialog.cancel`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::DIALOG_OK, |ctx| {
        if let Some(h) = ctx.ext.get::<ModalKeys>().and_then(|k| k.top()) {
            (h.ok)();
        }
    });
    reg.register(cmd::DIALOG_CANCEL, |ctx| {
        if let Some(h) = ctx.ext.get::<ModalKeys>().and_then(|k| k.top()) {
            (h.cancel)();
        }
    });
}

/// Create the (empty) stack.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(ModalKeys::default()));
}

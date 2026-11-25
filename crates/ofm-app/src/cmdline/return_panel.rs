//! `[terminal] return_to_panels` (ADR-0003): after the command line started a command and
//! brought the terminal up, the panels come back by themselves when the command ends. "Ended"
//! is the shell's next prompt ([`ShellState::Idle`], reported through the OSC 7 of the rc
//! wrapper; a shell that never reports one never ends a command this way, so nothing happens
//! and Ctrl+O brings the panels back as usual).
//!
//! One-shot, and the user wins: armed by `cmdline.execute`, it fires at the first prompt
//! afterwards and only while the terminal is still shown. A Ctrl+O during the command already
//! brought the panels back, so there is nothing left to do.

use std::cell::RefCell;
use std::rc::Rc;

use crate::context::AppContext;
use crate::signal::SignalId;
use crate::terminal::{self, ShellHost, ShellState};

/// The pending return (state in `ctx.ext`, created on first use): one `ShellHost::changed`
/// subscription.
#[derive(Default)]
pub struct ReturnWatch {
    pending: RefCell<Option<SignalId>>,
}

impl ReturnWatch {
    /// Run `on_end` once when `host` next reports [`ShellState::Idle`]. Replaces a pending
    /// return (a second command started from the command line).
    pub fn arm(&self, host: &Rc<ShellHost>, on_end: impl Fn() + 'static) {
        self.cancel(host);
        let weak = Rc::downgrade(self);
        let id = host.changed.connect(move |state| {
            if *state != ShellState::Idle {
                return;
            }
            let Some(watch) = weak.upgrade() else { return };
            // One shot: forget the subscription before acting.
            if let Some(id) = watch.pending.borrow_mut().take() {
                host.changed.disconnect(id);
            }
            on_end();
        });
        *self.pending.borrow_mut() = Some(id);
    }

    /// Drop a pending return.
    pub fn cancel(&self, host: &ShellHost) {
        if let Some(id) = self.pending.borrow_mut().take() {
            host.changed.disconnect(id);
        }
    }

    /// Whether a command's end is awaited.
    pub fn is_armed(&self) -> bool {
        self.pending.borrow().is_some()
    }
}

/// Arm the return for the command the command line just started (if a shell is installed).
pub fn arm(ctx: &AppContext) {
    let watch = ctx.ext.get::<ReturnWatch>().unwrap_or_else(|| {
        let watch = Rc::new(ReturnWatch::default());
        ctx.ext.insert(Rc::clone(&watch));
        watch
    });
    let Some(host) = terminal::shell_host(ctx) else {
        return;
    };
    watch.arm(&host, move || {
        // Only while the terminal is still up: Ctrl+O during the command already returned.
        if let Some(h) = terminal::handle(ctx) {
            h.hide();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_once_at_the_first_prompt_and_not_before() {
        let host = Rc::new(ShellHost::new());
        let watch = ReturnWatch::default();
        let ends = Rc::new(std::cell::Cell::new(0));
        let count = Rc::clone(&ends);
        watch.arm(&host, move || count.set(count.get() + 1));
        assert!(watch.is_armed());
        // Starting (Busy) and other transitions do nothing; the first prompt ends the command.
        host.set(ShellState::Busy);
        host.set(ShellState::Exited);
        assert_eq!(ends.get(), 0);
        host.set(ShellState::Idle);
        assert_eq!(ends.get(), 1);
        assert!(!watch.is_armed(), "one shot");
        // Later prompts (a `cd` sync) do not touch the terminal again.
        host.set(ShellState::Busy);
        host.set(ShellState::Idle);
        assert_eq!(ends.get(), 1);
        assert_eq!(host.changed.len(), 0, "the subscription is gone");
    }

    #[test]
    fn a_second_command_replaces_the_pending_return() {
        let host = Rc::new(ShellHost::new());
        let watch = ReturnWatch::default();
        let first = Rc::new(std::cell::Cell::new(0));
        let second = Rc::new(std::cell::Cell::new(0));
        let (a, b) = (Rc::clone(&first), Rc::clone(&second));
        watch.arm(&host, move || a.set(a.get() + 1));
        watch.arm(&host, move || b.set(b.get() + 1));
        assert_eq!(host.changed.len(), 1);
        host.set(ShellState::Busy);
        host.set(ShellState::Idle);
        assert_eq!((first.get(), second.get()), (0, 1));
        watch.cancel(&host);
        assert!(!watch.is_armed());
    }
}

//! Tiny single-threaded observer list used between controllers and views (main thread only).
//!
//! Handlers may connect/disconnect while an emission is running; they take effect from the next
//! emission. Prefer this over GObject signals for plain-Rust controllers.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Handle returned by [`Signal::connect`]; pass to [`Signal::disconnect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalId(u64);

type Handler<T> = Rc<dyn Fn(&T)>;
type Slot<T> = (u64, Handler<T>);

/// A list of handlers for events of type `T`.
pub struct Signal<T> {
    slots: RefCell<Vec<Slot<T>>>,
    next: Cell<u64>,
}

impl<T> Default for Signal<T> {
    fn default() -> Self {
        Signal {
            slots: RefCell::new(Vec::new()),
            next: Cell::new(1),
        }
    }
}

impl<T> Signal<T> {
    /// Empty signal.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a handler.
    pub fn connect(&self, f: impl Fn(&T) + 'static) -> SignalId {
        let id = self.next.get();
        self.next.set(id + 1);
        self.slots.borrow_mut().push((id, Rc::new(f)));
        SignalId(id)
    }

    /// Remove a handler (no-op if already gone).
    pub fn disconnect(&self, id: SignalId) {
        self.slots.borrow_mut().retain(|(i, _)| *i != id.0);
    }

    /// Call every handler in connection order.
    pub fn emit(&self, value: &T) {
        let snapshot: Vec<Handler<T>> =
            self.slots.borrow().iter().map(|(_, f)| f.clone()).collect();
        for f in snapshot {
            f(value);
        }
    }

    /// Number of connected handlers.
    pub fn len(&self) -> usize {
        self.slots.borrow().len()
    }

    /// True when no handler is connected.
    pub fn is_empty(&self) -> bool {
        self.slots.borrow().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emit_connect_disconnect() {
        let s: Signal<u32> = Signal::new();
        let sum = Rc::new(Cell::new(0));
        let s1 = sum.clone();
        let id = s.connect(move |v| s1.set(s1.get() + *v));
        let s2 = sum.clone();
        s.connect(move |v| s2.set(s2.get() + *v * 10));
        s.emit(&1);
        assert_eq!(sum.get(), 11);
        s.disconnect(id);
        s.emit(&1);
        assert_eq!(sum.get(), 21);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn handler_may_connect_during_emit() {
        let s: Rc<Signal<()>> = Rc::new(Signal::new());
        let s2 = s.clone();
        s.connect(move |_| {
            s2.connect(|_| {});
        });
        s.emit(&());
        assert_eq!(s.len(), 2);
    }
}

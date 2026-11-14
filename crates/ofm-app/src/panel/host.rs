//! One side (left/right) of the two-panel layout: a list of tabs, one of them active.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use ofm_core::Location;

use super::controller::PanelController;
use crate::context::Services;
use crate::signal::Signal;

/// Tabs of one panel side. Always has at least one tab.
pub struct PanelHost {
    services: Services,
    tabs: RefCell<Vec<Rc<PanelController>>>,
    active: Cell<usize>,
    /// Emitted after the active tab changed (switch, close, add with activate); views rebind.
    pub active_changed: Signal<Rc<PanelController>>,
    /// Emitted after tabs were added, closed or reordered; the tab bar rebuilds.
    pub tabs_changed: Signal<()>,
}

impl PanelHost {
    /// Host with one tab showing `initial` (not loaded until [`PanelController::load`]).
    pub fn new(services: Services, initial: Location) -> Rc<Self> {
        let first = PanelController::new(services.clone(), initial);
        Rc::new(PanelHost {
            services,
            tabs: RefCell::new(vec![first]),
            active: Cell::new(0),
            active_changed: Signal::new(),
            tabs_changed: Signal::new(),
        })
    }

    /// The active tab's controller.
    pub fn active(&self) -> Rc<PanelController> {
        let tabs = self.tabs.borrow();
        let idx = self.active.get().min(tabs.len().saturating_sub(1));
        // Invariant: `tabs` is never empty (new() adds one, close_tab keeps one).
        Rc::clone(&tabs[idx])
    }

    /// Index of the active tab.
    pub fn active_index(&self) -> usize {
        self.active.get()
    }

    /// Number of tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.borrow().len()
    }

    /// Snapshot of all tab controllers, left to right.
    pub fn tabs(&self) -> Vec<Rc<PanelController>> {
        self.tabs.borrow().clone()
    }

    /// Open a new tab at `loc` right of the active one; loads it. `activate` switches to it.
    pub fn add_tab(&self, loc: Location, activate: bool) -> Rc<PanelController> {
        let ctrl = PanelController::new(self.services.clone(), loc);
        let at = (self.active.get() + 1).min(self.tabs.borrow().len());
        self.tabs.borrow_mut().insert(at, Rc::clone(&ctrl));
        if !activate && at <= self.active.get() {
            self.active.set(self.active.get() + 1);
        }
        ctrl.reload();
        self.tabs_changed.emit(&());
        if activate {
            self.set_active(at);
        }
        ctrl
    }

    /// Close tab `idx`; refuses (returns false) to close the last tab or a locked one.
    pub fn close_tab(&self, idx: usize) -> bool {
        let (removed_active, new_active) = {
            let mut tabs = self.tabs.borrow_mut();
            if tabs.len() <= 1 || idx >= tabs.len() || tabs[idx].locked() {
                return false;
            }
            let gone = tabs.remove(idx);
            gone.shutdown();
            let cur = self.active.get();
            let new_active = if idx < cur {
                cur - 1
            } else {
                cur.min(tabs.len() - 1)
            };
            (idx == cur, new_active)
        };
        self.active.set(new_active);
        self.tabs_changed.emit(&());
        if removed_active {
            self.active_changed.emit(&self.active());
        }
        true
    }

    /// Activate tab `idx` (ignored when out of range or already active).
    pub fn set_active(&self, idx: usize) {
        if idx >= self.tab_count() || idx == self.active.get() {
            return;
        }
        self.active.set(idx);
        self.active_changed.emit(&self.active());
    }
}

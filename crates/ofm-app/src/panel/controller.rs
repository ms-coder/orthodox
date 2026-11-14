//! `PanelController`: one panel tab. Owns the `PanelModel`, the current `Location`, history and
//! the *load generation*. Listing runs on the I/O pool; batches come back through an
//! `async-channel` and are applied on the main thread by a `glib::spawn_future_local` task.
//! Results of an older generation are dropped (ARCHITECTURE §3 "Staleness").

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::ffi::OsString;
use std::ops::ControlFlow;
use std::rc::{Rc, Weak};

use ofm_core::panel::{CursorMove, ModelChange, PanelModel};
use ofm_core::{CancelToken, Location};
use ofm_vfs::ListEvent;

use super::history::NavHistory;
use crate::context::Services;
use crate::signal::Signal;
use crate::wiring;

/// What observers (views, status line, tab bar) are told.
#[derive(Debug, Clone, PartialEq)]
pub enum PanelEvent {
    /// The visible list changed as described (apply incrementally or redraw).
    Model(ModelChange),
    /// Cursor, selection or other state changed without altering the list: redraw.
    Redraw,
    /// The tab navigated to a new location (title, path header, tab label).
    Location,
    /// Loading state changed (spinner, error text).
    State,
}

/// Listing state of a tab.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LoadState {
    /// Nothing to do (or never loaded).
    #[default]
    Idle,
    /// A listing is streaming in.
    Loading,
    /// The listing failed; entries received so far stay valid. Holds a user-presentable message.
    Failed(String),
}

/// Options of [`PanelController::load`].
#[derive(Debug, Clone, Default)]
pub struct LoadOpts {
    /// Keep the selection (set only when reloading the *same* location).
    pub keep_selection: bool,
    /// Put the cursor on this name once it arrives (e.g. the directory we just left).
    pub cursor_name: Option<OsString>,
    /// Do not record this navigation in the history (history back/forward, refresh).
    pub skip_history: bool,
}

/// Geometry reported by the view so page/column cursor commands know the page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ViewGeometry {
    /// Rows visible in one column.
    pub rows_per_page: u32,
    /// Number of columns (1 in Full mode).
    pub columns: u32,
}

impl Default for ViewGeometry {
    fn default() -> Self {
        ViewGeometry {
            rows_per_page: 20,
            columns: 1,
        }
    }
}

/// One tab of a panel.
pub struct PanelController {
    services: Services,
    model: RefCell<PanelModel>,
    location: RefCell<Location>,
    state: RefCell<LoadState>,
    generation: Cell<u64>,
    cancel: RefCell<CancelToken>,
    /// Cursor target that has not arrived yet (cleared when found, on finish, or on user move).
    pending_cursor: RefCell<Option<OsString>>,
    geometry: Cell<ViewGeometry>,
    locked: Cell<bool>,
    /// Back/forward history of this tab (F07). T2.3 drives it.
    pub history: RefCell<NavHistory>,
    /// Observer list; see [`PanelEvent`].
    pub changed: Signal<PanelEvent>,
}

/// Parent test without I/O: whether the synthetic `..` row is shown.
fn has_parent(loc: &Location) -> bool {
    match loc {
        Location::Local(p) => p.parent().is_some(),
        Location::Archive { .. } => true,
        Location::List { .. } => false,
    }
}

impl PanelController {
    /// New tab at `loc`. Nothing is listed until [`load`](Self::load) / [`reload`](Self::reload).
    pub fn new(services: Services, loc: Location) -> Rc<Self> {
        let mut history = NavHistory::new();
        history.visit(loc.clone());
        Rc::new(PanelController {
            services,
            model: RefCell::new(PanelModel::new()),
            location: RefCell::new(loc),
            state: RefCell::new(LoadState::Idle),
            generation: Cell::new(0),
            cancel: RefCell::new(CancelToken::new()),
            pending_cursor: RefCell::new(None),
            geometry: Cell::new(ViewGeometry::default()),
            locked: Cell::new(false),
            history: RefCell::new(history),
            changed: Signal::new(),
        })
    }

    // ---- read access --------------------------------------------------------------------------
    /// Current location.
    pub fn location(&self) -> Location {
        self.location.borrow().clone()
    }

    /// Read access to the model (views, status line). Do not hold across `.await` or emit calls.
    pub fn model(&self) -> Ref<'_, PanelModel> {
        self.model.borrow()
    }

    /// Load state.
    pub fn state(&self) -> LoadState {
        self.state.borrow().clone()
    }

    /// Current load generation (increments on every `load`).
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Tab lock (F06): a locked tab cannot be closed (navigation rules are T2.3's).
    pub fn locked(&self) -> bool {
        self.locked.get()
    }

    /// Set the tab lock.
    pub fn set_locked(&self, locked: bool) {
        self.locked.set(locked);
        self.changed.emit(&PanelEvent::State);
    }

    /// The services (VFS registry, I/O pool) this controller lists with.
    pub fn services(&self) -> &Services {
        &self.services
    }

    // ---- view geometry ------------------------------------------------------------------------
    /// Geometry last reported by the view.
    pub fn geometry(&self) -> ViewGeometry {
        self.geometry.get()
    }

    /// Called by the view on size/view-mode change; does not emit.
    pub fn set_geometry(&self, g: ViewGeometry) {
        self.geometry.set(g);
    }

    // ---- mutation -----------------------------------------------------------------------------
    /// Mutate the model with an arbitrary closure (cursor, selection, ...); emits `Redraw`.
    /// For closures that return a [`ModelChange`] prefer [`mutate_view`](Self::mutate_view).
    pub fn mutate<R>(&self, f: impl FnOnce(&mut PanelModel) -> R) -> R {
        let r = f(&mut self.model.borrow_mut());
        self.changed.emit(&PanelEvent::Redraw);
        r
    }

    /// Mutate the model with a closure returning the resulting [`ModelChange`] (sort, filter,
    /// hidden toggle); emits `Model(change)`.
    pub fn mutate_view(&self, f: impl FnOnce(&mut PanelModel) -> ModelChange) {
        let change = f(&mut self.model.borrow_mut());
        self.changed.emit(&PanelEvent::Model(change));
    }

    /// Mutable model access for callers that emit events themselves. Prefer `mutate*`.
    pub fn model_mut(&self) -> RefMut<'_, PanelModel> {
        self.model.borrow_mut()
    }

    /// Move the cursor (user action: also cancels a pending "cursor on name" request).
    pub fn cursor_move(&self, mv: CursorMove) {
        self.pending_cursor.borrow_mut().take();
        self.mutate(|m| {
            m.move_cursor(mv);
        });
    }

    // ---- loading ------------------------------------------------------------------------------
    /// Re-list the current location, keeping selection and cursor name.
    pub fn reload(self: &Rc<Self>) {
        let loc = self.location();
        let cursor = self.model().cursor_entry().map(|e| e.name.clone());
        self.load(
            loc,
            LoadOpts {
                keep_selection: true,
                cursor_name: cursor,
                skip_history: true,
            },
        );
    }

    /// Navigate to `loc` and list it. Cancels a running listing, bumps the generation, resets the
    /// model and streams the new entries in. Never blocks.
    pub fn load(self: &Rc<Self>, loc: Location, opts: LoadOpts) {
        self.cancel.borrow().cancel();
        let token = CancelToken::new();
        *self.cancel.borrow_mut() = token.clone();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);

        let same = *self.location.borrow() == loc;
        if !opts.skip_history && !same {
            self.history.borrow_mut().visit(loc.clone());
        }
        *self.location.borrow_mut() = loc.clone();
        *self.pending_cursor.borrow_mut() = opts.cursor_name;
        self.model
            .borrow_mut()
            .begin_load(has_parent(&loc), opts.keep_selection && same);
        self.set_state(LoadState::Loading);
        self.changed.emit(&PanelEvent::Location);
        self.changed.emit(&PanelEvent::Model(ModelChange::Reset));

        let vfs = match self.services.vfs.get(&loc) {
            Ok(v) => v,
            Err(e) => {
                self.set_state(LoadState::Failed(e.to_string()));
                return;
            }
        };
        let rx = wiring::run_on_pool(&self.services.pool, move |tx| {
            vfs.list(&loc, &token, &mut |ev| {
                // A closed channel means the controller moved on or died: the sender error is
                // deliberately ignored, the listing finishes quickly via the cancel token.
                let _ = tx.send_blocking(ev);
            });
        });
        let weak: Weak<PanelController> = Rc::downgrade(self);
        let weak_closed = weak.clone();
        wiring::attach(
            rx,
            move |ev| match weak.upgrade() {
                Some(this) if this.generation.get() == generation => {
                    this.apply_list_event(ev);
                    ControlFlow::Continue(())
                }
                _ => ControlFlow::Break(()), // stale or controller gone
            },
            move || {
                // Channel closed without Done/Error (worker panicked): do not stay "Loading".
                if let Some(this) = weak_closed.upgrade()
                    && this.generation.get() == generation
                    && this.state() == LoadState::Loading
                {
                    this.set_state(LoadState::Failed("listing aborted".to_owned()));
                }
            },
        );
    }

    /// Cancel a running listing (Esc during loading) and drop its pending events.
    pub fn cancel_load(&self) {
        self.cancel.borrow().cancel();
        self.generation.set(self.generation.get() + 1);
        if self.state() == LoadState::Loading {
            self.set_state(LoadState::Idle);
        }
    }

    /// Called when the tab is closed: stop background work.
    pub fn shutdown(&self) {
        self.cancel_load();
    }

    fn apply_list_event(&self, ev: ListEvent) {
        match ev {
            ListEvent::Batch(entries) => {
                let change = self.model.borrow_mut().apply_batch(entries);
                self.try_pending_cursor();
                self.changed.emit(&PanelEvent::Model(change));
            }
            ListEvent::Done { total } => {
                tracing::debug!(total, "listing done");
                self.model.borrow_mut().finish_load();
                self.try_pending_cursor();
                self.pending_cursor.borrow_mut().take();
                self.set_state(LoadState::Idle);
                self.changed.emit(&PanelEvent::Redraw);
            }
            ListEvent::Error(e) => {
                tracing::warn!(error = %e, "listing failed");
                self.pending_cursor.borrow_mut().take();
                self.set_state(LoadState::Failed(e.to_string()));
            }
        }
    }

    /// Deferred-cursor handling (M1 review finding): the wanted name may arrive in a later batch.
    fn try_pending_cursor(&self) {
        let name = self.pending_cursor.borrow().clone();
        if let Some(name) = name
            && self.model.borrow_mut().set_cursor_name(&name)
        {
            self.pending_cursor.borrow_mut().take();
        }
    }

    fn set_state(&self, state: LoadState) {
        *self.state.borrow_mut() = state;
        self.changed.emit(&PanelEvent::State);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_row_rules() {
        assert!(has_parent(&Location::Local("/a".into())));
        assert!(!has_parent(&Location::Local("/".into())));
        assert!(!has_parent(&Location::List {
            id: ofm_core::ListId(1)
        }));
    }
}

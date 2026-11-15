//! `PanelController`: one panel tab. Owns the `PanelModel`, the current `Location`, history and
//! the *load generation*. Listing runs on the I/O pool; batches come back through an
//! `async-channel` and are applied on the main thread by a `glib::spawn_future_local` task.
//! Results of an older generation are dropped (ARCHITECTURE §3 "Staleness").
//!
//! **Directory watching (M3, T3.0).** For `Location::Local` the controller keeps a debounced
//! `ofm_vfs::watch` subscription on its directory and applies changes incrementally
//! (`PanelModel::apply_update`), so file operations, other programs and the shell show up without
//! a manual refresh. Rules: the watch is (re)started by `load` only when the directory changes
//! (reloads of the same directory keep it); it is stopped on navigation to a non-local location
//! and on `shutdown` (tab close); it is started *before* the listing is queued so changes made
//! while listing are not lost; events arriving while a listing is running are buffered and flushed
//! at `Done`; `Changed` names are re-`stat`ed on the I/O pool, one batch in flight at a time, and
//! the result is dropped if the load generation changed meanwhile; `Rescan` triggers a `reload`;
//! `DirGone` navigates to the nearest existing ancestor. Use [`PanelController::focus_name`] /
//! [`PanelController::reload_and_focus`] after an operation to put the cursor on a new item.

use std::cell::{Cell, Ref, RefCell, RefMut};
use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use ofm_core::panel::{CursorMove, ModelChange, PanelModel};
use ofm_core::{CancelToken, Entry, Location};
use ofm_vfs::watch::{WatchEvent, WatchHandle, watch_dir};
use ofm_vfs::{ListEvent, VfsError};

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

/// Debounce of the directory watcher (ofm-vfs default).
const WATCH_DEBOUNCE: Duration = Duration::from_millis(100);
/// How long [`PanelController::focus_name`] waits for the named entry to appear.
const FOCUS_TTL: Duration = Duration::from_secs(5);

/// State of the directory watch of one tab.
struct WatchState {
    /// Identifies this watch; in-flight results of an older watch are dropped.
    epoch: u64,
    /// Watched directory.
    dir: PathBuf,
    /// Present once the pool thread created the subscription (dropping it stops watching).
    handle: Option<WatchHandle>,
    /// Names to re-stat.
    pending: BTreeSet<OsString>,
    /// A rescan (reload) was requested.
    rescan: bool,
    /// A stat batch is running on the pool.
    in_flight: bool,
}

/// Result of re-statting watcher names on the pool.
struct Restat {
    changed: Vec<Entry>,
    removed: Vec<OsString>,
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
    /// Deadline of a `focus_name` request (`None` for load-originated pending cursors).
    pending_until: Cell<Option<Instant>>,
    watch: RefCell<Option<WatchState>>,
    watch_epoch: Cell<u64>,
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
            pending_until: Cell::new(None),
            watch: RefCell::new(None),
            watch_epoch: Cell::new(0),
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
        self.pending_until.set(None);
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

    /// Re-list the current location (selection kept) and put the cursor on `name` once it is
    /// listed: the explicit after-operation refresh (e.g. cursor on a just-copied file) for
    /// locations the watcher does not cover. For local directories prefer
    /// [`focus_name`](Self::focus_name): the watcher already delivers the change.
    pub fn reload_and_focus(self: &Rc<Self>, name: OsString) {
        let loc = self.location();
        self.load(
            loc,
            LoadOpts {
                keep_selection: true,
                cursor_name: Some(name),
                skip_history: true,
            },
        );
    }

    /// Put the cursor on `name`: immediately if it is listed, otherwise as soon as it shows up
    /// through a listing batch or a watcher update, but for at most 5 seconds. A user cursor move
    /// cancels the request (never fights the user).
    pub fn focus_name(&self, name: OsString) {
        let found = self.model.borrow_mut().set_cursor_name(&name);
        if found {
            self.pending_cursor.borrow_mut().take();
            self.pending_until.set(None);
            self.changed.emit(&PanelEvent::Redraw);
        } else {
            *self.pending_cursor.borrow_mut() = Some(name);
            self.pending_until.set(Some(Instant::now() + FOCUS_TTL));
        }
    }

    /// Whether a directory watch is established (tests, status).
    pub fn watching(&self) -> bool {
        self.watch
            .borrow()
            .as_ref()
            .is_some_and(|w| w.handle.is_some())
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
        self.pending_until.set(None);
        self.model
            .borrow_mut()
            .begin_load(has_parent(&loc), opts.keep_selection && same);
        self.set_state(LoadState::Loading);
        self.changed.emit(&PanelEvent::Location);
        self.changed.emit(&PanelEvent::Model(ModelChange::Reset));

        // Before the listing is queued: changes made while listing must not be missed.
        self.sync_watch(&loc);

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
        self.stop_watch();
    }

    fn apply_list_event(self: &Rc<Self>, ev: ListEvent) {
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
                self.pump_watch(); // flush changes buffered while listing
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
        if let Some(until) = self.pending_until.get()
            && Instant::now() > until
        {
            self.pending_cursor.borrow_mut().take();
            self.pending_until.set(None);
            return;
        }
        let name = self.pending_cursor.borrow().clone();
        if let Some(name) = name
            && self.model.borrow_mut().set_cursor_name(&name)
        {
            self.pending_cursor.borrow_mut().take();
            self.pending_until.set(None);
        }
    }

    // ---- directory watching -----------------------------------------------------------------
    /// Make the watch match `loc`: keep it for the same directory, (re)start for another local
    /// directory, stop for anything else.
    fn sync_watch(self: &Rc<Self>, loc: &Location) {
        match loc.local_path() {
            Some(dir) => {
                let keep = self.watch.borrow().as_ref().is_some_and(|w| w.dir == dir);
                if !keep {
                    self.start_watch(dir.to_path_buf());
                }
            }
            None => self.stop_watch(),
        }
    }

    /// Stop watching and invalidate in-flight watcher work.
    fn stop_watch(&self) {
        self.watch_epoch.set(self.watch_epoch.get() + 1);
        self.watch.borrow_mut().take(); // drops the handle: the debounce thread ends
    }

    fn start_watch(self: &Rc<Self>, dir: PathBuf) {
        let epoch = self.watch_epoch.get() + 1;
        self.watch_epoch.set(epoch);
        *self.watch.borrow_mut() = Some(WatchState {
            epoch,
            dir: dir.clone(),
            handle: None,
            pending: BTreeSet::new(),
            rescan: false,
            in_flight: false,
        });
        // inotify_add_watch is a syscall on the directory: do it on the pool, not here.
        let rx = wiring::run_on_pool(&self.services.pool, move |tx| {
            let _ = tx.send_blocking(watch_dir(&dir, WATCH_DEBOUNCE));
        });
        let weak = Rc::downgrade(self);
        wiring::attach(
            rx,
            move |res| {
                if let Some(this) = weak.upgrade() {
                    this.watch_started(epoch, res);
                }
                ControlFlow::Break(())
            },
            || {},
        );
    }

    fn watch_started(self: &Rc<Self>, epoch: u64, res: Result<WatchHandle, VfsError>) {
        if self.watch_epoch.get() != epoch {
            return; // navigated away meanwhile; dropping `res` stops the watch
        }
        let handle = match res {
            Ok(h) => h,
            Err(e) => {
                tracing::warn!(error = %e, "cannot watch directory; panel will not auto-refresh");
                return;
            }
        };
        let rx = handle.events.clone();
        match self.watch.borrow_mut().as_mut() {
            Some(w) if w.epoch == epoch => w.handle = Some(handle),
            _ => return,
        }
        let weak = Rc::downgrade(self);
        wiring::attach(
            rx,
            move |ev| match weak.upgrade() {
                Some(this) if this.watch_epoch.get() == epoch => {
                    this.on_watch_event(ev);
                    ControlFlow::Continue(())
                }
                _ => ControlFlow::Break(()),
            },
            || {},
        );
    }

    fn on_watch_event(self: &Rc<Self>, ev: WatchEvent) {
        match ev {
            WatchEvent::Changed(names) => {
                if let Some(w) = self.watch.borrow_mut().as_mut() {
                    w.pending.extend(names);
                }
                self.pump_watch();
            }
            WatchEvent::Rescan => {
                if let Some(w) = self.watch.borrow_mut().as_mut() {
                    w.rescan = true;
                }
                self.pump_watch();
            }
            WatchEvent::DirGone => self.on_dir_gone(),
        }
    }

    /// Dispatch buffered watcher work unless a listing or another batch is running.
    fn pump_watch(self: &Rc<Self>) {
        if self.state() == LoadState::Loading {
            return; // flushed from `apply_list_event(Done)`
        }
        let (epoch, dir, names) = {
            let mut guard = self.watch.borrow_mut();
            let Some(w) = guard.as_mut() else { return };
            if w.in_flight {
                return;
            }
            if w.rescan {
                w.rescan = false;
                w.pending.clear();
                drop(guard);
                self.reload();
                return;
            }
            if w.pending.is_empty() {
                return;
            }
            w.in_flight = true;
            (w.epoch, w.dir.clone(), std::mem::take(&mut w.pending))
        };
        let generation = self.generation.get();
        let services = self.services.clone();
        let rx = wiring::run_on_pool(&self.services.pool, move |tx| {
            let mut out = Restat {
                changed: Vec::new(),
                removed: Vec::new(),
            };
            for name in names {
                match services.vfs.get(&Location::Local(dir.clone())) {
                    Ok(vfs) => match vfs.stat(&Location::Local(dir.join(&name))) {
                        Ok(entry) => out.changed.push(entry),
                        Err(VfsError::NotFound(_)) => out.removed.push(name),
                        Err(e) => {
                            tracing::debug!(error = %e, "watch re-stat failed; keeping entry")
                        }
                    },
                    Err(e) => tracing::debug!(error = %e, "no provider for watch re-stat"),
                }
            }
            let _ = tx.send_blocking(out);
        });
        let weak = Rc::downgrade(self);
        wiring::attach(
            rx,
            move |restat| {
                if let Some(this) = weak.upgrade() {
                    this.apply_restat(epoch, generation, restat);
                }
                ControlFlow::Break(())
            },
            || {},
        );
    }

    fn apply_restat(self: &Rc<Self>, epoch: u64, generation: u64, restat: Restat) {
        if self.watch_epoch.get() != epoch {
            return;
        }
        if let Some(w) = self.watch.borrow_mut().as_mut() {
            w.in_flight = false;
        }
        if self.generation.get() == generation {
            let change = self
                .model
                .borrow_mut()
                .apply_update(restat.changed, &restat.removed);
            self.try_pending_cursor();
            self.changed.emit(&PanelEvent::Model(change));
        } // else: a reload replaced the model; its listing is newer than this restat
        self.pump_watch();
    }

    /// The watched directory vanished: go to the nearest existing ancestor.
    fn on_dir_gone(self: &Rc<Self>) {
        let Some(gone) = self.location().local_path().map(PathBuf::from) else {
            return;
        };
        let generation = self.generation.get();
        let services = self.services.clone();
        let probe = gone.clone();
        let rx = wiring::run_on_pool(&self.services.pool, move |tx| {
            let mut child = probe.clone();
            let mut cur = probe.parent().map(PathBuf::from);
            while let Some(dir) = cur {
                let exists = services
                    .vfs
                    .get(&Location::Local(dir.clone()))
                    .is_ok_and(|v| v.stat(&Location::Local(dir.clone())).is_ok());
                if exists {
                    let _ = tx.send_blocking((dir, child.file_name().map(OsStr::to_owned)));
                    return;
                }
                child = dir.clone();
                cur = dir.parent().map(PathBuf::from);
            }
        });
        let weak = Rc::downgrade(self);
        wiring::attach(
            rx,
            move |(ancestor, child)| {
                if let Some(this) = weak.upgrade()
                    && this.generation.get() == generation
                {
                    this.load(
                        Location::Local(ancestor),
                        LoadOpts {
                            keep_selection: false,
                            cursor_name: child,
                            skip_history: false,
                        },
                    );
                }
                ControlFlow::Break(())
            },
            || {},
        );
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

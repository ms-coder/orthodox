//! `SearchRun`: one search of the Find window, independent of the widgets.
//!
//! ```text
//! start() --pool--> SearchHandle::start (worker)         engine threads (ofm-search)
//!                        |                                   | bounded channel (64)
//!                        v                                   v
//!        main thread: consumer future  --buffer-->  Batcher  --50 ms timer-->  HitModel + stats
//! ```
//!
//! The consumer only buffers (cheap); a 50 ms timer moves the buffer into the list model and
//! publishes counters, so the UI is updated at most 20 times a second however fast hits arrive.
//! Cancelling is instant for the UI (`stop`), the engine ends with `Done { cancelled }`; dropping
//! the run (window closed) cancels the engine and ends the consumer.

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::glib;
use ofm_core::CancelToken;
use ofm_search::{
    Hit, SearchError, SearchEvent, SearchHandle, SearchProgress, SearchQuery, SearchSummary,
    SearchWarning,
};

use super::batch::{Batcher, UPDATE_INTERVAL};
use super::model::HitModel;
use crate::context::Services;
use crate::signal::Signal;
use crate::wiring;

/// Starts the engine; the default is `SearchHandle::start`, tests inject a fake.
pub type Starter =
    Arc<dyn Fn(SearchQuery, CancelToken) -> Result<SearchHandle, SearchError> + Send + Sync>;

/// The real engine.
pub fn real_starter() -> Starter {
    Arc::new(SearchHandle::start)
}

/// Warnings kept for the details view (the counter keeps counting).
const MAX_KEPT_WARNINGS: usize = 2000;
/// A stopped search that has not reported its end after this long is abandoned.
const STOP_GRACE: Duration = Duration::from_secs(3);
/// The consumer hands the main loop back after this much continuous work.
const SLICE: Duration = Duration::from_millis(4);

/// Where a run is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RunState {
    /// Nothing started yet (or cleared).
    #[default]
    Idle,
    /// The engine is being started on a worker.
    Starting,
    /// Searching.
    Running,
    /// `stop` was requested; waiting for the engine's final event.
    Stopping,
    /// Ended (also after cancel or truncation).
    Finished {
        /// Ended because of `stop`.
        cancelled: bool,
        /// `max_hits` reached.
        truncated: bool,
    },
    /// The search could not run or died; holds the message.
    Failed(String),
}

impl RunState {
    /// True while the engine may still produce results.
    pub fn is_active(&self) -> bool {
        matches!(
            self,
            RunState::Starting | RunState::Running | RunState::Stopping
        )
    }
}

/// Counters shown under the results.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunStats {
    /// Hits in the list.
    pub found: u64,
    /// Directories scanned.
    pub dirs: u64,
    /// Files examined.
    pub files: u64,
    /// Directory being scanned.
    pub current: Option<PathBuf>,
    /// Warnings so far.
    pub warnings: u64,
    /// Wall time of a finished search.
    pub elapsed: Option<Duration>,
}

/// What observers are told (all coalesced to <= 20 per second, except `State`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEvent {
    /// Counters and/or results changed.
    Stats,
    /// The run state changed.
    State,
}

/// One search.
pub struct SearchRun {
    model: HitModel,
    state: RefCell<RunState>,
    stats: RefCell<RunStats>,
    warnings: RefCell<Vec<SearchWarning>>,
    pending: RefCell<Batcher<Hit>>,
    cancel: RefCell<CancelToken>,
    handle: RefCell<Option<SearchHandle>>,
    generation: Cell<u64>,
    dirty: Cell<bool>,
    /// `stop` was requested: hits that still arrive are dropped.
    stopping: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    /// Observers.
    pub changed: Signal<RunEvent>,
}

impl SearchRun {
    /// A run that fills `model`.
    pub fn new(model: HitModel) -> Rc<Self> {
        Rc::new(SearchRun {
            model,
            state: RefCell::new(RunState::Idle),
            stats: RefCell::new(RunStats::default()),
            warnings: RefCell::new(Vec::new()),
            pending: RefCell::new(Batcher::new(UPDATE_INTERVAL, Instant::now())),
            cancel: RefCell::new(CancelToken::new()),
            handle: RefCell::new(None),
            generation: Cell::new(0),
            dirty: Cell::new(false),
            stopping: Cell::new(false),
            timer: RefCell::new(None),
            changed: Signal::new(),
        })
    }

    /// The result model.
    pub fn model(&self) -> &HitModel {
        &self.model
    }

    /// Current state.
    pub fn state(&self) -> RunState {
        self.state.borrow().clone()
    }

    /// True while searching or stopping.
    pub fn is_active(&self) -> bool {
        self.state.borrow().is_active()
    }

    /// Counters.
    pub fn stats(&self) -> RunStats {
        self.stats.borrow().clone()
    }

    /// The kept warnings (at most 2000).
    pub fn warnings(&self) -> Vec<SearchWarning> {
        self.warnings.borrow().clone()
    }

    fn set_state(&self, state: RunState) {
        *self.state.borrow_mut() = state;
        self.changed.emit(&RunEvent::State);
    }

    /// Forget results and counters of the previous search.
    fn reset(&self) {
        self.model.clear();
        *self.stats.borrow_mut() = RunStats::default();
        self.warnings.borrow_mut().clear();
        self.pending.borrow_mut().take_all(Instant::now());
    }

    /// Start a new search (a running one is cancelled and abandoned first). The engine is started
    /// on the I/O pool; its events are consumed on the main loop.
    pub fn start(self: &Rc<Self>, services: &Services, starter: &Starter, query: SearchQuery) {
        self.shutdown();
        let generation = self.generation.get() + 1;
        self.generation.set(generation);
        self.reset();
        self.stopping.set(false);
        let token = CancelToken::new();
        *self.cancel.borrow_mut() = token.clone();
        self.set_state(RunState::Starting);
        self.changed.emit(&RunEvent::Stats);

        let starter = Arc::clone(starter);
        let rx = wiring::run_on_pool(&services.pool, move |tx| {
            // Starting validates and may stat the roots: a worker, never the GTK thread.
            let _ = tx.send_blocking(starter(query, token));
        });
        let weak = Rc::downgrade(self);
        wiring::attach(
            rx,
            move |started| {
                if let Some(this) = weak.upgrade()
                    && this.generation.get() == generation
                {
                    this.engine_started(generation, started);
                } // else: abandoned meanwhile; dropping the handle cancels the engine
                ControlFlow::Break(())
            },
            || {},
        );
    }

    fn engine_started(self: &Rc<Self>, generation: u64, started: Result<SearchHandle, SearchError>) {
        let handle = match started {
            Ok(h) => h,
            Err(e) => {
                self.set_state(RunState::Failed(e.to_string()));
                return;
            }
        };
        let events = handle.events().clone();
        *self.handle.borrow_mut() = Some(handle);
        self.set_state(RunState::Running);
        self.start_timer(generation);
        let weak: Weak<SearchRun> = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut slice_start = Instant::now();
            while let Ok(ev) = events.recv().await {
                let Some(this) = weak.upgrade() else { return };
                if this.generation.get() != generation {
                    return;
                }
                this.on_event(ev);
                if slice_start.elapsed() > SLICE {
                    glib::timeout_future(Duration::from_millis(1)).await;
                    slice_start = Instant::now();
                }
            }
            // The channel closed. Normally `Done`/`Failed` came first.
            if let Some(this) = weak.upgrade()
                && this.generation.get() == generation
                && this.is_active()
            {
                this.finish(RunState::Failed(
                    crate::i18n::tr("The search ended unexpectedly.").to_owned(),
                ));
            }
        });
    }

    fn on_event(&self, ev: SearchEvent) {
        match ev {
            SearchEvent::Found(_) if self.stopping.get() => {}
            SearchEvent::Found(batch) => {
                self.stats.borrow_mut().found += batch.len() as u64;
                self.pending.borrow_mut().push(batch);
                self.dirty.set(true);
            }
            SearchEvent::Progress(SearchProgress {
                dirs_scanned,
                files_scanned,
                current,
                ..
            }) => {
                let mut s = self.stats.borrow_mut();
                s.dirs = dirs_scanned;
                s.files = files_scanned;
                s.current = current;
                self.dirty.set(true);
            }
            SearchEvent::Warning(w) => {
                self.stats.borrow_mut().warnings += 1;
                let mut kept = self.warnings.borrow_mut();
                if kept.len() < MAX_KEPT_WARNINGS {
                    kept.push(w);
                }
                self.dirty.set(true);
            }
            SearchEvent::Done(SearchSummary {
                hits,
                files_scanned,
                dirs_scanned,
                elapsed,
                cancelled,
                truncated,
            }) => {
                {
                    let mut s = self.stats.borrow_mut();
                    s.found = hits.max(s.found);
                    s.files = files_scanned;
                    s.dirs = dirs_scanned;
                    s.current = None;
                    s.elapsed = Some(elapsed);
                }
                self.finish(RunState::Finished {
                    cancelled,
                    truncated,
                });
            }
            SearchEvent::Failed(e) => self.finish(RunState::Failed(e.to_string())),
        }
    }

    /// Move everything received so far into the model now (before a panelize).
    pub fn settle(&self) {
        self.flush(true);
    }

    /// Apply the buffer to the model and publish counters (also called by the timer).
    fn flush(&self, force: bool) {
        let now = Instant::now();
        let batch = if force {
            self.pending.borrow_mut().take_all(now)
        } else {
            self.pending.borrow_mut().take_if_due(now)
        };
        let had_hits = !batch.is_empty();
        self.model.append(batch);
        // The timer calls this at most 20 times a second: that bounds the counter updates too.
        if self.dirty.replace(false) || had_hits {
            self.changed.emit(&RunEvent::Stats);
        }
    }

    fn start_timer(self: &Rc<Self>, generation: u64) {
        let weak = Rc::downgrade(self);
        let id = glib::timeout_add_local(UPDATE_INTERVAL, move || match weak.upgrade() {
            Some(this) if this.generation.get() == generation && this.is_active() => {
                this.flush(false);
                glib::ControlFlow::Continue
            }
            _ => glib::ControlFlow::Break,
        });
        *self.timer.borrow_mut() = Some(id);
    }

    fn stop_timer(&self) {
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }

    fn finish(&self, state: RunState) {
        self.flush(true);
        self.stop_timer();
        // The engine is done: its handle (channel, token) is no longer needed.
        self.handle.borrow_mut().take();
        self.set_state(state);
        self.changed.emit(&RunEvent::Stats);
    }

    /// Ask the engine to stop. Results found so far stay in the list. The state becomes
    /// `Stopping`, then `Finished { cancelled: true }` when the engine reports its end.
    pub fn stop(self: &Rc<Self>) {
        if !self.is_active() {
            return;
        }
        self.cancel.borrow().cancel();
        self.stopping.set(true);
        if let Some(h) = self.handle.borrow().as_ref() {
            h.cancel();
        }
        self.set_state(RunState::Stopping);
        // A misbehaving engine must not leave the window stuck in "Stopping".
        let weak = Rc::downgrade(self);
        let generation = self.generation.get();
        glib::timeout_add_local_once(STOP_GRACE, move || {
            if let Some(this) = weak.upgrade()
                && this.generation.get() == generation
                && this.state() == RunState::Stopping
            {
                tracing::warn!("search did not report its end after cancel; abandoning it");
                this.generation.set(generation + 1);
                this.finish(RunState::Finished {
                    cancelled: true,
                    truncated: false,
                });
            }
        });
    }

    /// Cancel and abandon the running search without notifying anyone (window closed, new
    /// search): late events are ignored and the engine's threads end on their own.
    pub fn shutdown(&self) {
        self.cancel.borrow().cancel();
        self.generation.set(self.generation.get() + 1);
        self.stop_timer();
        self.handle.borrow_mut().take();
        if self.is_active() {
            *self.state.borrow_mut() = RunState::Idle;
        }
    }
}

impl Drop for SearchRun {
    fn drop(&mut self) {
        self.cancel.borrow().cancel();
        if let Some(id) = self.timer.borrow_mut().take() {
            id.remove();
        }
    }
}

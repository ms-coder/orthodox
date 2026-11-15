//! [`JobsController`]: the single UI-side owner of the `JobEngine`, the job table the jobs view
//! renders and the pump that turns engine events into UI reactions. Main thread only.
//!
//! The engine is created lazily by [`JobsController::submit`]. UI tests that must not spawn worker
//! threads feed synthetic events through [`JobsController::handle_event`] (an `Ask` event can be
//! built with `ofm_core::Responder::channel()`); T3.3 tests the dialogs that way before the real
//! engine (T3.1) is merged.

use std::cell::{OnceCell, RefCell};
use std::rc::Rc;

use ofm_jobs::{JobEngine, JobEvent, JobId, JobSpec, JobState, Outcome, Progress, RunMode};

use crate::context::AppContext;
use crate::signal::Signal;

/// One row of the jobs view.
#[derive(Debug, Clone)]
pub struct JobEntry {
    /// Engine id.
    pub id: JobId,
    /// Title from `JobSpec::title`.
    pub title: String,
    /// Scheduling mode.
    pub mode: RunMode,
    /// Lifecycle state.
    pub state: JobState,
    /// Last progress snapshot.
    pub progress: Progress,
    /// True while a question dialog for this job is open (the worker is blocked).
    pub asking: bool,
    /// Set when the job finished.
    pub outcome: Option<Outcome>,
}

/// What observers of the job table are told (the jobs view, status line, quit prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobsEvent {
    /// A job was added.
    Added(JobId),
    /// State, progress or question flag of a job changed.
    Updated(JobId),
    /// A job finished (its `outcome` is set).
    Finished(JobId),
    /// Finished jobs were removed from the table.
    Cleared,
}

/// Owner of the engine and the job table.
pub struct JobsController {
    engine: OnceCell<JobEngine>,
    jobs: RefCell<Vec<JobEntry>>,
    /// Observers; see [`JobsEvent`].
    pub changed: Signal<JobsEvent>,
}

impl JobsController {
    /// Empty controller; no threads are started until the first [`submit`](Self::submit).
    pub fn new() -> Self {
        JobsController {
            engine: OnceCell::new(),
            jobs: RefCell::new(Vec::new()),
            changed: Signal::new(),
        }
    }

    /// Use a prepared engine (tests: `JobEngine::with_fs(FaultyFs)`); must be called before the
    /// first submit. Returns false if an engine already exists.
    pub fn set_engine(&self, engine: JobEngine) -> bool {
        self.engine.set(engine).is_ok()
    }

    /// Submit a job and start pumping engine events on the GTK main loop (first call only).
    /// Returns immediately; the worker does all I/O.
    pub fn submit(self: &Rc<Self>, _ctx: &Rc<AppContext>, _spec: JobSpec, _mode: RunMode) -> JobId {
        // T3.3: create the engine if needed (`JobEngine::new`), `wiring::attach(engine.events(), ..)`
        // calling `handle_event` with a Weak<Self> / Weak<AppContext>, then `engine.submit`.
        todo!("T3.3")
    }

    /// Pause a job.
    pub fn pause(&self, id: JobId) {
        if let Some(e) = self.engine.get() {
            e.pause(id);
        }
    }

    /// Resume a job.
    pub fn resume(&self, id: JobId) {
        if let Some(e) = self.engine.get() {
            e.resume(id);
        }
    }

    /// Cancel a job.
    pub fn cancel(&self, id: JobId) {
        if let Some(e) = self.engine.get() {
            e.cancel(id);
        }
    }

    /// Cancel every unfinished job (after the user confirmed quitting).
    pub fn cancel_all(&self) {
        if let Some(e) = self.engine.get() {
            e.cancel_all();
        }
    }

    /// Snapshot of the job table, oldest first.
    pub fn jobs(&self) -> Vec<JobEntry> {
        self.jobs.borrow().clone()
    }

    /// True while any job is queued, running or paused (quit confirmation).
    pub fn has_unfinished(&self) -> bool {
        self.jobs
            .borrow()
            .iter()
            .any(|j| j.state != JobState::Finished)
    }

    /// Drop finished jobs from the table (`jobs.clear_finished`).
    pub fn clear_finished(&self) {
        let ids: Vec<JobId> = {
            let mut jobs = self.jobs.borrow_mut();
            let ids = jobs
                .iter()
                .filter(|j| j.state == JobState::Finished)
                .map(|j| j.id)
                .collect();
            jobs.retain(|j| j.state != JobState::Finished);
            ids
        };
        if let Some(e) = self.engine.get() {
            for id in ids {
                e.forget(id);
            }
        }
        self.changed.emit(&JobsEvent::Cleared);
    }

    /// Apply one engine event on the main thread: update the table, emit [`JobsEvent`]s, open the
    /// conflict/error/trash dialog for `Ask` (`dialogs::conflict::present`), and on `Finished`
    /// refresh panels and show errors. Public so tests can feed synthetic events.
    pub fn handle_event(self: &Rc<Self>, _ctx: &Rc<AppContext>, _ev: JobEvent) {
        todo!("T3.3")
    }
}

impl Default for JobsController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_controller_has_nothing_unfinished_and_no_engine() {
        let c = JobsController::new();
        assert!(!c.has_unfinished());
        assert!(c.jobs().is_empty());
        // No engine yet: control calls are harmless no-ops.
        c.pause(JobId(1));
        c.cancel_all();
        c.clear_finished();
    }
}

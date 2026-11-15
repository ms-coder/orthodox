//! The job engine (T3.1 owns the body; the public API below is frozen by T3.0, see ADR-0006).
//!
//! Scheduling: one FIFO **queue lane** thread runs `RunMode::Queued` jobs one after another; every
//! `RunMode::Parallel` job gets its own thread immediately. Each job runs `runner::run_job` with a
//! `JobCtx` whose sink turns progress into `JobEvent::Progress` (unbounded channel, never blocks)
//! and questions into `JobEvent::Ask` via `ctx::ask_over_events`.
//!
//! Required behaviour (T3.1 acceptance):
//! - events per job arrive in order `Queued, Started, (Progress|Ask|StateChanged)*, Finished`;
//!   `Queued` is sent before `submit` returns; exactly one `Finished` per job;
//! - `cancel` on a queued job removes it (`Finished{Cancelled}` without `Started`); on a running
//!   job it fires the `CancelToken` and wakes a paused/asking worker;
//! - `pause`/`resume` close/open the job's `PauseGate` and emit `StateChanged`;
//! - a panic in a worker never kills the lane (`run_job` catches it; a panic outside it is caught
//!   again here and reported as `Finished{Failed}`);
//! - dropping the engine cancels everything and joins the workers (bounded wait);
//! - `JobEngine::with_fs` lets tests inject `FaultyFs`.

use std::sync::Arc;

use crate::event::{JobEvent, JobState};
use crate::fs::Fs;
use crate::spec::{JobId, JobSpec, RunMode};

/// Snapshot of one job for the jobs view / quit prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    /// Id.
    pub id: JobId,
    /// Title (`JobSpec::title`).
    pub title: String,
    /// Scheduling mode.
    pub mode: RunMode,
    /// Current state.
    pub state: JobState,
}

/// Handle to the engine; owns the worker threads.
#[derive(Debug)]
pub struct JobEngine {
    #[allow(dead_code, reason = "T3.1 fills in")]
    events: async_channel::Receiver<JobEvent>,
}

impl JobEngine {
    /// Start an engine using the real file system (`RealFs`) and the system clock.
    pub fn new() -> Self {
        todo!("T3.1")
    }

    /// Start an engine on a custom `Fs` (tests: `FaultyFs`).
    pub fn with_fs(_fs: Arc<dyn Fs>) -> Self {
        todo!("T3.1")
    }

    /// Submit a job; returns immediately with its id (ids are unique, increasing).
    pub fn submit(&self, _spec: JobSpec, _mode: RunMode) -> JobId {
        todo!("T3.1")
    }

    /// Pause a job at its next checkpoint (no effect on queued/finished jobs).
    pub fn pause(&self, _id: JobId) {
        todo!("T3.1")
    }

    /// Resume a paused job.
    pub fn resume(&self, _id: JobId) {
        todo!("T3.1")
    }

    /// Cancel a job (queued: removed; running: stops, temp files removed).
    pub fn cancel(&self, _id: JobId) {
        todo!("T3.1")
    }

    /// Cancel every unfinished job (quit with running jobs).
    pub fn cancel_all(&self) {
        todo!("T3.1")
    }

    /// Event stream for the UI. There must be exactly ONE consumer (the receiver is a work
    /// queue): `JobsController` owns it.
    pub fn events(&self) -> async_channel::Receiver<JobEvent> {
        self.events.clone()
    }

    /// Ids of jobs not yet finished (queued, running or paused): the quit confirmation.
    pub fn running(&self) -> Vec<JobId> {
        todo!("T3.1")
    }

    /// Snapshot of all jobs known to the engine (finished ones until `forget`).
    pub fn jobs(&self) -> Vec<JobInfo> {
        todo!("T3.1")
    }

    /// Drop the bookkeeping of a finished job (jobs view "clear finished").
    pub fn forget(&self, _id: JobId) {
        todo!("T3.1")
    }
}

impl Default for JobEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.1"]
    fn event_order_and_single_finished() {}

    #[test]
    #[ignore = "T3.1"]
    fn queued_jobs_run_in_order_parallel_jobs_run_at_once() {}

    #[test]
    #[ignore = "T3.1"]
    fn cancel_wakes_paused_and_asking_workers() {}
}

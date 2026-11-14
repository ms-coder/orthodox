//! The job engine: one queued lane + one thread per parallel job. Worker panics are caught
//! (`catch_unwind`) and reported as `Outcome::Failed`.

use crate::event::JobEvent;
use crate::spec::{JobId, JobSpec, RunMode};

/// Handle to the engine; owns the worker threads.
#[derive(Debug)]
pub struct JobEngine {
    #[allow(dead_code, reason = "M3 fills in")]
    events: async_channel::Receiver<JobEvent>,
}

impl JobEngine {
    /// Start an engine using the real file system.
    pub fn new() -> Self {
        todo!("M3")
    }
    /// Submit a job; returns immediately.
    pub fn submit(&self, _spec: JobSpec, _mode: RunMode) -> JobId {
        todo!("M3")
    }
    /// Pause a running job at the next safe point.
    pub fn pause(&self, _id: JobId) {
        todo!("M3")
    }
    /// Resume a paused job.
    pub fn resume(&self, _id: JobId) {
        todo!("M3")
    }
    /// Cancel a job (queued: removed; running: stops, temp files removed).
    pub fn cancel(&self, _id: JobId) {
        todo!("M3")
    }
    /// Event stream for the UI (clone the receiver freely).
    pub fn events(&self) -> async_channel::Receiver<JobEvent> {
        self.events.clone()
    }
    /// Ids of jobs not yet finished (quit confirmation).
    pub fn running(&self) -> Vec<JobId> {
        todo!("M3")
    }
}

impl Default for JobEngine {
    fn default() -> Self {
        Self::new()
    }
}

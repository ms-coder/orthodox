//! Fixed I/O thread pool (4 threads) for listing/stat/preview work (owned by T1.2).
//! Panics in a task are caught and logged; they never kill a worker or cross threads silently.

/// Handle to the pool; cheap to clone.
#[derive(Debug, Clone)]
pub struct IoPool {
    #[allow(dead_code, reason = "T1.2 fills in")]
    threads: usize,
}

impl IoPool {
    /// Start a pool with `threads` workers (production: 4).
    pub fn new(threads: usize) -> Self {
        IoPool { threads }
    }
    /// Queue a task; runs on one of the workers.
    pub fn spawn(&self, _task: impl FnOnce() + Send + 'static) {
        todo!("T1.2")
    }
}

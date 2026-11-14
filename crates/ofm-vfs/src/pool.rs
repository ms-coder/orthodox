//! Fixed I/O thread pool (4 threads) for listing/stat/preview work (owned by T1.2).
//! Panics in a task are caught and logged; they never kill a worker or cross threads silently.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

type Task = Box<dyn FnOnce() + Send + 'static>;

/// Handle to the pool; cheap to clone. Workers exit when the last handle is dropped (after
/// finishing queued tasks).
#[derive(Debug, Clone)]
pub struct IoPool {
    tx: Arc<Sender<Task>>,
    threads: usize,
}

impl IoPool {
    /// Start a pool with `threads` workers (production: 4; at least 1).
    pub fn new(threads: usize) -> Self {
        let threads = threads.max(1);
        let (tx, rx) = channel::<Task>();
        let rx = Arc::new(Mutex::new(rx));
        for i in 0..threads {
            let rx = Arc::clone(&rx);
            let spawned = std::thread::Builder::new()
                .name(format!("ofm-io-{i}"))
                .spawn(move || worker(&rx));
            if let Err(e) = spawned {
                tracing::error!(error = %e, "failed to spawn I/O worker");
            }
        }
        IoPool {
            tx: Arc::new(tx),
            threads,
        }
    }

    /// Number of workers requested.
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Queue a task; runs on one of the workers.
    pub fn spawn(&self, task: impl FnOnce() + Send + 'static) {
        if self.tx.send(Box::new(task)).is_err() {
            tracing::error!("I/O pool has no live workers; task dropped");
        }
    }
}

fn worker(rx: &Mutex<Receiver<Task>>) {
    loop {
        // Hold the lock only while waiting for a task, never while running it.
        let task = {
            let Ok(guard) = rx.lock() else { return };
            match guard.recv() {
                Ok(t) => t,
                Err(_) => return,
            }
        };
        if let Err(payload) = catch_unwind(AssertUnwindSafe(task)) {
            let msg = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_owned());
            tracing::error!(panic = %msg, "I/O pool task panicked");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[test]
    fn runs_all_tasks() {
        let pool = IoPool::new(4);
        let n = Arc::new(AtomicUsize::new(0));
        let (done_tx, done_rx) = channel();
        for _ in 0..100 {
            let n = n.clone();
            let d = done_tx.clone();
            pool.spawn(move || {
                n.fetch_add(1, Ordering::SeqCst);
                let _ = d.send(());
            });
        }
        for _ in 0..100 {
            assert!(done_rx.recv_timeout(Duration::from_secs(5)).is_ok());
        }
        assert_eq!(n.load(Ordering::SeqCst), 100);
    }

    #[test]
    fn survives_panics_and_uses_multiple_threads() {
        let pool = IoPool::new(2);
        for _ in 0..8 {
            pool.spawn(|| panic!("boom"));
        }
        let (tx, rx) = channel();
        for _ in 0..8 {
            let tx = tx.clone();
            pool.spawn(move || {
                std::thread::sleep(Duration::from_millis(20));
                let _ = tx.send(std::thread::current().id());
            });
        }
        let ids: HashSet<_> = (0..8)
            .filter_map(|_| rx.recv_timeout(Duration::from_secs(5)).ok())
            .collect();
        assert_eq!(pool.threads(), 2);
        assert_eq!(ids.len(), 2);
    }
}

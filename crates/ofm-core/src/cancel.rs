//! Cooperative cancellation shared between UI and worker threads.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Cheap-to-clone cancellation flag. Workers check it between syscalls and every 1 MiB of copied data.
#[derive(Debug, Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    /// New, not cancelled.
    pub fn new() -> Self {
        Self::default()
    }
    /// Request cancellation (idempotent, visible to all clones).
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Relaxed);
    }
    /// True once [`cancel`](Self::cancel) was called on any clone.
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clones_share_state() {
        let a = CancelToken::new();
        let b = a.clone();
        assert!(!b.is_cancelled());
        a.cancel();
        assert!(b.is_cancelled());
    }
}

//! One-shot question/answer channel: a worker asks the UI (conflict, error, password) and blocks;
//! the UI answers without ever blocking.

use std::sync::mpsc;
use std::time::Duration;

/// UI side: answer the question exactly once. Dropping it without answering makes
/// [`Pending::wait`] return `None` (the worker treats that as "abort").
#[derive(Debug)]
pub struct Responder<T>(mpsc::SyncSender<T>);

/// Worker side: blocks until answered.
#[derive(Debug)]
pub struct Pending<T>(mpsc::Receiver<T>);

/// Result of [`Pending::wait_timeout`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait<T> {
    /// The UI answered.
    Answered(T),
    /// No answer yet; the caller may check for cancellation and wait again.
    TimedOut,
    /// The responder was dropped without an answer.
    Dropped,
}

impl<T> Responder<T> {
    /// Create a connected pair.
    pub fn channel() -> (Responder<T>, Pending<T>) {
        let (tx, rx) = mpsc::sync_channel(1);
        (Responder(tx), Pending(rx))
    }
    /// Deliver the answer. Returns false if the worker is gone.
    pub fn answer(self, value: T) -> bool {
        self.0.send(value).is_ok()
    }
}

impl<T> Pending<T> {
    /// Block until answered; `None` if the responder was dropped.
    pub fn wait(self) -> Option<T> {
        self.0.recv().ok()
    }

    /// Wait at most `timeout` (poll loop for workers that must stay cancellable while asking).
    pub fn wait_timeout(&self, timeout: Duration) -> Wait<T> {
        match self.0.recv_timeout(timeout) {
            Ok(v) => Wait::Answered(v),
            Err(mpsc::RecvTimeoutError::Timeout) => Wait::TimedOut,
            Err(mpsc::RecvTimeoutError::Disconnected) => Wait::Dropped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn answer_and_drop() {
        let (r, p) = Responder::channel();
        assert!(r.answer(5));
        assert_eq!(p.wait(), Some(5));
        let (r, p) = Responder::<u8>::channel();
        drop(r);
        assert_eq!(p.wait(), None);
    }

    #[test]
    fn wait_timeout_states() {
        let (r, p) = Responder::<u8>::channel();
        assert_eq!(p.wait_timeout(Duration::from_millis(5)), Wait::TimedOut);
        assert!(r.answer(3));
        assert_eq!(p.wait_timeout(Duration::from_millis(5)), Wait::Answered(3));
        let (r, p) = Responder::<u8>::channel();
        drop(r);
        assert_eq!(p.wait_timeout(Duration::from_millis(5)), Wait::Dropped);
    }
}

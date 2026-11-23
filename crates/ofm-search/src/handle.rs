//! `SearchHandle`: start a search on worker threads and receive its events.

use std::time::Duration;

use async_channel::Receiver;
use ofm_core::CancelToken;

use crate::error::SearchError;
use crate::event::SearchEvent;
use crate::query::SearchQuery;

/// Hits per `SearchEvent::Found` batch.
pub const BATCH_SIZE: usize = 256;
/// A partial batch is flushed after this long, so the first hits show up quickly.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(50);
/// Minimum time between `Progress` events (<= 10 Hz).
pub const PROGRESS_INTERVAL: Duration = Duration::from_millis(100);
/// Capacity of the event channel. Bounded on purpose: a stalled UI makes workers wait instead of
/// buffering millions of hits. Workers must never block past a cancel or a dropped receiver
/// (`send` fails when the receiver is gone: stop then).
pub const CHANNEL_CAPACITY: usize = 64;

/// A running (or finished) search. Dropping it cancels the search without blocking: workers see the
/// token / the closed channel and stop on their own.
#[derive(Debug)]
pub struct SearchHandle {
    events: Receiver<SearchEvent>,
    cancel: CancelToken,
}

impl SearchHandle {
    /// Validate `query` and start the workers. Returns immediately; never blocks on I/O
    /// beyond one `stat` (and one `read_dir` probe for directory roots) per root — the
    /// documented `SearchError::Root` precheck. `cancel` is shared with the caller (the
    /// dialog's Stop button, the jobs-style owner); `handle.cancel()` triggers it too.
    /// Errors: `InvalidQuery`, and `Root` for a root that cannot be read at all.
    ///
    /// Thread model (T7.1): one walker thread (iterative DFS, stable order) feeds file
    /// tasks to a bounded pool of `min(4, cores)` content matchers; archive entries go
    /// through `ofm_vfs::archive::open_archive` with `NoPassword`. A panic in a matcher is
    /// caught and reported as a `Warning` for that file; a panic in the walker or the
    /// supervisor ends the search with `SearchEvent::Failed(SearchError::Panic)`.
    pub fn start(query: SearchQuery, cancel: CancelToken) -> Result<SearchHandle, SearchError> {
        query.validate()?;
        let roots = crate::engine::stat_roots(&query.roots)?;
        let events = crate::engine::spawn(query, roots, cancel.clone());
        Ok(SearchHandle::from_parts(events, cancel))
    }

    /// Assemble a handle from a channel and a token (used by `start`; public so tests of other
    /// crates can fake a search).
    pub fn from_parts(events: Receiver<SearchEvent>, cancel: CancelToken) -> Self {
        SearchHandle { events, cancel }
    }

    /// The event stream. Single consumer (the receiver is a work queue, not a broadcast); the UI
    /// reads it with `glib::spawn_future_local`.
    pub fn events(&self) -> &Receiver<SearchEvent> {
        &self.events
    }

    /// Ask the search to stop; it ends with `Done { cancelled: true }`. Idempotent.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }

    /// The shared token.
    pub fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }
}

impl Drop for SearchHandle {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::event::SearchEvent;
    use crate::query::SearchQuery;

    #[test]
    fn start_validates_first_and_rejects_a_bad_root() {
        let bad = SearchHandle::start(SearchQuery::default(), CancelToken::new());
        assert!(matches!(bad, Err(SearchError::InvalidQuery { .. })));
        let missing = SearchHandle::start(
            SearchQuery {
                roots: vec!["/no/such/root/for/ofm/tests".into()],
                ..SearchQuery::default()
            },
            CancelToken::new(),
        );
        assert!(matches!(missing, Err(SearchError::Root { .. })));
    }

    #[test]
    fn start_runs_a_real_search() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), b"needle\n").unwrap();
        let handle = SearchHandle::start(
            SearchQuery {
                roots: vec![dir.path().to_owned()],
                ..SearchQuery::default()
            },
            CancelToken::new(),
        )
        .unwrap();
        let mut hits = 0;
        let mut done = false;
        while let Some(ev) = next_event(handle.events()) {
            match ev {
                SearchEvent::Found(batch) => hits += batch.len(),
                SearchEvent::Done(_) => {
                    done = true;
                    break;
                }
                SearchEvent::Failed(e) => panic!("search failed: {e}"),
                SearchEvent::Warning(_) | SearchEvent::Progress(_) => {}
            }
        }
        assert!(done);
        assert_eq!(hits, 1); // a.txt; the search root itself is not a hit
    }

    /// Drain one event with a 30 s watchdog (keeps a broken engine from hanging tests).
    fn next_event(rx: &async_channel::Receiver<SearchEvent>) -> Option<SearchEvent> {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        loop {
            match rx.try_recv() {
                Ok(ev) => return Some(ev),
                Err(async_channel::TryRecvError::Empty) => {
                    if std::time::Instant::now() >= deadline {
                        return None;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(async_channel::TryRecvError::Closed) => return None,
            }
        }
    }

    #[test]
    fn dropping_the_handle_cancels() {
        let (_tx, rx) = async_channel::bounded(CHANNEL_CAPACITY);
        let token = CancelToken::new();
        let h = SearchHandle::from_parts(rx, token.clone());
        assert!(!token.is_cancelled());
        drop(h);
        assert!(token.is_cancelled());
    }
}

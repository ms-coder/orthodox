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
    /// Validate `query` and start the workers. Returns immediately; never blocks on I/O.
    /// `cancel` is shared with the caller (the dialog's Stop button, the jobs-style owner);
    /// `handle.cancel()` triggers it too. Errors: `InvalidQuery`, and `Root` for a root that cannot
    /// be read at all. T7.1: stub (validates, then `NotImplemented`).
    ///
    /// Thread model (T7.1): a walker pool (`ignore::WalkBuilder::build_parallel`, standard filters
    /// off) feeds file tasks; content matching runs on the same workers; archive entries go through
    /// `ofm_vfs::archive::open_archive` with `NoPassword`. A panic in a worker is caught and ends
    /// the search with `SearchEvent::Failed(SearchError::Panic)`.
    pub fn start(query: SearchQuery, cancel: CancelToken) -> Result<SearchHandle, SearchError> {
        query.validate()?;
        let _ = cancel;
        Err(SearchError::NotImplemented("search engine (T7.1)"))
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
mod tests {
    use super::*;
    use crate::query::SearchQuery;

    #[test]
    fn start_validates_first_then_reports_the_stub() {
        let bad = SearchHandle::start(SearchQuery::default(), CancelToken::new());
        assert!(matches!(bad, Err(SearchError::InvalidQuery { .. })));
        let ok = SearchHandle::start(
            SearchQuery {
                roots: vec!["/srv".into()],
                ..SearchQuery::default()
            },
            CancelToken::new(),
        );
        assert!(matches!(ok, Err(SearchError::NotImplemented(_))));
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

//! ofm-search: find-files engine (F24, ADR-0009). No GTK. Frozen by T7.0; implemented by T7.1.
//!
//! ```text
//! SearchQuery --SearchHandle::start--> worker threads (walk + filter + grep)
//!                                         |  SearchEvent: Found(batch) | Progress | Warning | Done/Failed
//!                                         v
//!                         async_channel::Receiver<SearchEvent>   (bounded, workers block when full)
//! ```
//!
//! File ownership (T7.1): everything in this crate. Frozen API (architect only): the public items
//! re-exported below (add private modules freely: `walk.rs`, `content.rs`, `archives.rs`, ...).
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod error;
pub mod event;
pub mod handle;
pub mod query;

pub use error::SearchError;
pub use event::{
    ContentHit, ContentMatch, Hit, SearchEvent, SearchProgress, SearchSummary, SearchWarning,
};
pub use handle::{BATCH_INTERVAL, BATCH_SIZE, CHANNEL_CAPACITY, PROGRESS_INTERVAL, SearchHandle};
pub use query::{
    ContentMode, ContentQuery, NameFilter, NameMode, SearchEncoding, SearchQuery, SizeFilter,
    TimeFilter,
};

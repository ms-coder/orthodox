//! Search errors. Per-path problems (permission denied, unreadable file) are
//! `SearchEvent::Warning`s, not errors; this enum is for a search that cannot start or must stop.

use std::path::PathBuf;

/// A search could not be started or failed as a whole.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// The query is unusable (empty roots, bad mask, bad regex, min > max). `field` names the
    /// dialog field (`"roots"`, `"name"`, `"content"`, `"size"`, `"mtime"`) so the UI can focus it.
    #[error("invalid {field}: {message}")]
    InvalidQuery {
        /// Which field.
        field: &'static str,
        /// What is wrong.
        message: String,
    },
    /// A search root does not exist or cannot be read at all.
    #[error("{}: {source}", path.display())]
    Root {
        /// The root.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// A worker thread panicked (caught; the search ends with `SearchEvent::Failed`).
    #[error("internal error: {0}")]
    Panic(String),
    /// Not implemented yet (stub of T7.1).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

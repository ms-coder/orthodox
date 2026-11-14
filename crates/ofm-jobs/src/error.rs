//! Job errors.

use std::path::PathBuf;

/// Error of a file operation, always with the path involved.
#[derive(Debug, thiserror::Error)]
pub enum JobError {
    /// I/O error on `path`.
    #[error("{}: {source}", path.display())]
    Io {
        /// Path concerned.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Destination lies inside the source (copying a directory into itself).
    #[error("destination is inside source: {}", .0.display())]
    DestInsideSource(PathBuf),
    /// Source and destination are the same file (same dev+ino).
    #[error("source and destination are the same file: {}", .0.display())]
    SameFile(PathBuf),
    /// The job was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// A worker panicked; the message is the panic payload.
    #[error("internal error: {0}")]
    Panic(String),
}

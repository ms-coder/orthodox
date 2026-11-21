//! VFS errors, always carrying the path they concern.

use std::path::PathBuf;

/// Error of a VFS operation.
#[derive(Debug, thiserror::Error)]
pub enum VfsError {
    /// Path does not exist.
    #[error("not found: {}", .0.display())]
    NotFound(PathBuf),
    /// Permission denied (directory not listable, file unreadable).
    #[error("permission denied: {}", .0.display())]
    PermissionDenied(PathBuf),
    /// Expected a directory.
    #[error("not a directory: {}", .0.display())]
    NotADirectory(PathBuf),
    /// Operation is not supported by this provider/location.
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    /// No provider registered for this kind of location.
    #[error("no provider for location {0}")]
    NoProvider(String),
    /// The operation observed a cancelled [`ofm_core::CancelToken`].
    #[error("cancelled")]
    Cancelled,
    /// Archive is corrupt or has an unsupported format / unsafe entry (zip-slip).
    #[error("archive error: {0}")]
    Archive(String),
    /// The location is read-only (archive entries; v1.0 has no other read-only providers).
    #[error("read-only location: {}", .0.display())]
    ReadOnly(PathBuf),
    /// Archive needs a password (or the given one was wrong).
    #[error("password required")]
    PasswordRequired,
    /// Other I/O error with path context.
    #[error("{}: {source}", path.display())]
    Io {
        /// Path concerned.
        path: PathBuf,
        /// Underlying error.
        #[source]
        source: std::io::Error,
    },
}

impl VfsError {
    /// Map an `io::Error` for `path` to the closest variant.
    pub fn from_io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        let path = path.into();
        match source.kind() {
            std::io::ErrorKind::NotFound => VfsError::NotFound(path),
            std::io::ErrorKind::PermissionDenied => VfsError::PermissionDenied(path),
            _ => VfsError::Io { path, source },
        }
    }
}

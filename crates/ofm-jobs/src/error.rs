//! Job errors.

use std::path::{Path, PathBuf};

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
    /// The job was cancelled, or the user chose "abort" in a question (same effect: stop, clean
    /// up temp files, report `Outcome::Cancelled`).
    #[error("cancelled")]
    Cancelled,
    /// A worker panicked; the message is the panic payload.
    #[error("internal error: {0}")]
    Panic(String),
    /// Not available in this version (archive sources before M6, batch rename before M8, ...).
    #[error("not supported: {0}")]
    Unsupported(String),
    /// The request itself is invalid (empty source list, bad rename mask, relative destination).
    #[error("invalid request: {0}")]
    Invalid(String),
}

impl JobError {
    /// `Io` error with its path.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        JobError::Io {
            path: path.into(),
            source,
        }
    }

    /// The path this error is about, if it has one.
    pub fn path(&self) -> Option<&Path> {
        match self {
            JobError::Io { path, .. } => Some(path),
            JobError::DestInsideSource(p) | JobError::SameFile(p) => Some(p),
            _ => None,
        }
    }

    /// True for [`JobError::Cancelled`].
    pub fn is_cancelled(&self) -> bool {
        matches!(self, JobError::Cancelled)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_error_carries_path() {
        let e = JobError::io("/a/b", std::io::Error::from_raw_os_error(13));
        assert_eq!(e.path(), Some(Path::new("/a/b")));
        assert!(e.to_string().starts_with("/a/b: "));
        assert!(JobError::Cancelled.is_cancelled());
        assert_eq!(JobError::Cancelled.path(), None);
    }
}

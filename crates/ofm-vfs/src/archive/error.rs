//! Errors of the archive provider (ADR-0008). Every variant names the archive or entry it is about.

use std::path::PathBuf;

use crate::error::VfsError;

/// Why an entry was flagged unsafe at index time. Unsafe entries stay visible in
/// [`ArchiveIndex::unsafe_entries`](super::ArchiveIndex::unsafe_entries) but are never listed in a
/// panel directory, never extracted and never opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnsafeReason {
    /// The stored path is absolute (`/etc/passwd`).
    AbsolutePath,
    /// The stored path has a `..` component (even one that stays inside: `a/../b`).
    ParentTraversal,
    /// The name contains a NUL byte, or a parent component of the path is a non-directory entry.
    InvalidName,
    /// A symlink or hardlink target is absolute or leaves the archive root once resolved lexically.
    LinkEscape,
    /// An earlier symlink entry of the archive is a prefix of this path (`l -> /etc`, then `l/passwd`).
    ThroughSymlink,
}

/// Which resource guard fired ([`ArchiveError::LimitExceeded`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitKind {
    /// More entries than `ArchiveLimits::max_entries`.
    EntryCount,
    /// A path is deeper than `ArchiveLimits::max_depth`.
    Depth,
    /// A codec would need more memory than `ArchiveLimits::max_codec_memory` (LZMA dictionary,
    /// xz block size, zstd window).
    CodecMemory,
    /// An entry produced more bytes than its header declared (lying header, decompression bomb).
    ExceedsDeclaredSize,
}

/// Error of the archive provider.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The file is not in a supported archive format (e.g. a plain `.gz`). The panel opens such a
    /// file as an ordinary file instead of showing an error.
    #[error("not a supported archive: {}", .0.display())]
    NotAnArchive(PathBuf),
    /// Structurally damaged archive, bad checksum, truncated data.
    #[error("corrupt archive {}: {detail}", archive.display())]
    Corrupt {
        /// The archive file.
        archive: PathBuf,
        /// What is wrong (library message, never parsed by callers).
        detail: String,
    },
    /// Valid archive using a feature we do not support (multi-volume, unknown compression method).
    #[error("unsupported archive feature in {}: {what}", archive.display())]
    Unsupported {
        /// The archive file.
        archive: PathBuf,
        /// The feature, in words.
        what: String,
    },
    /// Encrypted and no password was available (provider returned `None` = user cancelled).
    #[error("password required for {}", .0.display())]
    NeedsPassword(PathBuf),
    /// The provider kept returning passwords that did not decrypt (backend gave up after
    /// `MAX_PASSWORD_ATTEMPTS`).
    #[error("wrong password for {}", .0.display())]
    WrongPassword(PathBuf),
    /// The entry was flagged unsafe at index time and was requested anyway.
    #[error("unsafe entry {} ({reason:?})", path.display())]
    Unsafe {
        /// Stored (raw) path of the entry.
        path: PathBuf,
        /// Why.
        reason: UnsafeReason,
    },
    /// A decompression-bomb / resource guard fired.
    #[error("limit exceeded in {}: {kind:?}", archive.display())]
    LimitExceeded {
        /// The archive file.
        archive: PathBuf,
        /// Which guard.
        kind: LimitKind,
    },
    /// No entry with this id in this index.
    #[error("no such archive entry: {0}")]
    NoSuchEntry(u32),
    /// Cancelled through the `CancelToken`.
    #[error("cancelled")]
    Cancelled,
    /// I/O error on a real file (the archive itself).
    #[error("{}: {source}", path.display())]
    Io {
        /// Path concerned.
        path: PathBuf,
        /// Cause.
        #[source]
        source: std::io::Error,
    },
    /// Not implemented yet (stub of T6.1; never reachable once T6.1 is merged).
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

impl ArchiveError {
    /// `Io` error with its path.
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        ArchiveError::Io {
            path: path.into(),
            source,
        }
    }
}

impl From<ArchiveError> for VfsError {
    fn from(e: ArchiveError) -> Self {
        match e {
            ArchiveError::Cancelled => VfsError::Cancelled,
            ArchiveError::NeedsPassword(_) | ArchiveError::WrongPassword(_) => {
                VfsError::PasswordRequired
            }
            ArchiveError::Io { path, source } => VfsError::from_io(path, source),
            ArchiveError::NotImplemented(what) => VfsError::Unsupported(what),
            other => VfsError::Archive(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_to_vfs_errors() {
        assert!(matches!(
            VfsError::from(ArchiveError::Cancelled),
            VfsError::Cancelled
        ));
        assert!(matches!(
            VfsError::from(ArchiveError::NeedsPassword("/a.zip".into())),
            VfsError::PasswordRequired
        ));
        assert!(matches!(
            VfsError::from(ArchiveError::NotImplemented("x")),
            VfsError::Unsupported("x")
        ));
        assert!(matches!(
            VfsError::from(ArchiveError::NotAnArchive("/a.gz".into())),
            VfsError::Archive(_)
        ));
    }
}

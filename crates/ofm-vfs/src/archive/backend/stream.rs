//! Helpers shared by the format backends: a per-entry stream guard and panic isolation.
//!
//! [`EntryStream`] wraps the raw decoder output for one entry and enforces the
//! `ArchiveReader::visit` contract: the cancel token is polled on every read, and a stream may
//! never yield more bytes than the entry declared at index time (lying header =
//! [`LimitKind::ExceedsDeclaredSize`]).
//!
//! Every backend wraps its library calls in [`catch_panic`]: a panic in third-party parsing or
//! decoding code is mapped to [`ArchiveError::Corrupt`] (invariant: panics never escape into the
//! file manager).

use std::io::{self, Read};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use ofm_core::CancelToken;

use crate::archive::error::{ArchiveError, LimitKind};

/// Wraps one entry's decoder stream with per-read cancel checks and a hard cap of
/// `declared + 1` bytes, so a lying header is reported before unbounded output escapes.
///
/// Fewer bytes than declared at EOF is detected by the backend after `f` returns (the backend
/// drains the stream); this wrapper only guards the "more" side and cancellation.
pub(crate) struct EntryStream<'a> {
    inner: &'a mut dyn Read,
    cancel: &'a CancelToken,
    archive: &'a Path,
    read: u64,
    declared: u64,
}

impl<'a> EntryStream<'a> {
    /// Wrap `inner`, expecting exactly `declared` bytes (from the index; never the raw header).
    pub(crate) fn new(
        inner: &'a mut dyn Read,
        cancel: &'a CancelToken,
        archive: &'a Path,
        declared: u64,
    ) -> Self {
        EntryStream {
            inner,
            cancel,
            archive,
            read: 0,
            declared,
        }
    }

    /// Bytes consumed so far.
    pub(crate) fn bytes_read(&self) -> u64 {
        self.read
    }
}

impl Read for EntryStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::other(ArchiveError::Cancelled));
        }
        // Cap at `declared + 1`: the read that crosses the boundary may still deliver its bytes
        // (the backend reports the shortfall/excess after the visitor returns); the next read
        // reports the lying header.
        if self.read > self.declared {
            return Err(io::Error::other(ArchiveError::LimitExceeded {
                archive: self.archive.to_owned(),
                kind: LimitKind::ExceedsDeclaredSize,
            }));
        }
        let n = self.inner.read(buf)?;
        self.read += n as u64;
        Ok(n)
    }
}

/// Runs `f`, converting any panic into `Corrupt` (invariant 7: decoder panics never escape).
pub(crate) fn catch_panic<T>(
    archive: &Path,
    f: impl FnOnce() -> Result<T, ArchiveError>,
) -> Result<T, ArchiveError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(_) => Err(ArchiveError::Corrupt {
            archive: archive.to_owned(),
            detail: "decoder panicked".to_owned(),
        }),
    }
}

/// Maps an `io::Error` from a decoder/stream to an [`ArchiveError`].
///
/// Preserves our own errors wrapped by [`EntryStream`] (cancel, limits) and maps codec memory
/// exhaustion (xz over `ArchiveLimits::max_codec_memory`) to [`LimitKind::CodecMemory`];
/// everything else is `Corrupt`.
pub(crate) fn map_io(archive: &Path, e: io::Error) -> ArchiveError {
    if let Some(ae) = e.get_ref().and_then(|p| p.downcast_ref::<ArchiveError>()) {
        return clone_error(ae);
    }
    if e.kind() == io::ErrorKind::OutOfMemory {
        return ArchiveError::LimitExceeded {
            archive: archive.to_owned(),
            kind: LimitKind::CodecMemory,
        };
    }
    ArchiveError::Corrupt {
        archive: archive.to_owned(),
        detail: format!("decode error: {e}"),
    }
}

/// `ArchiveError` is not `Clone`; reconstruct the two variants we wrap in `io::Error`.
fn clone_error(e: &ArchiveError) -> ArchiveError {
    match e {
        ArchiveError::Cancelled => ArchiveError::Cancelled,
        ArchiveError::LimitExceeded { archive, kind } => ArchiveError::LimitExceeded {
            archive: archive.clone(),
            kind: *kind,
        },
        other => ArchiveError::Corrupt {
            archive: PathBuf::new(),
            detail: format!("unexpected wrapped error: {other}"),
        },
    }
}

/// Drains `stream` after the visitor is done, returning the total bytes seen.
///
/// Backends call this so the underlying decoder reaches EOF (CRC/checksum verification) even
/// when the visitor stopped early, and so sequential formats advance to the next entry.
pub(crate) fn drain(
    mut stream: &mut dyn Read,
    archive: &Path,
    cancel: &CancelToken,
) -> Result<u64, ArchiveError> {
    let mut sink = io::sink();
    crate::archive::reader::copy_stream(&mut stream, &mut sink, cancel, archive)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn stream_caps_at_declared_plus_one() {
        let cancel = CancelToken::new();
        let data = b"hello world";
        let mut raw = &data[..];
        let mut s = EntryStream::new(&mut raw, &cancel, Path::new("/a.zip"), 5);
        let mut buf = [0u8; 16];
        let n = s.read(&mut buf).unwrap();
        assert_eq!(n, 11, "single read passes through");
        assert!(s.read(&mut buf).is_err(), "second read exceeds declared");
    }

    #[test]
    fn stream_maps_cancel() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut raw = &b"abc"[..];
        let mut s = EntryStream::new(&mut raw, &cancel, Path::new("/a.zip"), 10);
        let mut b = [0u8; 4];
        let e = s.read(&mut b).unwrap_err();
        assert!(matches!(
            e.get_ref().and_then(|p| p.downcast_ref::<ArchiveError>()),
            Some(ArchiveError::Cancelled)
        ));
        assert!(matches!(
            map_io(Path::new("/a.zip"), e),
            ArchiveError::Cancelled
        ));
    }

    #[test]
    fn catch_panic_maps_to_corrupt() {
        let r = catch_panic(Path::new("/a.zip"), || -> Result<(), ArchiveError> {
            panic!("boom");
        });
        assert!(matches!(r, Err(ArchiveError::Corrupt { .. })));
        let r = catch_panic(Path::new("/a.zip"), || Ok(42));
        assert_eq!(r.unwrap(), 42);
    }

    #[test]
    fn map_io_keeps_limits() {
        let e = io::Error::other(ArchiveError::LimitExceeded {
            archive: PathBuf::from("/a.zip"),
            kind: LimitKind::ExceedsDeclaredSize,
        });
        assert!(matches!(
            map_io(Path::new("/a.zip"), e),
            ArchiveError::LimitExceeded {
                kind: LimitKind::ExceedsDeclaredSize,
                ..
            }
        ));
        let e = io::Error::new(io::ErrorKind::OutOfMemory, "needed memory too big");
        assert!(matches!(
            map_io(Path::new("/a.zip"), e),
            ArchiveError::LimitExceeded {
                kind: LimitKind::CodecMemory,
                ..
            }
        ));
    }
}

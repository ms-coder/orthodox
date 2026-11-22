//! `ArchiveReader`: the one trait every format backend implements (ADR-0008), and the entry point
//! [`open_archive`] that picks the backend by magic bytes.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use ofm_core::CancelToken;

use super::backend;
use super::error::ArchiveError;
use super::format::detect_format;
use super::index::{ArchiveEntry, ArchiveIndex, EntryId};
use super::limits::ArchiveLimits;
use super::password::PasswordProvider;

/// Callback of [`ArchiveReader::visit`]: the entry and a stream of its data.
pub type VisitFn<'a> = dyn FnMut(&ArchiveEntry, &mut dyn Read) -> Result<(), ArchiveError> + 'a;

/// An opened archive: its index plus streaming access to entry data. One reader = one thread at a
/// time (`&mut self`); open another reader for parallel work (indexes are shared via `Arc`).
///
/// Streaming rules (see ADR-0008): data is never loaded whole; solid formats (tar.*, solid 7z)
/// are read in **one sequential pass** through [`visit`](Self::visit), never by seeking to an
/// entry per file (that would decompress the archive once per file).
pub trait ArchiveReader: Send {
    /// The index built when the archive was opened.
    fn index(&self) -> &Arc<ArchiveIndex>;

    /// Stream the entries `ids` to `f`, in **archive order** (not the order of `ids`).
    ///
    /// Contract for every backend:
    /// - `ids` must be safe winners of `index()` (as returned by `ArchiveIndex::select`); an
    ///   unsafe id fails with `ArchiveError::Unsafe`, an unknown one with `NoSuchEntry`, both
    ///   before any data is read;
    /// - `f` is called for every selected entry including dirs, symlinks and special entries (the
    ///   stream is empty for those), so extractors recreate the tree in order;
    /// - the stream yields exactly `entry.size` bytes for files: fewer = `Corrupt`, more =
    ///   `LimitExceeded(ExceedsDeclaredSize)`; checksum failures surface as `Corrupt` from `read`
    ///   (as `io::Error` wrapping the `ArchiveError` text) and are mapped by the backend;
    /// - a `Hardlink` entry's stream is its target's data (sequential backends may need a second
    ///   pass for that);
    /// - encrypted entries ask the `PasswordProvider` given at open (`retry = true` after a wrong
    ///   one, at most `MAX_PASSWORD_ATTEMPTS` times) and call `accepted` on success;
    /// - `cancel` is checked between entries and at least every 1 MiB of data, giving `Cancelled`;
    /// - an error returned by `f` aborts the visit and is returned as is;
    /// - library calls are wrapped in `catch_unwind`: a decoder panic is `Corrupt`, never a crash.
    fn visit(
        &mut self,
        ids: &[EntryId],
        cancel: &CancelToken,
        f: &mut VisitFn<'_>,
    ) -> Result<(), ArchiveError>;

    /// Copy the data of one entry into `out`; returns the bytes written. Convenience on top of
    /// `visit` (O(archive) for solid formats: use `visit` for more than one entry).
    fn read_entry(
        &mut self,
        id: EntryId,
        out: &mut dyn Write,
        cancel: &CancelToken,
    ) -> Result<u64, ArchiveError> {
        let archive = self.index().archive().to_owned();
        let mut written = 0;
        self.visit(&[id], cancel, &mut |_entry, data| {
            written = copy_stream(data, out, cancel, &archive)?;
            Ok(())
        })?;
        Ok(written)
    }
}

/// `io::copy` with a cancel check every 64 KiB; errors are attributed to `archive`.
pub fn copy_stream(
    from: &mut dyn Read,
    to: &mut dyn Write,
    cancel: &CancelToken,
    archive: &Path,
) -> Result<u64, ArchiveError> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let n = from
            .read(&mut buf)
            .map_err(|e| ArchiveError::io(archive, e))?;
        if n == 0 {
            return Ok(total);
        }
        to.write_all(&buf[..n])
            .map_err(|e| ArchiveError::io(archive, e))?;
        total += n as u64;
    }
}

/// Open the archive at `path`: detect the format by magic bytes, build the index (in the calling
/// worker thread; may take long for big archives, checks `cancel`) and return a reader.
///
/// `passwords` is consulted when the headers are encrypted (7z) or on the first encrypted entry
/// (zip); wrap it in a `PasswordSource` to remember the accepted password. Errors: `NotAnArchive`
/// (not a supported format: open the file normally), `Corrupt`, `Unsupported`, `NeedsPassword`,
/// `WrongPassword`, `LimitExceeded`, `Cancelled`, `Io`.
pub fn open_archive(
    path: &Path,
    passwords: &dyn PasswordProvider,
    limits: &ArchiveLimits,
    cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader>, ArchiveError> {
    let format = detect_format(path)?.ok_or_else(|| ArchiveError::NotAnArchive(path.to_owned()))?;
    backend::open(format, path, passwords, limits, cancel)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::archive::NoPassword;

    #[test]
    fn stub_backends_fail_with_a_typed_error() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("x.zip");
        std::fs::write(&zip, b"PK\x03\x04").unwrap();
        let open = |p: &Path| {
            open_archive(
                p,
                &NoPassword,
                &ArchiveLimits::default(),
                &CancelToken::new(),
            )
        };
        assert!(matches!(open(&zip), Err(ArchiveError::NotImplemented(_))));
        let plain = dir.path().join("plain.txt");
        std::fs::write(&plain, b"hello").unwrap();
        assert!(matches!(open(&plain), Err(ArchiveError::NotAnArchive(_))));
    }

    #[test]
    fn copy_stream_honours_cancel() {
        let cancel = CancelToken::new();
        let mut out = Vec::new();
        let n = copy_stream(&mut &b"abc"[..], &mut out, &cancel, Path::new("/a")).unwrap_or(0);
        assert_eq!((n, out.as_slice()), (3, b"abc".as_slice()));
        cancel.cancel();
        assert!(matches!(
            copy_stream(&mut &b"abc"[..], &mut out, &cancel, Path::new("/a")),
            Err(ArchiveError::Cancelled)
        ));
    }
}

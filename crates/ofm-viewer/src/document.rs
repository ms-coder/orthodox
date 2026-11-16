//! The document: a read-only view of one file, memory-mapped when possible.
//!
//! [`Document::open`] never reads file data — `memmap2` maps on open, so a 10 GB sparse file
//! opens in microseconds and the first screen read touches only the pages it needs.
//! Non-mappable sources (pipes, `/proc` files, sockets) fall back to a buffered read capped at
//! [`BUFFER_CAP`] bytes.
//!
//! Truncation safety: an access to a mapped region that extends past the file's *current* size
//! can raise SIGBUS. Every read here checks the current file size first and uses `pread` (which
//! short-reads) whenever the mapped length cannot be trusted; see the crate docs for the
//! remaining TOCTOU caveat.

use std::fs::File;
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::Path;

use memmap2::Mmap;
use thiserror::Error;

/// Buffered fallback cap for non-mappable sources (64 MiB).
pub const BUFFER_CAP: u64 = 64 * 1024 * 1024;

/// How the document data is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Regular file, memory-mapped (reads are page faults, no syscall per read).
    Mapped,
    /// Non-mappable source buffered into memory, capped at [`BUFFER_CAP`] bytes.
    Buffered,
}

/// Errors from [`Document::open`] and read operations.
#[derive(Debug, Error)]
pub enum DocumentError {
    /// The file could not be opened or stat'ed.
    #[error("open failed: {0}")]
    Open(#[source] io::Error),
    /// A read (or size re-check) failed.
    #[error("read failed: {0}")]
    Read(#[source] io::Error),
    /// The file shrank below an already-buffered length (buffered sources only).
    #[error("file truncated while buffering")]
    Truncated,
}

/// A read-only, random-access view of one file. `Send + Sync`; all methods take `&self`.
pub struct Document {
    file: File,
    map: Option<Mmap>,
    buf: Option<Vec<u8>>,
    len: u64,
    kind: SourceKind,
}

// Mmap dereferences to &[u8]; Vec<u8> as well. Both are Sync enough for our read-only use.
unsafe impl Send for Document {}
unsafe impl Sync for Document {}

impl std::fmt::Debug for Document {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("kind", &self.kind)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

impl Document {
    /// Open a file for viewing. No data is read for mappable files.
    pub fn open(path: &Path) -> Result<Self, DocumentError> {
        let file = File::open(path).map_err(DocumentError::Open)?;
        let md = file.metadata().map_err(DocumentError::Open)?;
        let len = md.len();
        if md.is_file() && len > 0 {
            match unsafe { Mmap::map(&file) } {
                Ok(map) => {
                    return Ok(Self {
                        file,
                        map: Some(map),
                        buf: None,
                        len,
                        kind: SourceKind::Mapped,
                    });
                }
                Err(_) => {
                    // Some filesystems refuse mmap (e.g. certain fuse/proc files): buffer instead.
                }
            }
        }
        let take = len.min(BUFFER_CAP) as usize;
        let mut buf = Vec::with_capacity(take);
        let mut limited = (&file).take(BUFFER_CAP);
        limited.read_to_end(&mut buf).map_err(DocumentError::Read)?;
        let buffered_len = buf.len() as u64;
        Ok(Self {
            file,
            map: None,
            buf: Some(buf),
            len: buffered_len,
            kind: SourceKind::Buffered,
        })
    }

    /// How the data is held.
    pub fn source_kind(&self) -> SourceKind {
        self.kind
    }

    /// Length in bytes. For buffered sources this is the number of bytes actually read
    /// (≤ [`BUFFER_CAP`]; `/proc`-style files report 0 in metadata but have content).
    pub fn len(&self) -> u64 {
        self.len
    }

    /// True if the document has zero bytes.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Current size of the underlying file, re-stat'ed. Cheap; used to guard mmap access
    /// against truncation.
    pub fn current_file_len(&self) -> Result<u64, DocumentError> {
        self.file
            .metadata()
            .map_err(DocumentError::Read)
            .map(|md| md.len())
    }

    /// Read `buf.len()` bytes starting at `offset` (short-reads at EOF) into `buf`.
    ///
    /// Never faults on a truncated file: if the mapped length cannot be confirmed against the
    /// current file size, uses `pread` instead.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DocumentError> {
        if buf.is_empty() {
            return Ok(0);
        }
        let want = buf.len() as u64;
        if let Some(map) = &self.map {
            // Fast path: the whole range lies inside the length we mapped AND the file has not
            // shrunk since. The size check closes the SIGBUS window for shrink-then-read; a file
            // shrinking *between* this stat and the copy is the documented TOCTOU caveat.
            if offset.checked_add(want).is_some_and(|end| end <= self.len)
                && self.current_file_len()? >= self.len
            {
                let start = usize::try_from(offset).map_err(|_| {
                    DocumentError::Read(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "offset too large",
                    ))
                })?;
                let end = start + buf.len();
                buf.copy_from_slice(&map[start..end]);
                return Ok(buf.len());
            }
            // File shrank (or mapping was partial): pread short-reads, never SIGBUS.
            return self.file.read_at(buf, offset).map_err(DocumentError::Read);
        }
        let data = self.buf.as_ref().map_or(&[][..], Vec::as_slice);
        let start = usize::try_from(offset).unwrap_or(usize::MAX);
        if start >= data.len() {
            return Ok(0);
        }
        let n = (data.len() - start).min(buf.len());
        buf[..n].copy_from_slice(&data[start..start + n]);
        Ok(n)
    }

    /// Read up to `max_len` bytes starting at `offset` into a fresh `Vec`.
    pub fn read_range(&self, offset: u64, max_len: usize) -> Result<Vec<u8>, DocumentError> {
        let mut buf = vec![0u8; max_len];
        let n = self.read_at(offset, &mut buf)?;
        buf.truncate(n);
        Ok(buf)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn open_and_read_small_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("a.txt");
        std::fs::File::create(&p)
            .unwrap()
            .write_all(b"hello world")
            .unwrap();
        let doc = Document::open(&p).unwrap();
        assert_eq!(doc.source_kind(), SourceKind::Mapped);
        assert_eq!(doc.len(), 11);
        assert_eq!(doc.read_range(6, 5).unwrap(), b"world");
        assert_eq!(doc.read_range(0, 100).unwrap(), b"hello world");
        assert_eq!(doc.read_range(11, 5).unwrap(), b"");
    }

    #[test]
    fn empty_file_is_buffered_and_empty() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("empty");
        std::fs::File::create(&p).unwrap();
        let doc = Document::open(&p).unwrap();
        assert!(doc.is_empty());
        assert_eq!(doc.read_range(0, 10).unwrap(), b"");
    }

    #[test]
    fn proc_file_falls_back_to_buffer() {
        // /proc files report st_size = 0 and cannot be mmap'ed: the buffered path must kick
        // in and expose the actual content length.
        let doc = Document::open(Path::new("/proc/version")).unwrap();
        assert_eq!(doc.source_kind(), SourceKind::Buffered);
        assert!(!doc.is_empty());
        let head = doc.read_range(0, 16).unwrap();
        assert!(!head.is_empty());
    }

    #[test]
    fn truncation_does_not_fault() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.bin");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&[7u8; 4096]).unwrap();
        let doc = Document::open(&p).unwrap();
        // Truncate after mapping: the fast path must detect the shrink and pread instead.
        f.set_len(16).unwrap();
        drop(f);
        assert!(doc.current_file_len().unwrap() <= 16);
        let got = doc.read_range(0, 4096).unwrap();
        assert!(
            got.len() <= 16,
            "short-read expected, got {} bytes",
            got.len()
        );
    }

    #[test]
    fn non_utf8_path_works() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir().unwrap();
        let name = OsStr::from_bytes(b"file-\xff.bin");
        let p = dir.path().join(name);
        std::fs::File::create(&p).unwrap().write_all(b"x").unwrap();
        let doc = Document::open(&p).unwrap();
        assert_eq!(doc.len(), 1);
    }
}

//! The document: a read-only, random-access view of one file.
//!
//! [`Document::open`] opens the path with `O_NONBLOCK` and stats the opened fd, so a path
//! swapped for a FIFO or socket between listing and opening can never park the calling
//! thread; anything that is not a regular file is rejected with a clear error. No file data
//! is read at open time, so a 10 GB sparse file opens in microseconds.
//!
//! Reads always go through `pread` (which short-reads past EOF), so a file truncated or
//! extended by another process while open can never SIGBUS — the worst case is fewer bytes
//! than requested. Size-0 regular files that really have content (`/proc`, `/sys`) are the
//! exception: they are buffered up to [`BUFFER_CAP`] at open time.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::path::Path;

use thiserror::Error;

/// Buffered fallback cap for size-0 regular files such as `/proc` entries (64 MiB).
pub const BUFFER_CAP: u64 = 64 * 1024 * 1024;

/// How the document data is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// Regular file; reads are `pread` calls on the open fd (short-reads at EOF).
    File,
    /// Size-0 regular file (e.g. `/proc`) buffered into memory at open, capped at
    /// [`BUFFER_CAP`] bytes.
    Buffered,
}

/// Errors from [`Document::open`] and read operations.
#[derive(Debug, Error)]
pub enum DocumentError {
    /// The file could not be opened or stat'ed.
    #[error("open failed: {0}")]
    Open(#[source] io::Error),
    /// The path is not a regular file (directory, FIFO, device, or socket). The document is
    /// opened `O_NONBLOCK` and fstat'ed so a path swapped after listing fails here instead of
    /// blocking the calling thread forever.
    #[error("not a regular file")]
    NotRegularFile,
    /// A read failed.
    #[error("read failed: {0}")]
    Read(#[source] io::Error),
    /// The file shrank below an already-buffered length (buffered sources only).
    #[error("file truncated while buffering")]
    Truncated,
}

/// A read-only, random-access view of one file. `Send + Sync`; all methods take `&self`.
#[derive(Debug)]
pub struct Document {
    file: File,
    /// Buffered content for size-0 regular files (`/proc`); `None` for regular reads.
    buf: Option<Vec<u8>>,
    len: u64,
    kind: SourceKind,
}

impl Document {
    /// Open a file for viewing. No data is read for regular files.
    ///
    /// The open is `O_NONBLOCK` and the fd is fstat'ed: directories, FIFOs, devices, and
    /// sockets are rejected with [`DocumentError::NotRegularFile`] instead of blocking (a
    /// FIFO swapped in after a directory listing would otherwise hang the thread forever).
    pub fn open(path: &Path) -> Result<Self, DocumentError> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .map_err(DocumentError::Open)?;
        let md = file.metadata().map_err(DocumentError::Open)?;
        if !md.file_type().is_file() {
            return Err(DocumentError::NotRegularFile);
        }
        let len = md.len();
        if len > 0 {
            return Ok(Self {
                file,
                buf: None,
                len,
                kind: SourceKind::File,
            });
        }
        // Size-0 regular files (procfs, sysfs) report no content in metadata but read()
        // delivers data: buffer up to BUFFER_CAP. O_NONBLOCK does not affect reads on
        // regular files, so this read cannot return EAGAIN here.
        let mut buf = Vec::new();
        (&file)
            .take(BUFFER_CAP)
            .read_to_end(&mut buf)
            .map_err(DocumentError::Read)?;
        let buffered_len = buf.len() as u64;
        Ok(Self {
            file,
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

    /// Current size of the underlying file, re-stat'ed. Cheap; useful to detect a file that
    /// has grown or shrunk since [`open`](Self::open).
    pub fn current_file_len(&self) -> Result<u64, DocumentError> {
        self.file
            .metadata()
            .map_err(DocumentError::Read)
            .map(|md| md.len())
    }

    /// Read `buf.len()` bytes starting at `offset` (short-reads at EOF) into `buf`.
    ///
    /// Never faults on a truncated file: reads are `pread` on the open fd, so if another
    /// process truncates the file mid-read the worst case is fewer bytes than requested.
    pub fn read_at(&self, offset: u64, buf: &mut [u8]) -> Result<usize, DocumentError> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.buf.is_none() {
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
        assert_eq!(doc.source_kind(), SourceKind::File);
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
        // /proc files report st_size = 0 but read() delivers content: the buffered path
        // must kick in and expose the actual content length.
        let doc = Document::open(Path::new("/proc/version")).unwrap();
        assert_eq!(doc.source_kind(), SourceKind::Buffered);
        assert!(!doc.is_empty());
        let head = doc.read_range(0, 16).unwrap();
        assert!(!head.is_empty());
    }

    #[test]
    fn directory_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("subdir");
        std::fs::create_dir(&p).unwrap();
        assert!(matches!(
            Document::open(&p),
            Err(DocumentError::NotRegularFile)
        ));
    }

    #[test]
    fn fifo_is_rejected_without_hanging() {
        // A path swapped for a FIFO between listing and opening must fail fast instead of
        // blocking the thread in open(). Skip if mkfifo is unavailable.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("fifo");
        let ok = std::process::Command::new("mkfifo")
            .arg(&p)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            eprintln!("mkfifo unavailable; skipping FIFO open test");
            return;
        }
        assert!(matches!(
            Document::open(&p),
            Err(DocumentError::NotRegularFile)
        ));
    }

    #[test]
    fn truncation_does_not_fault() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.bin");
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(&[7u8; 4096]).unwrap();
        let doc = Document::open(&p).unwrap();
        // Truncate after opening: pread short-reads, never SIGBUS.
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

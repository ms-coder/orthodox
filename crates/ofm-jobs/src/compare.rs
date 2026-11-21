//! Content comparison job (T8.1b, F27 "compare directories, mode content"): a byte-for-byte
//! comparison of file pairs that the pure engine (`ofm_core::compare`, `CompareMode::Content`)
//! could not decide by name and size alone.
//!
//! **Read-only**: this job only ever calls `Fs::lstat`, `Fs::read_link` and `Fs::open_read` (plus
//! `FsFile::read`/`stat`); it never creates, writes, renames or deletes anything.
//!
//! Behaviour:
//! - Pairs run **sequentially** (one disk read stream at a time; no parallel knob in v1.0).
//! - Each pair stops at the first difference. Results are delivered **incrementally** through
//!   [`CompareContentOpts::on_result`] as soon as each pair is decided, in pair order, on the
//!   worker thread (the callback must not block; send into a channel). The job's `Outcome` is
//!   `Ok(Report)` (`files_done` = pairs decided; unreadable pairs also appear as warnings), or
//!   `Cancelled` with whatever was delivered before.
//! - A per-pair problem (unreadable file, I/O error, file changed while reading) is a
//!   [`PairResult::Error`], never a job failure.
//! - Special files (FIFO, socket, device) are never read: they are refused by `Fs::open_read`
//!   (`O_NONBLOCK` + `O_NOFOLLOW` + fstat) and by a `lstat` pre-check, reported as
//!   [`PairResult::Skipped`]. Directories are skipped too.
//! - Symlinks are not followed: two symlinks are equal when their targets are equal; a symlink
//!   against a regular file differs.
//! - Files that change during the read: sizes are compared again on the open descriptors, a short
//!   read on one side only is `Differs`, both sides ending early identically is an `Error`
//!   ("changed while comparing"), data beyond the size seen at open is `Differs`. Never a panic.
//! - Progress: `files_done`/`files_total` count pairs, bytes count compared content once per pair
//!   (an early difference tops the pair up, so `bytes_done` reaches `bytes_total` at the end).
//!   `ctx.checkpoint()` runs between pairs and at least every 1 MiB.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::fs::{FileType, Fs, FsFile};

/// Default read buffer (per side).
pub const DEFAULT_BUFFER: usize = 256 * 1024;
/// Smallest accepted buffer.
pub const MIN_BUFFER: usize = 4 * 1024;
/// Largest accepted buffer: the cancel/pause checkpoint runs once per chunk (<= 1 MiB).
pub const MAX_BUFFER: usize = 1024 * 1024;

/// Why a pair was not compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// FIFO, socket, block or character device on either side: never opened.
    SpecialFile,
    /// A directory on either side (this job compares file contents only).
    Directory,
}

/// A per-pair I/O problem (carries the path of the side concerned).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairError {
    /// Path the problem is about.
    pub path: PathBuf,
    /// Human-readable message (OS message or `changed while comparing`).
    pub message: String,
    /// Raw OS error number when there is one.
    pub os_error: Option<i32>,
}

impl PairError {
    fn io(path: &Path, e: &io::Error) -> Self {
        PairError {
            path: path.to_path_buf(),
            message: e.to_string(),
            os_error: e.raw_os_error(),
        }
    }
}

/// Result for one pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairResult {
    /// Same content (or same link target, or the very same inode).
    Equal,
    /// Different content, size or kind.
    Differs,
    /// Not compared (special file, directory).
    Skipped(SkipReason),
    /// Could not be compared (unreadable, I/O error, changed while reading).
    Error(PairError),
}

type ResultFn = dyn Fn(usize, &PairResult) + Send + Sync;

/// Receiver of incremental results: called on the worker thread with `(pair index, result)` as
/// soon as a pair is decided. Must not block (send into an `async-channel`/`mpsc` and return).
#[derive(Clone)]
pub struct ResultSink(Arc<ResultFn>);

impl ResultSink {
    /// Wrap a callback.
    pub fn new(f: impl Fn(usize, &PairResult) + Send + Sync + 'static) -> Self {
        ResultSink(Arc::new(f))
    }

    fn emit(&self, index: usize, result: &PairResult) {
        (self.0)(index, result);
    }
}

impl fmt::Debug for ResultSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ResultSink(..)")
    }
}

/// Options of the content comparison job. Stop at the first difference is always on.
#[derive(Debug, Clone)]
pub struct CompareContentOpts {
    /// Read buffer per side in bytes, clamped to [`MIN_BUFFER`]..=[`MAX_BUFFER`].
    pub buffer_size: usize,
    /// Incremental per-pair results (see [`ResultSink`]); `None` = only the progress counters.
    pub on_result: Option<ResultSink>,
}

impl Default for CompareContentOpts {
    fn default() -> Self {
        CompareContentOpts {
            buffer_size: DEFAULT_BUFFER,
            on_result: None,
        }
    }
}

/// Run the comparison. Returns every result in pair order (also delivered through
/// `opts.on_result`); `Err(Cancelled)` when cancelled (results delivered so far stay valid).
pub fn run(
    ctx: &mut JobCtx,
    pairs: &[(PathBuf, PathBuf)],
    opts: &CompareContentOpts,
) -> Result<Vec<PairResult>, JobError> {
    let fs = ctx.fs();
    let buf_size = opts.buffer_size.clamp(MIN_BUFFER, MAX_BUFFER);
    let mut bufs = (vec![0u8; buf_size], vec![0u8; buf_size]);

    // Plan: expected bytes (left sizes of regular files; unreadable ones count 0).
    ctx.set_planning(true);
    let mut sizes = Vec::with_capacity(pairs.len());
    let mut total = 0u64;
    for (left, _) in pairs {
        ctx.checkpoint()?;
        let size = fs
            .lstat(left)
            .ok()
            .filter(|s| s.is_file())
            .map_or(0, |s| s.size);
        total = total.saturating_add(size);
        sizes.push(size);
    }
    ctx.set_totals(pairs.len() as u64, total);

    let mut results = Vec::with_capacity(pairs.len());
    for (index, (left, right)) in pairs.iter().enumerate() {
        ctx.checkpoint()?;
        ctx.set_current(left);
        let mut credited = 0u64;
        let result = compare_pair(ctx, &*fs, left, right, &mut bufs, &mut credited)?;
        let planned = sizes.get(index).copied().unwrap_or(0);
        if credited < planned {
            ctx.add_bytes(planned - credited);
        }
        ctx.file_done();
        if let PairResult::Error(e) = &result {
            ctx.warn(e.path.clone(), format!("cannot compare: {}", e.message));
        }
        if let Some(sink) = &opts.on_result {
            sink.emit(index, &result);
        }
        results.push(result);
    }
    Ok(results)
}

/// Compare one pair. Only `Err(Cancelled)` escapes; everything else is a [`PairResult`].
fn compare_pair(
    ctx: &mut JobCtx,
    fs: &dyn Fs,
    left: &Path,
    right: &Path,
    bufs: &mut (Vec<u8>, Vec<u8>),
    credited: &mut u64,
) -> Result<PairResult, JobError> {
    let ls = match fs.lstat(left) {
        Ok(s) => s,
        Err(e) => return Ok(PairResult::Error(PairError::io(left, &e))),
    };
    let rs = match fs.lstat(right) {
        Ok(s) => s,
        Err(e) => return Ok(PairResult::Error(PairError::io(right, &e))),
    };
    let (lt, rt) = (ls.file_type(), rs.file_type());
    if lt == FileType::Symlink && rt == FileType::Symlink {
        return Ok(compare_links(fs, left, right));
    }
    if lt == FileType::Dir || rt == FileType::Dir {
        return Ok(PairResult::Skipped(SkipReason::Directory));
    }
    if lt == FileType::Other || rt == FileType::Other {
        return Ok(PairResult::Skipped(SkipReason::SpecialFile));
    }
    if lt == FileType::Symlink || rt == FileType::Symlink {
        return Ok(PairResult::Differs); // link against a regular file; links are never followed
    }
    if ls.dev == rs.dev && ls.ino == rs.ino {
        return Ok(PairResult::Equal); // the same inode (hard link): identical by definition
    }
    if ls.size != rs.size {
        return Ok(PairResult::Differs);
    }

    let mut lf = match open(fs, left) {
        Ok(f) => f,
        Err(r) => return Ok(r),
    };
    let mut rf = match open(fs, right) {
        Ok(f) => f,
        Err(r) => return Ok(r),
    };
    // Re-check on the open descriptors: the paths may have been swapped or grown since lstat.
    let lfs = match lf.stat() {
        Ok(s) => s,
        Err(e) => return Ok(PairResult::Error(PairError::io(left, &e))),
    };
    let rfs = match rf.stat() {
        Ok(s) => s,
        Err(e) => return Ok(PairResult::Error(PairError::io(right, &e))),
    };
    for st in [&lfs, &rfs] {
        match st.file_type() {
            FileType::File => {}
            FileType::Dir => return Ok(PairResult::Skipped(SkipReason::Directory)),
            _ => return Ok(PairResult::Skipped(SkipReason::SpecialFile)),
        }
    }
    if lfs.size != rfs.size {
        return Ok(PairResult::Differs);
    }
    compare_streams(
        ctx,
        &mut *lf,
        &mut *rf,
        lfs.size,
        bufs,
        (left, right),
        credited,
    )
}

/// `Fs::open_read` (no-follow, non-blocking open, special files refused), errors as results.
fn open(fs: &dyn Fs, path: &Path) -> Result<Box<dyn FsFile>, PairResult> {
    fs.open_read(path).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidInput && e.raw_os_error().is_none() {
            // `RealFs` refuses FIFOs/sockets/devices swapped in after the lstat with this.
            PairResult::Skipped(SkipReason::SpecialFile)
        } else {
            PairResult::Error(PairError::io(path, &e))
        }
    })
}

fn compare_links(fs: &dyn Fs, left: &Path, right: &Path) -> PairResult {
    let l = match fs.read_link(left) {
        Ok(t) => t,
        Err(e) => return PairResult::Error(PairError::io(left, &e)),
    };
    let r = match fs.read_link(right) {
        Ok(t) => t,
        Err(e) => return PairResult::Error(PairError::io(right, &e)),
    };
    if l == r {
        PairResult::Equal
    } else {
        PairResult::Differs
    }
}

/// Fill `buf` as far as possible (loops over short reads; `Ok(n < buf.len())` = end of file).
fn read_full(f: &mut dyn FsFile, buf: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match f.read(&mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(filled)
}

/// Compare `size` bytes of two open regular files chunk by chunk, then probe both for data past
/// the expected end (a file that grew while we read).
fn compare_streams(
    ctx: &mut JobCtx,
    a: &mut dyn FsFile,
    b: &mut dyn FsFile,
    size: u64,
    bufs: &mut (Vec<u8>, Vec<u8>),
    paths: (&Path, &Path),
    credited: &mut u64,
) -> Result<PairResult, JobError> {
    let (left, right) = paths;
    let chunk = bufs.0.len().min(bufs.1.len());
    let mut remaining = size;
    while remaining > 0 {
        ctx.checkpoint()?;
        // `want <= chunk`, so the conversion cannot truncate.
        let want = usize::try_from(remaining.min(chunk as u64)).unwrap_or(chunk);
        let na = match read_full(a, &mut bufs.0[..want]) {
            Ok(n) => n,
            Err(e) => return Ok(PairResult::Error(PairError::io(left, &e))),
        };
        let nb = match read_full(b, &mut bufs.1[..want]) {
            Ok(n) => n,
            Err(e) => return Ok(PairResult::Error(PairError::io(right, &e))),
        };
        if na != nb || bufs.0[..na] != bufs.1[..nb] {
            return Ok(PairResult::Differs); // includes "one side was truncated meanwhile"
        }
        if na < want {
            // Both files ended at the same early offset: they no longer match their size.
            return Ok(PairResult::Error(PairError {
                path: left.to_path_buf(),
                message: "file changed while comparing".to_owned(),
                os_error: None,
            }));
        }
        ctx.add_bytes(na as u64);
        *credited = credited.saturating_add(na as u64);
        remaining -= na as u64;
    }
    let mut probe_a = [0u8; 1];
    let mut probe_b = [0u8; 1];
    let pa = match read_full(a, &mut probe_a) {
        Ok(n) => n,
        Err(e) => return Ok(PairResult::Error(PairError::io(left, &e))),
    };
    let pb = match read_full(b, &mut probe_b) {
        Ok(n) => n,
        Err(e) => return Ok(PairResult::Error(PairError::io(right, &e))),
    };
    Ok(if pa == 0 && pb == 0 {
        PairResult::Equal
    } else {
        PairResult::Differs
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::ctx::ScriptedSink;
    use crate::fs::{FsStat, RealFs};
    use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

    /// In-memory file: `data` is what reads return; used to simulate size races.
    struct MemFile {
        data: Vec<u8>,
        pos: usize,
        fd: OwnedFd,
        /// Cancel this token once `pos` passes the given offset (a user cancelling mid-file).
        cancel_at: Option<(usize, ofm_core::CancelToken)>,
    }

    impl MemFile {
        fn new(data: &[u8]) -> Self {
            let fd = rustix::fs::open(
                "/dev/null",
                rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::CLOEXEC,
                rustix::fs::Mode::empty(),
            )
            .unwrap();
            MemFile {
                data: data.to_vec(),
                pos: 0,
                fd,
                cancel_at: None,
            }
        }
    }

    fn unsupported<T>() -> io::Result<T> {
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }

    impl FsFile for MemFile {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            // Short reads on purpose: at most 3 bytes per call.
            let n = buf.len().min(3).min(self.data.len() - self.pos);
            buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
            self.pos += n;
            if let Some((at, token)) = &self.cancel_at
                && self.pos >= *at
            {
                token.cancel();
            }
            Ok(n)
        }
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            unsupported()
        }
        fn stat(&self) -> io::Result<FsStat> {
            unsupported()
        }
        fn as_fd(&self) -> BorrowedFd<'_> {
            self.fd.as_fd()
        }
        fn try_clone_from(&mut self, _: &dyn FsFile) -> io::Result<bool> {
            unsupported()
        }
        fn copy_range_from(&mut self, _: &mut dyn FsFile, _: u64) -> io::Result<Option<u64>> {
            unsupported()
        }
        fn seek_data(&mut self, _: u64) -> io::Result<Option<u64>> {
            unsupported()
        }
        fn seek_hole(&mut self, _: u64) -> io::Result<u64> {
            unsupported()
        }
        fn seek(&mut self, _: u64) -> io::Result<()> {
            unsupported()
        }
        fn set_len(&mut self, _: u64) -> io::Result<()> {
            unsupported()
        }
        fn set_mode(&mut self, _: u32) -> io::Result<()> {
            unsupported()
        }
        fn set_times(&mut self, _: i128, _: i128) -> io::Result<()> {
            unsupported()
        }
        fn set_owner(&mut self, _: Option<u32>, _: Option<u32>) -> io::Result<()> {
            unsupported()
        }
        fn sync(&mut self) -> io::Result<()> {
            unsupported()
        }
    }

    fn streams(a: &[u8], b: &[u8], claimed: u64) -> PairResult {
        let (sink, _) = ScriptedSink::refusing();
        let mut ctx = JobCtx::simple(Arc::new(RealFs), sink);
        let mut bufs = (vec![0u8; MIN_BUFFER], vec![0u8; MIN_BUFFER]);
        let mut credited = 0;
        compare_streams(
            &mut ctx,
            &mut MemFile::new(a),
            &mut MemFile::new(b),
            claimed,
            &mut bufs,
            (Path::new("a"), Path::new("b")),
            &mut credited,
        )
        .unwrap()
    }

    #[test]
    fn equal_streams_with_short_reads() {
        assert_eq!(streams(b"0123456789", b"0123456789", 10), PairResult::Equal);
    }

    #[test]
    fn streams_that_shrank_on_one_side_differ() {
        assert_eq!(streams(b"0123456789", b"01234", 10), PairResult::Differs);
        assert_eq!(streams(b"01234", b"0123456789", 10), PairResult::Differs);
    }

    #[test]
    fn streams_that_both_shrank_identically_are_an_error_not_equal() {
        let r = streams(b"01234", b"01234", 10);
        assert!(matches!(r, PairResult::Error(_)), "{r:?}");
    }

    #[test]
    fn stream_that_grew_past_the_size_seen_at_open_differs() {
        assert_eq!(
            streams(b"0123456789", b"01234567890", 10),
            PairResult::Differs
        );
        assert_eq!(
            streams(b"01234567890", b"01234567890", 10),
            PairResult::Differs
        );
    }

    #[test]
    fn difference_in_the_last_byte_is_found() {
        assert_eq!(
            streams(b"0123456789", b"012345678X", 10),
            PairResult::Differs
        );
    }

    #[test]
    fn cancel_in_the_middle_of_a_file_stops_at_the_next_chunk() {
        let token = ofm_core::CancelToken::new();
        let (sink, _) = ScriptedSink::refusing();
        let mut ctx = JobCtx::new(
            Arc::new(RealFs),
            token.clone(),
            crate::ctx::PauseGate::new(),
            Box::new(sink),
            Arc::new(crate::progress::SystemClock::new()),
        );
        let data = vec![7u8; 5 * MIN_BUFFER];
        let mut a = MemFile::new(&data);
        a.cancel_at = Some((100, token));
        let mut b = MemFile::new(&data);
        let mut bufs = (vec![0u8; MIN_BUFFER], vec![0u8; MIN_BUFFER]);
        let mut credited = 0;
        let r = compare_streams(
            &mut ctx,
            &mut a,
            &mut b,
            data.len() as u64,
            &mut bufs,
            (Path::new("a"), Path::new("b")),
            &mut credited,
        );
        assert!(matches!(r, Err(JobError::Cancelled)), "{r:?}");
        // Stopped after the chunk in which the cancel happened, not at the end.
        assert_eq!(credited, MIN_BUFFER as u64);
        assert!(a.pos < data.len());
    }
}

//! `FaultyFs`: wraps any [`Fs`] and injects faults from an `ofm-testkit` [`FaultPlan`]
//! (ENOSPC/EIO/EACCES/short writes at the N-th call of an operation). Available with the
//! `testing` feature (and in this crate's unit tests).
//!
//! Every [`Fs`] and [`FsFile`] method is intercepted, including the kernel-assisted copies
//! (`try_clone_from` = [`FaultOp::TryCloneFrom`], `copy_range_from` = [`FaultOp::CopyRangeFrom`]):
//! to make a copy fail "at the N-th data write" you may need faults on `FileWrite` AND
//! `CopyRangeFrom`, depending on which tier the engine picks on that file system. A
//! `ShortWrite` effect only has a meaning on `FileWrite`; on any other op it is ignored.
//! `FileClose` faults the checked close of a handle (`FsFile::close`); the descriptor is still
//! released, as with close(2).
//!
//! ```ignore
//! let plan = FaultPlan::new().fail_nth(FaultOp::FileWrite, 3, FaultErrno::NoSpace);
//! let fs = Arc::new(FaultyFs::over_real(plan));
//! let engine = JobEngine::with_fs(fs.clone());
//! // ... run a copy, assert dest not half-written, fs.calls_of(FaultOp::FileWrite) >= 3
//! ```

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use ofm_testkit::fault::{FaultEffect, FaultOp, FaultPlan, FaultState};

use super::{Fs, FsFile, FsStat, RealFs, TrashError};

type Shared = Arc<Mutex<FaultState>>;

fn hit(state: &Shared, op: FaultOp) -> Option<FaultEffect> {
    state
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .next(op)
}

/// Count the call; fail it if the plan says so (`ShortWrite` is ignored here).
fn check(state: &Shared, op: FaultOp) -> io::Result<()> {
    match hit(state, op) {
        Some(FaultEffect::Error(e)) => Err(e.to_io_error()),
        _ => Ok(()),
    }
}

/// Fault-injecting [`Fs`] wrapper.
pub struct FaultyFs {
    inner: Arc<dyn Fs>,
    state: Shared,
}

impl FaultyFs {
    /// Wrap `inner`.
    pub fn new(inner: Arc<dyn Fs>, plan: FaultPlan) -> Self {
        FaultyFs {
            inner,
            state: Arc::new(Mutex::new(FaultState::new(plan))),
        }
    }

    /// Wrap [`RealFs`].
    pub fn over_real(plan: FaultPlan) -> Self {
        Self::new(Arc::new(RealFs), plan)
    }

    /// How many calls of `op` happened so far (also counts faulted calls).
    pub fn calls_of(&self, op: FaultOp) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .calls_of(op)
    }

    fn open_wrap(&self, f: Box<dyn FsFile>) -> Box<dyn FsFile> {
        Box::new(FaultyFile {
            inner: f,
            state: Arc::clone(&self.state),
        })
    }
}

impl std::fmt::Debug for FaultyFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FaultyFs").finish_non_exhaustive()
    }
}

struct FaultyFile {
    inner: Box<dyn FsFile>,
    state: Shared,
}

impl FsFile for FaultyFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        check(&self.state, FaultOp::FileRead)?;
        self.inner.read(buf)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match hit(&self.state, FaultOp::FileWrite) {
            Some(FaultEffect::Error(e)) => Err(e.to_io_error()),
            Some(FaultEffect::ShortWrite { bytes }) => {
                let n = bytes.min(buf.len());
                if n == 0 {
                    return Ok(0);
                }
                self.inner.write(&buf[..n])
            }
            None => self.inner.write(buf),
        }
    }

    fn stat(&self) -> io::Result<FsStat> {
        check(&self.state, FaultOp::FileStat)?;
        self.inner.stat()
    }

    fn as_fd(&self) -> BorrowedFd<'_> {
        self.inner.as_fd()
    }

    fn try_clone_from(&mut self, src: &dyn FsFile) -> io::Result<bool> {
        check(&self.state, FaultOp::TryCloneFrom)?;
        self.inner.try_clone_from(src)
    }

    fn copy_range_from(&mut self, src: &mut dyn FsFile, len: u64) -> io::Result<Option<u64>> {
        check(&self.state, FaultOp::CopyRangeFrom)?;
        self.inner.copy_range_from(src, len)
    }

    fn seek_data(&mut self, from: u64) -> io::Result<Option<u64>> {
        check(&self.state, FaultOp::SeekData)?;
        self.inner.seek_data(from)
    }

    fn seek_hole(&mut self, from: u64) -> io::Result<u64> {
        check(&self.state, FaultOp::SeekHole)?;
        self.inner.seek_hole(from)
    }

    fn seek(&mut self, offset: u64) -> io::Result<()> {
        check(&self.state, FaultOp::Seek)?;
        self.inner.seek(offset)
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        check(&self.state, FaultOp::SetLen)?;
        self.inner.set_len(len)
    }

    fn set_mode(&mut self, mode: u32) -> io::Result<()> {
        check(&self.state, FaultOp::FileSetMode)?;
        self.inner.set_mode(mode)
    }

    fn set_times(&mut self, atime_ns: i128, mtime_ns: i128) -> io::Result<()> {
        check(&self.state, FaultOp::FileSetTimes)?;
        self.inner.set_times(atime_ns, mtime_ns)
    }

    fn set_owner(&mut self, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
        check(&self.state, FaultOp::FileSetOwner)?;
        self.inner.set_owner(uid, gid)
    }

    fn sync(&mut self) -> io::Result<()> {
        check(&self.state, FaultOp::FileSync)?;
        self.inner.sync()
    }

    fn close(self: Box<Self>) -> io::Result<()> {
        // The descriptor is released either way (like close(2)); an injected error is reported
        // as a deferred write error would be.
        let injected = check(&self.state, FaultOp::FileClose);
        let real = self.inner.close();
        injected.and(real)
    }
}

impl Fs for FaultyFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        check(&self.state, FaultOp::OpenRead)?;
        Ok(self.open_wrap(self.inner.open_read(path)?))
    }

    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        check(&self.state, FaultOp::OpenRead)?;
        Ok(self.open_wrap(self.inner.open_read_follow(path)?))
    }

    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        check(&self.state, FaultOp::CreateExcl)?;
        Ok(self.open_wrap(self.inner.create_excl(path, mode)?))
    }

    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        check(&self.state, FaultOp::Stat)?;
        self.inner.stat(path)
    }

    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        check(&self.state, FaultOp::Lstat)?;
        self.inner.lstat(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        check(&self.state, FaultOp::ReadDir)?;
        self.inner.read_dir(path)
    }

    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        check(&self.state, FaultOp::Mkdir)?;
        self.inner.mkdir(path, mode)
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::RenameNoreplace)?;
        self.inner.rename_noreplace(from, to)
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::RenameReplace)?;
        self.inner.rename_replace(from, to)
    }

    fn unlink(&self, path: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::Unlink)?;
        self.inner.unlink(path)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::Rmdir)?;
        self.inner.rmdir(path)
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::Symlink)?;
        self.inner.symlink(target, link)
    }

    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::HardLink)?;
        self.inner.hard_link(from, to)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        check(&self.state, FaultOp::ReadLink)?;
        self.inner.read_link(path)
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        check(&self.state, FaultOp::SetMode)?;
        self.inner.set_mode(path, mode)
    }

    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()> {
        check(&self.state, FaultOp::SetTimes)?;
        self.inner.set_times(path, atime_ns, mtime_ns, follow)
    }

    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()> {
        check(&self.state, FaultOp::SetOwner)?;
        self.inner.set_owner(path, uid, gid, follow)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        check(&self.state, FaultOp::SyncDir)?;
        self.inner.sync_dir(path)
    }

    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        check(&self.state, FaultOp::ListXattrs)?;
        self.inner.list_xattrs(path, follow)
    }

    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        check(&self.state, FaultOp::GetXattr)?;
        self.inner.get_xattr(path, name, follow)
    }

    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        check(&self.state, FaultOp::SetXattr)?;
        self.inner.set_xattr(path, name, value, follow)
    }

    fn remove_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<()> {
        check(&self.state, FaultOp::RemoveXattr)?;
        self.inner.remove_xattr(path, name, follow)
    }

    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        check(&self.state, FaultOp::Trash)?;
        self.inner.trash(path)
    }

    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_testkit::fault::FaultErrno;
    use ofm_testkit::temp::TempDir;

    #[test]
    fn faults_fire_at_the_nth_call_and_count() {
        let t = TempDir::new().unwrap();
        let plan = FaultPlan::new()
            .fail_nth(FaultOp::Unlink, 2, FaultErrno::Io)
            .fail_nth(FaultOp::Mkdir, 1, FaultErrno::PermissionDenied);
        let fs = FaultyFs::over_real(plan);
        assert_eq!(
            fs.mkdir(&t.path().join("d"), 0o700)
                .unwrap_err()
                .raw_os_error(),
            Some(13)
        );
        fs.mkdir(&t.path().join("d"), 0o700).unwrap();
        for n in ["a", "b", "c"] {
            fs.create_excl(&t.path().join(n), 0o600).unwrap();
        }
        fs.unlink(&t.path().join("a")).unwrap();
        assert_eq!(
            fs.unlink(&t.path().join("b")).unwrap_err().raw_os_error(),
            Some(5)
        );
        fs.unlink(&t.path().join("b")).unwrap();
        assert_eq!(fs.calls_of(FaultOp::Unlink), 3);
        assert!(t.path().join("c").exists());
    }

    #[test]
    fn short_write_and_write_error() {
        let t = TempDir::new().unwrap();
        let plan = FaultPlan::new().short_write_at(1, 3).fail_nth(
            FaultOp::FileWrite,
            2,
            FaultErrno::NoSpace,
        );
        let fs = FaultyFs::over_real(plan);
        let mut f = fs.create_excl(&t.path().join("f"), 0o600).unwrap();
        assert_eq!(f.write(b"abcdefgh").unwrap(), 3);
        assert_eq!(f.write(b"defgh").unwrap_err().raw_os_error(), Some(28));
        assert_eq!(f.write(b"defgh").unwrap(), 5);
        drop(f);
        assert_eq!(std::fs::read(t.path().join("f")).unwrap(), b"abcdefgh");
    }

    #[test]
    fn kernel_assisted_copies_are_interceptable_and_sources_can_be_wrapped() {
        let t = TempDir::new().unwrap();
        std::fs::write(t.path().join("src"), b"0123456789").unwrap();
        let plan = FaultPlan::new()
            .fail_nth(FaultOp::TryCloneFrom, 1, FaultErrno::Io)
            .fail_nth(FaultOp::CopyRangeFrom, 2, FaultErrno::NoSpace);
        let fs = FaultyFs::over_real(plan);
        let mut src = fs.open_read(&t.path().join("src")).unwrap();
        let mut dst = fs.create_excl(&t.path().join("dst"), 0o600).unwrap();
        // The source is itself a FaultyFile: as_fd forwarding makes the real call possible.
        assert_eq!(
            dst.try_clone_from(&*src).unwrap_err().raw_os_error(),
            Some(5)
        );
        let first = dst.copy_range_from(&mut *src, 4).unwrap();
        if first.is_some() {
            assert_eq!(
                dst.copy_range_from(&mut *src, 4)
                    .unwrap_err()
                    .raw_os_error(),
                Some(28)
            );
        }
        assert_eq!(fs.calls_of(FaultOp::TryCloneFrom), 1);
        assert!(fs.calls_of(FaultOp::CopyRangeFrom) >= 1);
    }
}

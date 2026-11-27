//! The file system seam of the job engine (ADR-0004, ADR-0006). ALL file system calls in
//! `ofm-jobs` go through [`Fs`], so tests can inject faults (`faulty::FaultyFs` wraps any `Fs` and
//! consults an `ofm-testkit` `FaultPlan`: ENOSPC, EIO, EACCES, short writes at the N-th call).
//! Paths are `&Path` (bytes); never convert to `String`.
//!
//! Files: `mod.rs` (this trait, frozen) · `real.rs` ([`RealFs`], rustix-based) · `faulty.rs`
//! (`FaultyFs`, feature `testing`) · `anchored.rs` ([`FsPin`] and the handle-anchored view used by
//! extraction).
//!
//! The wrapper-friendly part of the design: [`FsFile::as_fd`] exposes the descriptor, so the
//! kernel-assisted copies ([`FsFile::try_clone_from`], [`FsFile::copy_range_from`]) can take
//! `&dyn FsFile` sources (even a wrapped one) and still reach the real fd. Every one of these
//! methods is therefore interceptable by `FaultyFs`.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};

mod anchored;
#[cfg(any(test, feature = "testing"))]
pub mod faulty;
mod real;

pub(crate) use anchored::AnchoredFs;
pub use anchored::FsPin;
pub use real::RealFs;

/// Kind of a file system object, from `st_mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileType {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link (only from `lstat`).
    Symlink,
    /// FIFO, socket, block or character device.
    Other,
}

impl FileType {
    /// Classify an `st_mode`.
    pub fn from_mode(mode: u32) -> FileType {
        match mode & 0o170_000 {
            0o100_000 => FileType::File,
            0o040_000 => FileType::Dir,
            0o120_000 => FileType::Symlink,
            _ => FileType::Other,
        }
    }
}

/// Result of `stat`/`lstat`/`fstat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStat {
    /// Device id (`st_dev`).
    pub dev: u64,
    /// Inode.
    pub ino: u64,
    /// `st_mode` (type and permission bits).
    pub mode: u32,
    /// Hard link count.
    pub nlink: u64,
    /// Size in bytes.
    pub size: u64,
    /// 512-byte blocks allocated (sparse detection: `blocks * 512 < size`).
    pub blocks: u64,
    /// Owner.
    pub uid: u32,
    /// Group.
    pub gid: u32,
    /// Access time, ns since epoch.
    pub atime_ns: i128,
    /// Modification time, ns since epoch.
    pub mtime_ns: i128,
}

impl FsStat {
    /// Kind of object.
    pub fn file_type(&self) -> FileType {
        FileType::from_mode(self.mode)
    }
    /// Directory?
    pub fn is_dir(&self) -> bool {
        self.file_type() == FileType::Dir
    }
    /// Regular file?
    pub fn is_file(&self) -> bool {
        self.file_type() == FileType::File
    }
    /// Symlink? (only meaningful for `lstat` results)
    pub fn is_symlink(&self) -> bool {
        self.file_type() == FileType::Symlink
    }
    /// Permission bits only (`mode & 0o7777`).
    pub fn perm(&self) -> u32 {
        self.mode & 0o7777
    }
    /// Sparse file: fewer blocks allocated than the size needs.
    pub fn is_sparse(&self) -> bool {
        self.is_file() && self.blocks.saturating_mul(512) < self.size
    }
}

/// Why moving something to the trash did not work.
#[derive(Debug, thiserror::Error)]
pub enum TrashError {
    /// No usable trash for this path (other mount without `.Trash-$UID`, sandbox, ...): the engine
    /// asks the user before deleting permanently (`Question::TrashUnavailable`).
    #[error("trash unavailable: {0}")]
    Unavailable(String),
    /// Trashing failed for another reason (retry/skip/abort applies).
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// An open file handle.
pub trait FsFile: Send {
    /// Read at the current offset.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Write at the current offset (may be short; callers loop and treat `Ok(0)` as an error).
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    /// `fstat`.
    fn stat(&self) -> io::Result<FsStat>;
    /// The underlying descriptor. Wrappers (`FaultyFs`) forward to the inner file so that
    /// `try_clone_from`/`copy_range_from` work with any `&dyn FsFile` source.
    fn as_fd(&self) -> BorrowedFd<'_>;
    /// Try a reflink clone of the whole `src` into `self` (`ioctl(FICLONE)`). `Ok(false)` = not
    /// supported here (different fs, no reflink): fall back to [`copy_range_from`](Self::copy_range_from).
    fn try_clone_from(&mut self, src: &dyn FsFile) -> io::Result<bool>;
    /// `copy_file_range` up to `len` bytes from `src` (at its current offset) to `self` (at its
    /// current offset); both offsets advance. `Ok(None)` = unsupported (EXDEV/ENOSYS/EINVAL...):
    /// fall back to read/write. `Ok(Some(0))` = end of source.
    fn copy_range_from(&mut self, src: &mut dyn FsFile, len: u64) -> io::Result<Option<u64>>;
    /// `lseek(SEEK_DATA)`; `None` if there is no more data (only a hole to EOF).
    fn seek_data(&mut self, from: u64) -> io::Result<Option<u64>>;
    /// `lseek(SEEK_HOLE)` (EOF counts as a hole).
    fn seek_hole(&mut self, from: u64) -> io::Result<u64>;
    /// Seek to an absolute offset.
    fn seek(&mut self, offset: u64) -> io::Result<()>;
    /// Truncate / extend (used to create trailing holes).
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    /// `fchmod`.
    fn set_mode(&mut self, mode: u32) -> io::Result<()>;
    /// `futimens` (ns since epoch).
    fn set_times(&mut self, atime_ns: i128, mtime_ns: i128) -> io::Result<()>;
    /// `fchown`.
    fn set_owner(&mut self, uid: Option<u32>, gid: Option<u32>) -> io::Result<()>;
    /// `fsync`.
    fn sync(&mut self) -> io::Result<()>;
    /// Checked `close` (T3.6, additive): NFS and FUSE may report a deferred write error only
    /// here, so the engine closes every temp file with this before publishing it. The default
    /// (for wrappers and test doubles without a descriptor of their own) just drops the handle.
    fn close(self: Box<Self>) -> io::Result<()> {
        drop(self);
        Ok(())
    }
}

/// File system operations used by the engine. Implementations must be cheap to call from many
/// threads. `follow` parameters: `false` = operate on a symlink itself (`AT_SYMLINK_NOFOLLOW`).
pub trait Fs: Send + Sync {
    /// `open(O_RDONLY | O_NOFOLLOW | O_CLOEXEC)`: sources of the copy (ADR-0004 step 1).
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
    /// `open(O_RDONLY | O_CLOEXEC)` following symlinks (copy with `follow_symlinks`).
    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
    /// `open(O_WRONLY | O_CREAT | O_EXCL | O_NOFOLLOW | O_CLOEXEC, mode)` (the temp file).
    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>>;
    /// `stat` (follows symlinks).
    fn stat(&self, path: &Path) -> io::Result<FsStat>;
    /// `lstat`.
    fn lstat(&self, path: &Path) -> io::Result<FsStat>;
    /// Directory entry names (no `.`/`..`), unsorted.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>>;
    /// `mkdir`.
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()>;
    /// `renameat2(RENAME_NOREPLACE)`; fails with `AlreadyExists` if `to` exists. Files and dirs.
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Plain atomic `rename` (replaces `to`).
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// `unlink` (also for symlinks; never follows).
    fn unlink(&self, path: &Path) -> io::Result<()>;
    /// `rmdir`.
    fn rmdir(&self, path: &Path) -> io::Result<()>;
    /// `symlink(target, link)`; `target` is stored verbatim.
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    /// `link(from, to)` (hard link; does not follow a symlink `from`).
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// `readlink`.
    fn read_link(&self, path: &Path) -> io::Result<PathBuf>;
    /// `chmod` on a path (callers skip symlinks: Linux cannot chmod a link itself).
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()>;
    /// `utimensat` (ns since epoch).
    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()>;
    /// `chown`/`lchown`.
    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()>;
    /// `fsync` of a directory.
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    /// List extended attribute names.
    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>>;
    /// Read an xattr value (`None` if it vanished).
    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>>;
    /// Set an xattr.
    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()>;
    /// Remove an xattr (`removexattr`/`lremovexattr`). The default reports `Unsupported` for
    /// implementations without removal (T3.7a, additive; e.g. wrappers over a limited base).
    /// A vanished xattr fails with `NotFound`, like the raw syscall.
    fn remove_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<()> {
        let _ = (path, name, follow);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "remove_xattr: not implemented",
        ))
    }
    /// Pin the object at `path` with an `O_PATH` handle (FIX-M6M7, additive): the handle keeps
    /// naming that object when `path` is renamed or replaced. `follow = false` pins a symlink
    /// itself instead of its target (check [`FsPin::stat`]); only the last component is covered,
    /// so a caller that must not follow any symlink pins one component at a time through
    /// [`FsPin::path`]. The default reports `Unsupported`, which makes extraction fail closed.
    fn pin(&self, path: &Path, follow: bool) -> io::Result<FsPin> {
        let _ = (path, follow);
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "pin: not implemented",
        ))
    }
    /// Move to the freedesktop trash (`ops::trash`, T3.2). `Unavailable` makes the engine ask.
    fn trash(&self, path: &Path) -> Result<(), TrashError>;
    /// Whether the process runs as root (decides `fchown`/`security.*` xattrs on copy).
    fn is_root(&self) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_type_from_mode() {
        assert_eq!(FileType::from_mode(0o100_644), FileType::File);
        assert_eq!(FileType::from_mode(0o040_755), FileType::Dir);
        assert_eq!(FileType::from_mode(0o120_777), FileType::Symlink);
        assert_eq!(FileType::from_mode(0o010_600), FileType::Other);
    }
}

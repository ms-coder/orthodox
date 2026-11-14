//! The file system seam of the job engine. ALL file system calls in `ofm-jobs` go through [`Fs`],
//! so tests can inject faults (`ofm-testkit::FaultPlan` wraps an `Fs`: ENOSPC, EIO, EACCES, short
//! writes at the N-th call). Paths are `&Path` (bytes); never convert to `String`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// Result of `stat`/`lstat`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FsStat {
    /// Device id (`st_dev`).
    pub dev: u64,
    /// Inode.
    pub ino: u64,
    /// `st_mode`.
    pub mode: u32,
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

/// An open file handle.
pub trait FsFile: Send {
    /// Read at the current offset.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Write at the current offset (may be short; callers loop).
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    /// `fstat`.
    fn stat(&self) -> io::Result<FsStat>;
    /// Try a reflink clone of the whole source file into `self` (FICLONE). `Ok(false)` = unsupported.
    fn try_clone_from(&mut self, src: &dyn FsFile) -> io::Result<bool>;
    /// `copy_file_range` up to `len` bytes from `src` at its offset; `Ok(None)` = unsupported.
    fn copy_range_from(&mut self, src: &mut dyn FsFile, len: u64) -> io::Result<Option<u64>>;
    /// `lseek(SEEK_DATA)`; `None` if no more data.
    fn seek_data(&mut self, from: u64) -> io::Result<Option<u64>>;
    /// `lseek(SEEK_HOLE)`.
    fn seek_hole(&mut self, from: u64) -> io::Result<u64>;
    /// Seek to absolute offset.
    fn seek(&mut self, offset: u64) -> io::Result<()>;
    /// Truncate / extend (used to create trailing holes).
    fn set_len(&mut self, len: u64) -> io::Result<()>;
    /// `fchmod`.
    fn set_mode(&mut self, mode: u32) -> io::Result<()>;
    /// `futimens`.
    fn set_times(&mut self, atime_ns: i128, mtime_ns: i128) -> io::Result<()>;
    /// `fchown`.
    fn set_owner(&mut self, uid: Option<u32>, gid: Option<u32>) -> io::Result<()>;
    /// `fsync`.
    fn sync(&mut self) -> io::Result<()>;
}

/// File system operations used by the engine.
pub trait Fs: Send + Sync {
    /// `open(O_RDONLY | O_NOFOLLOW | O_CLOEXEC)`.
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>>;
    /// `open(O_WRONLY | O_CREAT | O_EXCL | O_CLOEXEC, mode)`.
    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>>;
    /// `stat` (follows symlinks).
    fn stat(&self, path: &Path) -> io::Result<FsStat>;
    /// `lstat`.
    fn lstat(&self, path: &Path) -> io::Result<FsStat>;
    /// Directory entry names (no `.`/`..`).
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>>;
    /// `mkdir`.
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()>;
    /// `renameat2(RENAME_NOREPLACE)`; fails with `AlreadyExists` if `to` exists.
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Plain atomic `rename` (replaces `to`).
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// `unlink`.
    fn unlink(&self, path: &Path) -> io::Result<()>;
    /// `rmdir`.
    fn rmdir(&self, path: &Path) -> io::Result<()>;
    /// `symlink(target, link)`.
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()>;
    /// `link(from, to)` (hard link).
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// `readlink`.
    fn read_link(&self, path: &Path) -> io::Result<PathBuf>;
    /// `chmod` on a path (no follow for symlinks: callers skip symlinks).
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()>;
    /// `utimensat(AT_SYMLINK_NOFOLLOW)`.
    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()>;
    /// `lchown`/`chown`.
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
    /// Read an xattr value.
    fn get_xattr(
        &self,
        path: &Path,
        name: &std::ffi::OsStr,
        follow: bool,
    ) -> io::Result<Option<Vec<u8>>>;
    /// Set an xattr.
    fn set_xattr(
        &self,
        path: &Path,
        name: &std::ffi::OsStr,
        value: &[u8],
        follow: bool,
    ) -> io::Result<()>;
    /// Whether the process runs as root (decides `fchown` on copy).
    fn is_root(&self) -> bool;
}

/// Real implementation backed by libc/rustix (M3).
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

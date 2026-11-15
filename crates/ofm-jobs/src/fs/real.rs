//! [`RealFs`]: the production [`Fs`], a thin layer over `rustix` (no libc crate). The only
//! `unsafe` is the checked `close` behind [`FsFile::close`] (T3.6 M4), which `OwnedFd` cannot
//! express (its `Drop` ignores the result).
//! Delivered with T3.0 so the trait design is proven; T3.1 owns it from here (bug fixes, tuning).

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, IntoRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use rustix::fs::{
    AtFlags, CWD, Gid, Mode, OFlags, RenameFlags, SeekFrom, Timespec, Timestamps, Uid, XattrFlags,
};
use rustix::io::Errno;

use super::{FileType, Fs, FsFile, FsStat, TrashError};

/// Real implementation backed by `rustix`.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

fn mode_of(mode: u32) -> Mode {
    Mode::from_bits_truncate(mode as _)
}

fn at_flags(follow: bool) -> AtFlags {
    if follow {
        AtFlags::empty()
    } else {
        AtFlags::SYMLINK_NOFOLLOW
    }
}

fn timespec(ns: i128) -> Timespec {
    let sec = ns.div_euclid(1_000_000_000);
    let nsec = ns.rem_euclid(1_000_000_000);
    Timespec {
        tv_sec: i64::try_from(sec).unwrap_or(0),
        tv_nsec: i64::try_from(nsec).unwrap_or(0) as _,
    }
}

fn timestamps(atime_ns: i128, mtime_ns: i128) -> Timestamps {
    Timestamps {
        last_access: timespec(atime_ns),
        last_modification: timespec(mtime_ns),
    }
}

fn ts_ns(sec: i128, nsec: i128) -> i128 {
    sec * 1_000_000_000 + nsec
}

#[allow(clippy::useless_conversion, reason = "field widths differ per target")]
fn convert(st: &rustix::fs::Stat) -> FsStat {
    FsStat {
        dev: u64::try_from(st.st_dev).unwrap_or(0),
        ino: u64::try_from(st.st_ino).unwrap_or(0),
        mode: u32::try_from(st.st_mode).unwrap_or(0),
        nlink: u64::try_from(st.st_nlink).unwrap_or(0),
        size: u64::try_from(st.st_size).unwrap_or(0),
        blocks: u64::try_from(st.st_blocks).unwrap_or(0),
        uid: st.st_uid,
        gid: st.st_gid,
        atime_ns: ts_ns(i128::from(st.st_atime), i128::from(st.st_atime_nsec)),
        mtime_ns: ts_ns(i128::from(st.st_mtime), i128::from(st.st_mtime_nsec)),
    }
}

/// Retry a syscall interrupted by a signal.
fn retry<T>(mut f: impl FnMut() -> Result<T, Errno>) -> io::Result<T> {
    loop {
        match f() {
            Err(Errno::INTR) => {}
            other => return other.map_err(io::Error::from),
        }
    }
}

struct RealFile {
    fd: OwnedFd,
}

impl FsFile for RealFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        retry(|| rustix::io::read(&self.fd, &mut *buf))
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        retry(|| rustix::io::write(&self.fd, buf))
    }

    fn stat(&self) -> io::Result<FsStat> {
        Ok(convert(&rustix::fs::fstat(&self.fd)?))
    }

    fn as_fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }

    fn try_clone_from(&mut self, src: &dyn FsFile) -> io::Result<bool> {
        match rustix::fs::ioctl_ficlone(&self.fd, src.as_fd()) {
            Ok(()) => Ok(true),
            // Different file system, no reflink support, not a regular file, refused by a
            // seccomp filter or for an immutable/swap file (EPERM, ETXTBSY): use the next tier.
            Err(
                Errno::XDEV
                | Errno::OPNOTSUPP
                | Errno::INVAL
                | Errno::NOSYS
                | Errno::NOTTY
                | Errno::PERM
                | Errno::TXTBSY,
            ) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }

    fn copy_range_from(&mut self, src: &mut dyn FsFile, len: u64) -> io::Result<Option<u64>> {
        let len = usize::try_from(len).unwrap_or(usize::MAX);
        loop {
            match rustix::fs::copy_file_range(src.as_fd(), None, &self.fd, None, len) {
                Ok(n) => return Ok(Some(n as u64)),
                Err(Errno::INTR) => {}
                // Like coreutils: unsupported here, or refused by seccomp (EPERM) / for a swap
                // file (ETXTBSY). The read/write tier reports a real error if there is one.
                Err(
                    Errno::XDEV
                    | Errno::NOSYS
                    | Errno::OPNOTSUPP
                    | Errno::INVAL
                    | Errno::BADF
                    | Errno::PERM
                    | Errno::TXTBSY,
                ) => {
                    return Ok(None);
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn seek_data(&mut self, from: u64) -> io::Result<Option<u64>> {
        match rustix::fs::seek(&self.fd, SeekFrom::Data(from)) {
            Ok(pos) => Ok(Some(pos)),
            Err(Errno::NXIO) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    fn seek_hole(&mut self, from: u64) -> io::Result<u64> {
        Ok(rustix::fs::seek(&self.fd, SeekFrom::Hole(from))?)
    }

    fn seek(&mut self, offset: u64) -> io::Result<()> {
        rustix::fs::seek(&self.fd, SeekFrom::Start(offset))?;
        Ok(())
    }

    fn set_len(&mut self, len: u64) -> io::Result<()> {
        Ok(rustix::fs::ftruncate(&self.fd, len)?)
    }

    fn set_mode(&mut self, mode: u32) -> io::Result<()> {
        Ok(rustix::fs::fchmod(&self.fd, mode_of(mode))?)
    }

    fn set_times(&mut self, atime_ns: i128, mtime_ns: i128) -> io::Result<()> {
        Ok(rustix::fs::futimens(
            &self.fd,
            &timestamps(atime_ns, mtime_ns),
        )?)
    }

    fn set_owner(&mut self, uid: Option<u32>, gid: Option<u32>) -> io::Result<()> {
        Ok(rustix::fs::fchown(
            &self.fd,
            uid.map(Uid::from_raw),
            gid.map(Gid::from_raw),
        )?)
    }

    fn sync(&mut self) -> io::Result<()> {
        Ok(rustix::fs::fsync(&self.fd)?)
    }

    fn close(self: Box<Self>) -> io::Result<()> {
        let raw = self.fd.into_raw_fd();
        // SAFETY: `raw` was just released from the `OwnedFd` this handle owned exclusively, so it
        // is open and nothing else uses or closes it; `try_close` consumes it (also on error).
        match unsafe { rustix::io::try_close(raw) } {
            // Linux has released the descriptor; EINTR does not report lost data.
            Ok(()) | Err(Errno::INTR) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// Open a copy source without blocking: `O_NONBLOCK` makes opening a FIFO (swapped in after the
/// engine's `lstat`) return at once instead of waiting forever for a writer, where not even
/// cancel could reach the job. FIFOs, sockets and devices are then refused, and the flag is
/// cleared again for the normal blocking reads.
fn open_source(path: &Path, follow: bool) -> io::Result<Box<dyn FsFile>> {
    let mut flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK | OFlags::NOCTTY;
    if !follow {
        flags |= OFlags::NOFOLLOW;
    }
    let fd = retry(|| rustix::fs::openat(CWD, path, flags, Mode::empty()))?;
    let st = convert(&rustix::fs::fstat(&fd)?);
    if st.file_type() == FileType::Other {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "special file (FIFO, socket or device); not opened for copying",
        ));
    }
    let fl = rustix::fs::fcntl_getfl(&fd)?;
    rustix::fs::fcntl_setfl(&fd, fl - OFlags::NONBLOCK)?;
    Ok(Box::new(RealFile { fd }))
}

/// Run a "fill this buffer" xattr syscall, growing the buffer on `ERANGE`.
fn xattr_buf(mut f: impl FnMut(&mut [u8]) -> Result<usize, Errno>) -> io::Result<Vec<u8>> {
    let mut size = f(&mut [])?;
    for _ in 0..4 {
        let mut buf = vec![0u8; size.max(1)];
        match f(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                return Ok(buf);
            }
            Err(Errno::RANGE) => size = size.saturating_mul(2).max(256),
            Err(e) => return Err(e.into()),
        }
    }
    Err(io::Error::other("xattr changed while reading"))
}

impl Fs for RealFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        open_source(path, false)
    }

    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        open_source(path, true)
    }

    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        let flags =
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
        let fd = retry(|| rustix::fs::openat(CWD, path, flags, mode_of(mode)))?;
        Ok(Box::new(RealFile { fd }))
    }

    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        Ok(convert(&rustix::fs::statat(CWD, path, AtFlags::empty())?))
    }

    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        Ok(convert(&rustix::fs::statat(
            CWD,
            path,
            AtFlags::SYMLINK_NOFOLLOW,
        )?))
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        std::fs::read_dir(path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect()
    }

    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        Ok(rustix::fs::mkdirat(CWD, path, mode_of(mode))?)
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        Ok(rustix::fs::renameat_with(
            CWD,
            from,
            CWD,
            to,
            RenameFlags::NOREPLACE,
        )?)
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        Ok(rustix::fs::renameat(CWD, from, CWD, to)?)
    }

    fn unlink(&self, path: &Path) -> io::Result<()> {
        Ok(rustix::fs::unlinkat(CWD, path, AtFlags::empty())?)
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        Ok(rustix::fs::unlinkat(CWD, path, AtFlags::REMOVEDIR)?)
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        Ok(rustix::fs::symlink(target, link)?)
    }

    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        Ok(rustix::fs::link(from, to)?)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        let target = rustix::fs::readlink(path, Vec::new())?;
        Ok(PathBuf::from(OsString::from_vec(target.into_bytes())))
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        Ok(rustix::fs::chmod(path, mode_of(mode))?)
    }

    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()> {
        Ok(rustix::fs::utimensat(
            CWD,
            path,
            &timestamps(atime_ns, mtime_ns),
            at_flags(follow),
        )?)
    }

    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()> {
        Ok(rustix::fs::chownat(
            CWD,
            path,
            uid.map(Uid::from_raw),
            gid.map(Gid::from_raw),
            at_flags(follow),
        )?)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC;
        let fd = retry(|| rustix::fs::openat(CWD, path, flags, Mode::empty()))?;
        Ok(rustix::fs::fsync(&fd)?)
    }

    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        let raw = xattr_buf(|buf| {
            if follow {
                rustix::fs::listxattr(path, buf)
            } else {
                rustix::fs::llistxattr(path, buf)
            }
        })?;
        Ok(raw
            .split(|&b| b == 0)
            .filter(|n| !n.is_empty())
            .map(|n| OsStr::from_bytes(n).to_owned())
            .collect())
    }

    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        let res = xattr_buf(|buf| {
            if follow {
                rustix::fs::getxattr(path, name, buf)
            } else {
                rustix::fs::lgetxattr(path, name, buf)
            }
        });
        match res {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.raw_os_error() == Some(Errno::NODATA.raw_os_error()) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        let res = if follow {
            rustix::fs::setxattr(path, name, value, XattrFlags::empty())
        } else {
            rustix::fs::lsetxattr(path, name, value, XattrFlags::empty())
        };
        Ok(res?)
    }

    fn remove_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<()> {
        let res = if follow {
            rustix::fs::removexattr(path, name)
        } else {
            rustix::fs::lremovexattr(path, name)
        };
        Ok(res?)
    }

    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        crate::ops::trash::trash_path(path)
    }

    fn is_root(&self) -> bool {
        rustix::process::geteuid().is_root()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_testkit::temp::TempDir;

    fn write(fs: &RealFs, p: &Path, data: &[u8]) {
        let mut f = fs.create_excl(p, 0o600).unwrap();
        let mut off = 0;
        while off < data.len() {
            off += f.write(&data[off..]).unwrap();
        }
    }

    fn read_all(fs: &RealFs, p: &Path) -> Vec<u8> {
        let mut f = fs.open_read(p).unwrap();
        let mut out = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = f.read(&mut buf).unwrap();
            if n == 0 {
                return out;
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    #[test]
    fn create_excl_refuses_existing_and_symlinks() {
        let t = TempDir::new().unwrap();
        let fs = RealFs;
        let p = t.path().join("a");
        write(&fs, &p, b"hello");
        assert_eq!(
            fs.create_excl(&p, 0o600).err().map(|e| e.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        let l = t.path().join("l");
        fs.symlink(&p, &l).unwrap();
        // O_NOFOLLOW: a symlink source cannot be opened by open_read.
        assert!(fs.open_read(&l).is_err());
        assert_eq!(read_all(&fs, &p), b"hello");
        assert!(fs.lstat(&l).unwrap().is_symlink());
        assert_eq!(fs.read_link(&l).unwrap(), p);
    }

    #[test]
    fn rename_noreplace_vs_replace() {
        let t = TempDir::new().unwrap();
        let fs = RealFs;
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        write(&fs, &a, b"A");
        write(&fs, &b, b"B");
        assert_eq!(
            fs.rename_noreplace(&a, &b).err().map(|e| e.kind()),
            Some(io::ErrorKind::AlreadyExists)
        );
        assert_eq!(read_all(&fs, &b), b"B");
        fs.rename_replace(&a, &b).unwrap();
        assert_eq!(read_all(&fs, &b), b"A");
        assert!(fs.stat(&a).is_err());
    }

    #[test]
    fn copy_range_and_clone_fallback_chain() {
        let t = TempDir::new().unwrap();
        let fs = RealFs;
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        write(&fs, &a, b"0123456789");
        let mut src = fs.open_read(&a).unwrap();
        let mut dst = fs.create_excl(&b, 0o600).unwrap();
        // Reflink may or may not be supported by the temp fs; both answers are valid.
        let cloned = dst.try_clone_from(&*src).unwrap();
        if !cloned {
            let n = dst.copy_range_from(&mut *src, 1 << 20).unwrap();
            if let Some(n) = n {
                assert_eq!(n, 10);
            } else {
                // unsupported: read/write fallback is the engine's job
                return;
            }
        }
        drop(dst);
        assert_eq!(read_all(&fs, &b), b"0123456789");
    }

    #[test]
    fn sparse_seek_data_hole() {
        let t = TempDir::new().unwrap();
        let fs = RealFs;
        let p = t.path().join("sparse");
        {
            let mut f = fs.create_excl(&p, 0o600).unwrap();
            f.set_len(1 << 20).unwrap();
            f.seek(1 << 19).unwrap();
            f.write(b"x").unwrap();
        }
        let mut f = fs.open_read(&p).unwrap();
        match f.seek_data(0) {
            Ok(Some(d)) => {
                assert!(d <= 1 << 19);
                let h = f.seek_hole(d).unwrap();
                assert!(h > d && h <= 1 << 20);
            }
            Ok(None) => panic!("file has data"),
            Err(e) => panic!("seek_data: {e}"), // tmpfs/ext4/btrfs/xfs all support it
        }
        assert!(f.stat().unwrap().size == 1 << 20);
    }

    #[test]
    fn metadata_and_xattr_roundtrip() {
        let t = TempDir::new().unwrap();
        let fs = RealFs;
        let p = t.path().join("m");
        write(&fs, &p, b"x");
        fs.set_mode(&p, 0o640).unwrap();
        let ns = 1_000_000_123_456_789_012_i128;
        fs.set_times(&p, ns, ns, true).unwrap();
        let st = fs.lstat(&p).unwrap();
        assert_eq!(st.perm(), 0o640);
        assert_eq!(st.mtime_ns, ns);
        // xattrs may be unsupported on the temp fs (tmpfs without user_xattr): tolerate EOPNOTSUPP.
        match fs.set_xattr(&p, OsStr::new("user.ofm"), b"v", false) {
            Ok(()) => {
                assert_eq!(
                    fs.get_xattr(&p, OsStr::new("user.ofm"), false).unwrap(),
                    Some(b"v".to_vec())
                );
                assert!(
                    fs.list_xattrs(&p, false)
                        .unwrap()
                        .contains(&OsString::from("user.ofm"))
                );
                assert_eq!(
                    fs.get_xattr(&p, OsStr::new("user.nope"), false).unwrap(),
                    None
                );
            }
            Err(e) => assert_eq!(e.raw_os_error(), Some(Errno::OPNOTSUPP.raw_os_error())),
        }
    }

    #[test]
    fn dirs_and_hardlinks() {
        let t = TempDir::new().unwrap();
        let fs = RealFs;
        let d = t.path().join("d");
        fs.mkdir(&d, 0o700).unwrap();
        let (a, b) = (d.join("a"), d.join("b"));
        write(&fs, &a, b"1");
        fs.hard_link(&a, &b).unwrap();
        let (sa, sb) = (fs.lstat(&a).unwrap(), fs.lstat(&b).unwrap());
        assert_eq!((sa.dev, sa.ino, sa.nlink), (sb.dev, sb.ino, 2));
        let mut names = fs.read_dir(&d).unwrap();
        names.sort();
        assert_eq!(names, vec![OsString::from("a"), OsString::from("b")]);
        fs.unlink(&a).unwrap();
        fs.unlink(&b).unwrap();
        fs.sync_dir(&d).unwrap();
        fs.rmdir(&d).unwrap();
    }
}

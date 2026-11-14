//! `statx` (with `fstatat` fallback) to [`Entry`] conversion.

use std::ffi::{CStr, OsStr, OsString};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

use ofm_core::{Entry, EntryKind, TargetKind};
use rustix::fs::{AtFlags, FileType, StatxFlags};
use rustix::io::Errno;

/// Set once the kernel/sandbox rejected `statx`; later calls go straight to `fstatat`.
static NO_STATX: AtomicBool = AtomicBool::new(false);

/// Raw metadata independent of the syscall used.
struct Raw {
    mode: u32,
    size: u64,
    uid: u32,
    gid: u32,
    mtime_ns: i128,
}

fn raw_stat<Fd: AsFd>(dirfd: Fd, name: &CStr, flags: AtFlags) -> Result<Raw, Errno> {
    if !NO_STATX.load(Ordering::Relaxed) {
        let mask = StatxFlags::TYPE
            | StatxFlags::MODE
            | StatxFlags::UID
            | StatxFlags::GID
            | StatxFlags::SIZE
            | StatxFlags::MTIME;
        match rustix::fs::statx(dirfd.as_fd(), name, flags, mask) {
            Ok(s) => {
                return Ok(Raw {
                    mode: u32::from(s.stx_mode),
                    size: s.stx_size,
                    uid: s.stx_uid,
                    gid: s.stx_gid,
                    mtime_ns: i128::from(s.stx_mtime.tv_sec) * 1_000_000_000
                        + i128::from(s.stx_mtime.tv_nsec),
                });
            }
            Err(Errno::NOSYS | Errno::PERM | Errno::OPNOTSUPP) => {
                NO_STATX.store(true, Ordering::Relaxed);
            }
            Err(e) => return Err(e),
        }
    }
    let s = rustix::fs::statat(dirfd.as_fd(), name, flags)?;
    #[allow(clippy::useless_conversion, reason = "field widths differ per target")]
    Ok(Raw {
        mode: s.st_mode.into(),
        size: u64::try_from(s.st_size).unwrap_or(0),
        uid: s.st_uid,
        gid: s.st_gid,
        mtime_ns: i128::from(s.st_mtime) * 1_000_000_000 + i128::from(s.st_mtime_nsec),
    })
}

fn kind_of_mode(mode: u32) -> Option<TargetKind> {
    match FileType::from_raw_mode(mode) {
        FileType::RegularFile => Some(TargetKind::File),
        FileType::Directory => Some(TargetKind::Dir),
        _ => Some(TargetKind::Other),
    }
}

/// Build an [`Entry`] for `name` relative to `dirfd` (does not follow a final symlink; for a
/// symlink a second, following `stat` determines the target kind, `None` when broken).
pub fn entry_at<Fd: AsFd>(dirfd: Fd, name: &CStr, display: OsString) -> Result<Entry, Errno> {
    let raw = raw_stat(dirfd.as_fd(), name, AtFlags::SYMLINK_NOFOLLOW)?;
    let kind = match FileType::from_raw_mode(raw.mode) {
        FileType::RegularFile => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => {
            let target = raw_stat(dirfd.as_fd(), name, AtFlags::empty())
                .ok()
                .and_then(|t| kind_of_mode(t.mode));
            EntryKind::Symlink { target }
        }
        _ => EntryKind::Other,
    };
    let mut e = Entry::new(display, kind);
    e.size = raw.size;
    e.mtime_ns = raw.mtime_ns;
    e.mode = raw.mode;
    e.uid = raw.uid;
    e.gid = raw.gid;
    Ok(e)
}

/// Display name for a raw directory entry name.
pub fn os_name(bytes: &[u8]) -> OsString {
    OsStr::from_bytes(bytes).to_owned()
}

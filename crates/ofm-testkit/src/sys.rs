//! Thin wrappers around the OS specifics the kit needs (Linux only).
//!
//! Kept private so the public API stays free of OS types; swapping the
//! implementation (std xattr vs. a crate) touches only this file.

use std::ffi::{OsStr, OsString};
use std::fs::{File, FileTimes};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `setxattr(path, name, value, 0)` (follows symlinks; used on files and dirs).
pub(crate) fn set_xattr(path: &Path, name: &OsStr, value: &[u8]) -> io::Result<()> {
    xattr::set(path, name, value)
}

/// `getxattr` -> `None` when the attribute does not exist.
pub(crate) fn get_xattr(path: &Path, name: &OsStr) -> io::Result<Option<Vec<u8>>> {
    xattr::get(path, name)
}

/// `listxattr` as a sorted list of names (follows symlinks).
pub(crate) fn list_xattrs(path: &Path) -> io::Result<Vec<OsString>> {
    let mut names: Vec<OsString> = xattr::list(path)?.collect();
    names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}

/// `futimens` on an open handle; works for files and directories.
pub(crate) fn set_mtime(path: &Path, mtime_ns: i128) -> io::Result<()> {
    let f = File::open(path)?;
    f.set_times(FileTimes::new().set_modified(ns_to_systemtime(mtime_ns)))
}

/// mtime of a [`std::fs::Metadata`] as ns since the Unix epoch (may be negative).
pub(crate) fn mtime_ns(md: &std::fs::Metadata) -> i128 {
    systemtime_to_ns(md.modified().unwrap_or(UNIX_EPOCH))
}

pub(crate) fn systemtime_to_ns(t: SystemTime) -> i128 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_nanos() as i128,
        Err(e) => -(e.duration().as_nanos() as i128),
    }
}

pub(crate) fn ns_to_systemtime(ns: i128) -> SystemTime {
    let magnitude = u64::try_from(ns.unsigned_abs()).unwrap_or(u64::MAX);
    if ns >= 0 {
        UNIX_EPOCH + Duration::from_nanos(magnitude)
    } else {
        UNIX_EPOCH - Duration::from_nanos(magnitude)
    }
}

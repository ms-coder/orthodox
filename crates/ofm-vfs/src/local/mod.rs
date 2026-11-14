//! Local file system provider (owned by T1.2).
//!
//! `openat` + `getdents` + `statx` (rustix) with `fstatat` fallback; batches of 512 entries or
//! every 16 ms; symlink target kind via a second `stat`; user/group name cache; free space via
//! `statvfs`.

mod list;
mod names;
mod stat;

use std::ffi::{CString, OsString};
use std::fs::File;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use ofm_core::{CancelToken, Entry, Location};
use rustix::fs::CWD;

pub use list::{BATCH_INTERVAL, BATCH_SIZE};
pub use names::{group_name, user_name};

use crate::error::VfsError;
use crate::vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs};

/// Provider for `Location::Local`.
#[derive(Debug, Default)]
pub struct LocalVfs {}

impl LocalVfs {
    /// Create the provider.
    pub fn new() -> Self {
        LocalVfs {}
    }
}

fn local_path(loc: &Location) -> Result<&Path, VfsError> {
    loc.local_path()
        .ok_or(VfsError::Unsupported("not a local location"))
}

impl Vfs for LocalVfs {
    fn list(&self, loc: &Location, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent)) {
        match local_path(loc) {
            Ok(p) => list::list_dir(p, cancel, sink),
            Err(e) => sink(ListEvent::Error(e)),
        }
    }

    fn stat(&self, loc: &Location) -> Result<Entry, VfsError> {
        let path = local_path(loc)?;
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| VfsError::Unsupported("path contains a NUL byte"))?;
        let name: OsString = path
            .file_name()
            .map_or_else(|| path.as_os_str().to_owned(), OsString::from);
        stat::entry_at(CWD, &c, name).map_err(|e| list::map_errno(path, e))
    }

    fn open_read(&self, loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        let path = local_path(loc)?;
        let file = File::open(path).map_err(|e| VfsError::from_io(path, e))?;
        let meta = file.metadata().map_err(|e| VfsError::from_io(path, e))?;
        if meta.is_dir() {
            return Err(VfsError::Io {
                path: path.to_path_buf(),
                source: std::io::Error::from_raw_os_error(rustix::io::Errno::ISDIR.raw_os_error()),
            });
        }
        Ok(Box::new(file))
    }

    fn capabilities(&self, _loc: &Location) -> Caps {
        Caps {
            read: true,
            write: true,
            rename: true,
            watch: true,
            trash: true,
            real_paths: true,
        }
    }

    fn parent(&self, loc: &Location) -> Option<Location> {
        let p = loc.local_path()?.parent()?;
        if p.as_os_str().is_empty() {
            return None;
        }
        Some(Location::Local(p.to_path_buf()))
    }

    fn space(&self, loc: &Location) -> Result<SpaceInfo, VfsError> {
        let path = local_path(loc)?;
        let s = rustix::fs::statvfs(path).map_err(|e| list::map_errno(path, e))?;
        Ok(SpaceInfo {
            free: s.f_bavail.saturating_mul(s.f_frsize),
            total: s.f_blocks.saturating_mul(s.f_frsize),
        })
    }
}

#[cfg(test)]
mod tests;

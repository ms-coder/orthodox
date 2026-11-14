//! Local file system provider (owned by T1.2; split into more files under `local/` freely).
//!
//! `read_dir` + `statx` (rustix) with `symlink_metadata` fallback; batches of 512 entries or every
//! 16 ms; symlink target kind via a second `stat`; user/group name cache; free space via `statvfs`.

use ofm_core::{CancelToken, Entry, Location};

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

impl Vfs for LocalVfs {
    fn list(&self, _loc: &Location, _cancel: &CancelToken, _sink: &mut dyn FnMut(ListEvent)) {
        todo!("T1.2")
    }
    fn stat(&self, _loc: &Location) -> Result<Entry, VfsError> {
        todo!("T1.2")
    }
    fn open_read(&self, _loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        todo!("T1.2")
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
    fn parent(&self, _loc: &Location) -> Option<Location> {
        todo!("T1.2")
    }
    fn space(&self, _loc: &Location) -> Result<SpaceInfo, VfsError> {
        todo!("T1.2")
    }
}

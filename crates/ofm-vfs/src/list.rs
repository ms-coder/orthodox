//! In-memory panelized lists (owned by M7). Entries carry `real_path`; capabilities are those of
//! the real files, so F5/F6/F8 operate on them.

use ofm_core::{CancelToken, Entry, ListId, Location};

use crate::error::VfsError;
use crate::vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs};

/// Shared store of lists; create one per app and hand it to [`ListVfs`].
#[derive(Debug, Default)]
pub struct ListStore {}

impl ListStore {
    /// Store a new list and return its id.
    pub fn create(&self, _entries: Vec<Entry>) -> ListId {
        todo!("M7")
    }
    /// Drop a list.
    pub fn remove(&self, _id: ListId) {
        todo!("M7")
    }
}

/// Provider for `Location::List`.
#[derive(Debug, Default)]
pub struct ListVfs {}

impl Vfs for ListVfs {
    fn list(&self, _loc: &Location, _cancel: &CancelToken, _sink: &mut dyn FnMut(ListEvent)) {
        todo!("M7")
    }
    fn stat(&self, _loc: &Location) -> Result<Entry, VfsError> {
        todo!("M7")
    }
    fn open_read(&self, _loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        todo!("M7")
    }
    fn capabilities(&self, _loc: &Location) -> Caps {
        todo!("M7")
    }
    fn parent(&self, _loc: &Location) -> Option<Location> {
        todo!("M7")
    }
    fn space(&self, _loc: &Location) -> Result<SpaceInfo, VfsError> {
        todo!("M7")
    }
}

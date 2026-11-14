//! The provider trait and its data types.

use std::io::{Read, Seek};

use ofm_core::{CancelToken, Entry, Location};

use crate::error::VfsError;

/// `Read + Seek`, object-safe.
pub trait ReadSeek: Read + Seek {}
impl<T: Read + Seek> ReadSeek for T {}

/// Streamed listing events. Providers emit `Batch`es (512 entries or every 16 ms, whichever
/// first), then exactly one `Done` or `Error` (a cancelled listing emits `Error(Cancelled)`).
#[derive(Debug)]
pub enum ListEvent {
    /// A batch of entries, unsorted; `..` is NOT included (the model synthesizes it).
    Batch(Vec<Entry>),
    /// Listing complete.
    Done {
        /// Total number of entries emitted.
        total: usize,
    },
    /// Listing failed (e.g. permission denied); entries already emitted stay valid.
    Error(VfsError),
}

/// What a location supports. Frontends grey out commands accordingly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Caps {
    /// Can be listed / read.
    pub read: bool,
    /// Can create/modify/delete entries (v1.0: only local).
    pub write: bool,
    /// Can rename in place.
    pub rename: bool,
    /// Supports change notification.
    pub watch: bool,
    /// Delete can go to the trash.
    pub trash: bool,
    /// Entries map to real local paths (`Entry::real_path` or the location itself).
    pub real_paths: bool,
}

/// Free/total space of the file system holding a location.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SpaceInfo {
    /// Bytes available to an unprivileged user.
    pub free: u64,
    /// Total bytes.
    pub total: u64,
}

/// A file system provider. All methods block and must be called from worker threads
/// (see `pool::IoPool`), never from the GTK main thread.
pub trait Vfs: Send + Sync {
    /// Stream the directory listing of `loc` into `sink`; check `cancel` between batches.
    fn list(&self, loc: &Location, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent));
    /// Metadata of `loc` itself (does not follow a final symlink; `kind` carries the target kind).
    fn stat(&self, loc: &Location) -> Result<Entry, VfsError>;
    /// Open a file for reading.
    fn open_read(&self, loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError>;
    /// Capabilities of `loc`.
    fn capabilities(&self, loc: &Location) -> Caps;
    /// Parent directory location (`None` at a root; an archive root's parent is the directory
    /// containing the archive file).
    fn parent(&self, loc: &Location) -> Option<Location>;
    /// Free/total space; `Unsupported` where meaningless.
    fn space(&self, loc: &Location) -> Result<SpaceInfo, VfsError>;
}

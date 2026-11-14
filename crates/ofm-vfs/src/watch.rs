//! Directory watcher (owned by T1.2): `notify` (inotify), 100 ms debounce, emits changed names so
//! the panel re-stats only those.

use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

use crate::error::VfsError;

/// Debounced change notification for one watched directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchEvent {
    /// These names (in the watched dir) were created, modified, or removed: re-stat them.
    Changed(Vec<OsString>),
    /// Too many events or the kernel queue overflowed: re-list the directory.
    Rescan,
    /// The watched directory itself disappeared.
    DirGone,
}

/// Keeps a watch alive; dropping it stops watching.
#[derive(Debug)]
pub struct WatchHandle {
    /// Receiver for debounced events. Use with `glib::spawn_future_local` in the app.
    pub events: async_channel::Receiver<WatchEvent>,
}

/// Start watching a local directory (non-recursive). Default debounce: 100 ms.
pub fn watch_dir(_dir: &Path, _debounce: Duration) -> Result<WatchHandle, VfsError> {
    todo!("T1.2")
}

//! Directory entry as shown in panels. Plain data; produced by `ofm-vfs`.

use std::ffi::OsString;
use std::path::PathBuf;

/// What a symlink points at (`None` in [`EntryKind::Symlink`] = broken link).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TargetKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Anything else (device, fifo, socket).
    Other,
}

/// Kind of a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link; `target` is the kind of the final target, `None` if broken.
    Symlink {
        /// Kind of the resolved target.
        target: Option<TargetKind>,
    },
    /// Device, fifo, socket, ...
    Other,
}

impl EntryKind {
    /// True for directories and symlinks to directories (what "enter" works on).
    pub fn is_dir_like(&self) -> bool {
        matches!(
            self,
            EntryKind::Dir
                | EntryKind::Symlink {
                    target: Some(TargetKind::Dir)
                }
        )
    }
}

/// One directory entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Display name (file name only; for List entries may be a relative or full path).
    pub name: OsString,
    /// Kind.
    pub kind: EntryKind,
    /// Size in bytes (0 for directories until calculated).
    pub size: u64,
    /// For directories: `size` is a calculated recursive total (`file.calc_size`), not the
    /// file system's own directory size. The panel shows the number instead of `<DIR>` then.
    pub size_known: bool,
    /// Modification time, nanoseconds since the Unix epoch.
    pub mtime_ns: i128,
    /// `st_mode` (type and permission bits).
    pub mode: u32,
    /// Owner uid.
    pub uid: u32,
    /// Owner gid.
    pub gid: u32,
    /// Real local path for List/Archive-extracted entries.
    pub real_path: Option<PathBuf>,
}

impl Entry {
    /// Minimal entry for tests and synthetic rows (`..`): zero metadata.
    pub fn new(name: impl Into<OsString>, kind: EntryKind) -> Self {
        Entry {
            name: name.into(),
            kind,
            size: 0,
            size_known: false,
            mtime_ns: 0,
            mode: 0,
            uid: 0,
            gid: 0,
            real_path: None,
        }
    }
}

//! Where an entry lives: local path, inside an archive, or in a panelized list.

use std::path::{Path, PathBuf};

/// Identifier of an in-memory list (search results, command output) held by `ofm-vfs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ListId(pub u64);

/// A directory or file location. Paths are `PathBuf` (bytes), never `String`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Location {
    /// A path on the local file system.
    Local(PathBuf),
    /// A path inside an archive file that lives on the local file system (no nesting in v1.0).
    Archive {
        /// The archive file.
        archive: PathBuf,
        /// Path inside the archive; empty = archive root.
        inner: PathBuf,
    },
    /// A panelized list; entries carry `real_path`.
    List {
        /// Which list.
        id: ListId,
    },
}

/// Discriminant of [`Location`], used to pick a VFS provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LocationKind {
    /// [`Location::Local`]
    Local,
    /// [`Location::Archive`]
    Archive,
    /// [`Location::List`]
    List,
}

impl Location {
    /// Provider kind for this location.
    pub fn kind(&self) -> LocationKind {
        match self {
            Location::Local(_) => LocationKind::Local,
            Location::Archive { .. } => LocationKind::Archive,
            Location::List { .. } => LocationKind::List,
        }
    }

    /// The local path if this is `Local`.
    pub fn local_path(&self) -> Option<&Path> {
        match self {
            Location::Local(p) => Some(p),
            _ => None,
        }
    }

    /// Child location named `name` (Local and Archive only; `List` returns a clone).
    pub fn join(&self, name: impl AsRef<Path>) -> Location {
        match self {
            Location::Local(p) => Location::Local(p.join(name)),
            Location::Archive { archive, inner } => Location::Archive {
                archive: archive.clone(),
                inner: inner.join(name),
            },
            Location::List { .. } => self.clone(),
        }
    }

    /// Lossy, human-readable form for titles and logs (never for I/O).
    pub fn display_lossy(&self) -> String {
        match self {
            Location::Local(p) => p.to_string_lossy().into_owned(),
            Location::Archive { archive, inner } => {
                format!("{}#/{}", archive.to_string_lossy(), inner.to_string_lossy())
            }
            Location::List { id } => format!("list:{}", id.0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_and_kind() {
        let l = Location::Local("/a".into()).join("b");
        assert_eq!(l, Location::Local("/a/b".into()));
        assert_eq!(l.kind(), LocationKind::Local);
        let a = Location::Archive {
            archive: "/x.zip".into(),
            inner: "".into(),
        }
        .join("d");
        assert_eq!(a.display_lossy(), "/x.zip#/d");
    }
}

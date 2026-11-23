//! `ArchiveVfs`: how the panel sees an archive (ADR-0008). Read-only provider for
//! `Location::Archive { archive, inner }`.
//!
//! **Path scheme.** `archive` = absolute path of the archive file on the local file system (no
//! nesting in v1.0); `inner` = normalized path inside it: relative, plain components, no `..`, no
//! leading `/`; empty = archive root. `Location::join` already builds children. The parent of the
//! root is the directory containing the archive file (`Location::Local`), so Backspace at the root
//! returns there and the panel puts the cursor on the archive file.
//!
//! **Index cache.** The first listing opens the archive and builds an `ArchiveIndex` in the calling
//! worker; later listings of the same archive reuse it while the file's identity is unchanged
//! (T6.1: key = path, validated by (size, mtime_ns, ino) via `stat`; stale = rebuild).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_core::{CancelToken, Entry, EntryKind, Location};

use super::error::ArchiveError;
use super::index::{ArchiveEntryKind, ArchiveIndex};
use super::limits::ArchiveLimits;
use super::password::PasswordSource;
use super::reader::{ArchiveReader, open_archive};
use crate::error::VfsError;
use crate::vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs};

/// Entries larger than this are not opened through `Vfs::open_read` (it returns a seekable
/// in-memory buffer); callers extract them with an extract job instead (F3 on a big entry).
pub const MAX_OPEN_READ_BYTES: u64 = 64 << 20;

const BATCH: usize = 512;

/// Provider for `Location::Archive`. Cheap to share behind an `Arc`.
pub struct ArchiveVfs {
    passwords: PasswordSource,
    limits: ArchiveLimits,
    indexes: Mutex<HashMap<PathBuf, Arc<ArchiveIndex>>>,
    /// File identity (size, mtime, ino) an index was built from; a cached index is only reused
    /// while the archive file's identity matches.
    identities: Mutex<HashMap<PathBuf, FileIdentity>>,
}

/// Identity of the archive file an index was built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    size: u64,
    mtime_ns: i128,
    ino: u64,
}

fn stat_identity(path: &Path) -> Option<FileIdentity> {
    let md = std::fs::metadata(path).ok()?;
    Some(FileIdentity {
        size: md.len(),
        mtime_ns: md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| i128::try_from(d.as_nanos()).unwrap_or(i128::MAX)),
        ino: std::os::unix::fs::MetadataExt::ino(&md),
    })
}

impl std::fmt::Debug for ArchiveVfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArchiveVfs")
            .field("limits", &self.limits)
            .finish_non_exhaustive()
    }
}

impl ArchiveVfs {
    /// Provider asking `passwords` for encrypted archives. Share the same `PasswordSource` with
    /// extract jobs so a password is asked once per archive.
    pub fn new(passwords: PasswordSource) -> Self {
        Self::with_limits(passwords, ArchiveLimits::default())
    }

    /// Like [`new`](Self::new) with explicit limits (tests).
    pub fn with_limits(passwords: PasswordSource, limits: ArchiveLimits) -> Self {
        ArchiveVfs {
            passwords,
            limits,
            indexes: Mutex::new(HashMap::new()),
            identities: Mutex::new(HashMap::new()),
        }
    }

    /// The shared password source.
    pub fn passwords(&self) -> &PasswordSource {
        &self.passwords
    }

    /// Index of `archive`, building it if needed (blocking; worker threads only). A cached index
    /// is reused only while the file's identity (size, mtime, ino) is unchanged; a changed file
    /// drops the cached index (and the remembered password) and is rebuilt.
    pub fn index(
        &self,
        archive: &Path,
        cancel: &CancelToken,
    ) -> Result<Arc<ArchiveIndex>, ArchiveError> {
        if let Some(ix) = self.cached(archive) {
            return Ok(ix);
        }
        let reader = self.open_reader(archive, cancel)?;
        let ix = reader.index().clone();
        self.preload_index(archive, ix.clone());
        Ok(ix)
    }

    /// Open a fresh reader (own file handle/stream) for streaming entry data.
    pub fn open_reader(
        &self,
        archive: &Path,
        cancel: &CancelToken,
    ) -> Result<Box<dyn ArchiveReader + '_>, ArchiveError> {
        open_archive(archive, &self.passwords, &self.limits, cancel)
    }

    /// Put a ready-made index into the cache (T6.1 uses it after indexing; tests use it to
    /// exercise listing without a real archive).
    pub fn preload_index(&self, archive: &Path, index: Arc<ArchiveIndex>) {
        if let Ok(mut m) = self.indexes.lock() {
            m.insert(archive.to_owned(), index);
        }
        if let Ok(mut m) = self.identities.lock()
            && let Some(id) = stat_identity(archive)
        {
            m.insert(archive.to_owned(), id);
        }
    }

    /// Drop the cached index of `archive` (it changed on disk) and its remembered password.
    pub fn invalidate(&self, archive: &Path) {
        if let Ok(mut m) = self.indexes.lock() {
            m.remove(archive);
        }
        if let Ok(mut m) = self.identities.lock() {
            m.remove(archive);
        }
        self.passwords.forget(archive);
    }

    /// A cached index whose file identity still matches (staleness check). An index whose
    /// identity was never recorded (preloaded where the file cannot be stat-ed, e.g. in tests)
    /// is trusted; once the file can be stat-ed, its identity is adopted so later changes are
    /// still detected.
    fn cached(&self, archive: &Path) -> Option<Arc<ArchiveIndex>> {
        let ix = self.indexes.lock().ok()?.get(archive).cloned()?;
        let mut ids = self.identities.lock().ok()?;
        match (stat_identity(archive), ids.get(archive).copied()) {
            // Identity known and matching: fresh.
            (Some(cur), Some(id)) if cur == id => Some(ix),
            // Identity not yet recorded: adopt the current one (self-heal) and serve.
            (Some(cur), None) => {
                ids.insert(archive.to_owned(), cur);
                Some(ix)
            }
            // No file and no recorded identity: nothing to compare against (test preload).
            (None, None) => Some(ix),
            // File vanished after indexing, or its identity changed: rebuild.
            _ => {
                drop(ids);
                self.invalidate(archive);
                None
            }
        }
    }

    /// The error for any attempt to modify an archive location (all write operations).
    pub fn read_only_error(loc: &Location) -> VfsError {
        match loc {
            Location::Archive { archive, .. } => VfsError::ReadOnly(archive.clone()),
            other => VfsError::NoProvider(other.display_lossy()),
        }
    }
}

fn split(loc: &Location) -> Result<(&Path, &Path), VfsError> {
    match loc {
        Location::Archive { archive, inner } => Ok((archive, inner)),
        other => Err(VfsError::NoProvider(other.display_lossy())),
    }
}

impl Vfs for ArchiveVfs {
    fn list(&self, loc: &Location, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent)) {
        let (archive, inner) = match split(loc) {
            Ok(p) => p,
            Err(e) => return sink(ListEvent::Error(e)),
        };
        let index = match self.index(archive, cancel) {
            Ok(ix) => ix,
            Err(e) => return sink(ListEvent::Error(e.into())),
        };
        let Some(children) = index.list_dir(inner) else {
            return sink(ListEvent::Error(VfsError::NotADirectory(
                archive.join(inner),
            )));
        };
        let mut total = 0;
        for chunk in children.chunks(BATCH) {
            if cancel.is_cancelled() {
                return sink(ListEvent::Error(VfsError::Cancelled));
            }
            total += chunk.len();
            sink(ListEvent::Batch(
                chunk.iter().map(|e| index.to_entry(e)).collect(),
            ));
        }
        sink(ListEvent::Done { total });
    }

    fn stat(&self, loc: &Location) -> Result<Entry, VfsError> {
        let (archive, inner) = split(loc)?;
        let index = self.index(archive, &CancelToken::new())?;
        if inner.as_os_str().is_empty() {
            let mut root = Entry::new(archive.file_name().unwrap_or_default(), EntryKind::Dir);
            root.mode = 0o040_755;
            return Ok(root);
        }
        index
            .lookup(inner)
            .map(|e| index.to_entry(e))
            .ok_or_else(|| VfsError::NotFound(archive.join(inner)))
    }

    /// The entry's data as a seekable in-memory buffer if `size <= MAX_OPEN_READ_BYTES`,
    /// else `Unsupported` (callers extract big entries with a job); directories are
    /// `Unsupported`; encrypted entries ask the password source; unsafe entries are invisible
    /// (NotFound, like `lookup`).
    fn open_read(&self, loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        let (archive, inner) = split(loc)?;
        let index = self.index(archive, &CancelToken::new())?;
        if inner.as_os_str().is_empty() {
            return Err(VfsError::Unsupported(
                "cannot open a directory inside an archive",
            ));
        }
        let entry = index
            .lookup(inner)
            .ok_or_else(|| VfsError::NotFound(archive.join(inner)))?;
        if entry.kind == ArchiveEntryKind::Dir {
            return Err(VfsError::Unsupported(
                "cannot open a directory inside an archive",
            ));
        }
        if entry.size > MAX_OPEN_READ_BYTES {
            return Err(VfsError::Unsupported(
                "entry too large for in-memory viewing",
            ));
        }
        let mut reader = self.open_reader(archive, &CancelToken::new())?;
        let mut buf = Vec::with_capacity(entry.size.min(MAX_OPEN_READ_BYTES) as usize);
        reader.read_entry(entry.id, &mut buf, &CancelToken::new())?;
        Ok(Box::new(std::io::Cursor::new(buf)))
    }

    fn capabilities(&self, _loc: &Location) -> Caps {
        Caps {
            read: true,
            ..Caps::default()
        }
    }

    fn parent(&self, loc: &Location) -> Option<Location> {
        let (archive, inner) = split(loc).ok()?;
        if inner.as_os_str().is_empty() {
            return archive.parent().map(|p| Location::Local(p.to_owned()));
        }
        Some(Location::Archive {
            archive: archive.to_owned(),
            inner: inner.parent().map(Path::to_owned).unwrap_or_default(),
        })
    }

    fn space(&self, _loc: &Location) -> Result<SpaceInfo, VfsError> {
        Err(VfsError::Unsupported("space of an archive"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::archive::{ArchiveEntryKind, ArchiveFormat, IndexBuilder, RawEntry};

    fn vfs_with_index() -> ArchiveVfs {
        let vfs = ArchiveVfs::new(PasswordSource::none());
        let mut b = IndexBuilder::new("/x/a.zip", ArchiveFormat::Zip, ArchiveLimits::default());
        let mut f = RawEntry::new("d/f.txt", ArchiveEntryKind::File);
        f.size = 5;
        b.push(f).unwrap();
        vfs.preload_index(Path::new("/x/a.zip"), Arc::new(b.finish()));
        vfs
    }

    fn at(inner: &str) -> Location {
        Location::Archive {
            archive: "/x/a.zip".into(),
            inner: inner.into(),
        }
    }

    #[test]
    fn lists_stats_and_navigates_a_preloaded_index() {
        let vfs = vfs_with_index();
        let mut seen = Vec::new();
        vfs.list(&at("d"), &CancelToken::new(), &mut |ev| seen.push(ev));
        assert!(matches!(&seen[0], ListEvent::Batch(b) if b.len() == 1 && b[0].name == "f.txt"));
        assert!(matches!(seen[1], ListEvent::Done { total: 1 }));
        assert_eq!(vfs.stat(&at("d/f.txt")).unwrap().size, 5);
        assert!(matches!(vfs.stat(&at("nope")), Err(VfsError::NotFound(_))));
        assert_eq!(vfs.parent(&at("d/f.txt")), Some(at("d")));
        assert_eq!(vfs.parent(&at("d")), Some(at("")));
        assert_eq!(
            vfs.parent(&at("")),
            Some(Location::Local("/x".into())),
            "root -> directory holding the archive"
        );
        let caps = vfs.capabilities(&at(""));
        assert!(caps.read && !caps.write && !caps.real_paths);
        assert!(matches!(
            ArchiveVfs::read_only_error(&at("d")),
            VfsError::ReadOnly(_)
        ));
    }

    #[test]
    fn unindexed_archive_and_open_read_are_typed_errors() {
        let vfs = ArchiveVfs::new(PasswordSource::none());
        let missing = Location::Archive {
            archive: "/nonexistent/none.zip".into(),
            inner: "".into(),
        };
        let mut seen = Vec::new();
        vfs.list(&missing, &CancelToken::new(), &mut |ev| seen.push(ev));
        assert!(matches!(seen.as_slice(), [ListEvent::Error(_)]));
        let e = vfs.open_read(&at("d/f.txt")).err().expect("an error");
        assert!(
            matches!(e, VfsError::NotFound(_)),
            "indexing a missing archive is a typed not-found error, got: {e:?}"
        );
    }
}

//! The in-memory index of one archive: flat entry list + directory tree lookup (ADR-0008).
//!
//! Backends push every stored entry into an [`IndexBuilder`]; the builder does the zip-slip
//! classification, synthesizes missing parent directories and builds the lookup tables. Entry
//! *data* is never part of the index (metadata only), so indexing is O(entries) memory.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use ofm_core::{Entry, EntryKind, TargetKind};

use super::error::{ArchiveError, LimitKind, UnsafeReason};
use super::format::ArchiveFormat;
use super::limits::ArchiveLimits;
use super::path::{normalize_entry_path, resolve_link_target};

const S_IFDIR: u32 = 0o040_000;
const S_IFREG: u32 = 0o100_000;
const S_IFLNK: u32 = 0o120_000;
const MAX_SYMLINK_HOPS: usize = 8;

/// Position of an entry in [`ArchiveIndex::entries`]; also the archive order (ids ascend in the
/// order entries are stored, which is the order a sequential read visits them).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EntryId(pub u32);

/// Kind of an archive entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveEntryKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link (`link_target` holds the stored target text).
    Symlink,
    /// tar hardlink to an earlier entry (`link_target` = normalized archive path of that entry).
    /// Its data is the target's data.
    Hardlink,
    /// Device, fifo, socket: listed, never extracted (a warning instead).
    Other,
}

/// Safety verdict of an entry, decided once at index time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntrySafety {
    /// May be listed and extracted.
    Safe,
    /// Must never be listed, extracted or opened.
    Unsafe(UnsafeReason),
}

/// An entry as a backend reports it, before normalization. Unknown metadata stays `None`.
#[derive(Debug, Clone)]
pub struct RawEntry {
    /// Path exactly as stored (bytes).
    pub path: PathBuf,
    /// Kind.
    pub kind: ArchiveEntryKind,
    /// Declared unpacked size.
    pub size: u64,
    /// Packed (compressed) size, if the format records it per entry (zip does; tar/solid 7z not).
    pub packed_size: Option<u64>,
    /// Modification time, ns since the epoch.
    pub mtime_ns: Option<i128>,
    /// Permission bits (`& 0o7777`); type bits are ignored.
    pub mode: Option<u32>,
    /// Owner uid.
    pub uid: Option<u32>,
    /// Owner gid.
    pub gid: Option<u32>,
    /// Link target as stored (symlink text / hardlink path).
    pub link_target: Option<PathBuf>,
    /// Entry data is encrypted.
    pub encrypted: bool,
}

impl RawEntry {
    /// Entry with unknown metadata.
    pub fn new(path: impl Into<PathBuf>, kind: ArchiveEntryKind) -> Self {
        RawEntry {
            path: path.into(),
            kind,
            size: 0,
            packed_size: None,
            mtime_ns: None,
            mode: None,
            uid: None,
            gid: None,
            link_target: None,
            encrypted: false,
        }
    }
}

/// One indexed entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArchiveEntry {
    /// Id (index into [`ArchiveIndex::entries`]).
    pub id: EntryId,
    /// Normalized relative path (plain components only). For `Unsafe` entries this is the RAW
    /// stored path, for diagnostics only: never join it to a destination.
    pub path: PathBuf,
    /// Kind.
    pub kind: ArchiveEntryKind,
    /// Declared unpacked size (0 for dirs).
    pub size: u64,
    /// Packed size if known.
    pub packed_size: Option<u64>,
    /// Modification time in ns since the epoch; 0 when the archive has none.
    pub mtime_ns: i128,
    /// Permission bits (`0o7777`); defaults 0o755 dirs, 0o644 files, 0o777 symlinks.
    pub mode: u32,
    /// Owner uid (0 if unknown). Never applied on extraction.
    pub uid: u32,
    /// Owner gid (0 if unknown). Never applied on extraction.
    pub gid: u32,
    /// Link target: symlinks as stored (verbatim text), hardlinks normalized.
    pub link_target: Option<PathBuf>,
    /// Data is encrypted (needs a password to read; the listing itself may not).
    pub encrypted: bool,
    /// Directory that the archive does not store explicitly, synthesized from a child path.
    pub synthetic: bool,
    /// Safe or flagged.
    pub safety: EntrySafety,
}

impl ArchiveEntry {
    /// True if the entry may be listed/extracted.
    pub fn is_safe(&self) -> bool {
        self.safety == EntrySafety::Safe
    }
}

/// Aggregated warnings the UI/extract job show before extracting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct IndexWarnings {
    /// Entries flagged `Unsafe` (skipped, never extracted).
    pub unsafe_entries: u32,
    /// Entries over the compression-ratio guard (`ArchiveLimits::max_ratio`).
    pub suspicious_ratio_entries: u32,
    /// Declared total unpacked size of all files.
    pub total_bytes: u64,
    /// `total_bytes > ArchiveLimits::warn_total_bytes`.
    pub total_size_exceeds_warn: bool,
}

/// Collects entries of one archive and produces the [`ArchiveIndex`].
#[derive(Debug)]
pub struct IndexBuilder {
    archive: PathBuf,
    format: ArchiveFormat,
    limits: ArchiveLimits,
    entries: Vec<ArchiveEntry>,
    symlinks: HashSet<PathBuf>,
    warnings: IndexWarnings,
}

impl IndexBuilder {
    /// Start an index for `archive` (used in errors only; the builder does no I/O).
    pub fn new(archive: impl Into<PathBuf>, format: ArchiveFormat, limits: ArchiveLimits) -> Self {
        IndexBuilder {
            archive: archive.into(),
            format,
            limits,
            entries: Vec::new(),
            symlinks: HashSet::new(),
            warnings: IndexWarnings::default(),
        }
    }

    fn limit(&self, kind: LimitKind) -> ArchiveError {
        ArchiveError::LimitExceeded {
            archive: self.archive.clone(),
            kind,
        }
    }

    /// Add one stored entry in archive order. Returns `Ok(None)` for the archive root (`./`).
    /// An unsafe entry is recorded (flagged) and still returns its id. Errors: only the hard
    /// limits (`EntryCount`, `Depth`).
    pub fn push(&mut self, raw: RawEntry) -> Result<Option<EntryId>, ArchiveError> {
        if self.entries.len() >= self.limits.max_entries {
            return Err(self.limit(LimitKind::EntryCount));
        }
        let (path, mut safety) = match normalize_entry_path(&raw.path) {
            Ok(p) if p.as_os_str().is_empty() => return Ok(None),
            Ok(p) => (p, EntrySafety::Safe),
            Err(reason) => (raw.path.clone(), EntrySafety::Unsafe(reason)),
        };
        if safety == EntrySafety::Safe {
            if path.components().count() > self.limits.max_depth {
                return Err(self.limit(LimitKind::Depth));
            }
            if path.ancestors().skip(1).any(|a| self.symlinks.contains(a)) {
                safety = EntrySafety::Unsafe(UnsafeReason::ThroughSymlink);
            }
        }
        let mut link_target = raw.link_target;
        if safety == EntrySafety::Safe {
            let checked = match (raw.kind, link_target.as_deref()) {
                (ArchiveEntryKind::Symlink, Some(t)) => resolve_link_target(&path, t).map(|_| None),
                (ArchiveEntryKind::Hardlink, Some(t)) => {
                    resolve_link_target(Path::new(""), t).map(Some)
                }
                (ArchiveEntryKind::Symlink | ArchiveEntryKind::Hardlink, None) => {
                    Err(UnsafeReason::LinkEscape)
                }
                _ => Ok(None),
            };
            match checked {
                Ok(Some(normalized)) => link_target = Some(normalized),
                Ok(None) => {}
                Err(reason) => safety = EntrySafety::Unsafe(reason),
            }
        }
        if safety == EntrySafety::Safe && raw.kind == ArchiveEntryKind::Symlink {
            self.symlinks.insert(path.clone());
        }
        let id = EntryId(self.entries.len() as u32);
        if let EntrySafety::Unsafe(_) = safety {
            self.warnings.unsafe_entries += 1;
        } else if raw.kind == ArchiveEntryKind::File {
            self.warnings.total_bytes = self.warnings.total_bytes.saturating_add(raw.size);
            if let Some(packed) = raw.packed_size
                && raw.size >= self.limits.ratio_min_size
                && raw.size / packed.max(1) > self.limits.max_ratio
            {
                self.warnings.suspicious_ratio_entries += 1;
            }
        }
        self.entries.push(ArchiveEntry {
            id,
            path,
            kind: raw.kind,
            size: if raw.kind == ArchiveEntryKind::Dir {
                0
            } else {
                raw.size
            },
            packed_size: raw.packed_size,
            mtime_ns: raw.mtime_ns.unwrap_or(0),
            mode: raw
                .mode
                .map_or_else(|| default_mode(raw.kind), |m| m & 0o7777),
            uid: raw.uid.unwrap_or(0),
            gid: raw.gid.unwrap_or(0),
            link_target,
            encrypted: raw.encrypted,
            synthetic: false,
            safety,
        });
        Ok(Some(id))
    }

    /// Build the lookup tables: last entry wins for a duplicated path, missing parent directories
    /// are synthesized, and an entry below a non-directory entry is flagged `InvalidName`.
    /// Synthetic directories count toward `max_entries` just like stored entries.
    pub fn finish(mut self) -> Result<ArchiveIndex, ArchiveError> {
        let mut by_path: HashMap<PathBuf, EntryId> = HashMap::new();
        for e in self.entries.iter().filter(|e| e.is_safe()) {
            by_path.insert(e.path.clone(), e.id);
        }
        // Children of non-directories (including later-defined symlinks) are unsafe.
        let mut bad = Vec::new();
        for (path, id) in &by_path {
            let blocked = path.ancestors().skip(1).any(|a| {
                by_path
                    .get(a)
                    .is_some_and(|p| self.entries[p.0 as usize].kind != ArchiveEntryKind::Dir)
            });
            if blocked {
                bad.push((path.clone(), *id));
            }
        }
        for (path, id) in bad {
            by_path.remove(&path);
            self.entries[id.0 as usize].safety = EntrySafety::Unsafe(UnsafeReason::InvalidName);
            self.warnings.unsafe_entries += 1;
        }
        // Synthesize missing ancestors (deterministic order: by id of the first child).
        let mut winners: Vec<(EntryId, PathBuf)> =
            by_path.iter().map(|(p, i)| (*i, p.clone())).collect();
        winners.sort();
        for (_, path) in &winners {
            for a in path.ancestors().skip(1) {
                if a.as_os_str().is_empty() || by_path.contains_key(a) {
                    continue;
                }
                if self.entries.len() >= self.limits.max_entries {
                    return Err(self.limit(LimitKind::EntryCount));
                }
                let id = EntryId(self.entries.len() as u32);
                let mut e = synthetic_dir(id, a);
                e.mtime_ns = 0;
                self.entries.push(e);
                by_path.insert(a.to_owned(), id);
            }
        }
        let mut children: HashMap<PathBuf, Vec<EntryId>> = HashMap::new();
        for (path, id) in &by_path {
            let parent = path.parent().map(Path::to_owned).unwrap_or_default();
            children.entry(parent).or_default().push(*id);
        }
        for v in children.values_mut() {
            v.sort();
        }
        let mut warnings = self.warnings;
        warnings.total_size_exceeds_warn = warnings.total_bytes > self.limits.warn_total_bytes;
        Ok(ArchiveIndex {
            archive: self.archive,
            format: self.format,
            entries: self.entries,
            by_path,
            children,
            warnings,
        })
    }
}

fn synthetic_dir(id: EntryId, path: &Path) -> ArchiveEntry {
    ArchiveEntry {
        id,
        path: path.to_owned(),
        kind: ArchiveEntryKind::Dir,
        size: 0,
        packed_size: None,
        mtime_ns: 0,
        mode: 0o755,
        uid: 0,
        gid: 0,
        link_target: None,
        encrypted: false,
        synthetic: true,
        safety: EntrySafety::Safe,
    }
}

fn default_mode(kind: ArchiveEntryKind) -> u32 {
    match kind {
        ArchiveEntryKind::Dir => 0o755,
        ArchiveEntryKind::Symlink => 0o777,
        _ => 0o644,
    }
}

/// Flat entry list of one archive plus a directory tree lookup. Immutable once built; share it
/// behind an `Arc`.
#[derive(Debug)]
pub struct ArchiveIndex {
    archive: PathBuf,
    format: ArchiveFormat,
    entries: Vec<ArchiveEntry>,
    /// Safe entries by normalized path; the last stored entry of a duplicated path wins.
    by_path: HashMap<PathBuf, EntryId>,
    /// Directory path ("" = root) -> ids of its direct children (winners only), ascending.
    children: HashMap<PathBuf, Vec<EntryId>>,
    warnings: IndexWarnings,
}

impl ArchiveIndex {
    /// The archive file this index describes.
    pub fn archive(&self) -> &Path {
        &self.archive
    }

    /// Format.
    pub fn format(&self) -> ArchiveFormat {
        self.format
    }

    /// All entries in archive order (synthetic directories last), including unsafe and shadowed
    /// duplicates. Panels use [`list_dir`](Self::list_dir); extraction uses
    /// [`select`](Self::select).
    pub fn entries(&self) -> &[ArchiveEntry] {
        &self.entries
    }

    /// Entry by id.
    pub fn get(&self, id: EntryId) -> Option<&ArchiveEntry> {
        self.entries.get(id.0 as usize)
    }

    /// The (winning, safe) entry at normalized path `path`; `None` for the root and unknown paths.
    pub fn lookup(&self, path: &Path) -> Option<&ArchiveEntry> {
        self.by_path.get(path).and_then(|id| self.get(*id))
    }

    /// Entries flagged unsafe (for the "N unsafe entries skipped" message).
    pub fn unsafe_entries(&self) -> impl Iterator<Item = &ArchiveEntry> {
        self.entries.iter().filter(|e| !e.is_safe())
    }

    /// Aggregated warnings.
    pub fn warnings(&self) -> IndexWarnings {
        self.warnings
    }

    /// Direct children of directory `dir` (`""` = root), in archive order; `None` if `dir` is not
    /// a directory of this archive. A symlink entry that lexically resolves to a directory inside
    /// the archive is entered (at most 8 hops).
    pub fn list_dir(&self, dir: &Path) -> Option<Vec<&ArchiveEntry>> {
        let mut dir = dir.to_owned();
        for _ in 0..=MAX_SYMLINK_HOPS {
            if dir.as_os_str().is_empty() {
                break;
            }
            let e = self.lookup(&dir)?;
            match e.kind {
                ArchiveEntryKind::Dir => break,
                ArchiveEntryKind::Symlink => {
                    dir = resolve_link_target(&e.path, e.link_target.as_deref()?).ok()?;
                }
                _ => return None,
            }
        }
        if !dir.as_os_str().is_empty() && self.lookup(&dir)?.kind != ArchiveEntryKind::Dir {
            return None;
        }
        let ids = self.children.get(&dir).map(Vec::as_slice).unwrap_or(&[]);
        Some(ids.iter().filter_map(|i| self.get(*i)).collect())
    }

    /// Ids to extract for the user's selection: each path (`""` = everything) plus everything
    /// below it. Safe winners only, ascending id = archive order, no duplicates. Unknown paths are
    /// ignored (the caller compares counts if it cares).
    pub fn select(&self, paths: &[PathBuf]) -> Vec<EntryId> {
        let mut out: Vec<EntryId> = self
            .by_path
            .iter()
            .filter(|(p, _)| paths.iter().any(|sel| p.starts_with(sel)))
            .map(|(_, id)| *id)
            .collect();
        out.sort();
        out
    }

    /// Panel row for an entry: name = last component; symlinks report the kind of what they point
    /// at inside the archive (`None` target = broken/outside); `real_path` is always `None`.
    pub fn to_entry(&self, e: &ArchiveEntry) -> Entry {
        let kind = match e.kind {
            ArchiveEntryKind::Dir => EntryKind::Dir,
            ArchiveEntryKind::File | ArchiveEntryKind::Hardlink => EntryKind::File,
            ArchiveEntryKind::Other => EntryKind::Other,
            ArchiveEntryKind::Symlink => EntryKind::Symlink {
                target: e
                    .link_target
                    .as_deref()
                    .and_then(|t| resolve_link_target(&e.path, t).ok())
                    .and_then(|p| self.lookup(&p))
                    .and_then(|t| match t.kind {
                        ArchiveEntryKind::Dir => Some(TargetKind::Dir),
                        ArchiveEntryKind::File | ArchiveEntryKind::Hardlink => {
                            Some(TargetKind::File)
                        }
                        ArchiveEntryKind::Other => Some(TargetKind::Other),
                        ArchiveEntryKind::Symlink => None,
                    }),
            },
        };
        let type_bits = match e.kind {
            ArchiveEntryKind::Dir => S_IFDIR,
            ArchiveEntryKind::Symlink => S_IFLNK,
            _ => S_IFREG,
        };
        Entry {
            name: e.path.file_name().unwrap_or_default().to_owned(),
            kind,
            size: e.size,
            mtime_ns: e.mtime_ns,
            mode: type_bits | e.mode,
            uid: e.uid,
            gid: e.gid,
            real_path: None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn builder() -> IndexBuilder {
        IndexBuilder::new("/a.zip", ArchiveFormat::Zip, ArchiveLimits::default())
    }

    fn file(path: &str, size: u64) -> RawEntry {
        let mut e = RawEntry::new(path, ArchiveEntryKind::File);
        e.size = size;
        e
    }

    fn link(path: &str, target: &str) -> RawEntry {
        let mut e = RawEntry::new(path, ArchiveEntryKind::Symlink);
        e.link_target = Some(target.into());
        e
    }

    fn names(v: Option<Vec<&ArchiveEntry>>) -> Vec<String> {
        v.unwrap_or_default()
            .iter()
            .map(|e| e.path.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn synthesizes_dirs_and_lists_the_tree() {
        let mut b = builder();
        b.push(file("a/b/c.txt", 3)).unwrap();
        b.push(RawEntry::new("./", ArchiveEntryKind::Dir)).unwrap();
        b.push(file("top.txt", 1)).unwrap();
        let ix = b.finish().unwrap();
        assert_eq!(names(ix.list_dir(Path::new(""))), ["top.txt", "a"]);
        assert_eq!(names(ix.list_dir(Path::new("a"))), ["a/b"]);
        assert_eq!(names(ix.list_dir(Path::new("a/b"))), ["a/b/c.txt"]);
        assert!(ix.list_dir(Path::new("top.txt")).is_none());
        assert!(ix.list_dir(Path::new("nope")).is_none());
        assert!(ix.lookup(Path::new("a")).unwrap().synthetic);
        let row = ix.to_entry(ix.lookup(Path::new("a/b/c.txt")).unwrap());
        assert_eq!(row.name, "c.txt");
        assert_eq!(row.mode, 0o100_644);
    }

    #[test]
    fn zip_slip_entries_are_flagged_not_listed() {
        let mut b = builder();
        b.push(file("../evil", 1)).unwrap();
        b.push(file("/etc/passwd", 1)).unwrap();
        b.push(link("l", "/etc")).unwrap();
        b.push(link("up", "../../x")).unwrap();
        b.push(link("ok", "d/f")).unwrap();
        b.push(file("ok/inside", 1)).unwrap(); // through a (safe) symlink
        b.push(file("d/f", 1)).unwrap();
        let ix = b.finish().unwrap();
        assert_eq!(ix.warnings().unsafe_entries, 5);
        assert_eq!(names(ix.list_dir(Path::new(""))), ["ok", "d"]);
        let reasons: Vec<_> = ix
            .unsafe_entries()
            .map(|e| match e.safety {
                EntrySafety::Unsafe(r) => r,
                EntrySafety::Safe => unreachable!(),
            })
            .collect();
        assert_eq!(
            reasons,
            [
                UnsafeReason::ParentTraversal,
                UnsafeReason::AbsolutePath,
                UnsafeReason::LinkEscape,
                UnsafeReason::LinkEscape,
                UnsafeReason::ThroughSymlink
            ]
        );
        assert_eq!(ix.select(&[PathBuf::new()]).len(), 3, "ok, d, d/f");
    }

    #[test]
    fn symlink_defined_after_its_children_blocks_them() {
        let mut b = builder();
        b.push(file("l/x", 1)).unwrap();
        b.push(link("l", "elsewhere")).unwrap();
        let ix = b.finish().unwrap();
        assert_eq!(ix.warnings().unsafe_entries, 1);
        assert!(ix.lookup(Path::new("l/x")).is_none());
    }

    #[test]
    fn later_duplicate_wins_and_select_expands_dirs() {
        let mut b = builder();
        b.push(file("d/f", 1)).unwrap();
        b.push(file("d/f", 2)).unwrap();
        b.push(file("d/g", 3)).unwrap();
        b.push(file("other", 4)).unwrap();
        let ix = b.finish().unwrap();
        assert_eq!(ix.lookup(Path::new("d/f")).unwrap().size, 2);
        let sel = ix.select(&[PathBuf::from("d")]);
        let paths: Vec<_> = sel
            .iter()
            .map(|i| ix.get(*i).unwrap().path.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            paths,
            ["d/f", "d/g", "d"],
            "archive order, synthetic dir last"
        );
    }

    #[test]
    fn symlink_to_dir_is_entered() {
        let mut b = builder();
        b.push(file("real/f", 1)).unwrap();
        b.push(link("alias", "real")).unwrap();
        let ix = b.finish().unwrap();
        assert_eq!(names(ix.list_dir(Path::new("alias"))), ["real/f"]);
        let row = ix.to_entry(ix.lookup(Path::new("alias")).unwrap());
        assert!(row.kind.is_dir_like());
    }

    #[test]
    fn synthetic_directories_count_toward_entry_limit() {
        let limits = ArchiveLimits {
            max_entries: 2,
            ..ArchiveLimits::default()
        };
        let mut b = IndexBuilder::new("/a.zip", ArchiveFormat::Zip, limits);
        b.push(file("a/b/c.txt", 1)).unwrap();
        assert!(matches!(
            b.finish(),
            Err(ArchiveError::LimitExceeded {
                kind: LimitKind::EntryCount,
                ..
            })
        ));

        let mut b = IndexBuilder::new("/a.zip", ArchiveFormat::Zip, limits);
        b.push(file("a/b.txt", 1)).unwrap();
        assert_eq!(b.finish().unwrap().entries().len(), 2);
    }

    #[test]
    fn limits_and_warnings() {
        let limits = ArchiveLimits {
            max_entries: 2,
            max_depth: 2,
            warn_total_bytes: 10,
            ..ArchiveLimits::default()
        };
        let mut b = IndexBuilder::new("/a.zip", ArchiveFormat::Zip, limits);
        b.push(file("a", 8)).unwrap();
        assert!(matches!(
            b.push(file("a/b/c", 1)),
            Err(ArchiveError::LimitExceeded {
                kind: LimitKind::Depth,
                ..
            })
        ));
        b.push(file("b", 8)).unwrap();
        assert!(matches!(
            b.push(file("c", 1)),
            Err(ArchiveError::LimitExceeded {
                kind: LimitKind::EntryCount,
                ..
            })
        ));
        assert!(b.finish().unwrap().warnings().total_size_exceeds_warn);

        let mut b = builder();
        let mut bomb = file("bomb", 10 << 20);
        bomb.packed_size = Some(100);
        b.push(bomb).unwrap();
        assert_eq!(b.finish().unwrap().warnings().suspicious_ratio_entries, 1);
    }
}

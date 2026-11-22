//! Panelized lists (M7, ADR-0009): search results or command output shown in a panel. Frozen by
//! T7.0; the marked parts are implemented by T7.2.
//!
//! A list holds **real local paths**. Its rows are ordinary `Entry`s with `real_path = Some(..)`;
//! the row `name` is the display path (relative to `base` when inside it, else the full path), unique
//! within the list. File operations never see `Location::List`: the app turns the selected rows into
//! `Location::Local(real_path)` (see [`ListStore::real_paths`]) and submits normal copy/move/delete
//! jobs, so F5/F6/F8 act on the real files. Writing *into* the list pseudo-directory itself
//! (mkdir/create "in the list") is impossible: `Caps.write` is `false` and rows have no child
//! locations — the app maps row selections to `Location::Local(real_path)` before any job, so
//! delete/rename/copy of items always runs against the real file through the local provider.
//!
//! File ownership (T7.2): everything below `list/`.

mod stat;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use ofm_core::{CancelToken, Entry, EntryKind, ListId, Location};

use crate::error::VfsError;
use crate::vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs};

const BATCH: usize = 512;

/// The content of one list.
#[derive(Debug, Clone, Default)]
pub struct PanelList {
    /// Title for the panel header ("Find: *.rs", "Panelize: git ls-files").
    pub title: String,
    /// Directory the display names are relative to (search root / command cwd); `None` = full paths.
    pub base: Option<PathBuf>,
    /// Rows; every entry has `real_path = Some(absolute path)`; no two share a `real_path`.
    pub entries: Vec<Entry>,
}

impl PanelList {
    /// List from ready rows (search hits already carry metadata). Rows without `real_path` are
    /// dropped, duplicates (same `real_path`) keep the first, names are set to
    /// [`display_name`](Self::display_name).
    pub fn from_entries(
        title: impl Into<String>,
        base: Option<PathBuf>,
        entries: Vec<Entry>,
    ) -> Self {
        let mut seen = std::collections::HashSet::new();
        let entries = entries
            .into_iter()
            .filter_map(|mut e| {
                let real = e.real_path.clone()?;
                seen.insert(real.clone()).then(|| {
                    e.name = Self::display_name(base.as_deref(), &real);
                    e
                })
            })
            .collect();
        PanelList {
            title: title.into(),
            base,
            entries,
        }
    }

    /// Stat (`lstat`) every path on the calling worker thread and build the list; paths that do
    /// not exist (or cannot be statted) are ignored and counted in the second tuple field.
    /// Duplicates (same path) keep the first occurrence, input order is preserved. Checks
    /// `cancel` between paths.
    pub fn from_paths(
        title: impl Into<String>,
        base: Option<PathBuf>,
        paths: Vec<PathBuf>,
        cancel: &CancelToken,
    ) -> Result<(PanelList, usize), VfsError> {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let mut seen = std::collections::HashSet::with_capacity(paths.len());
        let mut entries = Vec::with_capacity(paths.len());
        let mut ignored = 0usize;
        for path in paths {
            if cancel.is_cancelled() {
                return Err(VfsError::Cancelled);
            }
            if !seen.insert(path.clone()) {
                continue; // duplicate: keep the first occurrence
            }
            let name = Self::display_name(base.as_deref(), &path);
            match stat::stat_path(&path, name) {
                Ok(mut e) => {
                    e.real_path = Some(path);
                    entries.push(e);
                }
                Err(_) => ignored += 1,
            }
        }
        Ok((
            PanelList {
                title: title.into(),
                base,
                entries,
            },
            ignored,
        ))
    }

    /// Display name of `path`: relative to `base` when inside it, else the full path (always
    /// distinct from a relative name because it starts with `/`).
    pub fn display_name(base: Option<&Path>, path: &Path) -> OsString {
        base.and_then(|b| path.strip_prefix(b).ok())
            .filter(|rel| !rel.as_os_str().is_empty())
            .unwrap_or(path)
            .as_os_str()
            .to_owned()
    }
}

/// Shared store of lists; one per app, handed to [`ListVfs`] and to whoever creates lists.
#[derive(Debug, Default)]
pub struct ListStore {
    next: AtomicU64,
    lists: Mutex<HashMap<ListId, PanelList>>,
}

impl ListStore {
    /// Store a new list and return its id.
    pub fn create(&self, list: PanelList) -> ListId {
        let id = ListId(self.next.fetch_add(1, Ordering::Relaxed) + 1);
        if let Ok(mut m) = self.lists.lock() {
            m.insert(id, list);
        }
        id
    }

    /// Snapshot of a list.
    pub fn get(&self, id: ListId) -> Option<PanelList> {
        self.lists.lock().ok()?.get(&id).cloned()
    }

    /// Drop a list (the panel left it).
    pub fn remove(&self, id: ListId) {
        if let Ok(mut m) = self.lists.lock() {
            m.remove(&id);
        }
    }

    /// Append rows to a list (streaming results); duplicates by `real_path` are ignored.
    /// Returns the number of rows added.
    pub fn append(&self, id: ListId, entries: Vec<Entry>) -> Result<usize, VfsError> {
        let mut m = self
            .lists
            .lock()
            .map_err(|_| VfsError::Unsupported("list store poisoned"))?;
        let list = m
            .get_mut(&id)
            .ok_or_else(|| VfsError::NotFound(PathBuf::from(format!("list:{}", id.0))))?;
        let extra = PanelList::from_entries(String::new(), list.base.clone(), entries);
        let known: std::collections::HashSet<_> = list
            .entries
            .iter()
            .filter_map(|e| e.real_path.clone())
            .collect();
        let before = list.entries.len();
        list.entries.extend(
            extra
                .entries
                .into_iter()
                .filter(|e| e.real_path.as_ref().is_some_and(|p| !known.contains(p))),
        );
        Ok(list.entries.len() - before)
    }

    /// Real paths of the rows called `names` (selection -> file operation sources), in the order
    /// given; unknown names are skipped.
    pub fn real_paths(&self, id: ListId, names: &[OsString]) -> Result<Vec<PathBuf>, VfsError> {
        let list = self
            .get(id)
            .ok_or_else(|| VfsError::NotFound(PathBuf::from(format!("list:{}", id.0))))?;
        let by_name: HashMap<&OsString, &PathBuf> = list
            .entries
            .iter()
            .filter_map(|e| Some((&e.name, e.real_path.as_ref()?)))
            .collect();
        Ok(names
            .iter()
            .filter_map(|n| by_name.get(n).map(|p| (*p).clone()))
            .collect())
    }

    /// Real path of the row called `name` (`None` when no row has that name); the single-row
    /// companion of [`real_paths`](Self::real_paths) for the job layer / UI to map a list item
    /// to the real file.
    pub fn real_path(
        &self,
        id: ListId,
        name: &std::ffi::OsStr,
    ) -> Result<Option<PathBuf>, VfsError> {
        let list = self
            .get(id)
            .ok_or_else(|| VfsError::NotFound(PathBuf::from(format!("list:{}", id.0))))?;
        Ok(list
            .entries
            .iter()
            .find(|e| e.name == name)
            .and_then(|e| e.real_path.clone()))
    }

    /// Remove rows whose `real_path` is in `paths` (after a delete/move job); returns how many.
    pub fn forget_paths(&self, id: ListId, paths: &[PathBuf]) -> usize {
        let Ok(mut m) = self.lists.lock() else {
            return 0;
        };
        let Some(list) = m.get_mut(&id) else { return 0 };
        let before = list.entries.len();
        list.entries
            .retain(|e| e.real_path.as_ref().is_none_or(|p| !paths.contains(p)));
        before - list.entries.len()
    }
}

/// Provider for `Location::List`.
#[derive(Debug, Clone)]
pub struct ListVfs {
    store: Arc<ListStore>,
}

impl ListVfs {
    /// Provider over `store`.
    pub fn new(store: Arc<ListStore>) -> Self {
        ListVfs { store }
    }

    fn list_of(&self, loc: &Location) -> Result<PanelList, VfsError> {
        match loc {
            Location::List { id } => self
                .store
                .get(*id)
                .ok_or_else(|| VfsError::NotFound(PathBuf::from(format!("list:{}", id.0)))),
            other => Err(VfsError::NoProvider(other.display_lossy())),
        }
    }
}

impl Vfs for ListVfs {
    /// Emits the rows of the list. Refresh: every `real_path` is re-`lstat`ed first (fresh
    /// sizes/times), rows whose file vanished are dropped — counted in a `tracing::warn!` —
    /// and removed from the store, other stat failures keep the stale row. Checks `cancel`
    /// between batches.
    fn list(&self, loc: &Location, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent)) {
        let list = match self.list_of(loc) {
            Ok(l) => l,
            Err(e) => return sink(ListEvent::Error(e)),
        };
        let mut total = 0;
        let mut vanished: Vec<PathBuf> = Vec::new();
        for chunk in list.entries.chunks(BATCH) {
            if cancel.is_cancelled() {
                return sink(ListEvent::Error(VfsError::Cancelled));
            }
            let mut batch = Vec::with_capacity(chunk.len());
            for e in chunk {
                let Some(real) = e.real_path.as_ref() else {
                    batch.push(e.clone());
                    continue;
                };
                match stat::stat_path(real, e.name.clone()) {
                    Ok(mut fresh) => {
                        fresh.real_path = Some(real.clone());
                        batch.push(fresh);
                    }
                    // Vanished since the list was built: drop the row.
                    Err(VfsError::NotFound(_)) => vanished.push(real.clone()),
                    // Permission etc.: keep the stale row rather than hiding a real file.
                    Err(err) => {
                        tracing::debug!(path = %real.display(), error = %err, "list refresh stat failed; keeping stale row");
                        batch.push(e.clone());
                    }
                }
            }
            total += batch.len();
            if !batch.is_empty() {
                sink(ListEvent::Batch(batch));
            }
        }
        if !vanished.is_empty() {
            tracing::warn!(
                list = %list.title,
                dropped = vanished.len(),
                "list refresh dropped vanished items"
            );
            if let Location::List { id } = loc {
                self.store.forget_paths(*id, &vanished);
            }
        }
        sink(ListEvent::Done { total });
    }

    /// Metadata of the list itself (a directory named after its title).
    fn stat(&self, loc: &Location) -> Result<Entry, VfsError> {
        let list = self.list_of(loc)?;
        let mut e = Entry::new(list.title, EntryKind::Dir);
        e.mode = 0o040_755;
        Ok(e)
    }

    /// A list row has no `Location` of its own: callers resolve rows with
    /// [`ListStore::real_paths`] and read the real file through the local provider.
    fn open_read(&self, _loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        Err(VfsError::Unsupported(
            "open a list row via its real path (ListStore::real_paths)",
        ))
    }

    /// Rows are real files: delete/rename/trash/real paths work; nothing can be created *in* a
    /// list (`write = false`), and there is no watcher (refresh re-stats).
    fn capabilities(&self, _loc: &Location) -> Caps {
        Caps {
            read: true,
            write: false,
            rename: true,
            watch: false,
            trash: true,
            real_paths: true,
        }
    }

    /// A list has no parent; leaving it returns to the location the panel came from (panel history).
    fn parent(&self, _loc: &Location) -> Option<Location> {
        None
    }

    fn space(&self, _loc: &Location) -> Result<SpaceInfo, VfsError> {
        Err(VfsError::Unsupported("space of a list"))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn row(path: impl Into<PathBuf>) -> Entry {
        let mut e = Entry::new("x", EntryKind::File);
        e.real_path = Some(path.into());
        e
    }

    #[test]
    fn rows_get_display_names_and_duplicates_collapse() {
        let l = PanelList::from_entries(
            "Find",
            Some("/src".into()),
            vec![row("/src/a/b.rs"), row("/src/a/b.rs"), row("/etc/hosts")],
        );
        let names: Vec<_> = l.entries.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, ["a/b.rs", "/etc/hosts"]);
        let mut no_real = row("/x");
        no_real.real_path = None;
        assert!(
            PanelList::from_entries("t", None, vec![no_real])
                .entries
                .is_empty()
        );
    }

    #[test]
    fn store_resolves_names_to_real_paths_and_forgets() {
        let tmp = std::env::temp_dir().join(format!("ofm-list-store-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("a"), b"a").unwrap();
        std::fs::write(tmp.join("c"), b"c").unwrap();
        let store = Arc::new(ListStore::default());
        let id = store.create(PanelList::from_entries(
            "Find",
            Some(tmp.clone()),
            vec![row(tmp.join("a")), row("/etc/hosts")],
        ));
        assert_eq!(
            store
                .append(id, vec![row(tmp.join("a")), row(tmp.join("c"))])
                .unwrap(),
            1
        );
        let real = store
            .real_paths(id, &["c".into(), "/etc/hosts".into(), "zz".into()])
            .unwrap();
        assert_eq!(real, [tmp.join("c"), PathBuf::from("/etc/hosts")]);
        // Single-row mapping helper: same resolution as the bulk one.
        assert_eq!(
            store.real_path(id, std::ffi::OsStr::new("c")).unwrap(),
            Some(tmp.join("c"))
        );
        assert_eq!(
            store.real_path(id, std::ffi::OsStr::new("zz")).unwrap(),
            None
        );
        assert_eq!(store.forget_paths(id, &[tmp.join("a")]), 1);

        let vfs = ListVfs::new(store.clone());
        let loc = Location::List { id };
        let mut got = Vec::new();
        vfs.list(&loc, &CancelToken::new(), &mut |ev| got.push(ev));
        assert!(matches!(&got[0], ListEvent::Batch(b) if b.len() == 2));
        assert!(matches!(got.last(), Some(ListEvent::Done { total: 2 })));
        assert!(vfs.capabilities(&loc).real_paths && !vfs.capabilities(&loc).write);
        assert!(vfs.parent(&loc).is_none());
        store.remove(id);
        assert!(vfs.stat(&loc).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn from_paths_stats_and_counts_missing() {
        let tmp = std::env::temp_dir().join(format!("ofm-list-from-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("x"), b"123").unwrap();
        std::os::unix::fs::symlink("x", tmp.join("link")).unwrap();
        let (list, ignored) = PanelList::from_paths(
            "t",
            Some(tmp.clone()),
            vec![
                tmp.join("x"),
                tmp.join("x"), // duplicate: dropped, not counted
                tmp.join("missing"),
                tmp.join("link"),
            ],
            &CancelToken::new(),
        )
        .unwrap();
        assert_eq!(ignored, 1);
        assert_eq!(list.entries.len(), 2);
        assert_eq!(list.entries[0].name, std::ffi::OsStr::new("x"));
        assert_eq!(list.entries[0].size, 3);
        assert_eq!(list.entries[0].real_path, Some(tmp.join("x")));
        assert_eq!(
            list.entries[1].kind,
            EntryKind::Symlink {
                target: Some(ofm_core::TargetKind::File)
            }
        );
        // Cancelled before the first path: a Cancelled error, no partial list.
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(matches!(
            PanelList::from_paths("t", None, vec![tmp.join("x")], &cancel),
            Err(VfsError::Cancelled)
        ));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

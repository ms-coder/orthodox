//! The app's panelized lists: the shared `ListStore` plus the things the store does not keep
//! (panel title, row count at creation). Lives in `Services`, so `PanelController` lists a
//! `Location::List` through the ordinary `Vfs` registry and the panel header can show the title
//! without cloning a (possibly million-row) list.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ofm_core::{ListId, Location};
use ofm_vfs::list::{ListStore, ListVfs, PanelList};

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Store of lists with their titles.
#[derive(Debug, Default)]
pub struct ListHub {
    store: Arc<ListStore>,
    titles: Mutex<HashMap<ListId, String>>,
    expected: Mutex<HashMap<ListId, usize>>,
}

impl ListHub {
    /// Empty hub.
    pub fn new() -> Self {
        Self::default()
    }

    /// The underlying store (shared with the provider).
    pub fn store(&self) -> &Arc<ListStore> {
        &self.store
    }

    /// The provider for `LocationKind::List`.
    pub fn provider(&self) -> ListVfs {
        ListVfs::new(Arc::clone(&self.store))
    }

    /// Store `list`, remember its title and row count; returns the location of the new list.
    pub fn create(&self, list: PanelList) -> Location {
        let title = list.title.clone();
        let rows = list.entries.len();
        let id = self.store.create(list);
        lock(&self.titles).insert(id, title);
        lock(&self.expected).insert(id, rows);
        Location::List { id }
    }

    /// Title of a list (cheap; no row cloning).
    pub fn title(&self, id: ListId) -> Option<String> {
        lock(&self.titles).get(&id).cloned()
    }

    /// Row count the list is expected to have (at creation or after the last refresh).
    pub fn expected(&self, id: ListId) -> Option<usize> {
        lock(&self.expected).get(&id).copied()
    }

    /// Record the row count seen by the latest refresh.
    pub fn set_expected(&self, id: ListId, rows: usize) {
        lock(&self.expected).insert(id, rows);
    }

    /// Drop every list whose id is not in `keep` (no panel shows it and no history refers to it).
    /// Returns how many were dropped.
    pub fn retain(&self, keep: &HashSet<ListId>) -> usize {
        let drop_ids: Vec<ListId> = lock(&self.titles)
            .keys()
            .filter(|id| !keep.contains(id))
            .copied()
            .collect();
        for id in &drop_ids {
            self.store.remove(*id);
            lock(&self.titles).remove(id);
            lock(&self.expected).remove(id);
        }
        drop_ids.len()
    }

    /// Number of lists held.
    pub fn len(&self) -> usize {
        lock(&self.titles).len()
    }

    /// True when no list is held.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Text for a panel header / tooltip: the list title for lists, else `Location::display_lossy`.
pub fn location_label(hub: &ListHub, loc: &Location) -> String {
    match loc {
        Location::List { id } => hub
            .title(*id)
            .unwrap_or_else(|| crate::i18n::tr("List").to_owned()),
        other => other.display_lossy(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_core::{Entry, EntryKind};

    use super::*;

    fn list(title: &str, paths: &[&str]) -> PanelList {
        let entries = paths
            .iter()
            .map(|p| {
                let mut e = Entry::new("x", EntryKind::File);
                e.real_path = Some((*p).into());
                e
            })
            .collect();
        PanelList::from_entries(title, None, entries)
    }

    #[test]
    fn titles_counts_and_gc() {
        let hub = ListHub::new();
        let a = hub.create(list("Search results", &["/a", "/b"]));
        let b = hub.create(list("Panelize", &["/c"]));
        let (Location::List { id: ia }, Location::List { id: ib }) = (&a, &b) else {
            panic!("not lists");
        };
        assert_eq!(location_label(&hub, &a), "Search results");
        assert_eq!(hub.expected(*ia), Some(2));
        hub.set_expected(*ia, 1);
        assert_eq!(hub.expected(*ia), Some(1));
        let keep: HashSet<ListId> = [*ib].into();
        assert_eq!(hub.retain(&keep), 1);
        assert!(hub.title(*ia).is_none() && hub.store().get(*ia).is_none());
        assert_eq!(location_label(&hub, &a), "List");
        assert_eq!(hub.len(), 1);
        assert_eq!(location_label(&hub, &Location::Local("/x".into())), "/x");
    }
}

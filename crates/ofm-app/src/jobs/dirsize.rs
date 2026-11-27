//! Calculated folder sizes (`file.calc_size`) survive refreshes: the watcher re-stats changed
//! entries and a reload lists everything again, and both bring back the file system's own size
//! for a folder. The cache remembers `(folder, name) -> (mtime, bytes)` and puts the calculated
//! size back while the folder's mtime is unchanged. A changed mtime means the content changed, so
//! the old number is dropped. Main thread only.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::OsString;
use std::rc::{Rc, Weak};

use ofm_core::{Entry, EntryKind, Location};

use crate::context::AppContext;
use crate::panel::{PanelController, PanelEvent};

/// Cache of calculated folder sizes (state in `ctx.ext`).
#[derive(Default)]
pub struct DirSizeCache {
    sizes: RefCell<HashMap<(Location, OsString), (i128, u64)>>,
    hooked: RefCell<Vec<Weak<PanelController>>>,
}

/// Entries of a listing whose folder size should be restored: directories whose mtime matches
/// the cached one but whose size differs (or whose "size known" mark was lost). Returns copies
/// carrying the cached size.
pub fn restorable<'a>(
    cached: &HashMap<OsString, (i128, u64)>,
    entries: impl Iterator<Item = &'a Entry>,
) -> Vec<Entry> {
    entries
        .filter(|e| e.kind == EntryKind::Dir)
        .filter_map(|e| {
            let (mtime, bytes) = cached.get(&e.name)?;
            (*mtime == e.mtime_ns && (*bytes != e.size || !e.size_known)).then(|| {
                let mut e = e.clone();
                e.size = *bytes;
                e.size_known = true;
                e
            })
        })
        .collect()
}

impl DirSizeCache {
    /// Remember a computed size and make sure `panel` restores it after refreshes.
    pub fn store(
        self: &Rc<Self>,
        panel: &Rc<PanelController>,
        dir: &Location,
        name: OsString,
        mtime_ns: i128,
        bytes: u64,
    ) {
        self.sizes
            .borrow_mut()
            .insert((dir.clone(), name), (mtime_ns, bytes));
        self.hook(panel);
    }

    fn for_dir(&self, dir: &Location) -> HashMap<OsString, (i128, u64)> {
        self.sizes
            .borrow()
            .iter()
            .filter(|((d, _), _)| d == dir)
            .map(|((_, n), v)| (n.clone(), *v))
            .collect()
    }

    /// Listen to `panel` once; every listing or watcher update re-applies the cached sizes.
    fn hook(self: &Rc<Self>, panel: &Rc<PanelController>) {
        {
            let mut hooked = self.hooked.borrow_mut();
            hooked.retain(|w| w.strong_count() > 0);
            if hooked
                .iter()
                .any(|w| w.upgrade().is_some_and(|p| Rc::ptr_eq(&p, panel)))
            {
                return;
            }
            hooked.push(Rc::downgrade(panel));
        }
        let (cache, weak) = (Rc::downgrade(self), Rc::downgrade(panel));
        let queued = Rc::new(Cell::new(false));
        panel.changed.connect(move |ev| {
            if !matches!(ev, PanelEvent::Model(_) | PanelEvent::State) || queued.replace(true) {
                return;
            }
            // Applied from an idle callback: changing the model inside its own notification
            // would re-enter the emission.
            let (cache, weak, queued) = (cache.clone(), weak.clone(), Rc::clone(&queued));
            gtk::glib::idle_add_local_once(move || {
                queued.set(false);
                if let (Some(cache), Some(panel)) = (cache.upgrade(), weak.upgrade()) {
                    cache.reapply(&panel);
                }
            });
        });
    }

    fn reapply(&self, panel: &Rc<PanelController>) {
        let cached = self.for_dir(&panel.location());
        if cached.is_empty() {
            return;
        }
        let changed = {
            let m = panel.model();
            restorable(&cached, (0..m.visible_len()).filter_map(|i| m.visible(i)))
        };
        if !changed.is_empty() {
            panel.mutate_view(|m| m.apply_update(changed, &[]));
        }
    }
}

/// Create the cache.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(DirSizeCache::default()));
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn dir(name: &str, mtime_ns: i128, size: u64) -> Entry {
        let mut e = Entry::new(name, EntryKind::Dir);
        e.mtime_ns = mtime_ns;
        e.size = size;
        e
    }

    #[test]
    fn restores_only_matching_mtime_and_differing_size() {
        let mut cached = HashMap::new();
        cached.insert(OsString::from("a"), (100, 5000));
        cached.insert(OsString::from("b"), (100, 7000));
        cached.insert(OsString::from("c"), (100, 9000));
        let mut restored = dir("c", 100, 9000); // already restored
        restored.size_known = true;
        let listing = [
            dir("a", 100, 4096), // re-stat returned the fs size: restore
            dir("b", 200, 4096), // content changed (new mtime): keep the fresh stat
            restored,
            dir("d", 100, 4096), // never calculated
        ];
        let out = restorable(&cached, listing.iter());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "a");
        assert_eq!(out[0].size, 5000);
        assert!(out[0].size_known);
    }

    #[test]
    fn restores_the_mark_when_the_size_happens_to_match() {
        // The recursive total equals the fs size (rare): the number stays, only the mark
        // comes back.
        let mut cached = HashMap::new();
        cached.insert(OsString::from("a"), (100, 4096));
        let out = restorable(&cached, [dir("a", 100, 4096)].iter());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].size, 4096);
        assert!(out[0].size_known);
    }

    #[test]
    fn files_are_never_touched() {
        let mut cached = HashMap::new();
        cached.insert(OsString::from("f"), (1, 1));
        let mut f = Entry::new("f", EntryKind::File);
        f.mtime_ns = 1;
        f.size = 9;
        assert!(restorable(&cached, [f].iter()).is_empty());
    }
}

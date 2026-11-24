//! `HitModel`: a `gio::ListModel` over the hits of one search, for `gtk::ColumnView`.
//!
//! The hits live in one `Vec` (shared with the panelize worker through an `RwLock`); the model
//! creates its small row objects (`HitRow`, just an index) lazily in `item()`, so a million hits
//! cost one vector, not a million GObjects. Rows are only ever appended, so an index stays valid.
//! `append` emits a single `items-changed` for the whole batch.

use std::sync::{Arc, PoisonError, RwLock};

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ofm_search::Hit;

mod row_imp {
    use std::cell::Cell;

    use super::*;

    #[derive(Default)]
    pub struct HitRow {
        pub index: Cell<u32>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HitRow {
        const NAME: &'static str = "OfmHitRow";
        type Type = super::HitRow;
    }

    impl ObjectImpl for HitRow {}
}

glib::wrapper! {
    /// One row of the results list: the index of a hit in its [`HitModel`].
    pub struct HitRow(ObjectSubclass<row_imp::HitRow>);
}

impl HitRow {
    fn new(index: u32) -> Self {
        let row: HitRow = glib::Object::new();
        row.imp().index.set(index);
        row
    }

    /// Index of the hit this row stands for.
    pub fn index(&self) -> usize {
        self.imp().index.get() as usize
    }
}

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct HitModel {
        pub hits: Arc<RwLock<Vec<Hit>>>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for HitModel {
        const NAME: &'static str = "OfmHitModel";
        type Type = super::HitModel;
        type Interfaces = (gio::ListModel,);
    }

    impl ObjectImpl for HitModel {}

    impl ListModelImpl for HitModel {
        fn item_type(&self) -> glib::Type {
            HitRow::static_type()
        }

        fn n_items(&self) -> u32 {
            let n = self
                .hits
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .len();
            u32::try_from(n).unwrap_or(u32::MAX)
        }

        fn item(&self, position: u32) -> Option<glib::Object> {
            ((position as usize) < self.n_items() as usize).then(|| HitRow::new(position).upcast())
        }
    }
}

glib::wrapper! {
    /// The result list model.
    pub struct HitModel(ObjectSubclass<imp::HitModel>) @implements gio::ListModel;
}

impl Default for HitModel {
    fn default() -> Self {
        Self::new()
    }
}

impl HitModel {
    /// Empty model.
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// The hits, shared with workers (read-only there).
    pub fn shared(&self) -> Arc<RwLock<Vec<Hit>>> {
        Arc::clone(&self.imp().hits)
    }

    /// Number of hits.
    pub fn len(&self) -> usize {
        self.imp()
            .hits
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// True when there are no hits.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Append a batch and tell the views with one `items-changed`.
    pub fn append(&self, batch: Vec<Hit>) {
        if batch.is_empty() {
            return;
        }
        let (old, added) = {
            let mut hits = self
                .imp()
                .hits
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let old = hits.len();
            hits.extend(batch);
            (old, hits.len() - old)
        };
        self.items_changed(
            u32::try_from(old).unwrap_or(u32::MAX),
            0,
            u32::try_from(added).unwrap_or(u32::MAX),
        );
    }

    /// Remove every hit (new search).
    pub fn clear(&self) {
        let old = {
            let mut hits = self
                .imp()
                .hits
                .write()
                .unwrap_or_else(PoisonError::into_inner);
            let old = hits.len();
            *hits = Vec::new();
            old
        };
        if old > 0 {
            self.items_changed(0, u32::try_from(old).unwrap_or(u32::MAX), 0);
        }
    }

    /// Run `f` on hit `index` (`None` when out of range).
    pub fn with_hit<R>(&self, index: usize, f: impl FnOnce(&Hit) -> R) -> Option<R> {
        let hits = self
            .imp()
            .hits
            .read()
            .unwrap_or_else(PoisonError::into_inner);
        hits.get(index).map(f)
    }

    /// Clone of hit `index`.
    pub fn hit(&self, index: usize) -> Option<Hit> {
        self.with_hit(index, Hit::clone)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use ofm_core::{Entry, EntryKind, Location};

    use super::*;

    fn hit(name: &str) -> Hit {
        let p = std::path::PathBuf::from("/t").join(name);
        let mut e = Entry::new(name, EntryKind::File);
        e.real_path = Some(p.clone());
        Hit {
            location: Location::Local(p),
            entry: e,
            content: None,
        }
    }

    #[test]
    fn rows_are_lazy_indexed_and_appends_notify_once() {
        let model = HitModel::new();
        let changes = Rc::new(RefCell::new(Vec::new()));
        let c2 = Rc::clone(&changes);
        model.connect_items_changed(move |_, pos, removed, added| {
            c2.borrow_mut().push((pos, removed, added));
        });
        model.append(Vec::new());
        model.append((0..300).map(|i| hit(&format!("f{i}"))).collect());
        model.append(vec![hit("last")]);
        assert_eq!(*changes.borrow(), [(0, 0, 300), (300, 0, 1)]);
        assert_eq!(model.n_items(), 301);
        let row = model.item(300).unwrap().downcast::<HitRow>().unwrap();
        assert_eq!(row.index(), 300);
        assert_eq!(
            model.with_hit(300, |h| h.entry.name.clone()).unwrap(),
            "last"
        );
        assert!(model.item(301).is_none());
        model.clear();
        assert_eq!(changes.borrow().last(), Some(&(0, 301, 0)));
        assert!(model.is_empty() && model.hit(0).is_none());
    }
}

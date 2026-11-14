//! Shared handle to the persisted `history.toml` (masks, hotlist, ...). Reads happen once at
//! startup; every change is written by a pool worker, never on the GTK main thread.

use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ofm_config::ConfigPaths;
use ofm_config::history::History;
use ofm_vfs::pool::IoPool;

use crate::context::AppContext;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Process-wide history data plus the asynchronous writer. Other modules (command line, copy
/// targets) use the same store through [`HistoryStore::ensure`].
pub struct HistoryStore {
    data: Arc<Mutex<History>>,
    save_lock: Arc<Mutex<()>>,
    paths: ConfigPaths,
    pool: IoPool,
}

impl HistoryStore {
    /// Store over `initial`, writing to `paths` through `pool`.
    pub fn new(paths: ConfigPaths, pool: IoPool, initial: History) -> Self {
        HistoryStore {
            data: Arc::new(Mutex::new(initial)),
            save_lock: Arc::new(Mutex::new(())),
            paths,
            pool,
        }
    }

    /// The store of `ctx`, created (and loaded from disk: startup exception to the no-I/O rule)
    /// on first use.
    pub fn ensure(ctx: &Rc<AppContext>) -> Rc<HistoryStore> {
        if let Some(s) = ctx.ext.get::<HistoryStore>() {
            return s;
        }
        let (history, warnings) = History::load(&ctx.paths);
        for w in &warnings {
            tracing::warn!(file = ?w.file, "{}", w.message);
        }
        let store = Rc::new(HistoryStore::new(
            ctx.paths.clone(),
            ctx.services.pool.clone(),
            history,
        ));
        ctx.ext.insert(Rc::clone(&store));
        store
    }

    /// Copy of the current data.
    pub fn snapshot(&self) -> History {
        lock(&self.data).clone()
    }

    /// Change the data and write it to disk in the background.
    pub fn update(&self, f: impl FnOnce(&mut History)) {
        f(&mut lock(&self.data));
        self.save_async();
    }

    /// Queue a write of the latest data. Writes are serialised and always write the newest
    /// snapshot, so two quick updates can never leave the older one on disk.
    pub fn save_async(&self) {
        let data = Arc::clone(&self.data);
        let save_lock = Arc::clone(&self.save_lock);
        let paths = self.paths.clone();
        self.pool.spawn(move || {
            let _serial = lock(&save_lock);
            let snapshot = lock(&data).clone();
            if let Err(e) = snapshot.save(&paths) {
                tracing::warn!(error = %e, "could not save history");
            }
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::time::{Duration, Instant};

    use ofm_config::history::push_recent;
    use ofm_testkit::tree::TreeSpec;

    use super::*;

    #[test]
    fn update_is_persisted_by_a_worker() {
        let tmp = TreeSpec::new().build_temp().unwrap();
        let paths = ConfigPaths::with_dirs(tmp.path().join("c"), tmp.path().join("s"));
        let store = HistoryStore::new(paths.clone(), IoPool::new(2), History::default());
        store.update(|h| push_recent(&mut h.masks, "*.rs"));
        store.update(|h| push_recent(&mut h.masks, "*.md"));
        let start = Instant::now();
        loop {
            let (h, _) = History::load(&paths);
            if h.masks == ["*.md", "*.rs"] {
                break;
            }
            assert!(start.elapsed() < Duration::from_secs(5), "not saved: {h:?}");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(store.snapshot().masks, ["*.md", "*.rs"]);
    }
}

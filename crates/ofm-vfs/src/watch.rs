//! Directory watcher (owned by T1.2): `notify` (inotify), 100 ms debounce, emits changed names so
//! the panel re-stats only those.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use notify::event::ModifyKind;
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::error::VfsError;

/// More distinct names than this in one debounce window collapse into [`WatchEvent::Rescan`].
const MAX_NAMES: usize = 2048;

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
pub struct WatchHandle {
    /// Receiver for debounced events. Use with `glib::spawn_future_local` in the app.
    pub events: async_channel::Receiver<WatchEvent>,
    /// Dropping the watcher closes the raw channel, which ends the debounce thread.
    _watcher: Box<dyn Send>,
}

impl std::fmt::Debug for WatchHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchHandle").finish_non_exhaustive()
    }
}

/// Accumulates raw events during one debounce window.
#[derive(Default)]
struct Pending {
    names: BTreeSet<OsString>,
    rescan: bool,
    gone: bool,
}

impl Pending {
    fn apply(&mut self, dir: &Path, res: notify::Result<Event>) {
        let ev = match res {
            Ok(ev) => ev,
            Err(e) => {
                tracing::debug!(error = %e, "watch error; requesting rescan");
                self.rescan = true;
                return;
            }
        };
        if ev.need_rescan() {
            self.rescan = true;
        }
        if matches!(ev.kind, EventKind::Access(_)) {
            return;
        }
        let dir_removed = matches!(
            ev.kind,
            EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
        );
        for p in &ev.paths {
            if p == dir {
                self.gone |= dir_removed;
            } else if p.parent() == Some(dir)
                && let Some(n) = p.file_name()
            {
                self.names.insert(n.to_owned());
            }
        }
    }

    fn into_event(self) -> Option<WatchEvent> {
        if self.gone {
            Some(WatchEvent::DirGone)
        } else if self.rescan || self.names.len() > MAX_NAMES {
            Some(WatchEvent::Rescan)
        } else if self.names.is_empty() {
            None
        } else {
            Some(WatchEvent::Changed(self.names.into_iter().collect()))
        }
    }
}

fn debounce_loop(
    dir: &Path,
    raw: &Receiver<notify::Result<Event>>,
    out: &async_channel::Sender<WatchEvent>,
    debounce: Duration,
) {
    // Under a constant event flood, still report at least this often.
    let max_wait = debounce.saturating_mul(10).max(Duration::from_millis(500));
    loop {
        let Ok(first) = raw.recv() else { return };
        let mut pending = Pending::default();
        pending.apply(dir, first);
        let start = Instant::now();
        let mut closed = false;
        while start.elapsed() < max_wait {
            match raw.recv_timeout(debounce) {
                Ok(m) => pending.apply(dir, m),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => {
                    closed = true;
                    break;
                }
            }
        }
        if let Some(ev) = pending.into_event()
            && out.send_blocking(ev).is_err()
        {
            return; // receiver dropped
        }
        if closed {
            return;
        }
    }
}

fn map_notify(path: &Path, e: notify::Error) -> VfsError {
    match e.kind {
        notify::ErrorKind::Io(io) => VfsError::from_io(path, io),
        notify::ErrorKind::PathNotFound => VfsError::NotFound(path.to_path_buf()),
        _ => VfsError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::other(e.to_string()),
        },
    }
}

/// Start watching a local directory (non-recursive). Default debounce: 100 ms.
pub fn watch_dir(dir: &Path, debounce: Duration) -> Result<WatchHandle, VfsError> {
    let dir: PathBuf = dir.to_path_buf();
    let (raw_tx, raw_rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        // The debounce thread may already be gone during shutdown.
        let _ = raw_tx.send(res);
    })
    .map_err(|e| map_notify(&dir, e))?;
    watcher
        .watch(&dir, RecursiveMode::NonRecursive)
        .map_err(|e| map_notify(&dir, e))?;

    let (tx, rx) = async_channel::unbounded();
    let thread_dir = dir.clone();
    std::thread::Builder::new()
        .name("ofm-watch".into())
        .spawn(move || debounce_loop(&thread_dir, &raw_rx, &tx, debounce))
        .map_err(|source| VfsError::Io { path: dir, source })?;
    Ok(WatchHandle {
        events: rx,
        _watcher: Box::new(watcher),
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const DEB: Duration = Duration::from_millis(100);
    const WAIT: Duration = Duration::from_secs(5);

    /// Collect events until `pred` accepts the accumulated list or the timeout passes.
    fn collect_until(h: &WatchHandle, pred: impl Fn(&[WatchEvent]) -> bool) -> Vec<WatchEvent> {
        let mut got = Vec::new();
        let end = Instant::now() + WAIT;
        while Instant::now() < end && !pred(&got) {
            if let Ok(ev) = h.events.try_recv() {
                got.push(ev);
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        got
    }

    fn names(evs: &[WatchEvent]) -> BTreeSet<OsString> {
        evs.iter()
            .filter_map(|e| match e {
                WatchEvent::Changed(n) => Some(n.clone()),
                _ => None,
            })
            .flatten()
            .collect()
    }

    #[test]
    fn create_modify_remove_report_names() {
        let t = tempfile::tempdir().unwrap();
        let h = watch_dir(t.path(), DEB).unwrap();
        let f = t.path().join("a.txt");
        std::fs::write(&f, b"1").unwrap();
        let evs = collect_until(&h, |e| {
            names(e).contains(OsString::from("a.txt").as_os_str())
        });
        assert!(
            names(&evs).contains(OsString::from("a.txt").as_os_str()),
            "{evs:?}"
        );

        std::fs::write(&f, b"22").unwrap();
        let evs = collect_until(&h, |e| !e.is_empty());
        assert!(
            names(&evs).contains(OsString::from("a.txt").as_os_str()),
            "{evs:?}"
        );

        std::fs::remove_file(&f).unwrap();
        let evs = collect_until(&h, |e| !e.is_empty());
        assert!(
            names(&evs).contains(OsString::from("a.txt").as_os_str()),
            "{evs:?}"
        );
    }

    #[test]
    fn burst_is_debounced_into_few_events() {
        let t = tempfile::tempdir().unwrap();
        let h = watch_dir(t.path(), Duration::from_millis(200)).unwrap();
        for i in 0..20 {
            std::fs::write(t.path().join(format!("f{i}")), b"x").unwrap();
        }
        let evs = collect_until(&h, |e| names(e).len() >= 20);
        assert_eq!(names(&evs).len(), 20, "{evs:?}");
        assert!(
            evs.len() <= 2,
            "expected coalescing, got {} events",
            evs.len()
        );
    }

    #[test]
    fn rename_reports_both_names() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("old"), b"x").unwrap();
        let h = watch_dir(t.path(), DEB).unwrap();
        std::fs::rename(t.path().join("old"), t.path().join("new")).unwrap();
        let evs = collect_until(&h, |e| names(e).len() >= 2);
        let n = names(&evs);
        assert!(
            n.contains(OsString::from("old").as_os_str())
                && n.contains(OsString::from("new").as_os_str()),
            "{evs:?}"
        );
    }

    #[test]
    fn too_many_names_become_rescan() {
        let t = tempfile::tempdir().unwrap();
        let h = watch_dir(t.path(), Duration::from_millis(500)).unwrap();
        for i in 0..(MAX_NAMES + 200) {
            std::fs::write(t.path().join(format!("f{i}")), b"").unwrap();
        }
        let evs = collect_until(&h, |e| e.contains(&WatchEvent::Rescan));
        assert!(evs.contains(&WatchEvent::Rescan), "{} events", evs.len());
    }

    #[test]
    fn removing_the_directory_reports_dir_gone() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path().join("w");
        std::fs::create_dir(&d).unwrap();
        let h = watch_dir(&d, DEB).unwrap();
        std::fs::remove_dir(&d).unwrap();
        let evs = collect_until(&h, |e| e.contains(&WatchEvent::DirGone));
        assert!(evs.contains(&WatchEvent::DirGone), "{evs:?}");
    }

    #[test]
    fn missing_dir_is_an_error_and_drop_stops_thread() {
        let t = tempfile::tempdir().unwrap();
        assert!(matches!(
            watch_dir(&t.path().join("nope"), DEB),
            Err(VfsError::NotFound(_))
        ));
        let h = watch_dir(t.path(), DEB).unwrap();
        let rx = h.events.clone();
        drop(h);
        // The debounce thread exits and drops its sender, closing the channel.
        let end = Instant::now() + WAIT;
        while !rx.is_closed() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(rx.is_closed());
    }
}

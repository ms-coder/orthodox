//! Live reload of theme files (F30) via `notify`.

use std::path::Path;
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use crate::error::ConfigError;

/// Quiet period after the last file-system event before a [`ThemesChanged`] is sent.
const DEBOUNCE: Duration = Duration::from_millis(200);

/// A theme directory changed (debounced 200 ms); the app re-runs `Theme::discover`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemesChanged;

/// Keeps the watch alive; dropping stops it.
#[derive(Debug)]
pub struct ThemeWatcher {
    /// Events; consume with `glib::spawn_future_local` in the app.
    pub events: async_channel::Receiver<ThemesChanged>,
    // Held only for their Drop: the OS watch, and (for a missing directory) the sender so that
    // `events` stays open and silent instead of reporting a closed channel.
    _watcher: Option<std::sync::Arc<Mutex<RecommendedWatcher>>>,
    _keepalive: async_channel::Sender<ThemesChanged>,
}

/// Lock a mutex, recovering from poisoning (the guard data is still valid; our critical
/// sections cannot panic while holding it).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Watch `themes_dir` for changes. The nearest existing ancestor — normally the config
/// directory — is watched for the directory's whole lifetime so that its creation *and*
/// deletion are noticed, and the directory itself is watched whenever it exists; the OS watch
/// on a directory dies with it, so a deleted and re-created themes directory is re-watched
/// (T8.8, M9). Events fire only for paths at or below the themes directory, never for
/// unrelated files next to it.
pub fn watch_themes(themes_dir: &Path) -> Result<ThemeWatcher, ConfigError> {
    let (tx, events) = async_channel::unbounded();
    let (raw_tx, raw_rx) = mpsc::channel::<()>();
    let dir_filter = themes_dir.to_path_buf();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !ev.kind.is_access()
            && ev.paths.iter().any(|p| p.starts_with(&dir_filter))
        {
            let _ = raw_tx.send(());
        }
    })
    .map_err(|e| notify_err(themes_dir, e))?;
    let watcher = std::sync::Arc::new(Mutex::new(watcher));

    // The directory itself, when it exists. Failing to watch an existing themes directory is
    // the one hard error: live reload would silently never work.
    let mut watching_dir = false;
    if themes_dir.is_dir() {
        lock(&watcher)
            .watch(themes_dir, RecursiveMode::NonRecursive)
            .map_err(|e| notify_err(themes_dir, e))?;
        watching_dir = true;
    }
    // The nearest existing ancestor, best-effort (no existing ancestor at all: stay unwatched
    // but alive). It is never unwatched: creations and deletions of the themes directory are
    // reported through it.
    if let Some(ancestor) = themes_dir.ancestors().skip(1).find(|a| a.is_dir()) {
        let _ = lock(&watcher).watch(ancestor, RecursiveMode::NonRecursive);
    }

    let out = tx.clone();
    let dir_to_watch = themes_dir.to_path_buf();
    let thread_watcher = std::sync::Arc::clone(&watcher);
    std::thread::Builder::new()
        .name("ofm-theme-watch".into())
        .spawn(move || {
            // Ends when the watcher (and with it `raw_tx`) is dropped or the receiver is gone.
            while raw_rx.recv().is_ok() {
                loop {
                    match raw_rx.recv_timeout(DEBOUNCE) {
                        Ok(()) => continue,
                        Err(mpsc::RecvTimeoutError::Timeout) => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    }
                }
                // Sync the watch with reality: the watch on a directory dies with the
                // directory, so re-add it when the themes directory (re)appears and drop the
                // dead watch when it goes away. The ancestor watch reports both transitions.
                let exists = dir_to_watch.is_dir();
                let mut w = lock(&thread_watcher);
                if exists && !watching_dir {
                    if w.watch(&dir_to_watch, RecursiveMode::NonRecursive).is_ok() {
                        watching_dir = true;
                    }
                } else if !exists && watching_dir {
                    let _ = w.unwatch(&dir_to_watch);
                    watching_dir = false;
                }
                drop(w);
                if out.send_blocking(ThemesChanged).is_err() {
                    return;
                }
            }
        })
        .map_err(|source| ConfigError::Io {
            path: themes_dir.to_path_buf(),
            source,
        })?;
    Ok(ThemeWatcher {
        events,
        _watcher: Some(watcher),
        _keepalive: tx,
    })
}

fn notify_err(dir: &Path, e: notify::Error) -> ConfigError {
    ConfigError::Io {
        path: dir.to_path_buf(),
        source: std::io::Error::other(e),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// `async_channel` has no timed blocking receive; poll instead.
    fn recv_timeout(
        chan: &async_channel::Receiver<ThemesChanged>,
        d: Duration,
    ) -> Result<ThemesChanged, async_channel::TryRecvError> {
        let deadline = std::time::Instant::now() + d;
        loop {
            match chan.try_recv() {
                Ok(ev) => return Ok(ev),
                Err(async_channel::TryRecvError::Closed) => {
                    return Err(async_channel::TryRecvError::Closed);
                }
                Err(async_channel::TryRecvError::Empty) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(e),
            }
        }
    }

    #[test]
    fn missing_dir_is_not_watched_but_ok() {
        let d = tempfile::tempdir().unwrap();
        let w = watch_themes(&d.path().join("nope")).unwrap();
        assert!(w.events.try_recv().is_err());
        assert!(!w.events.is_closed());
    }

    #[test]
    fn change_produces_one_debounced_event() {
        let d = tempfile::tempdir().unwrap();
        let w = watch_themes(d.path()).unwrap();
        for i in 0..5 {
            std::fs::write(d.path().join("t.toml"), format!("id = \"t{i}\"")).unwrap();
        }
        let got = w.events.recv_blocking();
        assert_eq!(got, Ok(ThemesChanged));
        // The burst collapsed into one event.
        std::thread::sleep(Duration::from_millis(500));
        assert!(w.events.try_recv().is_err());
    }

    #[test]
    fn themes_dir_created_later_is_noticed_and_then_watched() {
        let d = tempfile::tempdir().unwrap();
        let config_dir = d.path().join("ofm");
        std::fs::create_dir(&config_dir).unwrap();
        let themes = config_dir.join("themes");
        let w = watch_themes(&themes).unwrap();
        // Unrelated files in the config dir do not produce events.
        std::fs::write(config_dir.join("config.toml"), "version = 1").unwrap();
        assert!(recv_timeout(&w.events, Duration::from_millis(600)).is_err());
        // Creating the watched directory is noticed...
        std::fs::create_dir(&themes).unwrap();
        let got = recv_timeout(&w.events, Duration::from_secs(5));
        assert_eq!(got, Ok(ThemesChanged), "themes dir creation not noticed");
        // ...and from then on files dropped into it are noticed too (the watch was switched
        // over before the event above was sent).
        std::fs::write(themes.join("mine.toml"), "id = \"mine\"").unwrap();
        let got = recv_timeout(&w.events, Duration::from_secs(5));
        assert_eq!(
            got,
            Ok(ThemesChanged),
            "theme file after switch not noticed"
        );
    }

    #[test]
    fn themes_dir_deleted_and_recreated_is_watched_again() {
        let d = tempfile::tempdir().unwrap();
        let themes = d.path().join("themes");
        std::fs::create_dir(&themes).unwrap();
        let w = watch_themes(&themes).unwrap();
        // Baseline: a change inside the existing directory is noticed.
        std::fs::write(themes.join("a.toml"), "id = \"a\"").unwrap();
        assert_eq!(
            recv_timeout(&w.events, Duration::from_secs(5)),
            Ok(ThemesChanged)
        );
        // Deleting the directory is noticed (its own OS watch dies with it, the ancestor
        // watch reports the removal) ...
        std::fs::remove_dir_all(&themes).unwrap();
        assert_eq!(
            recv_timeout(&w.events, Duration::from_secs(5)),
            Ok(ThemesChanged),
            "removal not noticed"
        );
        // ... and a re-created directory is watched again: files dropped into it fire events.
        std::fs::create_dir(&themes).unwrap();
        assert_eq!(
            recv_timeout(&w.events, Duration::from_secs(5)),
            Ok(ThemesChanged),
            "recreation not noticed"
        );
        std::fs::write(themes.join("b.toml"), "id = \"b\"").unwrap();
        assert_eq!(
            recv_timeout(&w.events, Duration::from_secs(5)),
            Ok(ThemesChanged),
            "file in re-created dir not noticed"
        );
    }
}

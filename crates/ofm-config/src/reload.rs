//! Live reload of theme files (F30) via `notify`.

use std::path::{Path, PathBuf};
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

/// Watch `themes_dir` for changes. When the directory does not exist yet (fresh profile, or the
/// user has not dropped any theme files), the nearest existing ancestor — normally the config
/// directory — is watched instead so the directory's *creation* is noticed; the watch then
/// switches to the directory itself (T8.8). Events fire only for real changes of the themes
/// directory, never for unrelated files next to it.
pub fn watch_themes(themes_dir: &Path) -> Result<ThemeWatcher, ConfigError> {
    let (tx, events) = async_channel::unbounded();
    let (raw_tx, raw_rx) = mpsc::channel::<()>();
    let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !ev.kind.is_access()
        {
            let _ = raw_tx.send(());
        }
    })
    .map_err(|e| notify_err(themes_dir, e))?;
    let watcher = std::sync::Arc::new(Mutex::new(watcher));

    // Where the watch starts: the directory itself, or an ancestor while waiting for it to be
    // created. No existing ancestor (config home missing entirely): stay unwatched but alive.
    let initial: Option<PathBuf> = if themes_dir.is_dir() {
        Some(themes_dir.to_path_buf())
    } else {
        themes_dir
            .ancestors()
            .skip(1)
            .find(|a| a.is_dir())
            .map(Path::to_path_buf)
    };
    let mut watching_dir = false;
    if let Some(dir) = &initial {
        lock(&watcher)
            .watch(dir, RecursiveMode::NonRecursive)
            .map_err(|e| notify_err(themes_dir, e))?;
        watching_dir = dir == themes_dir;
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
                if !watching_dir && dir_to_watch.is_dir() {
                    // The directory appeared: switch the watch from the ancestor to it. The
                    // ancestor unwatch is best-effort (it may already be gone).
                    let mut w = lock(&thread_watcher);
                    if w.watch(&dir_to_watch, RecursiveMode::NonRecursive).is_ok() {
                        if let Some(ancestor) = initial.as_ref() {
                            let _ = w.unwatch(ancestor);
                        }
                        watching_dir = true;
                    }
                }
                if watching_dir && out.send_blocking(ThemesChanged).is_err() {
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
}

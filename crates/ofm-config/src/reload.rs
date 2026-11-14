//! Live reload of theme files (F30) via `notify`.

use std::path::Path;
use std::sync::mpsc;
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
    _watcher: Option<RecommendedWatcher>,
    _keepalive: async_channel::Sender<ThemesChanged>,
}

/// Watch `themes_dir` (created if missing is NOT done here: a missing dir is simply not watched).
pub fn watch_themes(themes_dir: &Path) -> Result<ThemeWatcher, ConfigError> {
    let (tx, events) = async_channel::unbounded();
    if !themes_dir.is_dir() {
        return Ok(ThemeWatcher {
            events,
            _watcher: None,
            _keepalive: tx,
        });
    }
    let (raw_tx, raw_rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && !ev.kind.is_access()
        {
            let _ = raw_tx.send(());
        }
    })
    .map_err(|e| notify_err(themes_dir, e))?;
    watcher
        .watch(themes_dir, RecursiveMode::NonRecursive)
        .map_err(|e| notify_err(themes_dir, e))?;

    let out = tx.clone();
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
}

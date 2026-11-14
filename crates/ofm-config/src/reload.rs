//! Live reload of theme files (F30) via `notify`.

use std::path::Path;

use crate::error::ConfigError;

/// A theme directory changed (debounced 200 ms); the app re-runs `Theme::discover`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemesChanged;

/// Keeps the watch alive; dropping stops it.
#[derive(Debug)]
pub struct ThemeWatcher {
    /// Events; consume with `glib::spawn_future_local` in the app.
    pub events: async_channel::Receiver<ThemesChanged>,
}

/// Watch `themes_dir` (created if missing is NOT done here: a missing dir is simply not watched).
pub fn watch_themes(_themes_dir: &Path) -> Result<ThemeWatcher, ConfigError> {
    todo!("T1.3")
}

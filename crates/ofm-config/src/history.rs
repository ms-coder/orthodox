//! Persistent histories in `$XDG_STATE_HOME/ofm/history.toml`: command line, masks, hotlist,
//! per-panel directory history is session-only (kept in `session.toml` tabs later if needed).

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Warning};
use crate::io;
use crate::paths::ConfigPaths;
use crate::session::{decode_os, encode_os};

/// A hotlist/bookmark entry (F07).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Bookmark {
    /// Display title.
    pub title: String,
    /// Directory (UTF-8 form).
    pub path: Option<String>,
    /// Directory (raw bytes, for non-UTF-8 paths).
    pub path_bytes: Option<Vec<u8>>,
}

/// All persisted histories (newest first, bounded by the writer).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct History {
    /// Schema version (1).
    pub version: u32,
    /// Command line history.
    pub commands: Vec<String>,
    /// Selection mask history (F10 dialog).
    pub masks: Vec<String>,
    /// Copy/move target history.
    pub targets: Vec<String>,
    /// Hotlist (kept in order, user editable).
    pub hotlist: Vec<Bookmark>,
}

impl Default for History {
    fn default() -> Self {
        History {
            version: 1,
            commands: Vec::new(),
            masks: Vec::new(),
            targets: Vec::new(),
            hotlist: Vec::new(),
        }
    }
}

impl Bookmark {
    /// Set the directory (lossless).
    pub fn set_path(&mut self, p: &std::path::Path) {
        (self.path, self.path_bytes) = encode_os(p.as_os_str());
    }
    /// Get the directory back (lossless).
    pub fn path_buf(&self) -> Option<std::path::PathBuf> {
        decode_os(&self.path, &self.path_bytes).map(std::path::PathBuf::from)
    }
}

/// Upper bound of entries kept per history list on [`History::save`].
pub const MAX_HISTORY: usize = 500;

/// Put `item` at the front of `list` (newest first), removing older duplicates and trimming to
/// [`MAX_HISTORY`]. Empty items are ignored.
pub fn push_recent(list: &mut Vec<String>, item: &str) {
    if item.is_empty() {
        return;
    }
    list.retain(|x| x != item);
    list.insert(0, item.to_owned());
    list.truncate(MAX_HISTORY);
}

impl History {
    /// Load; missing or invalid file -> default (+ warning if invalid).
    pub fn load(paths: &ConfigPaths) -> (History, Vec<Warning>) {
        io::load_toml(&paths.history_file())
    }
    /// Save atomically with mode `0600` (command lines may contain secrets); lists are trimmed
    /// to [`MAX_HISTORY`].
    pub fn save(&self, paths: &ConfigPaths) -> Result<(), ConfigError> {
        let mut h = self.clone();
        for l in [&mut h.commands, &mut h.masks, &mut h.targets] {
            l.truncate(MAX_HISTORY);
        }
        io::save_toml_private(&paths.history_file(), &h)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn round_trip_with_non_utf8_bookmark() {
        let d = tempfile::tempdir().unwrap();
        let p = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        assert_eq!(History::load(&p).0, History::default());
        let mut b = Bookmark {
            title: "odd".into(),
            ..Bookmark::default()
        };
        let raw = std::path::PathBuf::from(std::ffi::OsString::from_vec(b"/x/\xff".to_vec()));
        b.set_path(&raw);
        let mut ok = Bookmark::default();
        ok.set_path(std::path::Path::new("/home"));
        let h = History {
            commands: vec!["ls -l".into(), "make".into()],
            masks: vec!["*.rs".into()],
            targets: vec!["/tmp".into()],
            hotlist: vec![ok, b.clone()],
            ..History::default()
        };
        h.save(&p).unwrap();
        let (back, w) = History::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, h);
        assert_eq!(back.hotlist[1].path_buf(), Some(raw));
    }

    #[test]
    fn history_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        let h = History {
            commands: vec!["echo secret".into()],
            ..History::default()
        };
        h.save(&p).unwrap();
        let mode = std::fs::metadata(p.history_file())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn invalid_file_gives_default_and_warning() {
        let d = tempfile::tempdir().unwrap();
        let p = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        io::atomic_write(&p.history_file(), "commands = 3").unwrap();
        let (h, w) = History::load(&p);
        assert_eq!(h, History::default());
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn push_recent_dedups_and_trims() {
        let mut l = vec!["a".to_owned(), "b".to_owned()];
        push_recent(&mut l, "b");
        push_recent(&mut l, "");
        assert_eq!(l, ["b", "a"]);
        for i in 0..MAX_HISTORY + 10 {
            push_recent(&mut l, &i.to_string());
        }
        assert_eq!(l.len(), MAX_HISTORY);
    }
}

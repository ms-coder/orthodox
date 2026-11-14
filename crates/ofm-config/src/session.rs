//! Session state (F33) in `$XDG_STATE_HOME/ofm/session.toml`: stored data only, no behaviour.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Warning};
use crate::io;
use crate::paths::ConfigPaths;

/// Whole session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    /// Schema version (1).
    pub version: u32,
    /// Window size and state.
    pub window: WindowState,
    /// Split ratio of the two panels (0.0..1.0).
    pub split_ratio: f32,
    /// One-panel mode (the other panel hidden).
    pub one_panel: bool,
    /// 0 = left, 1 = right.
    pub active_panel: u8,
    /// Left and right panel (index = side).
    pub panels: Vec<PanelSession>,
}

/// Window geometry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct WindowState {
    /// Width in px.
    pub width: i32,
    /// Height in px.
    pub height: i32,
    /// Maximized.
    pub maximized: bool,
}

/// One panel: its tabs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PanelSession {
    /// Index of the active tab.
    pub active_tab: usize,
    /// Tabs.
    pub tabs: Vec<TabSession>,
}

/// One tab. Paths are stored as byte arrays-safe TOML: `path` is a UTF-8 string when possible,
/// otherwise `path_bytes` holds the raw bytes (paths are bytes, not Strings).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TabSession {
    /// Directory (UTF-8 form).
    pub path: Option<String>,
    /// Directory (raw bytes, for non-UTF-8 paths).
    pub path_bytes: Option<Vec<u8>>,
    /// `brief | full | custom`.
    pub view: String,
    /// `name | ext | mtime | size`.
    pub sort: String,
    /// Reverse sort.
    pub sort_reverse: bool,
    /// Directories first.
    pub dirs_first: bool,
    /// Show hidden files.
    pub show_hidden: bool,
    /// Tab is locked.
    pub locked: bool,
    /// File name under the cursor (UTF-8 form).
    pub cursor_name: Option<String>,
    /// File name under the cursor (raw bytes).
    pub cursor_name_bytes: Option<Vec<u8>>,
}

/// Split an `OsStr` into the stored pair: UTF-8 text when valid, raw bytes otherwise.
pub fn encode_os(s: &OsStr) -> (Option<String>, Option<Vec<u8>>) {
    match s.to_str() {
        Some(t) => (Some(t.to_owned()), None),
        None => (None, Some(s.as_bytes().to_vec())),
    }
}

/// Inverse of [`encode_os`]; the UTF-8 form wins when both are present.
pub fn decode_os(text: &Option<String>, bytes: &Option<Vec<u8>>) -> Option<OsString> {
    match (text, bytes) {
        (Some(t), _) => Some(OsString::from(t)),
        (None, Some(b)) => Some(OsString::from_vec(b.clone())),
        (None, None) => None,
    }
}

impl TabSession {
    /// Set `path`/`path_bytes` from a `PathBuf` (lossless).
    pub fn set_path(&mut self, p: &std::path::Path) {
        (self.path, self.path_bytes) = encode_os(p.as_os_str());
    }
    /// Get the path back (lossless).
    pub fn path_buf(&self) -> Option<PathBuf> {
        decode_os(&self.path, &self.path_bytes).map(PathBuf::from)
    }
    /// Set the cursor file name (lossless).
    pub fn set_cursor_name(&mut self, name: &OsStr) {
        (self.cursor_name, self.cursor_name_bytes) = encode_os(name);
    }
    /// Get the cursor file name back (lossless).
    pub fn cursor_name_os(&self) -> Option<OsString> {
        decode_os(&self.cursor_name, &self.cursor_name_bytes)
    }
}

impl Default for Session {
    fn default() -> Self {
        Session {
            version: 1,
            window: WindowState::default(),
            split_ratio: 0.5,
            one_panel: false,
            active_panel: 0,
            panels: Vec::new(),
        }
    }
}

impl Session {
    /// Load; missing or invalid file -> default + warning when invalid.
    pub fn load(paths: &ConfigPaths) -> (Session, Vec<Warning>) {
        let (mut s, w): (Session, _) = io::load_toml(&paths.session_file());
        // Keep the geometry sane: a hand-edited or corrupt ratio must not collapse a panel.
        if !(0.05..=0.95).contains(&s.split_ratio) {
            s.split_ratio = 0.5;
        }
        (s, w)
    }
    /// Save atomically.
    pub fn save(&self, paths: &ConfigPaths) -> Result<(), ConfigError> {
        io::save_toml(&paths.session_file(), self)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    fn paths(d: &tempfile::TempDir) -> ConfigPaths {
        ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"))
    }

    #[test]
    fn tab_paths_are_lossless_including_non_utf8() {
        let mut t = TabSession::default();
        t.set_path(std::path::Path::new("/home/\u{e4}/x"));
        assert_eq!(t.path.as_deref(), Some("/home/\u{e4}/x"));
        assert!(t.path_bytes.is_none());
        let raw = PathBuf::from(OsString::from_vec(b"/tmp/a\xffb".to_vec()));
        t.set_path(&raw);
        assert!(t.path.is_none());
        assert_eq!(t.path_buf(), Some(raw));
        t.set_cursor_name(OsStr::from_bytes(b"\xfe\xff"));
        assert_eq!(t.cursor_name_os().unwrap().as_bytes(), b"\xfe\xff");
        assert_eq!(TabSession::default().path_buf(), None);
    }

    #[test]
    fn session_round_trips_through_file() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let (s, w) = Session::load(&p);
        assert_eq!(s, Session::default());
        assert!(w.is_empty());

        let mut tab = TabSession {
            view: "full".into(),
            sort: "mtime".into(),
            sort_reverse: true,
            locked: true,
            ..TabSession::default()
        };
        tab.set_path(std::path::Path::new("/srv"));
        let mut weird = TabSession::default();
        weird.set_path(&PathBuf::from(OsString::from_vec(vec![b'/', 0xc3, 0x28])));
        weird.set_cursor_name(OsStr::from_bytes(&[0xff]));
        let s = Session {
            window: WindowState {
                width: 1200,
                height: 700,
                maximized: true,
            },
            split_ratio: 0.3,
            one_panel: true,
            active_panel: 1,
            panels: vec![
                PanelSession {
                    active_tab: 1,
                    tabs: vec![tab, weird],
                },
                PanelSession::default(),
            ],
            ..Session::default()
        };
        s.save(&p).unwrap();
        let (back, w) = Session::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(back, s);
    }

    #[test]
    fn invalid_session_gives_default_and_warning() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        io::atomic_write(&p.session_file(), "window = 5").unwrap();
        let (s, w) = Session::load(&p);
        assert_eq!(s, Session::default());
        assert_eq!(w.len(), 1);
        io::atomic_write(&p.session_file(), "split_ratio = 7.0\nfuture = true").unwrap();
        let (s, w) = Session::load(&p);
        assert!(w.is_empty());
        assert_eq!(s.split_ratio, 0.5);
    }
}

//! Session state (F33) in `$XDG_STATE_HOME/ofm/session.toml`: stored data only, no behaviour.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Warning};
use crate::paths::ConfigPaths;

/// Whole session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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

impl TabSession {
    /// Set `path`/`path_bytes` from a `PathBuf` (lossless).
    pub fn set_path(&mut self, _p: &std::path::Path) {
        todo!("T1.3")
    }
    /// Get the path back (lossless).
    pub fn path_buf(&self) -> Option<PathBuf> {
        todo!("T1.3")
    }
}

impl Session {
    /// Load; missing or invalid file -> default + warning when invalid.
    pub fn load(_paths: &ConfigPaths) -> (Session, Vec<Warning>) {
        todo!("T1.3")
    }
    /// Save atomically.
    pub fn save(&self, _paths: &ConfigPaths) -> Result<(), ConfigError> {
        todo!("T1.3")
    }
}

//! Persistent histories in `$XDG_STATE_HOME/ofm/history.toml`: command line, masks, hotlist,
//! per-panel directory history is session-only (kept in `session.toml` tabs later if needed).

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Warning};
use crate::paths::ConfigPaths;

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
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

impl History {
    /// Load; missing or invalid file -> default (+ warning if invalid).
    pub fn load(_paths: &ConfigPaths) -> (History, Vec<Warning>) {
        todo!("T1.3")
    }
    /// Save atomically.
    pub fn save(&self, _paths: &ConfigPaths) -> Result<(), ConfigError> {
        todo!("T1.3")
    }
}

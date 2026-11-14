//! User menu schema (F28): global `menu.toml` and per-directory `.ofm-menu.toml`.

use serde::{Deserialize, Serialize};

use crate::error::Warning;
use std::path::Path;

/// Whole menu file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct UserMenu {
    /// Schema version (1).
    pub version: u32,
    /// Items in display order.
    pub item: Vec<MenuItem>,
}

/// One menu item; placeholder expansion lives in the app/core later (M8).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MenuItem {
    /// Hotkey inside the menu.
    pub key: String,
    /// Title.
    pub title: String,
    /// Command with `%f %s %d %D %F %n %{input:Prompt}` placeholders.
    pub command: String,
    /// Only show when the cursor file matches this mask (empty = always).
    pub mask: String,
    /// Run in the embedded terminal.
    pub terminal: bool,
}

impl UserMenu {
    /// Load one menu file; missing file = empty menu; invalid = empty + warning.
    pub fn load(file: &Path) -> (UserMenu, Vec<Warning>) {
        crate::io::load_toml(file)
    }
}

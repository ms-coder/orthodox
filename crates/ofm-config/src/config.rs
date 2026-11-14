//! `config.toml` schema (see `defaults/config.toml` for the commented reference).

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Warning};
use crate::paths::ConfigPaths;

/// Root of `config.toml`. Every field has a default so partial user files work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version (1).
    pub version: u32,
    /// `[general]`.
    pub general: General,
    /// `[keymap]`.
    pub keymap: KeymapSection,
    /// `[theme]`.
    pub theme: ThemeSection,
    /// `[panels]`.
    pub panels: PanelsSection,
    /// `[jobs]`.
    pub jobs: JobsSection,
    /// `[editor]`.
    pub editor: EditorSection,
    /// `[terminal]`.
    pub terminal: TerminalSection,
    /// `[viewer]`.
    pub viewer: ViewerSection,
}

/// `[general]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct General {
    /// Ask before quitting with running jobs.
    pub confirm_quit_with_jobs: bool,
    /// Ask before delete.
    pub confirm_delete: bool,
    /// Ask on overwrite.
    pub confirm_overwrite: bool,
    /// One-panel mode.
    pub one_panel_mode: bool,
}

/// `[keymap]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KeymapSection {
    /// `far` | `tc` | `mc`.
    pub preset: String,
    /// `""` | `command_line` | `quick_search`.
    pub typing: String,
}

/// `[theme]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeSection {
    /// Theme id.
    pub name: String,
    /// Watch the themes directory.
    pub live_reload: bool,
}

/// `[panels]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PanelsSection {
    /// `brief` | `full` | `custom`.
    pub default_view: String,
    /// Directories first.
    pub dirs_first: bool,
    /// Show hidden files.
    pub show_hidden: bool,
    /// `name` | `ext` | `mtime` | `size`.
    pub sort: String,
    /// Columns of the custom view.
    pub custom_columns: Vec<String>,
    /// `binary` | `si` | `bytes`.
    pub size_format: String,
    /// strftime-style format.
    pub time_format: String,
    /// Filter-as-you-type quick search.
    pub filter_as_you_type: bool,
}

/// `[jobs]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobsSection {
    /// `queued` | `parallel`.
    pub default_run_mode: String,
    /// fsync after every copied file.
    pub fsync_after_copy: bool,
    /// Follow symlinks when copying.
    pub follow_symlinks: bool,
    /// Delete goes to trash.
    pub trash: bool,
}

/// `[editor]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorSection {
    /// Command template (`%f`); empty = $VISUAL/$EDITOR/GNOME Text Editor.
    pub command: String,
    /// Editors that run inside the embedded terminal.
    pub terminal_editors: Vec<String>,
    /// Per-mask overrides, first match wins.
    pub rule: Vec<EditorRule>,
}

/// `[[editor.rule]]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct EditorRule {
    /// File mask (`ofm_core::mask` syntax).
    pub mask: String,
    /// Command template.
    pub command: String,
}

/// `[terminal]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalSection {
    /// Shell; empty = $SHELL.
    pub shell: String,
    /// Font; empty = theme font.
    pub font: String,
    /// Scrollback lines.
    pub scrollback: u32,
}

/// `[viewer]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewerSection {
    /// Encoding name or `auto`.
    pub default_encoding: String,
    /// Wrap lines.
    pub wrap: bool,
}

// `Default` for every section = the values in defaults/config.toml; T1.3 implements them (a test
// asserts `Config::default() == toml::from_str(defaults::CONFIG)`).
macro_rules! todo_default {
    ($($t:ty),*) => {$(
        impl Default for $t {
            fn default() -> Self { todo!("T1.3: defaults from defaults/config.toml") }
        }
    )*};
}
todo_default!(
    Config,
    General,
    KeymapSection,
    ThemeSection,
    PanelsSection,
    JobsSection,
    EditorSection,
    TerminalSection,
    ViewerSection
);

impl Config {
    /// Load `config.toml` over the defaults; invalid file -> defaults + warning.
    pub fn load(_paths: &ConfigPaths) -> (Config, Vec<Warning>) {
        todo!("T1.3")
    }
    /// Save atomically.
    pub fn save(&self, _paths: &ConfigPaths) -> Result<(), ConfigError> {
        todo!("T1.3")
    }
}

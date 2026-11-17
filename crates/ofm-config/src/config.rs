//! `config.toml` schema (see `defaults/config.toml` for the commented reference).

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, Warning};
use crate::io;
use crate::keymap::{Preset, TypingMode};
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
    /// Ask the desktop to hand its shortcuts (GNOME: Alt+F2 run dialog, Alt+F7/F8/F10 window
    /// management, ...) to OFM while its window has the focus. The compositor may ask the user
    /// once; Super+Esc gives the shortcuts back. Needed for the Alt+Fn keys of the far/tc/mc
    /// presets.
    pub inhibit_system_shortcuts: bool,
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
    /// Force how `command` is run: `true` = inside the embedded terminal, `false` = detached GUI
    /// program, unset = decided by `terminal_editors` and the built-in list of terminal editors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
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
    /// Same meaning as `[editor] terminal`, for this rule's `command`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
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
    /// Bring the panels back automatically when a command started from the command line ends
    /// (default: never; Ctrl+O brings them back).
    pub return_to_panels: bool,
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

// `Default` for every section = the values in defaults/config.toml (a test asserts
// `Config::default() == toml::from_str(defaults::CONFIG)`).
impl Default for Config {
    fn default() -> Self {
        Config {
            version: 1,
            general: General::default(),
            keymap: KeymapSection::default(),
            theme: ThemeSection::default(),
            panels: PanelsSection::default(),
            jobs: JobsSection::default(),
            editor: EditorSection::default(),
            terminal: TerminalSection::default(),
            viewer: ViewerSection::default(),
        }
    }
}
impl Default for General {
    fn default() -> Self {
        General {
            confirm_quit_with_jobs: true,
            confirm_delete: true,
            confirm_overwrite: true,
            inhibit_system_shortcuts: true,
            one_panel_mode: false,
        }
    }
}
impl Default for KeymapSection {
    fn default() -> Self {
        KeymapSection {
            preset: "far".into(),
            typing: String::new(),
        }
    }
}
impl Default for ThemeSection {
    fn default() -> Self {
        ThemeSection {
            name: "far-blue".into(),
            live_reload: true,
        }
    }
}
impl Default for PanelsSection {
    fn default() -> Self {
        PanelsSection {
            default_view: "brief".into(),
            dirs_first: true,
            show_hidden: false,
            sort: "name".into(),
            custom_columns: ["name", "size", "mtime", "mode"].map(String::from).to_vec(),
            size_format: "binary".into(),
            time_format: "%Y-%m-%d %H:%M".into(),
            filter_as_you_type: false,
        }
    }
}
impl Default for JobsSection {
    fn default() -> Self {
        JobsSection {
            default_run_mode: "queued".into(),
            fsync_after_copy: false,
            follow_symlinks: false,
            trash: true,
        }
    }
}
impl Default for EditorSection {
    fn default() -> Self {
        EditorSection {
            command: String::new(),
            terminal_editors: ["vim", "nvim", "nano", "mcedit", "micro", "hx"]
                .map(String::from)
                .to_vec(),
            terminal: None,
            rule: vec![EditorRule {
                mask: "*.md;*.txt".into(),
                command: String::new(),
                terminal: None,
            }],
        }
    }
}
impl Default for TerminalSection {
    fn default() -> Self {
        TerminalSection {
            shell: String::new(),
            font: String::new(),
            scrollback: 10000,
            return_to_panels: false,
        }
    }
}
impl Default for ViewerSection {
    fn default() -> Self {
        ViewerSection {
            default_encoding: "auto".into(),
            wrap: false,
        }
    }
}

impl Config {
    /// Load `config.toml` over the defaults; invalid file -> defaults + warning. A `preset` or
    /// `typing` value that is not recognised is reset to its default with a warning.
    pub fn load(paths: &ConfigPaths) -> (Config, Vec<Warning>) {
        let file = paths.config_file();
        let mut warnings = Vec::new();
        let text = match io::read_optional(&file) {
            Ok(Some(t)) => t,
            Ok(None) => return (Config::default(), warnings),
            Err(e) => {
                warnings.push(Warning {
                    file: Some(file),
                    message: format!("{e}; using defaults"),
                });
                return (Config::default(), warnings);
            }
        };
        let mut cfg: Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                warnings.push(Warning {
                    file: Some(file),
                    message: format!("{e}; using defaults"),
                });
                return (Config::default(), warnings);
            }
        };
        if Preset::from_name(&cfg.keymap.preset).is_none() {
            warnings.push(Warning {
                file: Some(file.clone()),
                message: format!("unknown keymap preset {:?}; using far", cfg.keymap.preset),
            });
            cfg.keymap.preset = KeymapSection::default().preset;
        }
        if !cfg.keymap.typing.is_empty() && TypingMode::from_name(&cfg.keymap.typing).is_none() {
            warnings.push(Warning {
                file: Some(file),
                message: format!(
                    "unknown typing mode {:?}; using preset default",
                    cfg.keymap.typing
                ),
            });
            cfg.keymap.typing.clear();
        }
        (cfg, warnings)
    }
    /// Save atomically.
    pub fn save(&self, paths: &ConfigPaths) -> Result<(), ConfigError> {
        let text =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        io::atomic_write(&paths.config_file(), &text)
    }
    /// The selected keymap preset (`far` if the name is unknown).
    pub fn preset(&self) -> Preset {
        Preset::from_name(&self.keymap.preset).unwrap_or(Preset::Far)
    }
    /// The `typing` override, `None` = preset default.
    pub fn typing(&self) -> Option<TypingMode> {
        TypingMode::from_name(&self.keymap.typing)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::defaults;

    fn paths(d: &tempfile::TempDir) -> ConfigPaths {
        ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"))
    }

    #[test]
    fn emacs_is_not_a_default_terminal_editor() {
        // GUI emacs must not be forced into the terminal (`emacs -nw` is detected by flag).
        let d = Config::default();
        assert!(!d.editor.terminal_editors.iter().any(|e| e == "emacs"));
    }

    #[test]
    fn default_equals_shipped_config() {
        let shipped: Config = toml::from_str(defaults::CONFIG).unwrap();
        assert_eq!(Config::default(), shipped);
    }

    #[test]
    fn missing_file_gives_defaults() {
        let d = tempfile::tempdir().unwrap();
        let (c, w) = Config::load(&paths(&d));
        assert_eq!(c, Config::default());
        assert!(w.is_empty());
    }

    #[test]
    fn partial_file_overrides_only_given_keys_and_ignores_unknown() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        io::atomic_write(
            &p.config_file(),
            "future_key = 1\n[keymap]\npreset = \"tc\"\n[panels]\nshow_hidden = true\n[unknown]\nx = 2\n",
        )
        .unwrap();
        let (c, w) = Config::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.preset(), Preset::Tc);
        assert!(c.panels.show_hidden);
        assert!(c.panels.dirs_first);
        assert_eq!(c.theme, ThemeSection::default());
    }

    #[test]
    fn invalid_file_falls_back_with_warning() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        for bad in ["[panels\nx", "[jobs]\ntrash = \"yes\"\n", "scrollback = ]"] {
            io::atomic_write(&p.config_file(), bad).unwrap();
            let (c, w) = Config::load(&p);
            assert_eq!(c, Config::default(), "{bad}");
            assert_eq!(w.len(), 1, "{bad}");
            assert_eq!(w[0].file.as_deref(), Some(p.config_file().as_path()));
        }
    }

    #[test]
    fn unknown_preset_and_typing_warn() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        io::atomic_write(
            &p.config_file(),
            "[keymap]\npreset = \"emacs\"\ntyping = \"x\"\n",
        )
        .unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(w.len(), 2);
        assert_eq!(c.preset(), Preset::Far);
        assert_eq!(c.typing(), None);
    }

    #[test]
    fn save_round_trips() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        let mut c = Config::default();
        c.keymap.preset = "mc".into();
        c.panels.custom_columns = vec!["name".into()];
        c.editor.rule.push(EditorRule {
            mask: "*.rs".into(),
            command: "code %f".into(),
            terminal: Some(false),
        });
        c.terminal.scrollback = 5;
        c.save(&p).unwrap();
        let (back, w) = Config::load(&p);
        assert!(w.is_empty());
        assert_eq!(back, c);
    }
}

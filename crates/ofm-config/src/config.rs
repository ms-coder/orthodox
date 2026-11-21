//! `config.toml` schema (see `defaults/config.toml` for the commented reference).

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{ConfigError, ConfigWarning, Warning};
use crate::io;
use crate::keymap::{Preset, TypingMode};
use crate::paths::ConfigPaths;

/// Known key paths of `config.toml`, for typo detection (F29): anything else in the file is
/// reported as [`ConfigWarning::UnknownKey`] and ignored (forward compatibility). The
/// `known_keys_cover_the_default_schema` test keeps this list in sync with the structs below.
const KNOWN_TOP_LEVEL: &[&str] = &[
    "version", "general", "keymap", "theme", "panels", "jobs", "editor", "terminal", "viewer",
];
/// Known keys per table path (`editor.rule` applies to every `[[editor.rule]]` element).
const KNOWN_KEYS: &[(&str, &[&str])] = &[
    (
        "general",
        &[
            "confirm_quit_with_jobs",
            "confirm_delete",
            "confirm_overwrite",
            "one_panel_mode",
            "inhibit_system_shortcuts",
        ],
    ),
    ("keymap", &["preset", "typing"]),
    ("theme", &["name", "live_reload"]),
    (
        "panels",
        &[
            "default_view",
            "dirs_first",
            "show_hidden",
            "sort",
            "custom_columns",
            "size_format",
            "time_format",
            "filter_as_you_type",
        ],
    ),
    (
        "jobs",
        &[
            "default_run_mode",
            "fsync_after_copy",
            "follow_symlinks",
            "trash",
        ],
    ),
    (
        "editor",
        &["command", "terminal_editors", "terminal", "rule"],
    ),
    ("editor.rule", &["mask", "command", "terminal"]),
    (
        "terminal",
        &["shell", "font", "scrollback", "return_to_panels"],
    ),
    ("viewer", &["default_encoding", "wrap"]),
];

/// Collect keys of a parsed `config.toml` that are not in the schema (probably typos). `raw` is
/// only walked when the whole file already parsed against the schema, so shapes are as expected.
fn unknown_keys(raw: &toml::Value, file: &Path) -> Vec<ConfigWarning> {
    let mut out = Vec::new();
    let Some(table) = raw.as_table() else {
        return out;
    };
    for (key, value) in table {
        if !KNOWN_TOP_LEVEL.contains(&key.as_str()) {
            out.push(ConfigWarning::UnknownKey {
                file: file.to_path_buf(),
                path: key.clone(),
            });
            continue;
        }
        walk_known(key, value, file, &mut out);
    }
    out
}

/// Recurse into a recognised table value; `path` is the dotted path of `value`.
fn walk_known(path: &str, value: &toml::Value, file: &Path, out: &mut Vec<ConfigWarning>) {
    match value {
        toml::Value::Table(table) => {
            let known = KNOWN_KEYS
                .iter()
                .find(|(p, _)| *p == path)
                .map_or(&[][..], |(_, keys)| *keys);
            for (key, value) in table {
                let child = format!("{path}.{key}");
                if !known.contains(&key.as_str()) {
                    out.push(ConfigWarning::UnknownKey {
                        file: file.to_path_buf(),
                        path: child,
                    });
                } else {
                    walk_known(&child, value, file, out);
                }
            }
        }
        toml::Value::Array(items) if path == "editor.rule" => {
            for (i, item) in items.iter().enumerate() {
                if let toml::Value::Table(table) = item {
                    for key in table.keys() {
                        if !["mask", "command", "terminal"].contains(&key.as_str()) {
                            out.push(ConfigWarning::UnknownKey {
                                file: file.to_path_buf(),
                                path: format!("{path}[{i}].{key}"),
                            });
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

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
        let (cfg, warnings) = Config::load_with_warnings(paths);
        (
            cfg,
            warnings.iter().map(ConfigWarning::to_warning).collect(),
        )
    }

    /// Typed form of [`load`](Self::load): every warning is a [`ConfigWarning`] so the UI can
    /// present them (F29), including unknown keys/sections that serde would silently ignore.
    pub fn load_with_warnings(paths: &ConfigPaths) -> (Config, Vec<ConfigWarning>) {
        let file = paths.config_file();
        let text = match io::read_optional(&file) {
            Ok(Some(t)) => t,
            Ok(None) => return (Config::default(), Vec::new()),
            Err(e) => {
                return (
                    Config::default(),
                    vec![ConfigWarning::Unreadable {
                        file,
                        message: format!("{e}; using defaults"),
                    }],
                );
            }
        };
        let mut cfg: Config = match toml::from_str(&text) {
            Ok(c) => c,
            Err(e) => {
                return (
                    Config::default(),
                    vec![ConfigWarning::Invalid {
                        file,
                        message: format!("{e}; using defaults"),
                    }],
                );
            }
        };
        // The file is valid TOML of the right schema: now diff its keys against the schema so
        // typos like `[pannel]` show up instead of being silently ignored.
        let raw: toml::Value = toml::from_str::<toml::Table>(&text)
            .map(toml::Value::Table)
            .unwrap_or(toml::Value::Table(toml::Table::new()));
        let mut warnings = unknown_keys(&raw, &file);
        if Preset::from_name(&cfg.keymap.preset).is_none() {
            warnings.push(ConfigWarning::UnknownPreset {
                file: file.clone(),
                value: cfg.keymap.preset.clone(),
            });
            cfg.keymap.preset = KeymapSection::default().preset;
        }
        if !cfg.keymap.typing.is_empty() && TypingMode::from_name(&cfg.keymap.typing).is_none() {
            warnings.push(ConfigWarning::UnknownTypingMode {
                file: file.clone(),
                value: cfg.keymap.typing.clone(),
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
    /// Persist the theme choice with a minimal in-place edit of `config.toml` (F29): only the
    /// `name` line inside `[theme]` is rewritten — all comments and key order are preserved.
    /// The key is appended to `[theme]` when absent, or the section (and file) created. A file
    /// that does not parse is left untouched and reported as [`ConfigError::Parse`] rather than
    /// clobbered.
    pub fn save_theme_choice(paths: &ConfigPaths, name: &str) -> Result<(), ConfigError> {
        let file = paths.config_file();
        let text = io::read_optional(&file)?.unwrap_or_default();
        let mut doc = text
            .parse::<toml_edit::DocumentMut>()
            .map_err(|e| ConfigError::Parse {
                path: file.clone(),
                message: e.to_string(),
            })?;
        if doc.get("theme").is_none_or(|i| !i.is_table()) {
            doc["theme"] = toml_edit::table();
        }
        let Some(theme) = doc["theme"].as_table_mut() else {
            return Err(ConfigError::Parse {
                path: file.clone(),
                message: "[theme] is not a table".into(),
            });
        };
        match theme.get_mut("name") {
            Some(item) => match item.as_value_mut() {
                // Replace just the value, keeping the item's decor (comments, alignment).
                Some(value) => {
                    let mut new_value = toml_edit::Value::from(name);
                    *new_value.decor_mut() = value.decor().clone();
                    *value = new_value;
                }
                None => *item = toml_edit::value(name),
            },
            None => theme["name"] = toml_edit::value(name),
        }
        io::atomic_write(&file, &doc.to_string())
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
    fn partial_file_overrides_only_given_keys_and_reports_unknown() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        io::atomic_write(
            &p.config_file(),
            "future_key = 1\n[pannel]\nshow_hidden = true\n[keymap]\npreset = \"tc\"\n[panels]\nshow_hidden = true\nsortt = \"size\"\n[unknown]\nx = 2\n",
        )
        .unwrap();
        let (c, w) = Config::load(&p);
        assert_eq!(c.preset(), Preset::Tc);
        assert!(c.panels.show_hidden);
        assert!(c.panels.dirs_first);
        assert_eq!(c.theme, ThemeSection::default());
        // Typo'd keys are reported (F29) but everything still loads (forward compatible).
        let paths: Vec<_> = w.iter().map(|w| w.message.clone()).collect();
        assert_eq!(w.len(), 4, "{paths:?}");
        assert!(
            paths.iter().any(|m| m.contains("\"future_key\"")),
            "{paths:?}"
        );
        assert!(paths.iter().any(|m| m.contains("\"pannel\"")), "{paths:?}");
        assert!(
            paths.iter().any(|m| m.contains("\"panels.sortt\"")),
            "{paths:?}"
        );
        assert!(paths.iter().any(|m| m.contains("\"unknown\"")), "{paths:?}");
        // The typed API carries file and dotted path.
        let (_, cw) = Config::load_with_warnings(&p);
        assert!(
            cw.iter()
                .any(|w| matches!(w, ConfigWarning::UnknownKey { path, .. } if path == "pannel")),
            "{cw:?}"
        );
        assert!(
            cw.iter().any(
                |w| matches!(w, ConfigWarning::UnknownKey { path, .. } if path == "panels.sortt")
            ),
            "{cw:?}"
        );
        for w in &cw {
            assert_eq!(w.file(), p.config_file().as_path());
        }
    }

    #[test]
    fn known_keys_cover_the_default_schema() {
        // Every key the schema can emit must be in KNOWN_*; a new field without a list update
        // fails here.
        let text = toml::to_string_pretty(&Config::default()).unwrap();
        let raw: toml::Value = toml::Value::Table(toml::from_str::<toml::Table>(&text).unwrap());
        assert!(
            unknown_keys(&raw, Path::new("config.toml")).is_empty(),
            "KNOWN_* is out of sync with the schema"
        );
        // Option fields skipped by serialization are listed manually; exercise them too.
        let mut c = Config::default();
        c.editor.terminal = Some(true);
        c.editor.rule[0].terminal = Some(false);
        let text = toml::to_string_pretty(&c).unwrap();
        let raw: toml::Value = toml::Value::Table(toml::from_str::<toml::Table>(&text).unwrap());
        assert!(
            unknown_keys(&raw, Path::new("config.toml")).is_empty(),
            "KNOWN_* is out of sync with the schema (option fields)"
        );
    }

    #[test]
    fn save_theme_choice_edits_in_place_preserving_comments() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        io::atomic_write(
            &p.config_file(),
            "# my config\nversion = 1\n\n[theme]\nname = \"far-blue\" # favourite\nlive_reload = true\n\n[panels]\nshow_hidden = true # keep\n",
        )
        .unwrap();
        Config::save_theme_choice(&p, "adwaita").unwrap();
        let text = io::read_optional(&p.config_file()).unwrap().unwrap();
        assert!(
            text.contains("# my config") && text.contains("# favourite"),
            "{text}"
        );
        assert!(text.contains("name = \"adwaita\" # favourite"), "{text}");
        assert!(text.contains("show_hidden = true # keep"), "{text}");
        // The written file still parses and carries the new theme.
        let (c, w) = Config::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.theme.name, "adwaita");
        assert!(c.panels.show_hidden);
    }

    #[test]
    fn save_theme_choice_appends_key_section_and_file_when_absent() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        // No file at all: create a minimal one.
        Config::save_theme_choice(&p, "far-blue").unwrap();
        let text = io::read_optional(&p.config_file()).unwrap().unwrap();
        let (c, w) = Config::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.theme.name, "far-blue");
        assert!(text.contains("[theme]"), "{text}");
        // File without the key: appended inside the existing [theme] section.
        io::atomic_write(
            &p.config_file(),
            "[theme]\nlive_reload = false\n# trailing\n",
        )
        .unwrap();
        Config::save_theme_choice(&p, "commander-light").unwrap();
        let text = io::read_optional(&p.config_file()).unwrap().unwrap();
        assert!(text.contains("live_reload = false"), "{text}");
        assert!(text.contains("name = \"commander-light\""), "{text}");
        let (c, _) = Config::load(&p);
        assert_eq!(c.theme.name, "commander-light");
        assert!(!c.theme.live_reload);
        // Unparsable file: left untouched, reported.
        io::atomic_write(&p.config_file(), "[theme\n").unwrap();
        assert!(Config::save_theme_choice(&p, "adwaita").is_err());
        assert_eq!(
            io::read_optional(&p.config_file()).unwrap().unwrap(),
            "[theme\n"
        );
    }

    #[test]
    fn save_theme_choice_handles_dotted_and_wrong_typed_theme() {
        let d = tempfile::tempdir().unwrap();
        let p = paths(&d);
        // Dotted key form.
        io::atomic_write(&p.config_file(), "theme.name = \"far-blue\"\n").unwrap();
        Config::save_theme_choice(&p, "adwaita").unwrap();
        let (c, w) = Config::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.theme.name, "adwaita");
        // Wrong type for [theme]: replaced with a table.
        io::atomic_write(&p.config_file(), "theme = \"oops\"\n").unwrap();
        Config::save_theme_choice(&p, "far-blue").unwrap();
        let (c, w) = Config::load(&p);
        assert!(w.is_empty(), "{w:?}");
        assert_eq!(c.theme.name, "far-blue");
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

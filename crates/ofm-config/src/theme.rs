//! Theme schema (ADR-0005); examples in `defaults/themes/`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::defaults;
use crate::error::{ConfigError, Warning};
use crate::paths::ConfigPaths;

/// A colour: `#RRGGBB` or `#RRGGBBAA`, or an `@role` reference (`@accent`, `@dim`, `@error`,
/// `@success`, `@warning`) resolved by the app from the system palette; empty string = unset.
/// Kept as text in the schema so themes round-trip exactly; `parse_rgba` validates.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Color(pub String);

impl Color {
    /// Parse `#RRGGBB[AA]` to RGBA bytes; `None` for unset or `@role` colours.
    pub fn parse_rgba(&self) -> Option<[u8; 4]> {
        let hex = self.0.strip_prefix('#')?;
        if !hex.is_ascii() || !(hex.len() == 6 || hex.len() == 8) {
            return None;
        }
        let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        let a = if hex.len() == 8 { byte(6)? } else { 255 };
        Some([byte(0)?, byte(2)?, byte(4)?, a])
    }

    /// The system-palette role of an `@role` colour (`accent`, `dim`, ...), if it is one.
    pub fn role(&self) -> Option<&str> {
        let r = self.0.strip_prefix('@')?;
        ROLES.contains(&r).then_some(r)
    }

    /// True for unset, valid `#hex` or valid `@role` colours.
    pub fn is_valid(&self) -> bool {
        self.0.is_empty() || self.parse_rgba().is_some() || self.role().is_some()
    }
}

/// Valid `@role` names.
const ROLES: &[&str] = &["accent", "dim", "error", "success", "warning"];

/// Where the theme gets its colours.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Base {
    /// All roles come from the file.
    #[default]
    Fixed,
    /// Unspecified roles follow GNOME (light/dark, accent).
    Adwaita,
}

/// Panel frame drawing style (`[retro] frames`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Frames {
    /// No frame.
    #[default]
    None,
    /// 1px line.
    Hairline,
    /// Box-drawing single line.
    Single,
    /// Box-drawing double line.
    Double,
}

/// A whole theme file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Theme {
    /// Schema version (1).
    pub version: u32,
    /// Stable id (file stem for user themes).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Colour source.
    pub base: Base,
    /// `[font]`.
    pub font: FontSpec,
    /// `[retro]`.
    pub retro: Retro,
    /// `[colors.*]`.
    pub colors: Colors,
    /// `[[highlight]]`, evaluated in order, first match wins.
    pub highlight: Vec<HighlightRule>,
}

/// `[font]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FontSpec {
    /// Family; `monospace` or `system` or a name.
    pub family: String,
    /// Points; 0 = default.
    pub size: f32,
}

/// `[retro]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Retro {
    /// Snap to a character grid.
    pub monospace_grid: bool,
    /// Frame style.
    pub frames: Frames,
    /// Rounded corners.
    pub rounded_corners: bool,
    /// Animations.
    pub animations: bool,
}

/// All colour roles grouped like the TOML tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Colors {
    /// `[colors.panel]`.
    pub panel: PanelColors,
    /// `[colors.cursor]`.
    pub cursor: CursorColors,
    /// `[colors.marked]`.
    pub marked: MarkedColors,
    /// `[colors.status]`.
    pub status: FgBg,
    /// `[colors.cmdline]`.
    pub cmdline: FgBg,
    /// `[colors.fkey]`.
    pub fkey: FKeyColors,
    /// `[colors.viewer]`.
    pub viewer: ViewerColors,
    /// `[colors.dialog]`.
    pub dialog: DialogColors,
    /// `[colors.progress]`.
    pub progress: ProgressColors,
    /// `[colors.error]`.
    pub error: FgBg,
}

/// Foreground/background pair.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FgBg {
    /// Foreground.
    pub fg: Color,
    /// Background.
    pub bg: Color,
}

/// `[colors.panel]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct PanelColors {
    /// Background.
    pub bg: Color,
    /// Alternate row background.
    pub bg_alt: Color,
    /// Regular file text.
    pub fg: Color,
    /// Directory text.
    pub dir_fg: Color,
    /// Directories bold.
    pub dir_bold: bool,
    /// Frame.
    pub frame: Color,
    /// Header background.
    pub header_bg: Color,
    /// Header text.
    pub header_fg: Color,
    /// Sort indicator.
    pub sort_indicator: Color,
}

/// `[colors.cursor]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct CursorColors {
    /// Active panel cursor bar.
    pub active_bg: Color,
    /// Active panel cursor text.
    pub active_fg: Color,
    /// Inactive panel cursor fill (empty = outline only).
    pub inactive_bg: Color,
    /// Inactive panel cursor text.
    pub inactive_fg: Color,
    /// Inactive panel cursor outline.
    pub inactive_outline: Color,
}

/// `[colors.marked]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct MarkedColors {
    /// Text of marked files.
    pub fg: Color,
    /// Bold.
    pub bold: bool,
}

/// `[colors.fkey]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct FKeyColors {
    /// Digit.
    pub digit_fg: Color,
    /// Label text.
    pub label_fg: Color,
    /// Label background.
    pub label_bg: Color,
    /// Hover background.
    pub hover_bg: Color,
}

/// `[colors.viewer]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ViewerColors {
    /// Background.
    pub bg: Color,
    /// Text.
    pub fg: Color,
    /// Selection background.
    pub selection_bg: Color,
    /// Selection text.
    pub selection_fg: Color,
    /// Hex offset column.
    pub hex_offset_fg: Color,
    /// Search match background.
    pub match_bg: Color,
    /// Search match text.
    pub match_fg: Color,
}

/// `[colors.dialog]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DialogColors {
    /// Background.
    pub bg: Color,
    /// Text.
    pub fg: Color,
    /// Border.
    pub border: Color,
    /// Button background.
    pub button_bg: Color,
    /// Button text.
    pub button_fg: Color,
    /// Focused button background.
    pub button_focus_bg: Color,
    /// Focused button text.
    pub button_focus_fg: Color,
}

/// `[colors.progress]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct ProgressColors {
    /// Bar.
    pub bar: Color,
    /// Trough.
    pub trough: Color,
}

/// `[[highlight]]`: matches by `attr` and/or `mask`; both given = both must match.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct HighlightRule {
    /// `dir | hidden | executable | symlink | broken_symlink | archive | image | source`, or empty.
    pub attr: String,
    /// `;`-separated glob list, or empty.
    pub mask: String,
    /// Foreground.
    pub fg: Option<Color>,
    /// Background.
    pub bg: Option<Color>,
    /// Bold.
    pub bold: Option<bool>,
    /// Icon-theme name drawn before the file name (never a path). A theme in which any rule names
    /// one draws an icon column; rows whose rule names none get a generic icon.
    pub icon: Option<String>,
}

/// Built-in theme ids, in menu order.
pub const BUILTIN_THEMES: &[&str] = &["far-blue", "commander-light", "adwaita"];

impl Theme {
    /// Parse a theme file.
    pub fn from_toml(text: &str, origin: &str) -> Result<Theme, ConfigError> {
        toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: PathBuf::from(origin),
            message: e.to_string(),
        })
    }
    /// Built-in theme by id (always valid; a test enforces it).
    pub fn builtin(id: &str) -> Option<Theme> {
        let (origin, text) = match id {
            "far-blue" => ("<builtin far-blue.toml>", defaults::THEME_FAR_BLUE),
            "commander-light" => (
                "<builtin commander-light.toml>",
                defaults::THEME_COMMANDER_LIGHT,
            ),
            "adwaita" => ("<builtin adwaita.toml>", defaults::THEME_ADWAITA),
            _ => return None,
        };
        Theme::from_toml(text, origin).ok()
    }
    /// Colour fields (`colors.*`, highlight `fg`/`bg`) that are neither unset, `#RRGGBB[AA]` nor
    /// a known `@role`, as `path = value` strings. Invalid colours behave as unset.
    pub fn invalid_colors(&self) -> Vec<String> {
        let Ok(value) = toml::Value::try_from(self) else {
            return Vec::new();
        };
        let mut bad = Vec::new();
        fn walk(prefix: &str, v: &toml::Value, bad: &mut Vec<String>) {
            match v {
                toml::Value::String(s) => {
                    if !Color(s.clone()).is_valid() {
                        bad.push(format!("{prefix} = {s:?}"));
                    }
                }
                toml::Value::Table(t) => {
                    for (k, v) in t {
                        walk(&format!("{prefix}.{k}"), v, bad);
                    }
                }
                _ => {}
            }
        }
        if let Some(c) = value.get("colors") {
            walk("colors", c, &mut bad);
        }
        if let Some(toml::Value::Array(rules)) = value.get("highlight") {
            for (i, r) in rules.iter().enumerate() {
                for key in ["fg", "bg"] {
                    if let Some(v) = r.get(key) {
                        walk(&format!("highlight[{i}].{key}"), v, &mut bad);
                    }
                }
            }
        }
        bad
    }
    /// Highlight `icon` values that are set but are not icon names (see
    /// [`crate::highlight::is_icon_name`]), as `path = value` strings. They behave as unset.
    /// Whether a well-formed name exists in the icon theme can only be checked by the app.
    pub fn invalid_icons(&self) -> Vec<String> {
        self.highlight
            .iter()
            .enumerate()
            .filter_map(|(i, r)| {
                let icon = r.icon.as_deref().filter(|n| !n.is_empty())?;
                (!crate::highlight::is_icon_name(icon))
                    .then(|| format!("highlight[{i}].icon = {icon:?}"))
            })
            .collect()
    }
    /// The distinct valid icon names of the highlight rules, in rule order.
    pub fn icon_names(&self) -> Vec<&str> {
        let mut names = Vec::new();
        for name in self
            .highlight
            .iter()
            .filter_map(crate::highlight::rule_icon)
        {
            if !names.contains(&name) {
                names.push(name);
            }
        }
        names
    }
    /// Built-in + user themes found in `paths.themes_dir()`; broken files become warnings.
    ///
    /// Built-ins come first in [`BUILTIN_THEMES`] order. A user theme with the same id replaces the
    /// built-in in place; other user themes follow, sorted by file name. The id is the `id` key of
    /// the file, or the file stem when absent.
    pub fn discover(paths: &ConfigPaths) -> (Vec<Theme>, Vec<Warning>) {
        let mut themes: Vec<Theme> = BUILTIN_THEMES
            .iter()
            .filter_map(|id| Theme::builtin(id))
            .collect();
        let mut warnings = Vec::new();
        let dir = paths.themes_dir();
        let mut files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(Result::ok)
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|x| x == "toml"))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                warnings.push(Warning {
                    file: Some(dir.clone()),
                    message: format!("cannot read themes directory: {e}"),
                });
                Vec::new()
            }
        };
        files.sort();
        for file in files {
            let text = match crate::io::read_optional(&file) {
                Ok(Some(t)) => t,
                Ok(None) => continue,
                Err(e) => {
                    warnings.push(Warning {
                        file: Some(file),
                        message: e.to_string(),
                    });
                    continue;
                }
            };
            let mut theme = match Theme::from_toml(&text, &file.to_string_lossy()) {
                Ok(t) => t,
                Err(e) => {
                    warnings.push(Warning {
                        file: Some(file),
                        message: format!("{e}; theme skipped"),
                    });
                    continue;
                }
            };
            if theme.id.is_empty() {
                theme.id = file
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
            }
            if theme.id.is_empty() {
                continue;
            }
            if theme.name.is_empty() {
                theme.name = theme.id.clone();
            }
            for bad in theme.invalid_colors() {
                warnings.push(Warning {
                    file: Some(file.clone()),
                    message: format!("invalid colour {bad}"),
                });
            }
            for bad in theme.invalid_icons() {
                warnings.push(Warning {
                    file: Some(file.clone()),
                    message: format!("invalid icon name {bad}"),
                });
            }
            match themes.iter_mut().find(|t| t.id == theme.id) {
                Some(slot) => *slot = theme,
                None => themes.push(theme),
            }
        }
        (themes, warnings)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn color_parsing() {
        let c = |s: &str| Color(s.to_owned());
        assert_eq!(c("#102030").parse_rgba(), Some([0x10, 0x20, 0x30, 255]));
        assert_eq!(c("#10203040").parse_rgba(), Some([0x10, 0x20, 0x30, 0x40]));
        assert_eq!(c("#abcdef").parse_rgba(), Some([0xab, 0xcd, 0xef, 255]));
        for bad in [
            "",
            "102030",
            "#12345",
            "#gggggg",
            "@accent",
            "#\u{e4}\u{e4}\u{e4}",
        ] {
            assert_eq!(c(bad).parse_rgba(), None, "{bad}");
        }
        assert_eq!(c("@dim").role(), Some("dim"));
        assert!(c("").is_valid() && c("@accent").is_valid() && c("#000000").is_valid());
        assert!(!c("@nope").is_valid() && !c("red").is_valid());
    }

    #[test]
    fn builtins_are_valid_and_clean() {
        for id in BUILTIN_THEMES {
            let t = Theme::builtin(id).unwrap_or_else(|| panic!("{id}"));
            assert_eq!(t.id, *id);
            assert!(
                t.invalid_colors().is_empty(),
                "{id}: {:?}",
                t.invalid_colors()
            );
            assert!(
                t.invalid_icons().is_empty(),
                "{id}: {:?}",
                t.invalid_icons()
            );
        }
        assert!(Theme::builtin("nope").is_none());
        let far = Theme::builtin("far-blue").unwrap();
        assert!(!far.highlight.is_empty());
        assert_eq!(far.base, Base::Fixed);
        assert_eq!(Theme::builtin("adwaita").unwrap().base, Base::Adwaita);
    }

    #[test]
    fn theme_round_trips_through_toml() {
        for id in BUILTIN_THEMES {
            let t = Theme::builtin(id).unwrap();
            let text = toml::to_string_pretty(&t).unwrap();
            assert_eq!(Theme::from_toml(&text, "rt").unwrap(), t, "{id}");
        }
    }

    #[test]
    fn user_theme_icons_load_and_bad_names_warn() {
        let d = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        let dir = paths.themes_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("mine.toml"),
            "[[highlight]]\nattr = \"dir\"\nicon = \"folder-open\"\n\
             [[highlight]]\nattr = \"image\"\nicon = \"/tmp/x.png\"\n\
             [[highlight]]\nattr = \"source\"\nicon = \"folder-open\"\n\
             [[highlight]]\nattr = \"hidden\"\nicon = \"\"\n",
        )
        .unwrap();
        let (themes, w) = Theme::discover(&paths);
        let mine = themes.iter().find(|t| t.id == "mine").unwrap();
        assert_eq!(mine.icon_names(), ["folder-open"]);
        assert_eq!(w.len(), 1, "{w:?}");
        assert!(w[0].message.contains("highlight[1].icon"), "{w:?}");
    }

    #[test]
    fn discover_overrides_adds_and_warns() {
        let d = tempfile::tempdir().unwrap();
        let paths = ConfigPaths::with_dirs(d.path().join("c"), d.path().join("s"));
        let (themes, w) = Theme::discover(&paths);
        assert_eq!(themes.len(), 3);
        assert!(w.is_empty());

        let dir = paths.themes_dir();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("far-blue.toml"),
            "id = \"far-blue\"\nname = \"Mine\"\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("solar.toml"),
            "[colors.panel]\nbg = \"#002b36\"\nfg = \"nonsense\"\n",
        )
        .unwrap();
        std::fs::write(dir.join("broken.toml"), "id = [").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let (themes, w) = Theme::discover(&paths);
        let ids: Vec<_> = themes.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["far-blue", "commander-light", "adwaita", "solar"]);
        assert_eq!(themes[0].name, "Mine");
        assert_eq!(themes[3].name, "solar");
        assert_eq!(w.len(), 2, "{w:?}"); // broken file + invalid colour
    }
}

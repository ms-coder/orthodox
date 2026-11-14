//! Theme schema (ADR-0005); examples in `defaults/themes/`.

use serde::{Deserialize, Serialize};

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
        todo!("T1.3")
    }
}

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
    /// Icon name.
    pub icon: Option<String>,
}

/// Built-in theme ids, in menu order.
pub const BUILTIN_THEMES: &[&str] = &["far-blue", "commander-light", "adwaita"];

impl Theme {
    /// Parse a theme file.
    pub fn from_toml(_text: &str, _origin: &str) -> Result<Theme, ConfigError> {
        todo!("T1.3")
    }
    /// Built-in theme by id (always valid; a test enforces it).
    pub fn builtin(_id: &str) -> Option<Theme> {
        todo!("T1.3")
    }
    /// Built-in + user themes found in `paths.themes_dir()`; broken files become warnings.
    pub fn discover(_paths: &ConfigPaths) -> (Vec<Theme>, Vec<Warning>) {
        todo!("T1.3")
    }
}

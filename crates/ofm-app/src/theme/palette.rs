//! Render palette: theme colours resolved to `gdk::RGBA`.

use std::rc::Rc;

use gtk::gdk::RGBA;
use ofm_config::Theme;
use ofm_config::theme::{Base, Color};

/// Colours behind `@role` references (`@accent`, `@dim`, `@error`, `@success`, `@warning`) plus
/// the base colours that `base = "adwaita"` themes use for roles they leave unset.
/// T2.2 fills this from `adw::StyleManager`; [`SystemColors::fallback`] is for headless use.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SystemColors {
    /// View background (panel body for adwaita-based themes).
    pub view_bg: RGBA,
    /// View text.
    pub view_fg: RGBA,
    /// Header bar background (column header, path header).
    pub header_bg: RGBA,
    /// Header bar text.
    pub header_fg: RGBA,
    /// `@accent`
    pub accent: RGBA,
    /// `@dim`
    pub dim: RGBA,
    /// `@error`
    pub error: RGBA,
    /// `@success`
    pub success: RGBA,
    /// `@warning`
    pub warning: RGBA,
}

impl SystemColors {
    /// Fixed GNOME-ish colours (used before a display exists and in tests).
    pub fn fallback() -> Self {
        SystemColors {
            view_bg: RGBA::new(1.0, 1.0, 1.0, 1.0),
            view_fg: RGBA::new(0.12, 0.12, 0.12, 1.0),
            header_bg: RGBA::new(0.92, 0.92, 0.92, 1.0),
            header_fg: RGBA::new(0.12, 0.12, 0.12, 1.0),
            accent: RGBA::new(0.21, 0.52, 0.89, 1.0),
            dim: RGBA::new(0.5, 0.5, 0.5, 1.0),
            error: RGBA::new(0.88, 0.11, 0.14, 1.0),
            success: RGBA::new(0.15, 0.64, 0.41, 1.0),
            warning: RGBA::new(0.9, 0.65, 0.04, 1.0),
        }
    }
}

/// Panel body colours.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelPalette {
    /// Background.
    pub bg: RGBA,
    /// Alternate row background (`None` = no striping).
    pub bg_alt: Option<RGBA>,
    /// Default text.
    pub fg: RGBA,
    /// Directory text.
    pub dir_fg: RGBA,
    /// Directories drawn bold.
    pub dir_bold: bool,
    /// Frame lines.
    pub frame: RGBA,
    /// Column header background.
    pub header_bg: RGBA,
    /// Column header text.
    pub header_fg: RGBA,
}

/// Cursor bar colours.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorPalette {
    /// Active panel: bar background.
    pub active_bg: RGBA,
    /// Active panel: bar text.
    pub active_fg: RGBA,
    /// Inactive panel: bar fill (`None` = outline only).
    pub inactive_bg: Option<RGBA>,
    /// Inactive panel: bar text (`None` = unchanged).
    pub inactive_fg: Option<RGBA>,
    /// Inactive panel: outline.
    pub inactive_outline: Option<RGBA>,
}

/// Everything a custom widget needs to paint. Cheap to share as `Rc<Palette>`.
#[derive(Debug, Clone)]
pub struct Palette {
    /// The theme this palette was built from (widgets read further roles via [`Palette::resolve`]).
    pub theme: Rc<Theme>,
    /// System colours used for `@role` references.
    pub system: SystemColors,
    /// Panel body.
    pub panel: PanelPalette,
    /// Cursor bar.
    pub cursor: CursorPalette,
    /// Marked (selected) file text.
    pub marked_fg: RGBA,
    /// Marked files drawn bold.
    pub marked_bold: bool,
    /// Pango font description string for panel text, e.g. `"monospace 11"`.
    pub font: String,
}

const MAGENTA: RGBA = RGBA::new(1.0, 0.0, 1.0, 1.0);

impl Palette {
    /// Resolve a theme colour: `#RRGGBB[AA]` or `@role`; `None` when unset or invalid.
    pub fn resolve(&self, c: &Color) -> Option<RGBA> {
        resolve_color(c, &self.system)
    }

    /// Build the palette of `theme`. Unset required roles become magenta (visible, never a crash).
    pub fn from_theme(theme: Rc<Theme>, system: SystemColors) -> Palette {
        let r = |c: &Color| resolve_color(c, &system);
        // Unset required roles: derived from the system palette for `base = "adwaita"` themes,
        // magenta (visible, never a crash) for fixed themes.
        let adwaita = theme.base == Base::Adwaita;
        let req_or = |c: &Color, sys: RGBA| r(c).unwrap_or(if adwaita { sys } else { MAGENTA });
        let white = RGBA::new(1.0, 1.0, 1.0, 1.0);
        let colors = &theme.colors;
        let family = if theme.font.family.is_empty() || theme.font.family == "system" {
            "sans".to_owned()
        } else {
            theme.font.family.clone()
        };
        let font = if theme.font.size > 0.0 {
            format!("{family} {}", theme.font.size)
        } else {
            family
        };
        Palette {
            panel: PanelPalette {
                bg: req_or(&colors.panel.bg, system.view_bg),
                bg_alt: r(&colors.panel.bg_alt),
                fg: req_or(&colors.panel.fg, system.view_fg),
                dir_fg: r(&colors.panel.dir_fg)
                    .unwrap_or_else(|| req_or(&colors.panel.fg, system.view_fg)),
                dir_bold: colors.panel.dir_bold,
                frame: req_or(&colors.panel.frame, system.dim),
                header_bg: req_or(&colors.panel.header_bg, system.header_bg),
                header_fg: req_or(&colors.panel.header_fg, system.header_fg),
            },
            cursor: CursorPalette {
                active_bg: req_or(&colors.cursor.active_bg, system.accent),
                active_fg: req_or(&colors.cursor.active_fg, white),
                inactive_bg: r(&colors.cursor.inactive_bg),
                inactive_fg: r(&colors.cursor.inactive_fg),
                inactive_outline: r(&colors.cursor.inactive_outline),
            },
            marked_fg: req_or(&colors.marked.fg, system.warning),
            marked_bold: colors.marked.bold,
            font,
            system,
            theme,
        }
    }
}

fn resolve_color(c: &Color, system: &SystemColors) -> Option<RGBA> {
    if let Some([r, g, b, a]) = c.parse_rgba() {
        let f = |v: u8| f32::from(v) / 255.0;
        return Some(RGBA::new(f(r), f(g), f(b), f(a)));
    }
    match c.role()? {
        "accent" => Some(system.accent),
        "dim" => Some(system.dim),
        "error" => Some(system.error),
        "success" => Some(system.success),
        "warning" => Some(system.warning),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn builtin_themes_resolve_required_roles() {
        for id in ofm_config::theme::BUILTIN_THEMES {
            let theme = Theme::builtin(id).expect("builtin theme");
            let p = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
            assert_ne!(p.panel.bg, MAGENTA, "{id}: panel.bg");
            assert_ne!(p.panel.fg, MAGENTA, "{id}: panel.fg");
            assert_ne!(p.cursor.active_bg, MAGENTA, "{id}: cursor.active_bg");
        }
    }

    #[test]
    fn far_blue_values() {
        let theme = Theme::builtin("far-blue").expect("far-blue");
        let p = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
        assert_eq!(p.panel.bg, RGBA::new(0.0, 0.0, 128.0 / 255.0, 1.0));
        assert!(p.panel.bg_alt.is_none());
        assert_eq!(p.font, "monospace");
    }
}

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

impl SystemColors {
    /// The stock libadwaita light/dark colours (`window`, `view`, `headerbar`, accent, status).
    /// Used when no widget is available to ask for the live values.
    pub fn adwaita(dark: bool) -> Self {
        let rgb = |r: u8, g: u8, b: u8| {
            RGBA::new(
                f32::from(r) / 255.0,
                f32::from(g) / 255.0,
                f32::from(b) / 255.0,
                1.0,
            )
        };
        if dark {
            SystemColors {
                view_bg: rgb(0x1e, 0x1e, 0x1e),
                view_fg: rgb(0xff, 0xff, 0xff),
                header_bg: rgb(0x30, 0x30, 0x30),
                header_fg: rgb(0xff, 0xff, 0xff),
                accent: rgb(0x35, 0x84, 0xe4),
                dim: rgb(0x9a, 0x99, 0x96),
                error: rgb(0xff, 0x7b, 0x63),
                success: rgb(0x8f, 0xf0, 0xa4),
                warning: rgb(0xf8, 0xe4, 0x5c),
            }
        } else {
            SystemColors {
                view_bg: rgb(0xff, 0xff, 0xff),
                view_fg: rgb(0x33, 0x33, 0x33),
                header_bg: rgb(0xeb, 0xeb, 0xeb),
                header_fg: rgb(0x33, 0x33, 0x33),
                accent: rgb(0x35, 0x84, 0xe4),
                dim: rgb(0x77, 0x76, 0x7b),
                error: rgb(0xc0, 0x1c, 0x28),
                success: rgb(0x26, 0xa2, 0x69),
                warning: rgb(0xae, 0x7b, 0x03),
            }
        }
    }

    /// System colours following the GNOME light/dark preference (`adw::StyleManager`).
    pub fn from_style_manager(sm: &adw::StyleManager) -> Self {
        Self::adwaita(sm.is_dark())
    }

    /// Replace the colours with the live libadwaita named colours as seen by `widget` (this picks
    /// up the real stylesheet, including user overrides); roles the stylesheet does not define
    /// keep their current value. libadwaita 1.5 has no accent-colour API, so `accent_bg_color`
    /// is what the stylesheet says.
    #[allow(deprecated)] // StyleContext::lookup_color is the only way to read named colours in 4.14
    pub fn refined_from_widget(mut self, widget: &impl gtk::prelude::IsA<gtk::Widget>) -> Self {
        use gtk::prelude::*;
        let ctx = widget.style_context();
        let get = |name: &str| ctx.lookup_color(name);
        if let Some(c) = get("view_bg_color") {
            self.view_bg = opaque(c, self.view_bg);
        }
        if let Some(c) = get("view_fg_color") {
            self.view_fg = over(c, self.view_bg);
        }
        if let Some(c) = get("headerbar_bg_color") {
            self.header_bg = opaque(c, self.view_bg);
        }
        if let Some(c) = get("headerbar_fg_color") {
            self.header_fg = over(c, self.header_bg);
        }
        if let Some(c) = get("accent_bg_color") {
            self.accent = opaque(c, self.accent);
        }
        if let Some(c) = get("error_color") {
            self.error = opaque(c, self.error);
        }
        if let Some(c) = get("success_color") {
            self.success = opaque(c, self.success);
        }
        if let Some(c) = get("warning_color") {
            self.warning = opaque(c, self.warning);
        }
        // "dim": the view text at ~55% over the view background.
        self.dim = mix(self.view_fg, self.view_bg, 0.55);
        self
    }
}

/// `c` composited over `bg` (alpha flattened).
pub fn over(c: RGBA, bg: RGBA) -> RGBA {
    let a = c.alpha();
    RGBA::new(
        c.red() * a + bg.red() * (1.0 - a),
        c.green() * a + bg.green() * (1.0 - a),
        c.blue() * a + bg.blue() * (1.0 - a),
        1.0,
    )
}

fn opaque(c: RGBA, fallback_bg: RGBA) -> RGBA {
    over(c, fallback_bg)
}

/// Linear mix: `t` of `a`, the rest of `b`.
pub fn mix(a: RGBA, b: RGBA, t: f32) -> RGBA {
    RGBA::new(
        a.red() * t + b.red() * (1.0 - t),
        a.green() * t + b.green() * (1.0 - t),
        a.blue() * t + b.blue() * (1.0 - t),
        1.0,
    )
}

/// Relative luminance (sRGB, simple Rec. 709 weights): 0 black .. 1 white.
pub fn luminance(c: RGBA) -> f32 {
    0.2126 * c.red() + 0.7152 * c.green() + 0.0722 * c.blue()
}

/// WCAG 2.x relative luminance (sRGB linearised): 0 black .. 1 white.
pub fn wcag_luminance(c: RGBA) -> f32 {
    let lin = |v: f32| {
        if v <= 0.04045 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.red()) + 0.7152 * lin(c.green()) + 0.0722 * lin(c.blue())
}

/// WCAG contrast ratio of two colours (1 .. 21).
pub fn contrast_ratio(a: RGBA, b: RGBA) -> f32 {
    let (la, lb) = (wcag_luminance(a), wcag_luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Minimum contrast (WCAG AA, normal text) the chrome guarantees for text on a coloured bar.
pub const MIN_CONTRAST: f32 = 4.5;

/// `(bg, fg)` adjusted so that the text is readable (contrast >= [`MIN_CONTRAST`]). A pair that is
/// already readable is returned unchanged. Otherwise the background is first nudged away from the
/// text colour by up to 20% (keeps the theme's look: Far's teal bar gets a hair lighter); if that
/// is not enough the text turns black or white, whichever reads better.
pub fn readable(bg: RGBA, fg: RGBA) -> (RGBA, RGBA) {
    if contrast_ratio(bg, fg) >= MIN_CONTRAST {
        return (bg, fg);
    }
    let black = RGBA::new(0.0, 0.0, 0.0, 1.0);
    let white = RGBA::new(1.0, 1.0, 1.0, 1.0);
    let away = if wcag_luminance(fg) > wcag_luminance(bg) {
        black
    } else {
        white
    };
    for step in 1..=20u8 {
        let nudged = mix(away, bg, f32::from(step) / 100.0);
        if contrast_ratio(nudged, fg) >= MIN_CONTRAST {
            return (nudged, fg);
        }
    }
    let best = if contrast_ratio(bg, black) >= contrast_ratio(bg, white) {
        black
    } else {
        white
    };
    (bg, best)
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
        // Bars that carry text (column header / path bar, cursor bar / active path bar) must be
        // readable whatever the theme says (WCAG AA).
        let (header_bg, header_fg) = readable(
            req_or(&colors.panel.header_bg, system.header_bg),
            req_or(&colors.panel.header_fg, system.header_fg),
        );
        let (active_bg, active_fg) = readable(
            req_or(&colors.cursor.active_bg, system.accent),
            req_or(&colors.cursor.active_fg, white),
        );
        Palette {
            panel: PanelPalette {
                bg: req_or(&colors.panel.bg, system.view_bg),
                bg_alt: r(&colors.panel.bg_alt),
                fg: req_or(&colors.panel.fg, system.view_fg),
                dir_fg: r(&colors.panel.dir_fg)
                    .unwrap_or_else(|| req_or(&colors.panel.fg, system.view_fg)),
                dir_bold: colors.panel.dir_bold,
                frame: req_or(&colors.panel.frame, system.dim),
                header_bg,
                header_fg,
            },
            cursor: CursorPalette {
                active_bg,
                active_fg,
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
    fn adwaita_system_colours_follow_dark() {
        let l = SystemColors::adwaita(false);
        let d = SystemColors::adwaita(true);
        assert!(luminance(l.view_bg) > 0.9 && luminance(d.view_bg) < 0.2);
        assert_ne!(l.view_fg, d.view_fg);
    }

    #[test]
    fn over_flattens_alpha() {
        let c = over(RGBA::new(0.0, 0.0, 0.0, 0.5), RGBA::new(1.0, 1.0, 1.0, 1.0));
        assert!((c.red() - 0.5).abs() < 1e-6 && (c.alpha() - 1.0).abs() < 1e-6);
    }

    fn bars(p: &Palette) -> [(&'static str, RGBA, RGBA); 2] {
        [
            ("inactive path bar", p.panel.header_bg, p.panel.header_fg),
            ("active path bar", p.cursor.active_bg, p.cursor.active_fg),
        ]
    }

    #[test]
    fn path_bars_meet_wcag_aa_in_every_theme() {
        let systems = [
            ("fallback", SystemColors::fallback()),
            ("adwaita light", SystemColors::adwaita(false)),
            ("adwaita dark", SystemColors::adwaita(true)),
        ];
        for id in ofm_config::theme::BUILTIN_THEMES {
            for (sys_name, sys) in systems {
                let theme = Theme::builtin(id).expect("builtin theme");
                let p = Palette::from_theme(Rc::new(theme), sys);
                for (what, bg, fg) in bars(&p) {
                    let ratio = contrast_ratio(bg, fg);
                    assert!(
                        ratio >= MIN_CONTRAST,
                        "{id}/{sys_name}: {what} contrast {ratio:.2} < 4.5"
                    );
                }
            }
        }
    }

    #[test]
    fn readable_keeps_good_pairs_and_fixes_bad_ones() {
        let black = RGBA::new(0.0, 0.0, 0.0, 1.0);
        let white = RGBA::new(1.0, 1.0, 1.0, 1.0);
        assert_eq!(readable(white, black), (white, black));
        // White on orange (Commander Light's cursor) is unreadable: the text turns black.
        let orange = RGBA::new(242.0 / 255.0, 140.0 / 255.0, 40.0 / 255.0, 1.0);
        let (bg, fg) = readable(orange, white);
        assert!(contrast_ratio(bg, fg) >= MIN_CONTRAST);
        // Grey on grey always ends readable.
        let grey = RGBA::new(0.5, 0.5, 0.5, 1.0);
        let (bg, fg) = readable(grey, grey);
        assert!(contrast_ratio(bg, fg) >= MIN_CONTRAST);
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

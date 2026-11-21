//! CSS generation for standard GTK widgets (window background, labels, entries, menus, F-key bar,
//! dialogs). Pure function: testable without a display. Owner: T2.2.
//!
//! Widgets opt in through the `ofm-*` style classes; the generated text only uses colours that
//! come out of the [`Palette`] (themes never hard-code colours elsewhere).

use std::fmt::Write as _;

use gtk::gdk::RGBA;
use ofm_config::theme::{Color, Frames};

use super::Palette;
use super::palette::{luminance, mix, readable};

fn css(c: &RGBA) -> String {
    format!(
        "rgba({}, {}, {}, {:.3})",
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
        c.alpha()
    )
}

/// Colours of the window chrome, resolved from the theme roles with GNOME-ish fallbacks for roles
/// a theme leaves out (the Adwaita theme defines none of them).
struct Chrome {
    win_bg: RGBA,
    win_fg: RGBA,
    status_bg: RGBA,
    status_fg: RGBA,
    cmdline_bg: RGBA,
    cmdline_fg: RGBA,
    fkey_digit: RGBA,
    fkey_label: RGBA,
    fkey_bg: RGBA,
    fkey_hover: RGBA,
    menu_bg: RGBA,
    menu_fg: RGBA,
    menu_border: RGBA,
    menu_sel_bg: RGBA,
    menu_sel_fg: RGBA,
    dim: RGBA,
    error_fg: RGBA,
    error_bg: RGBA,
    progress_bar: RGBA,
    progress_trough: RGBA,
}

fn pick(p: &Palette, c: &Color, fallback: RGBA) -> RGBA {
    p.resolve(c).unwrap_or(fallback)
}

fn chrome(p: &Palette) -> Chrome {
    let colors = &p.theme.colors;
    let sys = &p.system;
    let hdr_bg = p.panel.header_bg;
    let hdr_fg = p.panel.header_fg;
    let menu_bg = pick(p, &colors.dialog.bg, hdr_bg);
    let menu_fg = pick(p, &colors.dialog.fg, hdr_fg);
    let (menu_sel_bg, menu_sel_fg) = readable(
        pick(p, &colors.dialog.button_focus_bg, p.cursor.active_bg),
        pick(p, &colors.dialog.button_focus_fg, p.cursor.active_fg),
    );
    Chrome {
        win_bg: pick(p, &colors.status.bg, hdr_bg),
        win_fg: pick(p, &colors.status.fg, hdr_fg),
        status_bg: pick(p, &colors.status.bg, hdr_bg),
        status_fg: pick(p, &colors.status.fg, hdr_fg),
        cmdline_bg: pick(p, &colors.cmdline.bg, p.panel.bg),
        cmdline_fg: pick(p, &colors.cmdline.fg, p.panel.fg),
        fkey_digit: pick(p, &colors.fkey.digit_fg, sys.dim),
        fkey_label: pick(p, &colors.fkey.label_fg, hdr_fg),
        fkey_bg: pick(p, &colors.fkey.label_bg, hdr_bg),
        fkey_hover: pick(p, &colors.fkey.hover_bg, p.cursor.active_bg),
        menu_bg,
        menu_fg,
        menu_border: pick(p, &colors.dialog.border, p.panel.frame),
        menu_sel_bg,
        menu_sel_fg,
        dim: mix(menu_fg, menu_bg, 0.6),
        error_fg: pick(p, &colors.error.fg, sys.error),
        error_bg: pick(p, &colors.error.bg, mix(sys.error, p.panel.bg, 0.15)),
        progress_bar: pick(p, &colors.progress.bar, p.cursor.active_bg),
        progress_trough: pick(p, &colors.progress.trough, hdr_bg),
    }
}

/// Font rules for the chrome labels (monospace themes also set the chrome in monospace).
fn font_rules(p: &Palette) -> String {
    let f = &p.theme.font;
    let mut out = String::new();
    if !f.family.is_empty() && f.family != "system" {
        let family: String = f
            .family
            .chars()
            .filter(|c| !matches!(c, '"' | '\'' | ';' | '{' | '}' | '\\'))
            .collect();
        if family == "monospace" {
            let _ = write!(out, "font-family: monospace; ");
        } else {
            let _ = write!(out, "font-family: \"{family}\"; ");
        }
    }
    if f.size > 0.0 {
        let _ = write!(out, "font-size: {}pt; ", f.size);
    }
    out
}

/// Rules for presented dialogs (`modal::present` adds `ofm-dialog`): the theme's `colors.dialog`
/// roles on the dialog surface, its labels, entries, buttons and list rows. Empty for themes
/// that leave the role out (the Adwaita theme keeps the stock libadwaita dialog).
fn dialog_rules(p: &Palette, font: &str, radius: &str) -> String {
    let d = &p.theme.colors.dialog;
    let (Some(bg), Some(fg)) = (p.resolve(&d.bg), p.resolve(&d.fg)) else {
        return String::new();
    };
    let border = pick(p, &d.border, mix(fg, bg, 0.5));
    let btn_bg = pick(p, &d.button_bg, mix(fg, bg, 0.12));
    let btn_fg = pick(p, &d.button_fg, fg);
    let (focus_bg, focus_fg) = readable(
        pick(p, &d.button_focus_bg, p.cursor.active_bg),
        pick(p, &d.button_focus_fg, p.cursor.active_fg),
    );
    let (btn_bg, btn_fg) = readable(btn_bg, btn_fg);
    let (entry_bg, entry_fg) = (p.panel.bg, p.panel.fg);
    let dim = mix(fg, bg, 0.65);
    let err_fg = pick(p, &p.theme.colors.error.fg, p.system.error);
    let err_bg = pick(p, &p.theme.colors.error.bg, mix(p.system.error, bg, 0.15));
    let mut s = String::new();
    let _ = writeln!(
        s,
        "dialog.ofm-dialog, dialog.ofm-dialog sheet, dialog.ofm-dialog .background, \
         dialog.ofm-dialog toolbarview, dialog.ofm-dialog headerbar {{ {font}background: {bg}; \
            background-color: {bg}; color: {fg}; }}\n\
         dialog.ofm-dialog headerbar {{ box-shadow: none; border-bottom: 1px solid {border}; }}\n\
         dialog.ofm-dialog label, dialog.ofm-dialog headerbar .title {{ color: {fg}; }}\n\
         dialog.ofm-dialog .dim-label, dialog.ofm-dialog label.dim-label, \
         dialog.ofm-dialog .caption {{ color: {dim}; opacity: 1; }}\n\
         dialog.ofm-dialog entry, dialog.ofm-dialog textview text, dialog.ofm-dialog spinbutton {{ \
            {radius}background: {ebg}; color: {efg}; border: 1px solid {border}; box-shadow: none; }}\n\
         dialog.ofm-dialog entry text {{ background: transparent; color: {efg}; }}\n\
         dialog.ofm-dialog button {{ {radius}background: {btn_bg}; color: {btn_fg}; \
            border: 1px solid {border}; box-shadow: none; background-image: none; }}\n\
         dialog.ofm-dialog button label {{ color: {btn_fg}; }}\n\
         dialog.ofm-dialog button:hover, dialog.ofm-dialog button:focus, \
         dialog.ofm-dialog button:checked {{ background: {focus_bg}; color: {focus_fg}; }}\n\
         dialog.ofm-dialog button:hover label, dialog.ofm-dialog button:focus label, \
         dialog.ofm-dialog button:checked label {{ color: {focus_fg}; }}\n\
         dialog.ofm-dialog button.suggested-action, dialog.ofm-dialog button.destructive-action {{ \
            font-weight: bold; }}\n\
         dialog.ofm-dialog checkbutton, dialog.ofm-dialog radiobutton {{ color: {fg}; }}\n\
         dialog.ofm-dialog check, dialog.ofm-dialog radio {{ background: {ebg}; color: {efg}; \
            border: 1px solid {border}; }}\n\
         dialog.ofm-dialog list, dialog.ofm-dialog listview, dialog.ofm-dialog row, \
         dialog.ofm-dialog .card {{ background: {ebg}; color: {efg}; }}\n\
         dialog.ofm-dialog row:selected, dialog.ofm-dialog row:hover {{ background: {focus_bg}; \
            color: {focus_fg}; }}\n\
         dialog.ofm-dialog row label {{ color: inherit; }}\n\
         dialog.ofm-dialog separator {{ background: {border}; min-height: 1px; }}\n\
         dialog.ofm-dialog .ofm-error {{ color: {err_fg}; background-color: {err_bg}; }}",
        bg = css(&bg),
        fg = css(&fg),
        border = css(&border),
        dim = css(&dim),
        ebg = css(&entry_bg),
        efg = css(&entry_fg),
        btn_bg = css(&btn_bg),
        btn_fg = css(&btn_fg),
        focus_bg = css(&focus_bg),
        focus_fg = css(&focus_fg),
        err_fg = css(&err_fg),
        err_bg = css(&err_bg),
    );
    s
}

/// Whether the theme is dark (decides `adw::ColorScheme` for standard widgets).
pub fn is_dark(palette: &Palette) -> bool {
    luminance(palette.panel.bg) < 0.45
}

/// CSS text for `palette`. Widgets opt in through the `ofm-*` style classes.
pub fn generate(palette: &Palette) -> String {
    let p = &palette.panel;
    let c = chrome(palette);
    let retro = &palette.theme.retro;
    let font = font_rules(palette);
    let radius = if retro.rounded_corners {
        ""
    } else {
        "border-radius: 0; "
    };
    let mut s = String::with_capacity(6000);

    // Window and panels ---------------------------------------------------------------------------
    let _ = writeln!(
        s,
        "window.ofm-window {{ background-color: {}; color: {}; }}",
        css(&c.win_bg),
        css(&c.win_fg)
    );
    // Active panel highlight: a 2px frame in the cursor colour, inactive panels get the theme's
    // frame colour only for frame-less/hairline themes (box-drawing themes draw their own frame).
    let inactive_border = match retro.frames {
        Frames::Hairline | Frames::None => css(&p.frame),
        Frames::Single | Frames::Double => "transparent".to_owned(),
    };
    let _ = writeln!(
        s,
        ".ofm-panel {{ border: 2px solid {inactive_border}; background-color: {bg}; }}\n\
         .ofm-panel.active {{ border-color: {active}; }}",
        bg = css(&p.bg),
        active = css(&palette.cursor.active_bg),
    );
    let _ = writeln!(
        s,
        ".ofm-path {{ {font}background-color: {}; color: {}; padding: 2px 8px; }}\n\
         .ofm-panel.active .ofm-path {{ background-color: {}; color: {}; }}",
        css(&p.header_bg),
        css(&p.header_fg),
        css(&palette.cursor.active_bg),
        css(&palette.cursor.active_fg),
    );
    let _ = writeln!(
        s,
        ".ofm-status {{ {font}background-color: {sbg}; color: {sfg}; padding: 2px 8px; }}\n\
         .ofm-statusbox {{ background-color: {sbg}; }}\n\
         .ofm-status-dim {{ {font}background-color: {sbg}; color: {dim}; padding: 2px 8px; }}",
        sbg = css(&c.status_bg),
        sfg = css(&c.status_fg),
        dim = css(&mix(c.status_fg, c.status_bg, 0.7)),
    );
    let _ = writeln!(
        s,
        ".ofm-cmdline {{ {font}background-color: {}; color: {}; }}\n\
         .ofm-cmdline entry, .ofm-cmdline text {{ background: transparent; color: {}; }}",
        css(&c.cmdline_bg),
        css(&c.cmdline_fg),
        css(&c.cmdline_fg),
    );

    // F-key bar -----------------------------------------------------------------------------------
    let _ = writeln!(
        s,
        ".ofm-fkeybar {{ background-color: {win}; }}\n\
         .ofm-fkeybar button.ofm-fkey {{ {font}{radius}min-height: 0; min-width: 0; padding: 1px 4px; margin: 0 1px; \
            border: none; box-shadow: none; background: {lbg}; color: {lfg}; }}\n\
         .ofm-fkeybar button.ofm-fkey:hover {{ background: {hover}; }}\n\
         .ofm-fkeybar .ofm-fkey-num {{ color: {digit}; padding-right: 4px; }}\n\
         .ofm-fkeybar button.ofm-fkey:hover .ofm-fkey-num {{ color: {lfg}; }}\n\
         .ofm-fkeybar .ofm-fkey-label {{ color: {lfg}; }}\n\
         .ofm-fkeybar button.ofm-fkey.empty {{ opacity: 0.55; }}",
        win = css(&c.win_bg),
        lbg = css(&c.fkey_bg),
        lfg = css(&c.fkey_label),
        hover = css(&c.fkey_hover),
        digit = css(&c.fkey_digit),
    );

    // Menu bar and drop-down menus ----------------------------------------------------------------
    let _ = writeln!(
        s,
        ".ofm-menubar {{ {font}background-color: {mbg}; color: {mfg}; }}\n\
         .ofm-menubar menubutton.ofm-menu-title > button {{ {radius}min-height: 0; padding: 2px 10px; border: none; \
            box-shadow: none; background: transparent; color: {mfg}; }}\n\
         .ofm-menubar menubutton.ofm-menu-title > button:hover, \
         .ofm-menubar menubutton.ofm-menu-title > button:checked {{ background: {sel}; color: {self_}; }}\n\
         popover.ofm-menu > contents {{ {font}{radius}background-color: {mbg}; color: {mfg}; \
            border: 1px solid {mborder}; padding: 2px; }}\n\
         popover.ofm-menu button.ofm-menu-item {{ {radius}min-height: 0; padding: 2px 8px; border: none; \
            box-shadow: none; background: transparent; color: {mfg}; }}\n\
         popover.ofm-menu button.ofm-menu-item:hover, popover.ofm-menu button.ofm-menu-item:focus {{ \
            background: {sel}; color: {self_}; }}\n\
         popover.ofm-menu .ofm-menu-accel {{ color: {mdim}; padding-left: 24px; }}\n\
         popover.ofm-menu button.ofm-menu-item:hover .ofm-menu-accel, \
         popover.ofm-menu button.ofm-menu-item:focus .ofm-menu-accel {{ color: {self_}; }}\n\
         popover.ofm-menu separator {{ background: {mborder}; min-height: 1px; margin: 2px 0; }}",
        mbg = css(&c.menu_bg),
        mfg = css(&c.menu_fg),
        mborder = css(&c.menu_border),
        mdim = css(&c.dim),
        sel = css(&c.menu_sel_bg),
        self_ = css(&c.menu_sel_fg),
    );

    // Standard widgets: entries, selection, errors, progress, tooltips ----------------------------
    let _ = writeln!(
        s,
        "window.ofm-window text selection, .ofm-panel text selection {{ background-color: {}; color: {}; }}\n\
         .ofm-error {{ color: {}; background-color: {}; padding: 2px 8px; }}\n\
         progressbar.ofm-progress progress {{ {radius}background: {}; }}\n\
         progressbar.ofm-progress trough {{ {radius}background: {}; }}",
        css(&palette.cursor.active_bg),
        css(&palette.cursor.active_fg),
        css(&c.error_fg),
        css(&c.error_bg),
        css(&c.progress_bar),
        css(&c.progress_trough),
    );
    s.push_str(&dialog_rules(palette, &font, radius));
    if !retro.rounded_corners {
        s.push_str(
            "window.ofm-window button, window.ofm-window entry, window.ofm-window popover > contents, \
             tooltip > * { border-radius: 0; }\n",
        );
    }
    if !retro.animations {
        s.push_str("* { transition: none; animation: none; }\n");
    }
    s
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::rc::Rc;

    use ofm_config::Theme;

    use super::*;
    use crate::theme::SystemColors;

    fn palette(id: &str) -> Palette {
        let theme = Theme::builtin(id).expect("builtin");
        Palette::from_theme(Rc::new(theme), SystemColors::fallback())
    }

    #[test]
    fn generates_rules() {
        let text = generate(&palette("far-blue"));
        assert!(text.contains("rgba(0, 0, 128, 1.000)"));
        assert!(text.contains(".ofm-status"));
        assert!(text.contains(".ofm-fkeybar"));
        assert!(text.contains("popover.ofm-menu"));
        assert!(text.contains("font-family: monospace"));
        // Retro options: no rounded corners, no animations.
        assert!(text.contains("transition: none"));
    }

    #[test]
    fn adwaita_theme_uses_system_colours_and_keeps_animations() {
        let p = palette("adwaita");
        let text = generate(&p);
        assert!(!text.contains("transition: none"));
        assert!(
            !text.contains("rgba(255, 0, 255"),
            "magenta fallback leaked: {text}"
        );
    }

    #[test]
    fn all_builtin_themes_generate_without_magenta() {
        for id in ofm_config::theme::BUILTIN_THEMES {
            let text = generate(&palette(id));
            assert!(!text.contains("rgba(255, 0, 255"), "{id}");
        }
    }

    #[test]
    fn darkness_detection() {
        assert!(is_dark(&palette("far-blue")));
        assert!(!is_dark(&palette("commander-light")));
        assert!(!is_dark(&palette("adwaita"))); // fallback system colours are light
    }

    #[test]
    fn font_family_is_sanitised() {
        let mut theme = Theme::builtin("far-blue").expect("builtin");
        theme.font.family = "Evil\"; } * { x".into();
        theme.font.size = 11.0;
        let p = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
        let rules = font_rules(&p);
        assert_eq!(rules, "font-family: \"Evil  *  x\"; font-size: 11pt; ");
    }
}

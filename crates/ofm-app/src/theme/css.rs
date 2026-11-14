//! CSS generation for standard GTK widgets (window background, labels, entries, dialogs).
//! Pure function: testable without a display. Owner: T2.2 (T2.0 ships a minimal version).

use gtk::gdk::RGBA;

use super::Palette;

fn css(c: &RGBA) -> String {
    format!(
        "rgba({}, {}, {}, {:.3})",
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
        c.alpha()
    )
}

/// CSS text for `palette`. Widgets opt in through the `ofm-*` style classes.
pub fn generate(palette: &Palette) -> String {
    let p = &palette.panel;
    format!(
        "window.ofm-window {{ background-color: {bg}; color: {fg}; }}\n\
         .ofm-path {{ background-color: {hbg}; color: {hfg}; padding: 2px 8px; }}\n\
         .ofm-panel.active .ofm-path {{ background-color: {cbg}; color: {cfg}; }}\n\
         .ofm-status {{ color: {fg}; padding: 2px 8px; }}\n",
        bg = css(&p.bg),
        fg = css(&p.fg),
        hbg = css(&p.header_bg),
        hfg = css(&p.header_fg),
        cbg = css(&palette.cursor.active_bg),
        cfg = css(&palette.cursor.active_fg),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::rc::Rc;

    use ofm_config::Theme;

    use super::*;
    use crate::theme::SystemColors;

    #[test]
    fn generates_rules() {
        let theme = Theme::builtin("far-blue").expect("far-blue");
        let palette = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
        let text = generate(&palette);
        assert!(text.contains("rgba(0, 0, 128, 1.000)"));
        assert!(text.contains(".ofm-status"));
    }
}

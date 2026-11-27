//! CSS of the Find window's results list, generated from the theme palette (panel colours, cursor
//! bar) so it follows theme switches. Own provider, like `navigation::style`.

use std::rc::Rc;

use gtk::gdk::{self, RGBA};

use crate::context::AppContext;
use crate::theme::Palette;

fn css(c: &RGBA) -> String {
    format!(
        "rgba({}, {}, {}, {:.3})",
        (c.red() * 255.0).round() as u8,
        (c.green() * 255.0).round() as u8,
        (c.blue() * 255.0).round() as u8,
        c.alpha()
    )
}

/// CSS for the results list of `palette`.
pub fn generate(palette: &Palette) -> String {
    let p = &palette.panel;
    let c = &palette.cursor;
    format!(
        "columnview.ofm-find-results, columnview.ofm-find-results listview {{ \
            background-color: {pbg}; color: {pfg}; }}\n\
         columnview.ofm-find-results > header {{ background-color: {hbg}; }}\n\
         columnview.ofm-find-results > header button {{ background: none; color: {hfg}; \
            border-radius: 0; min-height: 0; padding: 2px 6px; box-shadow: none; }}\n\
         columnview.ofm-find-results > header button label {{ color: {hfg}; font-weight: bold; }}\n\
         columnview.ofm-find-results listview > row {{ min-height: 0; padding: 0 2px; \
            background: none; color: {pfg}; border-radius: 0; }}\n\
         columnview.ofm-find-results listview > row cell {{ padding: 1px 6px; }}\n\
         columnview.ofm-find-results listview > row:selected {{ background-color: {abg}; \
            color: {afg}; }}\n\
         columnview.ofm-find-results listview > row:selected label {{ color: {afg}; }}\n\
         columnview.ofm-find-results listview > row label {{ color: inherit; }}\n\
         .ofm-find-status {{ padding: 2px 6px; }}\n\
         scrolledwindow.ofm-find-frame {{ border: 1px solid {frame}; }}\n",
        pbg = css(&p.bg),
        pfg = css(&p.fg),
        hbg = css(&p.header_bg),
        hfg = css(&p.header_fg),
        abg = css(&c.active_bg),
        afg = css(&c.active_fg),
        frame = css(&p.frame),
    )
}

/// The provider attached to the display; kept alive in `ctx.ext`.
pub struct FindStyle {
    _provider: gtk::CssProvider,
}

impl FindStyle {
    /// Create the provider once (idempotent), attach it and follow theme changes.
    pub fn ensure(ctx: &Rc<AppContext>) {
        if ctx.ext.get::<FindStyle>().is_some() {
            return;
        }
        let provider = gtk::CssProvider::new();
        provider.load_from_string(&generate(&ctx.theme.palette()));
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
        let p2 = provider.clone();
        ctx.theme
            .changed
            .connect(move |palette| p2.load_from_string(&generate(palette)));
        ctx.ext.insert(Rc::new(FindStyle {
            _provider: provider,
        }));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_config::Theme;

    use super::*;
    use crate::theme::SystemColors;

    #[test]
    fn follows_palette() {
        let theme = Theme::builtin("far-blue").expect("far-blue");
        let palette = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
        let text = generate(&palette);
        assert!(text.contains("columnview.ofm-find-results"));
        assert!(text.contains(":selected"));
    }
}

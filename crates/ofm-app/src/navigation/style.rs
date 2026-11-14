//! CSS of the navigation widgets (tab bar, quick-search bar), generated from the theme palette so
//! it follows theme switches. Own provider: the shared theme CSS belongs to T2.2.

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

/// CSS for the navigation widgets of `palette`.
pub fn generate(palette: &Palette) -> String {
    let p = &palette.panel;
    format!(
        ".ofm-tabbar {{ background-color: {hbg}; }}\n\
         .ofm-tab {{ color: {hfg}; padding: 1px 10px; }}\n\
         .ofm-tab.active {{ background-color: {pbg}; color: {pfg}; }}\n\
         .ofm-quicksearch {{ background-color: {hbg}; color: {hfg}; padding: 2px 6px; \
            border: 1px solid {frame}; }}\n\
         .ofm-quicksearch entry {{ min-height: 0; padding: 0 4px; border-radius: 0; box-shadow: none; outline: none; background-color: {pbg}; color: {pfg}; }}\n",
        hbg = css(&p.header_bg),
        hfg = css(&p.header_fg),
        pbg = css(&p.bg),
        pfg = css(&p.fg),
        frame = css(&p.frame),
    )
}

/// The provider attached to the display; kept alive in `ctx.ext`.
pub struct NavStyle {
    provider: gtk::CssProvider,
}

impl NavStyle {
    /// Create the provider, attach it to the default display (if any) and follow theme changes.
    pub fn install(ctx: &Rc<AppContext>) {
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
        ctx.ext.insert(Rc::new(NavStyle { provider }));
    }

    /// The provider (tests).
    pub fn provider(&self) -> &gtk::CssProvider {
        &self.provider
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
        assert!(text.contains(".ofm-tab.active"));
        assert!(text.contains(".ofm-quicksearch"));
    }
}

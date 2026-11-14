//! `ThemeHost`: current theme + palette + CSS provider; instant switching.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use ofm_config::Theme;

use super::{Palette, SystemColors, css};
use crate::signal::Signal;

/// Runtime theme state (`AppContext::theme`).
pub struct ThemeHost {
    palette: RefCell<Rc<Palette>>,
    system: RefCell<SystemColors>,
    provider: gtk::CssProvider,
    attached: RefCell<bool>,
    /// Emitted after every theme switch with the new palette; custom widgets redraw.
    pub changed: Signal<Rc<Palette>>,
}

impl ThemeHost {
    /// Host showing `theme`. Does not touch the display until [`apply_css`](Self::apply_css).
    pub fn new(theme: Theme) -> Rc<Self> {
        let system = SystemColors::fallback();
        let palette = Rc::new(Palette::from_theme(Rc::new(theme), system));
        Rc::new(ThemeHost {
            palette: RefCell::new(palette),
            system: RefCell::new(system),
            provider: gtk::CssProvider::new(),
            attached: RefCell::new(false),
            changed: Signal::new(),
        })
    }

    /// Current palette.
    pub fn palette(&self) -> Rc<Palette> {
        Rc::clone(&self.palette.borrow())
    }

    /// Current theme.
    pub fn theme(&self) -> Rc<Theme> {
        Rc::clone(&self.palette.borrow().theme)
    }

    /// Update the system colours behind `@role` references (T2.2: from `adw::StyleManager`) and
    /// rebuild the palette.
    pub fn set_system_colors(&self, system: SystemColors) {
        *self.system.borrow_mut() = system;
        let theme = self.theme();
        self.set_theme(Rc::unwrap_or_clone(theme));
    }

    /// Switch theme: rebuild palette, regenerate CSS, notify widgets.
    pub fn set_theme(&self, theme: Theme) {
        let palette = Rc::new(Palette::from_theme(Rc::new(theme), *self.system.borrow()));
        *self.palette.borrow_mut() = Rc::clone(&palette);
        self.apply_css();
        self.changed.emit(&palette);
    }

    /// Regenerate the CSS and (first time) attach the provider to the default display; a no-op
    /// without a display.
    pub fn apply_css(&self) {
        let text = css::generate(&self.palette());
        self.provider.load_from_string(&text);
        if !*self.attached.borrow()
            && let Some(display) = gdk::Display::default()
        {
            gtk::style_context_add_provider_for_display(
                &display,
                &self.provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            *self.attached.borrow_mut() = true;
        }
    }
}

//! Cache of shaped `pango::Layout`s. Scrolling shows the same names again and again; shaping text
//! is the expensive part of a frame, so a layout is created once per (text, style, width) and
//! reused until the font, scale or settings change.

use std::collections::HashMap;

use gtk::pango;
use gtk::prelude::*;

/// Upper bound of cached layouts; the cache is dropped as a whole when it is exceeded (the working
/// set is a few screens, so refilling is cheap).
const MAX_LAYOUTS: usize = 8192;

/// Everything besides the text that makes two layouts differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TextStyle {
    /// Bold weight.
    pub bold: bool,
    /// Right-aligned inside `width`.
    pub right: bool,
    /// Layout width in whole px (text is ellipsized at the end beyond it).
    pub width: i32,
}

/// Layout cache plus the fonts it renders with.
pub struct TextCache {
    regular: pango::FontDescription,
    bold: pango::FontDescription,
    map: HashMap<TextStyle, HashMap<String, pango::Layout>>,
    len: usize,
}

impl Default for TextCache {
    fn default() -> Self {
        TextCache::new("monospace")
    }
}

impl TextCache {
    /// Cache for the font description string `font` (e.g. `"monospace 11"`).
    pub fn new(font: &str) -> Self {
        let (regular, bold) = fonts(font);
        TextCache {
            regular,
            bold,
            map: HashMap::new(),
            len: 0,
        }
    }

    /// Switch fonts and drop every layout.
    pub fn set_font(&mut self, font: &str) {
        let (regular, bold) = fonts(font);
        self.regular = regular;
        self.bold = bold;
        self.clear();
    }

    /// Drop every layout (scale factor / system font changed).
    pub fn clear(&mut self) {
        self.map.clear();
        self.len = 0;
    }

    /// Number of cached layouts.
    pub fn len(&self) -> usize {
        self.len
    }

    /// Regular-weight font.
    pub fn regular_font(&self) -> &pango::FontDescription {
        &self.regular
    }

    /// The layout for `text` in `style`, created on a miss.
    pub fn get(
        &mut self,
        widget: &impl IsA<gtk::Widget>,
        text: &str,
        style: TextStyle,
    ) -> pango::Layout {
        if let Some(l) = self.map.get(&style).and_then(|m| m.get(text)) {
            return l.clone();
        }
        if self.len >= MAX_LAYOUTS {
            self.clear();
        }
        let layout = widget.create_pango_layout(Some(text));
        layout.set_font_description(Some(if style.bold {
            &self.bold
        } else {
            &self.regular
        }));
        layout.set_single_paragraph_mode(true);
        if style.width > 0 {
            layout.set_width(style.width * pango::SCALE);
            layout.set_ellipsize(pango::EllipsizeMode::End);
            if style.right {
                layout.set_alignment(pango::Alignment::Right);
            }
        }
        self.map
            .entry(style)
            .or_default()
            .insert(text.to_owned(), layout.clone());
        self.len += 1;
        layout
    }
}

fn fonts(font: &str) -> (pango::FontDescription, pango::FontDescription) {
    let regular = pango::FontDescription::from_string(font);
    let mut bold = regular.clone();
    bold.set_weight(pango::Weight::Bold);
    (regular, bold)
}

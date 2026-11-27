//! The icon paintables of one list (F30, highlight `icon`). Every name the theme can ask for is
//! looked up once when the theme, font size, scale factor or system settings change; painting a
//! row is a hash lookup, never an icon-theme lookup.

use std::collections::HashMap;

use gtk::gdk;
use ofm_config::highlight::{
    DEFAULT_DIR_ICON, DEFAULT_FILE_ICON, default_icon, rule_icon, uses_icons,
};
use ofm_config::theme::HighlightRule;
use ofm_core::Entry;

use crate::theme::icons::resolve_name;

/// Space between the icon and the name, px.
const GAP: f32 = 4.0;

/// Icon edge for rows `row_h` px high: the largest usual icon size that fits (crisp bitmaps).
pub fn icon_px(row_h: f32) -> i32 {
    let h = row_h.floor() as i32;
    [64, 48, 32, 24, 16]
        .into_iter()
        .find(|s| *s <= h)
        .unwrap_or(h.max(1))
}

/// Paintables by theme icon name; empty for themes that draw no icons.
#[derive(Default)]
pub struct IconCache {
    /// Edge length in logical px; 0 = the theme draws no icons.
    size: i32,
    map: HashMap<String, gtk::IconPaintable>,
}

impl IconCache {
    /// Look up every icon `rules` can ask for at `size` logical px and device `scale`.
    pub fn build(rules: &[HighlightRule], display: &gdk::Display, size: i32, scale: i32) -> Self {
        if !uses_icons(rules) {
            return IconCache::default();
        }
        let theme = gtk::IconTheme::for_display(display);
        let mut map = HashMap::new();
        let names = rules
            .iter()
            .filter_map(rule_icon)
            .chain([DEFAULT_DIR_ICON, DEFAULT_FILE_ICON]);
        for name in names {
            if map.contains_key(name) {
                continue;
            }
            let Some(resolved) = resolve_name(&theme, name) else {
                tracing::warn!(icon = name, "unknown icon in theme; using a generic one");
                continue;
            };
            // PRELOAD: GTK reads the image file on a thread of its own.
            let paintable = theme.lookup_icon(
                &resolved,
                &[],
                size,
                scale.max(1),
                gtk::TextDirection::None,
                gtk::IconLookupFlags::PRELOAD,
            );
            map.insert(name.to_owned(), paintable);
        }
        IconCache { size, map }
    }

    /// Edge length in logical px (0 without icons).
    pub fn size(&self) -> i32 {
        self.size
    }

    /// Horizontal space the icon column takes in the name cell (0 without icons).
    pub fn advance(&self) -> f32 {
        if self.size > 0 {
            self.size as f32 + GAP
        } else {
            0.0
        }
    }

    /// The icon of `entry` under its matching `rule`: the rule's icon, else (none set, or unknown)
    /// the generic folder / file icon. Returns the theme name with the paintable.
    pub fn get(
        &self,
        rule: Option<&HighlightRule>,
        entry: &Entry,
    ) -> Option<(&str, &gtk::IconPaintable)> {
        rule.and_then(rule_icon)
            .and_then(|name| self.map.get_key_value(name))
            .or_else(|| self.map.get_key_value(default_icon(entry)))
            .map(|(name, paintable)| (name.as_str(), paintable))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_sizes_fit_the_row() {
        assert_eq!(icon_px(12.0), 12);
        assert_eq!(icon_px(16.0), 16);
        assert_eq!(icon_px(19.0), 16);
        assert_eq!(icon_px(27.0), 24);
        assert_eq!(icon_px(38.0), 32);
        assert_eq!(icon_px(200.0), 64);
        assert_eq!(icon_px(0.0), 1);
    }
}

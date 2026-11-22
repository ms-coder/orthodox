//! Drawing of one frame (`snapshot()` helper). Colours come from the palette only; text comes from
//! the layout cache; only the visible window of the model is touched.

use gtk::gdk::RGBA;
use gtk::graphene;
use gtk::prelude::*;
use ofm_config::theme::{Color, Frames};
use ofm_core::Entry;
use ofm_core::panel::{PanelModel, SortKey, SortSpec};

use super::format::{self, is_parent};
use super::geo::{Geo, Metrics, PAD, Rect};
use super::settings::{Column, Settings, ViewMode};
use super::text::{TextCache, TextStyle};
use crate::i18n::tr;
use crate::theme::Palette;

/// Relative luminance (sRGB) for the contrast check.
fn luminance(c: &RGBA) -> f32 {
    let f = |v: f32| {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * f(c.red()) + 0.7152 * f(c.green()) + 0.0722 * f(c.blue())
}

/// WCAG contrast ratio of two colours.
pub fn contrast(a: &RGBA, b: &RGBA) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Where the cursor is, from the point of view of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CursorState {
    None,
    Active,
    Inactive,
}

/// Resolved look of one row.
struct RowStyle {
    fg: RGBA,
    bg: Option<RGBA>,
    bold: bool,
}

/// Everything needed to paint a frame.
pub struct Painter<'a> {
    /// Target.
    pub snapshot: &'a gtk::Snapshot,
    /// For creating Pango layouts.
    pub widget: &'a super::FileList,
    /// Colours.
    pub palette: &'a Palette,
    /// Layout cache.
    pub cache: &'a mut TextCache,
    /// Geometry of this frame.
    pub geo: &'a Geo,
    /// Font metrics.
    pub metrics: Metrics,
    /// `[panels]` settings.
    pub settings: &'a Settings,
    /// Active panel (cursor colours).
    pub active: bool,
}

fn grect(r: Rect) -> graphene::Rect {
    graphene::Rect::new(r.x, r.y, r.w, r.h)
}

impl Painter<'_> {
    fn fill(&self, color: &RGBA, r: Rect) {
        if r.w > 0.0 && r.h > 0.0 {
            self.snapshot.append_color(color, &grect(r));
        }
    }

    /// Rectangle outline of thickness `t` drawn inside `r`.
    fn outline(&self, color: &RGBA, r: Rect, t: f32) {
        self.fill(color, Rect { h: t, ..r });
        self.fill(
            color,
            Rect {
                y: r.y + r.h - t,
                h: t,
                ..r
            },
        );
        self.fill(color, Rect { w: t, ..r });
        self.fill(
            color,
            Rect {
                x: r.x + r.w - t,
                w: t,
                ..r
            },
        );
    }

    /// Draw `text` in the cell starting at (`x`, `y`) of width `w`, vertically centred in a row.
    #[allow(clippy::too_many_arguments)]
    fn text(&mut self, text: &str, bold: bool, right: bool, x: f32, y: f32, w: f32, c: &RGBA) {
        if text.is_empty() || w <= 0.0 {
            return;
        }
        let style = TextStyle {
            bold,
            right,
            width: w.floor() as i32,
        };
        let layout = self.cache.get(self.widget, text, style);
        let dy = ((self.metrics.row_h - self.metrics.text_h) / 2.0).max(0.0);
        self.snapshot.save();
        self.snapshot.translate(&graphene::Point::new(x, y + dy));
        self.snapshot.append_layout(&layout, c);
        self.snapshot.restore();
    }

    /// Paint everything. Returns the number of rows drawn.
    pub fn paint(&mut self, model: &PanelModel, first: u32, empty_message: Option<&str>) -> u32 {
        let geo = self.geo;
        let p = self.palette;
        self.fill(
            &p.panel.bg,
            Rect {
                x: 0.0,
                y: 0.0,
                w: geo.width,
                h: geo.height,
            },
        );
        self.paint_header(
            model.sort_spec(),
            geo.grid.used_cols(first, model.visible_len()),
        );
        let drawn = self.paint_body(model, first, empty_message);
        self.paint_frame();
        drawn
    }

    // ---- header -------------------------------------------------------------------------------
    /// `used_cols`: Brief columns that hold entries (the others get no header).
    fn paint_header(&mut self, sort: SortSpec, used_cols: u32) {
        let geo = self.geo;
        let p = self.palette;
        self.fill(&p.panel.header_bg, geo.header);
        let hfg = p.panel.header_fg;
        let ind = p
            .resolve(&p.theme.colors.panel.sort_indicator)
            .unwrap_or(hfg);
        let arrow = if sort.reverse { '\u{25BC}' } else { '\u{25B2}' };
        let y = geo.header.y;
        match geo.mode {
            ViewMode::Brief => {
                let label = if sort.key == SortKey::Name {
                    arrow.to_string()
                } else {
                    format!("{} {arrow}", sort_title(sort.key))
                };
                for c in 0..used_cols.min(geo.grid.cols) {
                    let (x, _) = geo.cell_origin(c, 0);
                    let w = geo.cell_w - 2.0 * PAD;
                    self.text(Column::Name.title(), false, false, x + PAD, y, w, &hfg);
                    self.text(&label, false, true, x + PAD, y, w, &ind);
                }
            }
            ViewMode::Full | ViewMode::Custom => {
                for slot in &geo.slots {
                    let x = geo.content.x + slot.x + PAD;
                    let w = slot.w - 2.0 * PAD;
                    let right = slot.column.right_aligned();
                    let sorted = slot.column.sort_key() == Some(sort.key);
                    let indicator_w = if sorted {
                        self.metrics.char_w * 1.5
                    } else {
                        0.0
                    };
                    let (lx, lw) = if right { (x, w - indicator_w) } else { (x, w) };
                    self.text(slot.column.title(), false, right, lx, y, lw, &hfg);
                    if sorted {
                        let s = arrow.to_string();
                        self.text(&s, false, true, x, y, w, &ind);
                    }
                }
            }
        }
    }

    // ---- body ---------------------------------------------------------------------------------
    fn paint_body(&mut self, model: &PanelModel, first: u32, empty_message: Option<&str>) -> u32 {
        let geo = self.geo;
        let len = model.visible_len();
        self.snapshot.push_clip(&grect(geo.body));
        let mut drawn = 0;
        if len == 0 {
            if let Some(msg) = empty_message {
                let fg = self.palette.system.dim;
                let (x, y) = (geo.body.x + PAD, geo.body.y);
                self.text(msg, false, false, x, y, geo.body.w - 2.0 * PAD, &fg);
            }
        } else {
            let cursor = model.cursor();
            let end = len.min(first.saturating_add(geo.grid.page()));
            for idx in first..end {
                let Some(entry) = model.visible(idx) else {
                    continue;
                };
                let Some((col, row)) = geo.grid.cell_of(first, idx) else {
                    continue;
                };
                let cs = if cursor == Some(idx) {
                    if self.active {
                        CursorState::Active
                    } else {
                        CursorState::Inactive
                    }
                } else {
                    CursorState::None
                };
                self.paint_row(entry, idx, col, row, model.is_selected(idx), cs);
                drawn += 1;
            }
        }
        self.snapshot.pop();
        drawn
    }

    fn row_style(&self, e: &Entry, selected: bool, cs: CursorState) -> RowStyle {
        let p = self.palette;
        let mut st = RowStyle {
            fg: if e.kind.is_dir_like() {
                p.panel.dir_fg
            } else {
                p.panel.fg
            },
            bg: None,
            bold: e.kind.is_dir_like() && p.panel.dir_bold,
        };
        if !is_parent(e)
            && let Some(h) = ofm_config::highlight::style_for(&p.theme.highlight, e)
        {
            if let Some(c) = h.fg.and_then(|s| p.resolve(&Color(s))) {
                st.fg = c;
            }
            if let Some(c) = h.bg.and_then(|s| p.resolve(&Color(s))) {
                st.bg = Some(c);
            }
            if let Some(b) = h.bold {
                st.bold = b;
            }
        }
        if selected {
            st.bold |= p.marked_bold;
            st.fg = p.marked_fg;
        }
        match cs {
            CursorState::None => {}
            CursorState::Active => {
                let bg = p.cursor.active_bg;
                st.bg = Some(bg);
                // Marked colour only when it stays readable on the cursor bar.
                if selected && contrast(&p.marked_fg, &bg) >= 3.0 {
                    st.fg = p.marked_fg;
                } else {
                    st.fg = p.cursor.active_fg;
                }
                st.bold |= selected;
            }
            CursorState::Inactive => {
                if let Some(bg) = p.cursor.inactive_bg {
                    st.bg = Some(bg);
                    if !selected {
                        st.fg = p.cursor.inactive_fg.unwrap_or(st.fg);
                    }
                }
            }
        }
        st
    }

    fn paint_row(
        &mut self,
        e: &Entry,
        idx: u32,
        col: u32,
        row: u32,
        selected: bool,
        cs: CursorState,
    ) {
        let geo = self.geo;
        let p = self.palette;
        let (x, y) = geo.cell_origin(col, row);
        let cell = Rect {
            x,
            y,
            w: geo.cell_w,
            h: geo.row_h,
        };
        let mut st = self.row_style(e, selected, cs);
        // Alternate-row stripes (Full/Custom only; Brief rows of different columns would zigzag).
        if st.bg.is_none()
            && geo.mode != ViewMode::Brief
            && idx % 2 == 1
            && let Some(alt) = p.panel.bg_alt
        {
            st.bg = Some(alt);
        }
        if let Some(bg) = &st.bg {
            self.fill(bg, cell);
        }
        if cs == CursorState::Inactive
            && let Some(o) = p.cursor.inactive_outline
        {
            self.outline(&o, cell, 1.0);
        }
        match geo.mode {
            ViewMode::Brief => {
                let text = format::name_cell(e);
                self.text(
                    &text,
                    st.bold,
                    false,
                    x + PAD,
                    y,
                    cell.w - 2.0 * PAD,
                    &st.fg,
                );
            }
            ViewMode::Full | ViewMode::Custom => {
                for slot in &geo.slots {
                    let text = format::cell_text(
                        e,
                        slot.column,
                        self.settings.size_format,
                        &self.settings.time_format,
                    );
                    self.text(
                        &text,
                        st.bold,
                        slot.column.right_aligned(),
                        x + slot.x + PAD,
                        y,
                        slot.w - 2.0 * PAD,
                        &st.fg,
                    );
                }
            }
        }
    }

    // ---- frame --------------------------------------------------------------------------------
    fn paint_frame(&self) {
        let geo = self.geo;
        let frames = self.palette.theme.retro.frames;
        if frames == Frames::None {
            return;
        }
        let c = self.palette.panel.frame;
        let whole = Rect {
            x: 0.0,
            y: 0.0,
            w: geo.width,
            h: geo.height,
        };
        let inset = |d: f32| Rect {
            x: d,
            y: d,
            w: (whole.w - 2.0 * d).max(0.0),
            h: (whole.h - 2.0 * d).max(0.0),
        };
        match frames {
            Frames::None => {}
            Frames::Hairline => self.outline(&c, whole, 1.0),
            Frames::Single => self.outline(&c, inset(1.0), 1.0),
            Frames::Double => {
                self.outline(&c, inset(1.0), 1.0);
                self.outline(&c, inset(4.0), 1.0);
            }
        }
        // Line under the header and between columns.
        let ct = geo.content;
        self.fill(
            &c,
            Rect {
                x: ct.x,
                y: geo.body.y - 1.0,
                w: ct.w,
                h: 1.0,
            },
        );
        let sep = |x: f32| {
            self.fill(
                &c,
                Rect {
                    x: x.floor(),
                    y: ct.y,
                    w: 1.0,
                    h: ct.h,
                },
            );
        };
        match geo.mode {
            ViewMode::Brief => {
                for i in 1..geo.grid.cols {
                    sep(ct.x + i as f32 * geo.cell_w);
                }
            }
            _ => {
                for s in geo.slots.iter().skip(1) {
                    sep(ct.x + s.x);
                }
            }
        }
    }
}

fn sort_title(key: SortKey) -> &'static str {
    match key {
        SortKey::Name => tr("Name"),
        SortKey::Ext => tr("Ext"),
        SortKey::Size => tr("Size"),
        SortKey::Mtime => tr("Modified"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contrast_ratio_extremes() {
        let black = RGBA::new(0.0, 0.0, 0.0, 1.0);
        let white = RGBA::new(1.0, 1.0, 1.0, 1.0);
        assert!((contrast(&black, &white) - 21.0).abs() < 0.01);
        assert!((contrast(&white, &white) - 1.0).abs() < 0.001);
        // Yellow on cyan (Far Blue marked-on-cursor) is not readable.
        let yellow = RGBA::new(1.0, 1.0, 0.0, 1.0);
        let teal = RGBA::new(0.0, 0.5, 0.5, 1.0);
        assert!(contrast(&yellow, &teal) < 7.0);
    }
}

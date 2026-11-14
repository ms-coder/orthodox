//! Geometry of one frame of the list: where the frame lines, header, columns and rows are, and
//! which entry sits under a point. Pure (no GTK), so hit testing is unit-tested.

use ofm_config::theme::Frames;

use super::layout::{ColSlot, Grid, column_slots};
use super::settings::{Column, ViewMode};

/// Horizontal padding inside a cell, px.
pub const PAD: f32 = 4.0;
/// Minimum width of a Brief column, in characters.
const BRIEF_MIN_CHARS: f32 = 24.0;
/// Minimum width of the name column in Full/Custom mode, in characters.
const MIN_NAME_CHARS: f32 = 14.0;

/// Font-derived sizes (px, logical).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    /// Row height (text height + vertical padding, whole px).
    pub row_h: f32,
    /// Advance of one character.
    pub char_w: f32,
    /// Height of the text itself.
    pub text_h: f32,
}

impl Default for Metrics {
    fn default() -> Self {
        Metrics {
            row_h: 18.0,
            char_w: 8.0,
            text_h: 16.0,
        }
    }
}

/// Axis-aligned rectangle in widget coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    /// Left.
    pub x: f32,
    /// Top.
    pub y: f32,
    /// Width.
    pub w: f32,
    /// Height.
    pub h: f32,
}

impl Rect {
    fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// Space the frame takes on every side (lines included), px.
pub fn frame_inset(f: Frames) -> f32 {
    match f {
        Frames::None => 0.0,
        Frames::Hairline => 1.0,
        Frames::Single => 4.0,
        Frames::Double => 7.0,
    }
}

/// What a point hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Header of a Full/Custom column.
    HeaderColumn(Column),
    /// A Brief-mode header (sorts by name).
    HeaderBrief,
    /// A visible row.
    Row(u32),
    /// Empty area (frame, below the last entry, ...).
    Blank,
}

/// Layout of the list for a given size, mode and font.
#[derive(Debug, Clone, PartialEq)]
pub struct Geo {
    /// Widget width.
    pub width: f32,
    /// Widget height.
    pub height: f32,
    /// View mode this was computed for.
    pub mode: ViewMode,
    /// Area inside the frame (header + body).
    pub content: Rect,
    /// Header band.
    pub header: Rect,
    /// Area of the rows.
    pub body: Rect,
    /// Row height.
    pub row_h: f32,
    /// Cell grid (`cols == 1` in Full/Custom).
    pub grid: Grid,
    /// Width of one cell column (Brief), or the whole content width (Full/Custom).
    pub cell_w: f32,
    /// Full/Custom columns (empty in Brief).
    pub slots: Vec<ColSlot>,
}

impl Geo {
    /// Compute the layout.
    pub fn compute(
        width: f32,
        height: f32,
        m: &Metrics,
        mode: ViewMode,
        columns: &[Column],
        frames: Frames,
    ) -> Geo {
        let inset = frame_inset(frames);
        let content = Rect {
            x: inset,
            y: inset,
            w: (width - 2.0 * inset).max(0.0),
            h: (height - 2.0 * inset).max(0.0),
        };
        let header_h = m.row_h.min(content.h);
        let header = Rect {
            h: header_h,
            ..content
        };
        let body = Rect {
            x: content.x,
            y: content.y + header_h,
            w: content.w,
            h: (content.h - header_h).max(0.0),
        };
        let (cols, slots) = match mode {
            ViewMode::Brief => (
                Grid::brief_columns(content.w, BRIEF_MIN_CHARS * m.char_w),
                Vec::new(),
            ),
            ViewMode::Full | ViewMode::Custom => (
                1,
                column_slots(content.w, m.char_w, PAD, columns, MIN_NAME_CHARS * m.char_w),
            ),
        };
        let grid = Grid::new(body.h, m.row_h, cols);
        Geo {
            width,
            height,
            mode,
            content,
            header,
            body,
            row_h: m.row_h,
            grid,
            cell_w: content.w / cols as f32,
            slots,
        }
    }

    /// Top-left of the cell `(col, row)`.
    pub fn cell_origin(&self, col: u32, row: u32) -> (f32, f32) {
        (
            self.content.x + col as f32 * self.cell_w,
            self.body.y + row as f32 * self.row_h,
        )
    }

    /// What is at `(x, y)` when the window starts at entry `first` of a list of `len`.
    pub fn hit(&self, x: f32, y: f32, first: u32, len: u32) -> Hit {
        if self.header.contains(x, y) {
            return match self.mode {
                ViewMode::Brief => Hit::HeaderBrief,
                _ => {
                    let rel = x - self.content.x;
                    self.slots
                        .iter()
                        .find(|s| rel >= s.x && rel < s.x + s.w)
                        .map_or(Hit::Blank, |s| Hit::HeaderColumn(s.column))
                }
            };
        }
        if !self.body.contains(x, y) || self.cell_w <= 0.0 || self.row_h <= 0.0 {
            return Hit::Blank;
        }
        let col = ((x - self.content.x) / self.cell_w) as u32;
        let row = ((y - self.body.y) / self.row_h) as u32;
        self.grid
            .index_of(first, col, row, len)
            .map_or(Hit::Blank, Hit::Row)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    const M: Metrics = Metrics {
        row_h: 20.0,
        char_w: 10.0,
        text_h: 16.0,
    };

    #[test]
    fn brief_grid_and_hit() {
        // 800 x 220, no frame: header 20, body 200 -> 10 rows; 800 / 240 -> 3 columns.
        let g = Geo::compute(800.0, 220.0, &M, ViewMode::Brief, &[], Frames::None);
        assert_eq!(g.grid, Grid { rows: 10, cols: 3 });
        assert!((g.cell_w - 800.0 / 3.0).abs() < 0.01);
        assert_eq!(g.hit(5.0, 5.0, 0, 100), Hit::HeaderBrief);
        // Column 1 (x = 300), row 2 (y = 20 + 2*20 + 3).
        assert_eq!(g.hit(300.0, 63.0, 0, 100), Hit::Row(12));
        // Scrolled by one column.
        assert_eq!(g.hit(300.0, 63.0, 10, 100), Hit::Row(22));
        // Past the last entry.
        assert_eq!(g.hit(300.0, 63.0, 0, 11), Hit::Blank);
        // Below the grid.
        assert_eq!(g.hit(10.0, 219.0, 0, 100), Hit::Row(9));
        assert_eq!(g.hit(10.0, 400.0, 0, 100), Hit::Blank);
    }

    #[test]
    fn full_columns_and_header_hits() {
        let cols = [Column::Name, Column::Size, Column::Mtime];
        let g = Geo::compute(800.0, 220.0, &M, ViewMode::Full, &cols, Frames::None);
        assert_eq!(g.grid.cols, 1);
        assert_eq!(g.hit(10.0, 10.0, 0, 5), Hit::HeaderColumn(Column::Name));
        let size = g.slots[1];
        assert_eq!(
            g.hit(size.x + 5.0, 10.0, 0, 5),
            Hit::HeaderColumn(Column::Size)
        );
        assert_eq!(g.hit(10.0, 25.0, 0, 5), Hit::Row(0));
        assert_eq!(g.hit(790.0, 25.0 + 40.0, 0, 5), Hit::Row(2));
        assert_eq!(g.hit(10.0, 25.0 + 100.0, 0, 5), Hit::Blank);
    }

    #[test]
    fn frames_shrink_the_content() {
        let none = Geo::compute(400.0, 300.0, &M, ViewMode::Full, &[], Frames::None);
        let dbl = Geo::compute(400.0, 300.0, &M, ViewMode::Full, &[], Frames::Double);
        assert_eq!(none.content.w, 400.0);
        assert_eq!(dbl.content.w, 400.0 - 14.0);
        assert!(dbl.grid.rows <= none.grid.rows);
        // The frame itself is a blank hit.
        assert_eq!(dbl.hit(2.0, 100.0, 0, 50), Hit::Blank);
    }

    #[test]
    fn tiny_sizes_do_not_panic() {
        for (w, h) in [(0.0, 0.0), (5.0, 5.0), (40.0, 10.0)] {
            for mode in [ViewMode::Brief, ViewMode::Full, ViewMode::Custom] {
                let g = Geo::compute(w, h, &M, mode, &[Column::Name], Frames::Double);
                assert!(g.grid.rows >= 1 && g.grid.cols >= 1);
                let _ = g.hit(1.0, 1.0, 0, 3);
            }
        }
    }
}

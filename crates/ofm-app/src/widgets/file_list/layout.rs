//! Pure layout math of the list: column-major grid, scrolling, column widths. No GTK here, so it
//! is unit-testable headless.
//!
//! The list is a grid of `cols` columns of `rows` cells each, filled **column-major**: the entry
//! at visible index `first + c * rows + r` sits in column `c`, row `r`. Brief mode has
//! `cols >= 1`; Full and Custom are the `cols == 1` case (so one code path covers all three).

use super::settings::Column;

/// Grid of cells for the visible window of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Grid {
    /// Rows per column (>= 1).
    pub rows: u32,
    /// Number of columns (>= 1).
    pub cols: u32,
}

impl Grid {
    /// Grid for a body of `height` px with rows of `row_h` px. `cols` is chosen by the caller.
    pub fn new(height: f32, row_h: f32, cols: u32) -> Grid {
        let rows = if row_h > 0.0 {
            (height / row_h).floor().max(1.0) as u32
        } else {
            1
        };
        Grid {
            rows,
            cols: cols.max(1),
        }
    }

    /// Number of brief columns that fit `width` when each needs at least `min_col_w` px.
    pub fn brief_columns(width: f32, min_col_w: f32) -> u32 {
        if min_col_w <= 0.0 {
            return 1;
        }
        (width / min_col_w).floor().max(1.0) as u32
    }

    /// Entries shown at once.
    pub fn page(&self) -> u32 {
        self.rows * self.cols
    }

    /// Cell `(col, row)` of visible index `idx` when the window starts at `first`.
    pub fn cell_of(&self, first: u32, idx: u32) -> Option<(u32, u32)> {
        let d = idx.checked_sub(first)?;
        let col = d / self.rows;
        (col < self.cols).then_some((col, d % self.rows))
    }

    /// Visible index shown in cell `(col, row)`, if it exists (`< len`).
    pub fn index_of(&self, first: u32, col: u32, row: u32, len: u32) -> Option<u32> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let idx = first
            .checked_add(col.checked_mul(self.rows)?)?
            .checked_add(row)?;
        (idx < len).then_some(idx)
    }

    /// Alignment of `first`: whole columns in multi-column grids, single rows otherwise.
    fn align(&self, first: u32) -> u32 {
        if self.cols > 1 {
            first - first % self.rows
        } else {
            first
        }
    }

    /// Largest useful `first` (the last page is full when possible).
    fn max_first(&self, len: u32) -> u32 {
        if len == 0 {
            return 0;
        }
        if self.cols > 1 {
            let last_col = (len - 1) / self.rows;
            last_col.saturating_sub(self.cols - 1) * self.rows
        } else {
            len.saturating_sub(self.rows)
        }
    }

    /// `first` made valid for a list of `len` entries (aligned and not scrolled past the end).
    pub fn clamp_first(&self, first: u32, len: u32) -> u32 {
        self.align(first).min(self.max_first(len))
    }

    /// Scroll by `delta` units (rows when `cols == 1`, columns otherwise), clamped.
    pub fn scroll(&self, first: u32, delta: i32, len: u32) -> u32 {
        let unit = if self.cols > 1 { self.rows } else { 1 };
        let target = i64::from(first) + i64::from(delta) * i64::from(unit);
        let target = target.clamp(0, i64::from(self.max_first(len)));
        self.clamp_first(target as u32, len)
    }

    /// Smallest scroll from `first` that makes `cursor` visible.
    pub fn reveal(&self, first: u32, cursor: u32, len: u32) -> u32 {
        let first = self.clamp_first(first, len);
        if cursor < first {
            return self.align(cursor);
        }
        if cursor < first + self.page() {
            return first;
        }
        let new = if self.cols > 1 {
            let col = cursor / self.rows;
            (col + 1 - self.cols) * self.rows
        } else {
            cursor + 1 - self.rows
        };
        self.clamp_first(new, len)
    }
}

/// One laid-out column of Full/Custom mode: x offset (from the left of the row) and width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ColSlot {
    /// Which data.
    pub column: Column,
    /// Left edge in px.
    pub x: f32,
    /// Width in px.
    pub w: f32,
}

/// Lay `cols` out over `width` px: fixed-width columns keep their width, the name column takes
/// the rest (a fixed column is `width_chars` wide plus `pad` px on both sides). Trailing columns are dropped while the name column would be narrower than
/// `min_name_w`. The first column is always shown.
pub fn column_slots(
    width: f32,
    char_w: f32,
    pad: f32,
    cols: &[Column],
    min_name_w: f32,
) -> Vec<ColSlot> {
    let mut shown: Vec<Column> = cols.to_vec();
    if shown.is_empty() {
        shown.push(Column::Name);
    }
    let fixed = |c: &Column| -> f32 {
        if *c == Column::Name {
            0.0
        } else {
            c.width_chars() as f32 * char_w + 2.0 * pad
        }
    };
    while shown.len() > 1 {
        let used: f32 = shown.iter().map(fixed).sum();
        let has_name = shown.contains(&Column::Name);
        if !has_name || width - used >= min_name_w {
            break;
        }
        shown.pop();
    }
    let used: f32 = shown.iter().map(fixed).sum();
    let flexible = shown.iter().filter(|c| **c == Column::Name).count().max(1) as f32;
    let name_w = ((width - used) / flexible).max(0.0);
    let mut x = 0.0;
    shown
        .into_iter()
        .map(|column| {
            let w = if column == Column::Name {
                name_w
            } else {
                fixed(&column)
            };
            let slot = ColSlot { column, x, w };
            x += w;
            slot
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn g(rows: u32, cols: u32) -> Grid {
        Grid { rows, cols }
    }

    #[test]
    fn rows_from_height() {
        assert_eq!(Grid::new(100.0, 20.0, 3), g(5, 3));
        assert_eq!(Grid::new(5.0, 20.0, 0), g(1, 1));
        assert_eq!(Grid::brief_columns(500.0, 200.0), 2);
        assert_eq!(Grid::brief_columns(50.0, 200.0), 1);
    }

    #[test]
    fn cell_index_round_trip_column_major() {
        let grid = g(4, 3);
        // first = 0: idx 5 is column 1, row 1.
        assert_eq!(grid.cell_of(0, 5), Some((1, 1)));
        assert_eq!(grid.index_of(0, 1, 1, 100), Some(5));
        // Scrolled by one column.
        assert_eq!(grid.cell_of(4, 5), Some((0, 1)));
        assert_eq!(grid.cell_of(4, 3), None, "before the window");
        assert_eq!(grid.cell_of(0, 12), None, "after the window");
        for idx in 0..12 {
            let (c, r) = grid.cell_of(0, idx).unwrap();
            assert_eq!(grid.index_of(0, c, r, 100), Some(idx));
        }
        // Past the end of the list.
        assert_eq!(grid.index_of(0, 2, 3, 11), None);
        assert_eq!(grid.index_of(0, 3, 0, 100), None);
    }

    #[test]
    fn single_column_is_plain_rows() {
        let grid = g(10, 1);
        assert_eq!(grid.cell_of(7, 9), Some((0, 2)));
        assert_eq!(grid.scroll(0, 3, 100), 3);
        assert_eq!(grid.scroll(3, -9, 100), 0);
        assert_eq!(grid.scroll(0, 1000, 100), 90, "last page stays full");
        assert_eq!(grid.clamp_first(5, 8), 0, "short list never scrolls");
    }

    #[test]
    fn brief_scrolls_whole_columns() {
        let grid = g(4, 3);
        assert_eq!(grid.scroll(0, 1, 100), 4);
        assert_eq!(grid.scroll(0, 2, 100), 8);
        assert_eq!(grid.clamp_first(6, 100), 4, "aligned to a column");
        // 10 entries -> 3 columns total -> never scroll.
        assert_eq!(grid.scroll(0, 5, 10), 0);
        // 20 entries -> 5 columns, 3 shown -> max first column 2.
        assert_eq!(grid.scroll(0, 9, 20), 8);
    }

    #[test]
    fn reveal_cursor() {
        let grid = g(4, 3);
        assert_eq!(grid.reveal(0, 5, 100), 0);
        assert_eq!(grid.reveal(0, 12, 100), 4, "cursor in 4th column");
        assert_eq!(grid.reveal(8, 1, 100), 0);
        let col = g(10, 1);
        assert_eq!(col.reveal(0, 10, 100), 1);
        assert_eq!(col.reveal(50, 49, 100), 49);
        assert_eq!(col.reveal(0, 99, 100), 90);
        assert_eq!(col.reveal(5, 7, 100), 5);
    }

    #[test]
    fn empty_list_is_safe() {
        let grid = g(5, 2);
        assert_eq!(grid.clamp_first(7, 0), 0);
        assert_eq!(grid.scroll(0, 3, 0), 0);
        assert_eq!(grid.index_of(0, 0, 0, 0), None);
    }

    #[test]
    fn column_slots_give_name_the_rest() {
        let cols = [Column::Name, Column::Size, Column::Mtime];
        let slots = column_slots(1000.0, 10.0, 0.0, &cols, 120.0);
        assert_eq!(slots.len(), 3);
        let size_w = Column::Size.width_chars() as f32 * 10.0;
        let mtime_w = Column::Mtime.width_chars() as f32 * 10.0;
        assert_eq!(slots[0].x, 0.0);
        assert_eq!(slots[0].w, 1000.0 - size_w - mtime_w);
        assert_eq!(slots[1].x, slots[0].w);
        assert_eq!(slots[2].x + slots[2].w, 1000.0);
    }

    #[test]
    fn column_slots_drop_trailing_when_narrow() {
        let cols = [Column::Name, Column::Size, Column::Mtime, Column::Mode];
        let slots = column_slots(300.0, 10.0, 0.0, &cols, 120.0);
        assert!(slots.len() < 4);
        assert_eq!(slots[0].column, Column::Name);
        assert!(slots[0].w >= 120.0 || slots.len() == 1);
        let last = slots[slots.len() - 1];
        assert!((last.x + last.w - 300.0).abs() < 0.001);
        // Never empty, even without a name column or any space.
        assert_eq!(column_slots(10.0, 10.0, 4.0, &[], 120.0).len(), 1);
        assert_eq!(
            column_slots(10.0, 10.0, 4.0, &[Column::Size], 120.0).len(),
            1
        );
    }
}

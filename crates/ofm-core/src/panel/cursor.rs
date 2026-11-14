//! Cursor movement requests.

/// Search / step direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards the start of the list.
    Backward,
    /// Towards the end of the list.
    Forward,
}

/// A cursor movement; the view supplies geometry (rows per page / column).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorMove {
    /// Move by `n` rows (negative = up), clamped.
    By(i32),
    /// Move by `pages` pages of `rows` rows (negative = up), clamped.
    Page {
        /// Rows per page as currently displayed.
        rows: u32,
        /// Pages (signed).
        pages: i32,
    },
    /// Brief mode: move by `columns` columns of `rows_per_column` rows (negative = left), clamped.
    Column {
        /// Rows per column as currently displayed.
        rows_per_column: u32,
        /// Columns (signed).
        columns: i32,
    },
    /// First visible entry.
    Home,
    /// Last visible entry.
    End,
}

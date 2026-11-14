//! Panel model: entries, sort, filter, cursor, selection, quick search. Pure logic, no I/O.
//! Owned by T1.1 (the whole `panel/` directory; split into more files freely, but keep the
//! public API below source-compatible).

mod change;
mod cursor;
mod model;
mod sort;

#[cfg(test)]
mod proptests;

pub use change::ModelChange;
pub use cursor::{CursorMove, Direction};
pub use model::PanelModel;
pub use sort::{SortKey, SortSpec, natural_cmp};

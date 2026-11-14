//! Sorting: natural order, keys, spec.

use std::cmp::Ordering;
use std::ffi::OsStr;

/// Column to sort by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SortKey {
    /// Natural name order (`file2` < `file10`).
    #[default]
    Name,
    /// Extension, then name.
    Ext,
    /// Size, then name.
    Size,
    /// Modification time, then name.
    Mtime,
}

/// Sort configuration of a panel. `..` is always first regardless of this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SortSpec {
    /// Key.
    pub key: SortKey,
    /// Descending order.
    pub reverse: bool,
    /// Directories before files.
    pub dirs_first: bool,
}

impl Default for SortSpec {
    fn default() -> Self {
        SortSpec {
            key: SortKey::Name,
            reverse: false,
            dirs_first: true,
        }
    }
}

/// Natural, case-insensitive-first comparison of file names (digit runs compare numerically;
/// ties broken by case-sensitive byte order so the result is a total order). Works on bytes.
pub fn natural_cmp(_a: &OsStr, _b: &OsStr) -> Ordering {
    todo!("T1.1")
}

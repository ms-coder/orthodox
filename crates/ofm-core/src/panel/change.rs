//! Incremental change notifications from the model to the view.

/// What changed in the *visible* (sorted + filtered) list; the UI applies it incrementally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChange {
    /// Nothing visible changed.
    None,
    /// Everything changed (new directory, re-sort, filter change): re-read the whole view.
    Reset,
    /// `removed` rows at `pos` were replaced by `added` rows.
    Spliced {
        /// First affected visible index.
        pos: u32,
        /// Number of rows removed.
        removed: u32,
        /// Number of rows inserted.
        added: u32,
    },
}

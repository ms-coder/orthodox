//! `PanelModel`: the testable heart of a file panel.

use std::ffi::{OsStr, OsString};

use super::{CursorMove, Direction, ModelChange, SortSpec};
use crate::entry::Entry;
use crate::mask::Mask;

/// Entries + sorted/filtered view + cursor + selection + quick search for one panel (tab).
///
/// Invariants (T1.1 proptests): the visible order is a total order consistent with `SortSpec`
/// (`..` first); `cursor()` is always `< visible_len()` or `None` iff the view is empty; selection is
/// stored by name so it survives reloads and re-sorts; selected names that vanish are dropped on
/// `finish_load`; hidden files never appear in the view unless `show_hidden`.
/// "Visible index" below = index into the sorted, filtered view.
#[derive(Debug, Default)]
#[allow(dead_code, reason = "T1.1 fills in the fields")]
pub struct PanelModel {
    entries: Vec<Entry>,
    visible: Vec<u32>,
}

impl PanelModel {
    /// Empty model (no `..`).
    pub fn new() -> Self {
        Self::default()
    }

    // ---- loading -------------------------------------------------------------------------------
    /// Start loading a new directory: clears entries, keeps sort/hidden/filter settings, remembers
    /// the previous selection for `restore_selection`. `has_parent` adds the synthetic `..` row.
    pub fn begin_load(&mut self, _has_parent: bool) -> ModelChange {
        todo!("T1.1")
    }
    /// Append a streamed batch (`ListEvent::Batch`); returns the minimal change of the view.
    pub fn apply_batch(&mut self, _batch: Vec<Entry>) -> ModelChange {
        todo!("T1.1")
    }
    /// Listing finished: drop selection names that no longer exist.
    pub fn finish_load(&mut self) -> ModelChange {
        todo!("T1.1")
    }
    /// Apply a watcher update: upsert `changed` entries (by name), remove `removed` names.
    pub fn apply_update(&mut self, _changed: Vec<Entry>, _removed: &[OsString]) -> ModelChange {
        todo!("T1.1")
    }

    // ---- view ----------------------------------------------------------------------------------
    /// Number of rows in the view.
    pub fn visible_len(&self) -> u32 {
        todo!("T1.1")
    }
    /// Entry at a visible index.
    pub fn visible(&self, _idx: u32) -> Option<&Entry> {
        todo!("T1.1")
    }
    /// Number of entries before filtering (excluding `..`).
    pub fn total_len(&self) -> usize {
        todo!("T1.1")
    }
    /// Current sort.
    pub fn sort_spec(&self) -> SortSpec {
        todo!("T1.1")
    }
    /// Change the sort; the cursor stays on the same name.
    pub fn set_sort(&mut self, _spec: SortSpec) -> ModelChange {
        todo!("T1.1")
    }
    /// Show or hide dot-files (and `.hidden`-listed ones are out of scope).
    pub fn set_show_hidden(&mut self, _show: bool) -> ModelChange {
        todo!("T1.1")
    }
    /// Whether dot-files are shown.
    pub fn show_hidden(&self) -> bool {
        todo!("T1.1")
    }
    /// Set or clear the filter-as-you-type text (case-insensitive substring).
    pub fn set_filter(&mut self, _text: Option<&str>) -> ModelChange {
        todo!("T1.1")
    }

    // ---- cursor --------------------------------------------------------------------------------
    /// Cursor as a visible index; `None` iff the view is empty.
    pub fn cursor(&self) -> Option<u32> {
        todo!("T1.1")
    }
    /// Entry under the cursor.
    pub fn cursor_entry(&self) -> Option<&Entry> {
        todo!("T1.1")
    }
    /// Move the cursor; returns the new index.
    pub fn move_cursor(&mut self, _mv: CursorMove) -> Option<u32> {
        todo!("T1.1")
    }
    /// Put the cursor on `name` (e.g. the dir we came from); false if not visible.
    pub fn set_cursor_name(&mut self, _name: &OsStr) -> bool {
        todo!("T1.1")
    }
    /// Put the cursor on a visible index (clamped).
    pub fn set_cursor(&mut self, _idx: u32) {
        todo!("T1.1")
    }

    // ---- selection -----------------------------------------------------------------------------
    /// Toggle selection of the entry at `idx` (`..` cannot be selected).
    pub fn toggle_select(&mut self, _idx: u32) {
        todo!("T1.1")
    }
    /// Set selection state of the inclusive visible range `a..=b`.
    pub fn select_range(&mut self, _a: u32, _b: u32, _selected: bool) {
        todo!("T1.1")
    }
    /// Select (`true`) or deselect (`false`) every visible non-`..` entry matching `mask`
    /// (directories are matched too unless the caller filters; Far selects files only by default:
    /// `files_only`).
    pub fn select_by_mask(&mut self, _mask: &Mask, _selected: bool, _files_only: bool) {
        todo!("T1.1")
    }
    /// Invert selection of all visible non-`..` entries.
    pub fn invert_selection(&mut self) {
        todo!("T1.1")
    }
    /// Select or clear everything visible.
    pub fn select_all(&mut self, _selected: bool) {
        todo!("T1.1")
    }
    /// Re-apply the selection that was active before the last clear/begin_load.
    pub fn restore_selection(&mut self) {
        todo!("T1.1")
    }
    /// Is the entry at `idx` selected?
    pub fn is_selected(&self, _idx: u32) -> bool {
        todo!("T1.1")
    }
    /// Number of selected entries (including ones currently filtered out).
    pub fn selected_count(&self) -> usize {
        todo!("T1.1")
    }
    /// Sum of sizes of selected files (directories count 0 unless a size is known).
    pub fn selected_size(&self) -> u64 {
        todo!("T1.1")
    }
    /// Selected names in view order; if none are selected callers use the cursor entry.
    pub fn selected_names(&self) -> Vec<OsString> {
        todo!("T1.1")
    }

    // ---- quick search --------------------------------------------------------------------------
    /// Jump to the first entry matching `query` at or after the cursor: prefix matches first, then
    /// substring, case-insensitive; wraps around. Returns the matched visible index.
    pub fn quick_search(&mut self, _query: &str) -> Option<u32> {
        todo!("T1.1")
    }
    /// Next/previous match of the same query (Ctrl+Down / Ctrl+Up).
    pub fn quick_search_step(&mut self, _query: &str, _dir: Direction) -> Option<u32> {
        todo!("T1.1")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T1.1"]
    fn model_invariants() {}
}

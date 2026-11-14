//! `PanelModel`: the testable heart of a file panel.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};

use super::ModelChange;
use super::cursor::{CursorMove, Direction};
use super::sort::{SortSpec, cmp_entries};
use super::sort_ids::sort_ids;
use crate::entry::{Entry, EntryKind};
use crate::mask::Mask;

/// Sentinel visible-row id for the synthetic `..` entry (real entries use their index).
const PARENT: u32 = u32::MAX;

fn parent_name() -> &'static OsStr {
    OsStr::new("..")
}

/// Entries + sorted/filtered view + cursor + selection + quick search for one panel (tab).
///
/// Invariants (T1.1 proptests): the visible order is a total order consistent with `SortSpec`
/// (`..` first); `cursor()` is always `< visible_len()` or `None` iff the view is empty; selection is
/// stored by name so it survives reloads and re-sorts; selected names that vanish are dropped on
/// `finish_load`; hidden files never appear in the view unless `show_hidden`.
/// "Visible index" below = index into the sorted, filtered view.
///
/// Additional semantics fixed by T1.1:
/// - Hidden means "name starts with a dot" (`.hidden` files are out of scope for v1.0).
/// - The filter and quick search compare ASCII-case-insensitively; non-ASCII bytes match exactly.
/// - Selection-mutating operations snapshot the previous selection first whenever they can
///   *remove* names, so [`PanelModel::restore_selection`] undoes the last deselect-like operation
///   (clear, mask-deselect, invert, toggle-off, and a same-directory `begin_load`). Navigating to
///   another directory (`begin_load(_, false)`) drops both the selection and the snapshot.
#[derive(Debug, Default)]
pub struct PanelModel {
    entries: Vec<Entry>,
    /// name -> index into `entries` (no `..`).
    index: HashMap<OsString, u32>,
    /// Sorted, filtered view: indices into `entries`, or [`PARENT`] pinned first.
    visible: Vec<u32>,
    /// The synthetic `..` row, present between `begin_load(true, false)` and the next `begin_load`.
    parent_entry: Option<Entry>,
    sort: SortSpec,
    show_hidden: bool,
    /// Filter-as-you-type text (lowercase comparison happens on the fly).
    filter: Option<String>,
    cursor: Option<u32>,
    /// Name under the cursor; keeps the cursor stable across re-sorts/re-filters/batches.
    cursor_name: Option<OsString>,
    selection: HashSet<OsString>,
    last_selection: Option<HashSet<OsString>>,
}

impl PanelModel {
    /// Empty model (no `..`).
    pub fn new() -> Self {
        Self::default()
    }

    // ---- loading -------------------------------------------------------------------------------
    /// Start loading a directory: clears entries, keeps sort/hidden/filter settings.
    /// `has_parent` adds the synthetic `..` row.
    ///
    /// `keep_selection` must be `true` only when *reloading the same directory*: the selection is
    /// kept by name (and a non-empty one is snapshotted for `restore_selection`). When navigating to
    /// a different directory pass `false`: the selection and the restore snapshot are discarded, so
    /// same-named files in the new directory are never selected by accident.
    pub fn begin_load(&mut self, has_parent: bool, keep_selection: bool) -> ModelChange {
        if keep_selection {
            // An empty selection must not overwrite an earlier snapshot.
            if !self.selection.is_empty() {
                self.snapshot_selection();
            }
        } else {
            self.selection.clear();
            self.last_selection = None;
        }
        self.entries.clear();
        self.index.clear();
        // The parent row is visible from the start, even before the first batch arrives.
        self.visible = if has_parent { vec![PARENT] } else { Vec::new() };
        self.parent_entry = has_parent.then(|| Entry::new("..", EntryKind::Dir));
        self.cursor = if has_parent { Some(0) } else { None };
        self.cursor_name = None;
        ModelChange::Reset
    }

    /// Append a streamed batch (`ListEvent::Batch`); returns the minimal change of the view.
    /// Duplicate names (which a conforming VFS never sends) are ignored.
    pub fn apply_batch(&mut self, batch: Vec<Entry>) -> ModelChange {
        let old = std::mem::take(&mut self.visible);
        let mut fresh: Vec<u32> = Vec::with_capacity(batch.len());
        for entry in batch {
            if self.index.contains_key(&entry.name) {
                continue;
            }
            let id = self.entries.len() as u32;
            self.index.insert(entry.name.clone(), id);
            self.entries.push(entry);
            if self.passes_view(&self.entries[id as usize]) {
                fresh.push(id);
            }
        }
        if fresh.is_empty() {
            self.visible = old;
            return ModelChange::None;
        }
        sort_ids(&self.sort, &self.entries, &mut fresh);
        // Merge the sorted batch into the sorted view (streaming must not re-sort everything).
        let has_parent = self.parent_entry.is_some();
        let old_real = real_slice(&old, has_parent);
        let mut merged: Vec<u32> = Vec::with_capacity(old_real.len() + fresh.len());
        if has_parent {
            merged.push(PARENT);
        }
        let (mut a, mut b) = (0usize, 0usize);
        while a < old_real.len() && b < fresh.len() {
            // `Equal` is impossible: fresh ids are all larger than any id in `old`.
            if cmp_entries(
                &self.sort,
                &self.entries[old_real[a] as usize],
                &self.entries[fresh[b] as usize],
            ) != Ordering::Greater
            {
                merged.push(old_real[a]);
                a += 1;
            } else {
                merged.push(fresh[b]);
                b += 1;
            }
        }
        merged.extend_from_slice(&old_real[a..]);
        merged.extend_from_slice(&fresh[b..]);
        self.visible = merged;
        self.fix_cursor();
        splice_diff(&old, &self.visible, |a, b| a == b)
    }

    /// Listing finished: drop selection names that no longer exist.
    pub fn finish_load(&mut self) -> ModelChange {
        self.selection.retain(|name| self.index.contains_key(name));
        self.fix_cursor();
        ModelChange::None
    }

    /// Apply a watcher update: upsert `changed` entries (by name), remove `removed` names.
    pub fn apply_update(&mut self, changed: Vec<Entry>, removed: &[OsString]) -> ModelChange {
        if changed.is_empty() && removed.is_empty() {
            return ModelChange::None;
        }
        // Entry ids are reshuffled by removals, so diff by name, not by id.
        let old: Vec<OsString> = self
            .visible
            .iter()
            .map(|&vi| self.name_at(vi).to_owned())
            .collect();
        let changed_names: HashSet<OsString> = changed.iter().map(|e| e.name.clone()).collect();
        if !removed.is_empty() {
            let removed_set: HashSet<&OsString> = removed.iter().collect();
            let mut touched = false;
            self.entries.retain(|e| {
                let keep = !removed_set.contains(&e.name);
                touched |= !keep;
                keep
            });
            if touched {
                self.index.clear();
                for (i, e) in self.entries.iter().enumerate() {
                    self.index.insert(e.name.clone(), i as u32);
                }
            }
            for name in removed {
                self.index.remove(name);
                self.selection.remove(name);
            }
        }
        for entry in changed {
            match self.index.get(&entry.name) {
                Some(&i) => self.entries[i as usize] = entry,
                None => {
                    let id = self.entries.len() as u32;
                    self.index.insert(entry.name.clone(), id);
                    self.entries.push(entry);
                }
            }
        }
        self.rebuild_visible();
        self.fix_cursor();
        let new: Vec<OsString> = self
            .visible
            .iter()
            .map(|&vi| self.name_at(vi).to_owned())
            .collect();
        // A row whose entry was replaced in place (size/mtime changed) must be re-read too.
        splice_diff(&old, &new, |a, b| a == b && !changed_names.contains(a))
    }

    // ---- view ----------------------------------------------------------------------------------
    /// Number of rows in the view.
    pub fn visible_len(&self) -> u32 {
        self.visible.len() as u32
    }

    /// Entry at a visible index.
    pub fn visible(&self, idx: u32) -> Option<&Entry> {
        match self.visible.get(idx as usize) {
            Some(&PARENT) => self.parent_entry.as_ref(),
            Some(&i) => self.entries.get(i as usize),
            None => None,
        }
    }

    /// Number of entries before filtering (excluding `..`).
    pub fn total_len(&self) -> usize {
        self.entries.len()
    }

    /// Current sort.
    pub fn sort_spec(&self) -> SortSpec {
        self.sort
    }

    /// Change the sort; the cursor stays on the same name.
    pub fn set_sort(&mut self, spec: SortSpec) -> ModelChange {
        if spec == self.sort {
            return ModelChange::None;
        }
        self.sort = spec;
        self.rebuild_visible();
        self.fix_cursor();
        ModelChange::Reset
    }

    /// Show or hide dot-files (and `.hidden`-listed ones are out of scope).
    pub fn set_show_hidden(&mut self, show: bool) -> ModelChange {
        if show == self.show_hidden {
            return ModelChange::None;
        }
        self.show_hidden = show;
        self.rebuild_visible();
        self.fix_cursor();
        ModelChange::Reset
    }

    /// Whether dot-files are shown.
    pub fn show_hidden(&self) -> bool {
        self.show_hidden
    }

    /// Set or clear the filter-as-you-type text (case-insensitive substring).
    /// An empty text clears the filter.
    pub fn set_filter(&mut self, text: Option<&str>) -> ModelChange {
        let text = text.filter(|t| !t.is_empty());
        if self.filter.as_deref() == text {
            return ModelChange::None;
        }
        self.filter = text.map(str::to_owned);
        self.rebuild_visible();
        self.fix_cursor();
        ModelChange::Reset
    }

    /// The active filter text, if any.
    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    // ---- cursor --------------------------------------------------------------------------------
    /// Cursor as a visible index; `None` iff the view is empty.
    pub fn cursor(&self) -> Option<u32> {
        self.cursor
    }

    /// Entry under the cursor.
    pub fn cursor_entry(&self) -> Option<&Entry> {
        self.cursor.and_then(|i| self.visible(i))
    }

    /// Move the cursor; returns the new index.
    pub fn move_cursor(&mut self, mv: CursorMove) -> Option<u32> {
        let len = self.visible_len();
        if len == 0 {
            self.cursor = None;
            return None;
        }
        let cur = i64::from(self.cursor.unwrap_or(0));
        let target = match mv {
            CursorMove::By(n) => cur + i64::from(n),
            CursorMove::Page { rows, pages } => cur + i64::from(pages) * i64::from(rows.max(1)),
            CursorMove::Column {
                rows_per_column,
                columns,
            } => cur + i64::from(columns) * i64::from(rows_per_column.max(1)),
            CursorMove::Home => 0,
            CursorMove::End => i64::from(len) - 1,
        };
        let clamped = target.clamp(0, i64::from(len) - 1) as u32;
        self.set_cursor(clamped);
        self.cursor
    }

    /// Put the cursor on `name` (e.g. the dir we came from); false if not visible.
    pub fn set_cursor_name(&mut self, name: &OsStr) -> bool {
        let found = self.visible.iter().position(|&vi| self.name_at(vi) == name);
        match found {
            Some(pos) => {
                self.cursor = Some(pos as u32);
                self.cursor_name = Some(name.to_owned());
                true
            }
            None => false,
        }
    }

    /// Put the cursor on a visible index (clamped).
    pub fn set_cursor(&mut self, idx: u32) {
        if self.visible.is_empty() {
            self.cursor = None;
            return;
        }
        let clamped = idx.min(self.visible.len() as u32 - 1);
        self.cursor = Some(clamped);
        self.cursor_name = Some(self.name_at(self.visible[clamped as usize]).to_owned());
    }

    // ---- selection -----------------------------------------------------------------------------
    /// Toggle selection of the entry at `idx` (`..` cannot be selected).
    pub fn toggle_select(&mut self, idx: u32) {
        let name = match self.visible.get(idx as usize) {
            Some(&PARENT) | None => return,
            Some(&vi) => self.entries[vi as usize].name.clone(),
        };
        if self.selection.contains(&name) {
            self.snapshot_selection();
            self.selection.remove(&name);
        } else {
            self.selection.insert(name);
        }
    }

    /// Set selection state of the inclusive visible range `a..=b` (bounds may be given in
    /// either order and are clamped).
    pub fn select_range(&mut self, a: u32, b: u32, selected: bool) {
        if self.visible.is_empty() {
            return;
        }
        let len = self.visible_len();
        let (lo, hi) = if a <= b { (a, b) } else { (b, a) };
        let (lo, hi) = (lo.min(len - 1), hi.min(len - 1));
        let names: Vec<OsString> = (lo..=hi).filter_map(|i| self.row_name(i)).collect();
        self.apply_selection(names, selected);
    }

    /// Select (`true`) or deselect (`false`) every visible non-`..` entry matching `mask`
    /// (directories are matched too unless the caller filters; Far selects files only by default:
    /// `files_only`).
    pub fn select_by_mask(&mut self, mask: &Mask, selected: bool, files_only: bool) {
        let names: Vec<OsString> = self
            .visible
            .iter()
            .filter_map(|&vi| self.row_entry(vi))
            .filter(|e| !files_only || !e.kind.is_dir_like())
            .filter(|e| mask.is_match(&e.name))
            .map(|e| e.name.clone())
            .collect();
        self.apply_selection(names, selected);
    }

    /// Invert selection of all visible non-`..` entries.
    pub fn invert_selection(&mut self) {
        let all: Vec<OsString> = self
            .visible
            .iter()
            .filter_map(|&vi| self.row_entry(vi))
            .map(|e| e.name.clone())
            .collect();
        self.snapshot_selection();
        for name in all {
            if !self.selection.remove(&name) {
                self.selection.insert(name);
            }
        }
    }

    /// Select or clear everything visible.
    pub fn select_all(&mut self, selected: bool) {
        let all: Vec<OsString> = self
            .visible
            .iter()
            .filter_map(|&vi| self.row_entry(vi))
            .map(|e| e.name.clone())
            .collect();
        self.apply_selection(all, selected);
    }

    /// Re-apply the selection that was active before the last clear/begin_load
    /// (names that no longer exist are ignored).
    pub fn restore_selection(&mut self) {
        if let Some(last) = self.last_selection.clone() {
            self.selection = last
                .into_iter()
                .filter(|name| self.index.contains_key(name))
                .collect();
        }
    }

    /// Is the entry at `idx` selected?
    pub fn is_selected(&self, idx: u32) -> bool {
        match self.visible.get(idx as usize) {
            Some(&vi) => match self.row_entry(vi) {
                Some(e) => self.selection.contains(&e.name),
                None => false,
            },
            None => false,
        }
    }

    /// Number of selected entries; covers all selected names exactly like [`Self::selected_names`]
    /// (including filtered-out/hidden ones and names that have not streamed in yet, until
    /// `finish_load` prunes them).
    pub fn selected_count(&self) -> usize {
        self.selection.len()
    }

    /// Sum of sizes of all selected entries known so far (same set as [`Self::selected_names`];
    /// directories count 0 unless a size is known).
    pub fn selected_size(&self) -> u64 {
        self.selection
            .iter()
            .filter_map(|name| self.index.get(name))
            .map(|&i| self.entries[i as usize].size)
            .sum()
    }

    /// All selected names (so `len() == selected_count()`), regardless of filter or hidden state,
    /// because an operation must touch exactly what its dialog announced. Order: visible rows in
    /// view order, then selected-but-not-visible entries in load order, then names that have not
    /// streamed in yet (sorted). If none are selected callers use the cursor entry.
    pub fn selected_names(&self) -> Vec<OsString> {
        let mut out: Vec<OsString> = Vec::with_capacity(self.selection.len());
        let mut seen: HashSet<&OsStr> = HashSet::new();
        for e in self.visible.iter().filter_map(|&vi| self.row_entry(vi)) {
            if self.selection.contains(&e.name) {
                seen.insert(&e.name);
                out.push(e.name.clone());
            }
        }
        for e in &self.entries {
            if self.selection.contains(&e.name) && !seen.contains(e.name.as_os_str()) {
                seen.insert(&e.name);
                out.push(e.name.clone());
            }
        }
        let mut pending: Vec<OsString> = self
            .selection
            .iter()
            .filter(|n| !self.index.contains_key(*n))
            .cloned()
            .collect();
        pending.sort();
        out.extend(pending);
        out
    }

    // ---- quick search --------------------------------------------------------------------------
    /// Jump to the first entry matching `query` at or after the cursor: prefix matches first, then
    /// substring, case-insensitively (ASCII); wraps around. Returns the matched visible index.
    pub fn quick_search(&mut self, query: &str) -> Option<u32> {
        let hit = self.quick_find(query, self.cursor.unwrap_or(0), Direction::Forward, true);
        if let Some(i) = hit {
            self.set_cursor(i);
        }
        hit
    }

    /// Next/previous match of the same query (Ctrl+Down / Ctrl+Up).
    pub fn quick_search_step(&mut self, query: &str, dir: Direction) -> Option<u32> {
        let hit = self.quick_find(query, self.cursor.unwrap_or(0), dir, false);
        if let Some(i) = hit {
            self.set_cursor(i);
        }
        hit
    }

    // ---- internals ------------------------------------------------------------------------------
    /// Name of visible row `i`, or `None` for the `..` row.
    fn row_name(&self, i: u32) -> Option<OsString> {
        self.visible
            .get(i as usize)
            .and_then(|&vi| self.row_entry(vi))
            .map(|e| e.name.clone())
    }

    /// Entry for a visible-row id, or `None` for the `..` row.
    fn row_entry(&self, vi: u32) -> Option<&Entry> {
        match vi {
            PARENT => None,
            i => self.entries.get(i as usize),
        }
    }

    fn name_at(&self, vi: u32) -> &OsStr {
        if vi == PARENT {
            parent_name()
        } else {
            &self.entries[vi as usize].name
        }
    }

    fn snapshot_selection(&mut self) {
        self.last_selection = Some(self.selection.clone());
    }

    /// Apply a selection change to `names`; deselecting snapshots the previous selection first.
    fn apply_selection(&mut self, names: Vec<OsString>, selected: bool) {
        if selected {
            self.selection.extend(names);
        } else {
            if names.iter().any(|n| self.selection.contains(n)) {
                self.snapshot_selection();
            }
            for name in names {
                self.selection.remove(&name);
            }
        }
    }

    /// Does `entry` pass the hidden-file and filter settings?
    fn passes_view(&self, entry: &Entry) -> bool {
        let bytes = entry.name.as_encoded_bytes();
        if !self.show_hidden && bytes.first() == Some(&b'.') {
            return false;
        }
        match &self.filter {
            Some(f) => fold_contains(bytes, f.as_bytes()),
            None => true,
        }
    }

    /// Recompute the whole view (filter + sort). Used outside streaming; `apply_batch` merges.
    fn rebuild_visible(&mut self) {
        self.visible.clear();
        if self.parent_entry.is_some() {
            self.visible.push(PARENT);
        }
        let mut ids: Vec<u32> = (0..self.entries.len() as u32)
            .filter(|&i| self.passes_view(&self.entries[i as usize]))
            .collect();
        sort_ids(&self.sort, &self.entries, &mut ids);
        self.visible.extend_from_slice(&ids);
    }

    /// Re-validate the cursor after any view change: keep it on `cursor_name` when that name is
    /// still visible, otherwise clamp the old index (0 for fresh views).
    fn fix_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = None;
            return;
        }
        if let Some(name) = &self.cursor_name {
            let pos = self.visible.iter().position(|&vi| self.name_at(vi) == name);
            if let Some(pos) = pos {
                self.cursor = Some(pos as u32);
                return;
            }
        }
        let len = self.visible.len() as u32;
        self.cursor = Some(self.cursor.map_or(0, |c| c.min(len - 1)));
    }

    /// Find the first match for `query` starting at `start` (inclusive or exclusive) in `dir`,
    /// wrapping around: a prefix match anywhere in the scan order beats every substring match.
    fn quick_find(&self, query: &str, start: u32, dir: Direction, inclusive: bool) -> Option<u32> {
        let needle = query.as_bytes();
        if needle.is_empty() {
            return None;
        }
        let len = self.visible_len();
        if len == 0 {
            return None;
        }
        let mut first_substring: Option<u32> = None;
        for k in 0..len {
            let i = match dir {
                Direction::Forward => {
                    let skip = u64::from(!inclusive);
                    ((u64::from(start) + skip + u64::from(k)) % u64::from(len)) as u32
                }
                Direction::Backward => {
                    // k-th candidate: (start - !inclusive - k) mod len.
                    let skip = u64::from(!inclusive);
                    ((u64::from(start) + 2 * u64::from(len) - skip - u64::from(k)) % u64::from(len))
                        as u32
                }
            };
            let name = self.name_at(self.visible[i as usize]).as_encoded_bytes();
            if fold_starts_with(name, needle) {
                return Some(i);
            }
            if first_substring.is_none() && fold_contains(name, needle) {
                first_substring = Some(i);
            }
        }
        first_substring
    }
}

/// The non-`..` part of a visible list (the parent is pinned at position 0 when present).
fn real_slice(visible: &[u32], has_parent: bool) -> &[u32] {
    if has_parent && visible.first() == Some(&PARENT) {
        &visible[1..]
    } else {
        visible
    }
}

/// Smallest single splice turning `old` into `new` (common prefix and suffix kept).
fn splice_diff<T>(old: &[T], new: &[T], same: impl Fn(&T, &T) -> bool) -> ModelChange {
    let mut prefix = 0usize;
    while prefix < old.len() && prefix < new.len() && same(&old[prefix], &new[prefix]) {
        prefix += 1;
    }
    let mut suffix = 0usize;
    while suffix < old.len() - prefix
        && suffix < new.len() - prefix
        && same(&old[old.len() - 1 - suffix], &new[new.len() - 1 - suffix])
    {
        suffix += 1;
    }
    let removed = (old.len() - prefix - suffix) as u32;
    let added = (new.len() - prefix - suffix) as u32;
    if removed == 0 && added == 0 {
        ModelChange::None
    } else {
        ModelChange::Spliced {
            pos: prefix as u32,
            removed,
            added,
        }
    }
}

/// ASCII-case-insensitive prefix test on raw bytes.
fn fold_starts_with(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .iter()
            .zip(needle)
            .all(|(h, n)| h.eq_ignore_ascii_case(n))
}

/// ASCII-case-insensitive substring test on raw bytes.
fn fold_contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if haystack.len() < needle.len() {
        return false;
    }
    (0..=haystack.len() - needle.len()).any(|off| fold_starts_with(&haystack[off..], needle))
}

#[cfg(test)]
mod tests {
    use super::super::SortKey;
    use super::*;
    use crate::entry::TargetKind;

    fn file(name: &str, size: u64) -> Entry {
        let mut e = Entry::new(name, EntryKind::File);
        e.size = size;
        e
    }
    fn dir(name: &str) -> Entry {
        Entry::new(name, EntryKind::Dir)
    }
    fn mask(src: &str) -> Mask {
        match Mask::parse(src, crate::mask::MaskOpts::default()) {
            Ok(m) => m,
            Err(e) => panic!("mask parse: {e}"),
        }
    }
    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }
    fn names(m: &PanelModel) -> Vec<String> {
        (0..m.visible_len())
            .filter_map(|i| m.visible(i).map(|e| e.name.to_string_lossy().into_owned()))
            .collect()
    }

    /// Applies a `ModelChange` to a shadow copy of the view (what the UI will do incrementally).
    fn apply_change(m: &PanelModel, shadow: &mut Vec<String>, change: ModelChange) {
        match change {
            ModelChange::None => {}
            ModelChange::Reset => *shadow = names(m),
            ModelChange::Spliced {
                pos,
                removed,
                added,
            } => {
                let fresh: Vec<String> = (pos..pos + added)
                    .filter_map(|i| m.visible(i).map(|e| e.name.to_string_lossy().into_owned()))
                    .collect();
                shadow.splice(pos as usize..(pos + removed) as usize, fresh);
            }
        }
    }

    fn assert_view_matches_shadow(m: &PanelModel, shadow: &[String]) {
        assert_eq!(
            names(m),
            shadow,
            "view must match the shadow after applying changes"
        );
    }

    #[test]
    fn streams_sorts_and_splices() {
        let mut m = PanelModel::new();
        let mut shadow = Vec::new();
        let reset = m.begin_load(true, false);
        apply_change(&m, &mut shadow, reset);
        assert_view_matches_shadow(&m, &shadow);
        // Stream in a deliberately nasty order: unsorted, hidden files, dirs.
        let batches: Vec<Vec<Entry>> = vec![
            vec![file("m.txt", 1), dir("zeta"), file(".hidden", 0)],
            vec![file("file10", 10), file("file2", 2)],
            vec![dir("beta"), file("a.txt", 5)],
            vec![],
        ];
        for batch in batches {
            let change = m.apply_batch(batch);
            apply_change(&m, &mut shadow, change);
            assert_view_matches_shadow(&m, &shadow);
        }
        let done = m.finish_load();
        apply_change(&m, &mut shadow, done);
        // `..` first, dirs before files, natural order, hidden gone.
        assert_eq!(
            shadow,
            vec!["..", "beta", "zeta", "a.txt", "file2", "file10", "m.txt"]
        );
        assert_eq!(m.total_len(), 7);
        assert_eq!(m.visible_len(), 7);
        assert_eq!(m.visible(0).map(|e| e.name.to_str()), Some(Some("..")));
        assert!(m.visible(7).is_none());
    }

    #[test]
    fn splice_is_minimal() {
        let mut m = PanelModel::new();
        m.begin_load(false, false);
        m.apply_batch(vec![file("c", 0), file("d", 0)]); // view: c, d
        // Insert before everything: c and d keep their ids and form the common suffix.
        assert_eq!(
            m.apply_batch(vec![file("a", 0)]),
            ModelChange::Spliced {
                pos: 0,
                removed: 0,
                added: 1
            }
        );
        // Insert between "c" and "d": pure insertion at pos 2.
        assert_eq!(
            m.apply_batch(vec![file("cc", 0)]),
            ModelChange::Spliced {
                pos: 2,
                removed: 0,
                added: 1
            }
        );
        // Append: pure insertion at the end.
        assert_eq!(
            m.apply_batch(vec![file("z", 0)]),
            ModelChange::Spliced {
                pos: 4,
                removed: 0,
                added: 1
            }
        );
        // Duplicate names are ignored.
        assert_eq!(m.apply_batch(vec![file("z", 5)]), ModelChange::None);
        assert_eq!(m.total_len(), 5);
        assert_eq!(m.visible(4).map(|e| e.size), Some(0)); // original kept
    }

    #[test]
    fn hidden_and_filter() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![
            file(".dot", 1),
            file("plain", 2),
            dir(".config"),
            file("a.SH", 3),
        ]);
        m.finish_load();
        assert_eq!(names(&m), vec!["..", "a.SH", "plain"]);

        let _ = m.set_show_hidden(true);
        assert_eq!(names(&m), vec!["..", ".config", ".dot", "a.SH", "plain"]);
        assert_eq!(m.total_len(), 4);

        let _ = m.set_filter(Some("o")); // substring, case-insensitive
        assert_eq!(names(&m), vec!["..", ".config", ".dot"]);
        assert_eq!(m.filter(), Some("o"));

        let _ = m.set_filter(Some("")); // empty clears
        assert_eq!(m.filter(), None);
        assert_eq!(names(&m).len(), 5);

        let _ = m.set_show_hidden(false);
        assert_eq!(names(&m), vec!["..", "a.SH", "plain"]);
    }

    #[test]
    fn cursor_moves_and_clamps() {
        let mut m = PanelModel::new();
        assert_eq!(m.cursor(), None);
        assert_eq!(m.move_cursor(CursorMove::End), None); // empty view stays None

        m.begin_load(true, false);
        for name in ["e", "d", "c", "b", "a"] {
            let _ = m.apply_batch(vec![file(name, 0)]);
        }
        m.finish_load();
        // view: .., a, b, c, d, e
        assert_eq!(m.cursor(), Some(0)); // cursor lands on `..` when the view fills

        assert_eq!(m.move_cursor(CursorMove::By(2)), Some(2));
        assert_eq!(m.move_cursor(CursorMove::By(-5)), Some(0)); // clamped
        assert_eq!(
            m.move_cursor(CursorMove::Page { rows: 2, pages: 1 }),
            Some(2)
        );
        assert_eq!(
            m.move_cursor(CursorMove::Page { rows: 0, pages: 3 }),
            Some(5)
        ); // rows >= 1
        assert_eq!(m.move_cursor(CursorMove::End), Some(5));
        assert_eq!(
            m.move_cursor(CursorMove::Column {
                rows_per_column: 2,
                columns: -1
            }),
            Some(3)
        );
        assert_eq!(m.move_cursor(CursorMove::Home), Some(0));
        assert!(m.set_cursor_name(OsStr::new("d")));
        assert_eq!(m.cursor(), Some(4));
        assert_eq!(m.cursor_entry().map(|e| e.name.to_str()), Some(Some("d")));
        assert!(!m.set_cursor_name(OsStr::new("nope")));
        assert_eq!(m.cursor(), Some(4));
        m.set_cursor(99);
        assert_eq!(m.cursor(), Some(5));

        // Cursor follows the same name through a re-sort.
        assert!(m.set_cursor_name(OsStr::new("d")));
        let _ = m.set_sort(SortSpec {
            key: SortKey::Name,
            reverse: true,
            dirs_first: true,
        });
        assert_eq!(m.cursor_entry().map(|e| e.name.to_str()), Some(Some("d")));
    }

    #[test]
    fn selection_semantics() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![
            file("a", 10),
            file("b", 20),
            dir("c"),
            file("x.tmp", 1),
            file(".h", 5),
        ]);
        m.finish_load();
        // visible (dirs first): .., c, a, b, x.tmp   (.h hidden)

        m.toggle_select(2); // a
        m.toggle_select(3); // b
        assert_eq!(m.selected_names(), os(&["a", "b"]));
        assert_eq!(m.selected_count(), 2);
        assert_eq!(m.selected_size(), 30);
        m.toggle_select(0); // `..` cannot be selected
        assert!(!m.is_selected(0));
        assert_eq!(m.selected_count(), 2);

        // Mask select, files only (dir "c" stays unselected).
        m.select_by_mask(&mask("*"), true, true);
        assert_eq!(m.selected_names(), os(&["a", "b", "x.tmp"]));
        assert_eq!(m.selected_size(), 31);
        // Mask deselect with exclusion (dirs are not exempt unless files_only).
        m.select_by_mask(&mask("*;!*.tmp"), false, false);
        assert_eq!(m.selected_names(), os(&["x.tmp"]));
        // Restore undoes the deselect.
        m.restore_selection();
        assert_eq!(m.selected_names(), os(&["a", "b", "x.tmp"]));

        // Invert with a dir selected too: everything visible was selected -> now nothing.
        m.toggle_select(1); // c
        assert_eq!(m.selected_names(), os(&["c", "a", "b", "x.tmp"]));
        m.invert_selection();
        assert_eq!(m.selected_names(), os(&[]));
        m.restore_selection();
        assert_eq!(m.selected_names(), os(&["c", "a", "b", "x.tmp"]));

        // Range select (parent row ignored, out-of-order and out-of-range args tolerated).
        m.select_range(9, 3, true); // clamped to 3..4 (b, x.tmp), already selected
        assert_eq!(m.selected_count(), 4);

        // Clear all + restore.
        m.select_all(false);
        assert_eq!(m.selected_count(), 0);
        m.restore_selection();
        assert_eq!(m.selected_count(), 4);

        // Toggle-off also snapshots: restore undoes the single toggle.
        m.toggle_select(1); // c off
        assert_eq!(m.selected_count(), 3);
        m.restore_selection();
        assert_eq!(m.selected_count(), 4);
    }

    #[test]
    fn navigation_clears_selection_and_snapshot() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![file("a", 1), file("b", 2)]);
        m.finish_load();
        m.select_all(true);
        m.select_all(false); // leaves a snapshot
        m.select_all(true);
        let _ = m.begin_load(true, false); // navigate elsewhere
        m.apply_batch(vec![file("a", 5), file("b", 6), file("c", 7)]);
        m.finish_load();
        assert_eq!(m.selected_count(), 0);
        assert!(m.selected_names().is_empty());
        m.restore_selection(); // must not resurrect the old directory's selection
        assert_eq!(m.selected_count(), 0);
    }

    #[test]
    fn reload_snapshots_only_non_empty_selection() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![file("a", 1), file("b", 2), file("c", 3)]);
        m.finish_load();
        m.toggle_select(1);
        m.toggle_select(2);
        m.select_all(false);
        assert_eq!(m.selected_count(), 0);
        let _ = m.begin_load(true, true); // empty selection: must keep the older snapshot
        m.apply_batch(vec![file("a", 1), file("b", 2), file("c", 3)]);
        m.finish_load();
        m.restore_selection();
        assert_eq!(m.selected_names(), os(&["a", "b"]));
    }

    #[test]
    fn selection_queries_cover_hidden_and_filtered() {
        let mut m = PanelModel::new();
        m.begin_load(false, false);
        m.apply_batch(vec![file(".dot", 4), file("a", 1), file("b", 2)]);
        m.finish_load();
        m.set_show_hidden(true);
        m.select_all(true);
        assert_eq!(m.selected_count(), 3);
        m.set_show_hidden(false);
        m.set_filter(Some("a"));
        assert_eq!(m.visible_len(), 1);
        assert_eq!(m.selected_count(), 3);
        assert_eq!(m.selected_names().len(), 3);
        assert_eq!(m.selected_size(), 7);
        assert_eq!(m.selected_names()[0], OsString::from("a")); // visible first
    }

    #[test]
    fn selection_survives_reload_and_updates() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![file("keep", 1), file("drop", 2), file("gone", 3)]);
        m.finish_load();
        m.select_all(true);
        assert_eq!(m.selected_count(), 3);

        // Reload the same directory: selection by name survives.
        let _ = m.begin_load(true, true);
        assert_eq!(m.selected_count(), 3); // kept by name until finish_load
        m.apply_batch(vec![file("keep", 1), file("drop", 2)]);
        m.finish_load();
        assert_eq!(m.selected_names(), os(&["drop", "keep"]));

        // Watcher removes a selected name -> pruned; changed entries keep their selection.
        m.apply_update(vec![file("keep", 9)], &[OsString::from("drop")]);
        assert_eq!(m.selected_names(), os(&["keep"]));
        assert_eq!(m.selected_size(), 9);

        // Reload again: still selected by name; clear + restore brings it back.
        let _ = m.begin_load(true, true);
        m.apply_batch(vec![file("keep", 9), file("new", 0)]);
        m.finish_load();
        assert_eq!(m.selected_names(), os(&["keep"]));
        m.select_all(false);
        assert_eq!(m.selected_names(), Vec::<OsString>::new());
        m.restore_selection();
        assert_eq!(m.selected_names(), os(&["keep"]));
    }

    #[test]
    fn quick_search_prefix_then_substring_with_wrap() {
        let mut m = PanelModel::new();
        m.begin_load(false, false);
        m.apply_batch(vec![
            file("beta", 0),
            file("abba", 0),
            file("cab", 0),
            file("Beta2", 0),
        ]);
        m.finish_load();
        // sorted view: abba(0), beta(1), Beta2(2), cab(3); cursor at 0.

        // Prefix match beats an earlier substring match ("abba" contains "b").
        assert_eq!(m.quick_search("b"), Some(1));
        assert_eq!(m.cursor(), Some(1));
        assert_eq!(m.quick_search_step("b", Direction::Forward), Some(2)); // "Beta2"
        assert_eq!(m.quick_search_step("b", Direction::Backward), Some(1)); // "beta"
        // Prefix first, then substring, wrapping.
        assert_eq!(m.quick_search("ab"), Some(0)); // prefix "abba" (wraps past cursor)
        m.set_cursor(3);
        assert_eq!(m.quick_search("eta"), Some(1)); // substring of "beta", wraps
        assert_eq!(m.quick_search("zzz"), None);
        assert_eq!(m.quick_search(""), None);
        m.set_cursor(3);
        assert_eq!(m.quick_search_step("a", Direction::Forward), Some(0)); // wraps to "abba"
        m.set_cursor(0);
        // Backward from 0 wraps: substring "cab"(3) is found first but prefix "Beta2"(2) wins.
        assert_eq!(m.quick_search_step("b", Direction::Backward), Some(2));
    }

    #[test]
    fn natural_order_in_view() {
        let mut m = PanelModel::new();
        m.begin_load(false, false);
        m.apply_batch(vec![
            file("file10", 0),
            file("file1", 0),
            file("file20", 0),
            file("file2", 0),
        ]);
        m.finish_load();
        assert_eq!(names(&m), vec!["file1", "file2", "file10", "file20"]);
    }

    #[test]
    fn update_reinserts_and_repositions() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![file("b", 0), file("c", 0)]);
        let mut shadow = Vec::new();
        apply_change(&m, &mut shadow, ModelChange::Reset);
        let change = m.apply_update(vec![file("a", 0), file("c2", 0)], &[OsString::from("b")]);
        apply_change(&m, &mut shadow, change);
        assert_view_matches_shadow(&m, &shadow);
        assert_eq!(shadow, vec!["..", "a", "c", "c2"]);
        assert_eq!(m.total_len(), 3);
        assert_eq!(m.apply_update(vec![], &[]), ModelChange::None);
    }

    #[test]
    fn update_removal_and_in_place_change_are_reported_exactly() {
        let mut m = PanelModel::new();
        m.begin_load(true, false);
        m.apply_batch(vec![file("a", 1), file("b", 1), file("c", 1), file("d", 1)]);
        let mut shadow = Vec::new();
        apply_change(&m, &mut shadow, ModelChange::Reset);
        // Removing an early entry must splice at its row, not at the tail.
        let change = m.apply_update(vec![], &[OsString::from("a")]);
        assert_eq!(
            change,
            ModelChange::Spliced {
                pos: 1,
                removed: 1,
                added: 0
            }
        );
        apply_change(&m, &mut shadow, change);
        assert_view_matches_shadow(&m, &shadow);
        // Same name, new metadata: the row stays but must be refreshed.
        let change = m.apply_update(vec![file("c", 99)], &[]);
        assert_eq!(
            change,
            ModelChange::Spliced {
                pos: 2,
                removed: 1,
                added: 1
            }
        );
        assert_eq!(m.visible(2).map(|e| e.size), Some(99));
    }

    #[test]
    fn symlink_to_dir_sorts_with_dirs() {
        let link = Entry::new(
            "zlink",
            EntryKind::Symlink {
                target: Some(TargetKind::Dir),
            },
        );
        let mut m = PanelModel::new();
        m.begin_load(false, false);
        m.apply_batch(vec![file("aaa", 0), link]);
        m.finish_load();
        assert_eq!(names(&m), vec!["zlink", "aaa"]); // dirs_first default
    }
}

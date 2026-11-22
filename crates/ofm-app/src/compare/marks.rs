//! From a [`CompareResult`] to selections: which names are marked per side, the summary counts
//! and the "replace the selection" step on a [`PanelModel`]. Pure (no GTK): unit-tested.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use ofm_core::compare::{
    CompareEntry, CompareKind, CompareOptions, CompareResult, MarkReason, Marked,
};
use ofm_core::panel::PanelModel;

use super::options::MarkFilter;

/// Does `filter` keep a mark with this reason?
pub fn keeps(filter: MarkFilter, reason: MarkReason) -> bool {
    match filter {
        MarkFilter::AllDiffering => true,
        MarkFilter::NewerHere => reason == MarkReason::NewerHere,
        MarkFilter::MissingOnOther => reason == MarkReason::MissingOnOtherSide,
    }
}

/// Names to select on one side.
pub fn names(marks: &[Marked], filter: MarkFilter) -> HashSet<OsString> {
    marks
        .iter()
        .filter(|m| keeps(filter, m.reason))
        .map(|m| m.name.clone())
        .collect()
}

/// What a finished comparison found, for the status message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Counts {
    /// Entries marked on the left.
    pub left: usize,
    /// Entries marked on the right.
    pub right: usize,
    /// Pairs that matched in every respect.
    pub identical: usize,
    /// Pairs still waiting for a content comparison.
    pub pending: usize,
}

/// Count the marks (after `filter`) and the identical pairs of `result`.
///
/// `left_total` is the number of left entries that took part in the comparison (see
/// [`compared_len`]): matched pairs = `left_total` - missing-on-the-other-side marks; the pairs
/// among them that carry any other mark differ, the rest are identical (minus the undecided
/// content candidates).
pub fn counts(result: &CompareResult, left_total: usize, filter: MarkFilter) -> Counts {
    let missing = result
        .left_marks
        .iter()
        .filter(|m| m.reason == MarkReason::MissingOnOtherSide)
        .count();
    let differing_pairs = result.left_marks.len() - missing;
    let pairs = left_total.saturating_sub(missing);
    let pending = result.content_candidates.len();
    Counts {
        left: names(&result.left_marks, filter).len(),
        right: names(&result.right_marks, filter).len(),
        identical: pairs.saturating_sub(differing_pairs + pending),
        pending,
    }
}

/// Number of entries of `list` that take part in a comparison with `opts`.
pub fn compared_len(list: &[CompareEntry], opts: &CompareOptions) -> usize {
    list.iter()
        .filter(|e| opts.include_dirs || e.kind != CompareKind::Dir)
        .count()
}

/// Replace the selection of the visible rows of `model` by exactly those whose name is in
/// `names`. The previous selection is snapshotted first, so `select.restore` undoes it.
/// Selection is stored by name in the model, so it survives re-sorts and watcher refreshes.
pub fn replace_selection(model: &mut PanelModel, names: &HashSet<OsString>) {
    model.select_all(false);
    let rows: Vec<u32> = (0..model.visible_len())
        .filter(|&i| {
            model
                .visible(i)
                .is_some_and(|e| e.name != OsStr::new("..") && names.contains(&e.name))
        })
        .collect();
    for i in rows {
        model.select_range(i, i, true);
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::ffi::OsStringExt;

    use ofm_core::compare::compare;
    use ofm_core::entry::{Entry, EntryKind};
    use ofm_core::panel::{SortKey, SortSpec};

    use super::*;
    use crate::compare::options::CompareSettings;

    fn file(name: &str, size: u64, mtime_s: i128) -> CompareEntry {
        CompareEntry {
            name: name.into(),
            kind: CompareKind::File,
            size,
            mtime_ns: mtime_s * 1_000_000_000,
            link_target: None,
        }
    }

    fn sample() -> (Vec<CompareEntry>, Vec<CompareEntry>) {
        let left = vec![
            file("same", 1, 100),
            file("only-left", 1, 100),
            file("bigger", 5, 100),
            file("newer", 1, 200),
        ];
        let right = vec![
            file("same", 1, 100),
            file("only-right", 1, 100),
            file("bigger", 3, 100),
            file("newer", 1, 100),
        ];
        (left, right)
    }

    fn set(items: &[&str]) -> HashSet<OsString> {
        items.iter().map(OsString::from).collect()
    }

    #[test]
    fn filters_pick_the_reason() {
        let (l, r) = sample();
        let res = compare(&l, &r, &CompareSettings::default().to_options());
        assert_eq!(
            names(&res.left_marks, MarkFilter::AllDiffering),
            set(&["only-left", "bigger", "newer"])
        );
        assert_eq!(
            names(&res.right_marks, MarkFilter::AllDiffering),
            set(&["only-right", "bigger", "newer"])
        );
        assert_eq!(
            names(&res.left_marks, MarkFilter::NewerHere),
            set(&["newer"])
        );
        assert!(names(&res.right_marks, MarkFilter::NewerHere).is_empty());
        assert_eq!(
            names(&res.left_marks, MarkFilter::MissingOnOther),
            set(&["only-left"])
        );
        assert_eq!(
            names(&res.right_marks, MarkFilter::MissingOnOther),
            set(&["only-right"])
        );
    }

    #[test]
    fn counts_match_the_summary() {
        let (l, r) = sample();
        let opts = CompareSettings::default().to_options();
        let res = compare(&l, &r, &opts);
        let c = counts(&res, compared_len(&l, &opts), MarkFilter::AllDiffering);
        assert_eq!(
            c,
            Counts {
                left: 3,
                right: 3,
                identical: 1,
                pending: 0
            }
        );
        // The filter changes the marked counts, not the identical pairs.
        let c = counts(&res, compared_len(&l, &opts), MarkFilter::MissingOnOther);
        assert_eq!((c.left, c.right, c.identical), (1, 1, 1));
    }

    #[test]
    fn directories_can_be_left_out() {
        let l = vec![file("f", 1, 0), CompareEntry::new("d", CompareKind::Dir)];
        let r = vec![file("f", 1, 0)];
        let mut opts = CompareSettings::default().to_options();
        let res = compare(&l, &r, &opts);
        assert_eq!(
            names(&res.left_marks, MarkFilter::AllDiffering),
            set(&["d"])
        );
        opts.include_dirs = false;
        let res = compare(&l, &r, &opts);
        assert!(res.no_marks());
        assert_eq!(compared_len(&l, &opts), 1);
        assert_eq!(counts(&res, 1, MarkFilter::AllDiffering).identical, 1);
    }

    fn load(m: &mut PanelModel, names: &[OsString], keep: bool) {
        m.begin_load(true, keep);
        m.apply_batch(
            names
                .iter()
                .map(|n| Entry::new(n.clone(), EntryKind::File))
                .collect(),
        );
        m.finish_load();
    }

    fn model_with(names: &[OsString]) -> PanelModel {
        let mut m = PanelModel::new();
        load(&mut m, names, false);
        m
    }

    fn abc() -> Vec<OsString> {
        ["a", "b", "c"].iter().map(OsString::from).collect()
    }

    #[test]
    fn selection_is_replaced_not_extended() {
        let mut m = model_with(&abc());
        m.select_all(true);
        replace_selection(&mut m, &set(&["b", "gone"]));
        assert_eq!(m.selected_names(), vec![OsString::from("b")]);
        // `..` is never selected, even when named.
        replace_selection(&mut m, &set(&["..", "c"]));
        assert_eq!(m.selected_names(), vec![OsString::from("c")]);
        // The previous selection can be brought back.
        replace_selection(&mut m, &HashSet::new());
        assert!(m.selected_names().is_empty());
        m.restore_selection();
        assert_eq!(m.selected_names(), vec![OsString::from("c")]);
    }

    #[test]
    fn marks_survive_resort_and_same_dir_reload() {
        let mut m = model_with(&abc());
        replace_selection(&mut m, &set(&["a", "c"]));
        let spec = SortSpec {
            key: SortKey::Size,
            ..m.sort_spec()
        };
        let _ = m.set_sort(spec);
        assert_eq!(m.selected_count(), 2);
        load(&mut m, &abc(), true);
        let sel: HashSet<OsString> = m.selected_names().into_iter().collect();
        assert_eq!(sel, set(&["a", "c"]));
    }

    #[test]
    fn non_utf8_names_are_matched_as_bytes() {
        let odd = OsString::from_vec(vec![b'f', 0xff, 0xfe]);
        let l = vec![CompareEntry::new(odd.clone(), CompareKind::File)];
        let res = compare(&l, &[], &CompareSettings::default().to_options());
        let sel = names(&res.left_marks, MarkFilter::AllDiffering);
        assert!(sel.contains(&odd));
        let mut m = model_with(std::slice::from_ref(&odd));
        replace_selection(&mut m, &sel);
        assert_eq!(m.selected_names(), vec![odd]);
    }
}

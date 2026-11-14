//! Property tests for the T1.1 invariants (PLAN: sort is a total order, selection invariants,
//! cursor stays valid after any change, `ModelChange` application == full view rebuild).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};

use proptest::collection::vec;
use proptest::prelude::*;

use super::cursor::{CursorMove, Direction};
use super::sort::{natural_cmp, SortKey, SortSpec};
use super::ModelChange;
use super::PanelModel;
use crate::entry::{Entry, EntryKind, TargetKind};
use crate::mask::{Mask, MaskOpts};

// ---- strategies -------------------------------------------------------------------------------

fn arb_kind() -> impl Strategy<Value = EntryKind> {
    (0..4u8).prop_map(|k| match k {
        0 => EntryKind::File,
        1 => EntryKind::Dir,
        2 => EntryKind::Symlink {
            target: Some(TargetKind::Dir),
        },
        _ => EntryKind::Symlink { target: None },
    })
}

fn entry_of(name: String, kind: EntryKind, size: u64, mtime: u64) -> Entry {
    let mut e = Entry::new(name, kind);
    e.size = size;
    e.mtime_ns = i128::from(mtime);
    e
}

/// 1 in 6 entries has a dot-name (hidden, but never `..` — that is not a real file name).
fn arb_entry() -> impl Strategy<Value = Entry> {
    prop_oneof![
        5 => (r"[a-zA-Z0-9][a-zA-Z0-9._-]{0,9}", arb_kind(), any::<u64>(), any::<u64>())
            .prop_map(|(n, k, s, m)| entry_of(n, k, s % 100_000, m % 100_000)),
        1 => (r"\.[a-zA-Z0-9_-][a-zA-Z0-9._-]{0,8}", arb_kind(), any::<u64>(), any::<u64>())
            .prop_map(|(n, k, s, m)| entry_of(n, k, s % 100_000, m % 100_000)),
    ]
}

fn arb_sort() -> impl Strategy<Value = SortSpec> {
    (0..4u8, any::<bool>(), any::<bool>()).prop_map(|(k, reverse, dirs_first)| SortSpec {
        key: match k {
            0 => SortKey::Name,
            1 => SortKey::Ext,
            2 => SortKey::Size,
            _ => SortKey::Mtime,
        },
        reverse,
        dirs_first,
    })
}

fn arb_move() -> impl Strategy<Value = CursorMove> {
    prop_oneof![
        4 => any::<i32>().prop_map(CursorMove::By),
        2 => (0u32..60, -6i32..6).prop_map(|(rows, pages)| CursorMove::Page { rows, pages }),
        2 => (0u32..60, -6i32..6)
            .prop_map(|(rows_per_column, columns)| CursorMove::Column { rows_per_column, columns }),
        1 => Just(CursorMove::Home),
        1 => Just(CursorMove::End),
    ]
}

// ---- helpers ----------------------------------------------------------------------------------

fn unique_entries(entries: Vec<Entry>) -> Vec<Entry> {
    let mut seen: HashSet<OsString> = HashSet::new();
    entries
        .into_iter()
        .filter(|e| seen.insert(e.name.clone()))
        .collect()
}

fn vis(m: &PanelModel, i: u32) -> &Entry {
    match m.visible(i) {
        Some(e) => e,
        None => panic!("visible({i}) out of range (len {})", m.visible_len()),
    }
}

fn view_names(m: &PanelModel) -> Vec<OsString> {
    (0..m.visible_len())
        .map(|i| vis(m, i).name.clone())
        .collect()
}

/// Reference implementation of the visible order, written against the documented rules.
fn ref_ext(name: &OsStr) -> &[u8] {
    let b = name.as_encoded_bytes();
    match b.iter().rposition(|&c| c == b'.') {
        Some(p) if p > 0 => &b[p + 1..],
        _ => b"",
    }
}

fn ref_order(entries: &mut [&Entry], spec: &SortSpec) {
    entries.sort_by(|a, b| {
        if spec.dirs_first && a.kind.is_dir_like() != b.kind.is_dir_like() {
            return if a.kind.is_dir_like() {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let ord = match spec.key {
            SortKey::Name => natural_cmp(&a.name, &b.name),
            SortKey::Ext => natural_cmp_bytes(ref_ext(&a.name), ref_ext(&b.name))
                .then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Size => a.size.cmp(&b.size).then_with(|| natural_cmp(&a.name, &b.name)),
            SortKey::Mtime => a
                .mtime_ns
                .cmp(&b.mtime_ns)
                .then_with(|| natural_cmp(&a.name, &b.name)),
        };
        if spec.reverse {
            ord.reverse()
        } else {
            ord
        }
    });
}

fn natural_cmp_bytes(a: &[u8], b: &[u8]) -> Ordering {
    use std::os::unix::ffi::OsStrExt;
    natural_cmp(OsStr::from_bytes(a), OsStr::from_bytes(b))
}

// ---- property: the view is exactly the spec'd total order, however batches arrive ------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    #[test]
    fn view_is_strict_total_order(
        (entries, spec, show_hidden, batch_size) in
        (vec(arb_entry(), 0..300), arb_sort(), any::<bool>(), 1usize..17)
    ) {
        let entries = unique_entries(entries);
        let mut m = PanelModel::new();
        m.set_sort(spec);
        m.set_show_hidden(show_hidden);
        m.begin_load(true);
        for chunk in entries.chunks(batch_size) {
            m.apply_batch(chunk.to_vec());
        }
        m.finish_load();

        // Expected view: filter, reference order, `..` pinned first.
        let mut expected: Vec<&Entry> = entries
            .iter()
            .filter(|e| show_hidden || e.name.as_encoded_bytes().first() != Some(&b'.'))
            .collect();
        ref_order(&mut expected, &spec);
        prop_assert_eq!(m.visible_len(), expected.len() as u32 + 1);

        if !expected.is_empty() {
            prop_assert_eq!(m.visible(0).map(|e| e.name.as_encoded_bytes()), Some(&b".."[..]));
            let plain_name_order =
                spec.key == SortKey::Name && !spec.reverse && !spec.dirs_first;
            let mut prev = vis(&m, 1);
            for (i, exp) in expected.iter().enumerate() {
                let got = vis(&m, i as u32 + 1);
                prop_assert_eq!(&got.name, &exp.name, "row {} mismatch", i + 1);
                prop_assert_eq!(got.kind, exp.kind);
                if i > 0 {
                    // Neighbouring rows are distinct; under a plain name sort the order is also
                    // strictly natural (`..` stays pinned first by the spec even when a name
                    // like ".-x" would sort before it).
                    prop_assert_ne!(&prev.name, &got.name, "duplicate row {}", i + 1);
                    if plain_name_order {
                        prop_assert_eq!(
                            natural_cmp(&prev.name, &got.name),
                            Ordering::Less,
                            "view not strictly ordered at row {}",
                            i + 1
                        );
                    }
                }
                prev = got;
            }
        }
        prop_assert_eq!(m.total_len(), entries.len());
        prop_assert!(m.visible(m.visible_len()).is_none());
    }
}

// ---- property: chaos driver --------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Op {
    Batch(Vec<Entry>),
    Finish,
    Sort(SortSpec),
    Hidden(bool),
    Filter(Option<String>),
    Move(CursorMove),
    SetCursor(u32),
    SetCursorName(String),
    Toggle(u32),
    Range(u32, u32, bool),
    Invert,
    All(bool),
    Restore,
    Update(Vec<Entry>, Vec<String>),
    Quick(String),
    Step(String, Direction),
}

fn arb_op() -> impl Strategy<Value = Op> {
    let arb_dir = prop_oneof![Just(Direction::Forward), Just(Direction::Backward)];
    let arb_filter = prop_oneof![
        1 => Just(None),
        3 => r"[a-zA-Z0-9._]{1,3}".prop_map(Some),
    ];
    prop_oneof![
        5 => vec(arb_entry(), 0..24).prop_map(Op::Batch),
        2 => arb_sort().prop_map(Op::Sort),
        1 => any::<bool>().prop_map(Op::Hidden),
        1 => arb_filter.prop_map(Op::Filter),
        5 => arb_move().prop_map(Op::Move),
        2 => (0u32..40).prop_map(Op::SetCursor),
        1 => r"[a-zA-Z0-9._]{1,6}".prop_map(Op::SetCursorName),
        3 => (0u32..40).prop_map(Op::Toggle),
        1 => ((0u32..40), (0u32..40), any::<bool>()).prop_map(|(a, b, s)| Op::Range(a, b, s)),
        1 => Just(Op::Invert),
        1 => any::<bool>().prop_map(Op::All),
        1 => Just(Op::Restore),
        2 => (vec(arb_entry(), 0..12), vec(r"[a-zA-Z0-9._]{1,6}", 0..4))
            .prop_map(|(c, r)| Op::Update(c, r)),
        2 => Just(Op::Finish),
        3 => r"[a-zA-Z0-9._]{0,3}".prop_map(Op::Quick),
        3 => (r"[a-zA-Z0-9._]{0,3}", arb_dir).prop_map(|(q, d)| Op::Step(q, d)),
    ]
}

/// Applies a `ModelChange` to a shadow view exactly like the UI will.
fn apply_change(m: &PanelModel, shadow: &mut Vec<String>, change: ModelChange) {
    match change {
        ModelChange::None => {}
        ModelChange::Reset => {
            *shadow = view_names(m)
                .iter()
                .map(|n| n.to_string_lossy().into_owned())
                .collect()
        }
        ModelChange::Spliced { pos, removed, added } => {
            let fresh: Vec<String> = (pos..pos + added)
                .map(|i| vis(m, i).name.to_string_lossy().into_owned())
                .collect();
            let (from, to) = (pos as usize, (pos + removed) as usize);
            assert!(
                from <= shadow.len() && to <= shadow.len(),
                "splice out of shadow range"
            );
            shadow.splice(from..to, fresh);
        }
    }
}

fn check_invariants(m: &PanelModel, shadow: &[String]) {
    let len = m.visible_len();
    // Shadow (incrementally updated) == full view rebuild.
    assert_eq!(shadow.len(), len as usize, "shadow length drift");
    for (i, s) in shadow.iter().enumerate() {
        assert_eq!(
            &vis(m, i as u32).name.to_string_lossy().into_owned(),
            s,
            "shadow row {}",
            i
        );
    }
    // Cursor valid after every change.
    match m.cursor() {
        None => assert_eq!(len, 0, "cursor None but view non-empty"),
        Some(c) => {
            assert!(c < len, "cursor {} >= len {}", c, len);
            assert!(m.visible(c).is_some());
            assert!(m.cursor_entry().is_some());
        }
    }
    // `..` always first, never selected.
    if len > 0 {
        assert_eq!(
            m.visible(0).map(|e| e.name.as_encoded_bytes()),
            Some(&b".."[..])
        );
    }
    assert!(!m.selected_names().iter().any(|n| n == ".."));
    // Hidden files invisible unless enabled (the pinned `..` row excepted).
    if !m.show_hidden() {
        for i in 0..len {
            if i == 0 && vis(m, i).name.as_encoded_bytes() == b".." {
                continue;
            }
            assert_ne!(vis(m, i).name.as_encoded_bytes().first(), Some(&b'.'));
        }
    }
    // selected_names: distinct, in view order, consistent with is_selected.
    let sel = m.selected_names();
    assert_eq!(
        sel.len(),
        sel.iter().collect::<HashSet<_>>().len(),
        "duplicate selection"
    );
    let names = view_names(m);
    let mut it = names.iter();
    for s in &sel {
        assert!(
            it.any(|n| n == s),
            "selected name {:?} not in view order after its position",
            s
        );
    }
    for (i, n) in names.iter().enumerate() {
        assert_eq!(
            m.is_selected(i as u32),
            sel.contains(n),
            "is_selected mismatch at {}",
            i
        );
    }
    assert!(m.selected_count() >= sel.len());
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn chaos_invariants_hold(
        (initial, ops) in (vec(arb_entry(), 0..40), vec(arb_op(), 1..60))
    ) {
        let mut m = PanelModel::new();
        m.begin_load(true);
        m.apply_batch(unique_entries(initial));
        m.finish_load();
        let mut shadow: Vec<String> = view_names(&m)
            .iter()
            .map(|n| n.to_string_lossy().into_owned())
            .collect();
        check_invariants(&m, &shadow);

        for op in ops {
            let change = match &op {
                Op::Batch(b) => m.apply_batch(b.clone()),
                Op::Finish => m.finish_load(),
                Op::Sort(s) => m.set_sort(*s),
                Op::Hidden(h) => m.set_show_hidden(*h),
                Op::Filter(f) => m.set_filter(f.as_deref()),
                Op::Update(c, r) => {
                    let removed: Vec<OsString> = r.iter().map(OsString::from).collect();
                    m.apply_update(c.clone(), &removed)
                }
                Op::Move(mv) => {
                    let _ = m.move_cursor(*mv);
                    ModelChange::None
                }
                Op::SetCursor(i) => {
                    m.set_cursor(*i);
                    ModelChange::None
                }
                Op::SetCursorName(n) => {
                    let _ = m.set_cursor_name(OsStr::new(n.as_str()));
                    ModelChange::None
                }
                Op::Toggle(i) => {
                    m.toggle_select(*i);
                    ModelChange::None
                }
                Op::Range(a, b, s) => {
                    m.select_range(*a, *b, *s);
                    ModelChange::None
                }
                Op::Invert => {
                    m.invert_selection();
                    ModelChange::None
                }
                Op::All(s) => {
                    m.select_all(*s);
                    ModelChange::None
                }
                Op::Restore => {
                    m.restore_selection();
                    ModelChange::None
                }
                Op::Quick(q) => {
                    let _ = m.quick_search(q);
                    ModelChange::None
                }
                Op::Step(q, d) => {
                    let _ = m.quick_search_step(q, *d);
                    ModelChange::None
                }
            };
            apply_change(&m, &mut shadow, change);
            check_invariants(&m, &shadow);
        }
    }

    #[test]
    fn selection_after_finish_exists_in_view_or_entries(
        (entries, extra) in (vec(arb_entry(), 1..40), vec(arb_entry(), 0..20))
    ) {
        let entries = unique_entries(entries);
        let mut m = PanelModel::new();
        m.begin_load(false);
        m.apply_batch(entries.clone());
        m.apply_batch(unique_entries(extra));
        m.select_all(true);
        m.finish_load();
        let existing: HashSet<OsString> = (0..m.visible_len())
            .map(|i| vis(&m, i).name.clone())
            .collect();
        for name in m.selected_names() {
            prop_assert!(existing.contains(&name), "selected {name:?} does not exist");
        }
        prop_assert_eq!(m.selected_count(), existing.len());
    }
}

// ---- property: natural_cmp is a total order ----------------------------------------------------

proptest! {
    #[test]
    fn natural_cmp_is_a_total_order(
        (a, b, c) in (r"[a-zA-Z0-9._]{0,10}", r"[a-zA-Z0-9._]{0,10}", r"[a-zA-Z0-9._]{0,10}")
    ) {
        let (a, b, c) = (OsStr::new(a.as_str()), OsStr::new(b.as_str()), OsStr::new(c.as_str()));
        prop_assert_eq!(natural_cmp(a, b).reverse(), natural_cmp(b, a));
        if natural_cmp(a, b) == Ordering::Equal {
            prop_assert_eq!(a, b); // Equal only for identical names
        }
        if natural_cmp(a, b) == Ordering::Less && natural_cmp(b, c) == Ordering::Less {
            prop_assert_eq!(natural_cmp(a, c), Ordering::Less);
        }
    }

    #[test]
    fn natural_cmp_sorts_numbers_numerically(
        (i, j) in (0u32..10000, 0u32..10000)
    ) {
        let (ni, nj) = (format!("f{i}.txt"), format!("f{j}.txt"));
        let (a, b) = (OsStr::new(ni.as_str()), OsStr::new(nj.as_str()));
        prop_assert_eq!(natural_cmp(a, b), i.cmp(&j));
    }
}

// ---- property: masks ---------------------------------------------------------------------------

fn parse_or_panic(mask: &str, opts: MaskOpts) -> Mask {
    match Mask::parse(mask, opts) {
        Ok(m) => m,
        Err(e) => panic!("mask {mask:?} should parse: {e}"),
    }
}

proptest! {
    #[test]
    fn star_semantics_matches_reference(
        (pre, post, name) in (r"[a-z]{0,4}", r"[a-z]{0,4}", r"[a-z]{1,10}")
    ) {
        let mask = parse_or_panic(&format!("{pre}*{post}"), MaskOpts::default());
        let expect = name.starts_with(&pre)
            && name.ends_with(&post)
            && name.len() >= pre.len() + post.len();
        prop_assert_eq!(mask.is_match(OsStr::new(name.as_str())), expect);

        let star = parse_or_panic("*", MaskOpts::default());
        prop_assert!(star.is_match(OsStr::new(name.as_str())));

        let excl = parse_or_panic(&format!("*;!{pre}*"), MaskOpts::default());
        prop_assert_eq!(excl.is_match(OsStr::new(name.as_str())), !name.starts_with(&pre));
    }

    #[test]
    fn brace_group_is_union_of_alternatives(
        (alts, name) in (vec(r"[a-z]{1,4}", 1..6), r"[a-z]{1,8}")
    ) {
        let group = format!("{{{}}}", alts.join(","));
        let mask = parse_or_panic(&group, MaskOpts::default());
        let expect = alts.iter().any(|a| name == *a);
        prop_assert_eq!(mask.is_match(OsStr::new(name.as_str())), expect);
    }

    #[test]
    fn parsing_arbitrary_input_never_panics(
        (junk, name, ci) in (r"[{}\[\]!,*?a-zA-Z0-9._;]{0,12}", r"[a-zA-Z0-9._]{1,10}", any::<bool>())
    ) {
        if let Ok(m) = Mask::parse(&junk, MaskOpts { case_insensitive: ci }) {
            let _ = m.is_match(OsStr::new(name.as_str()));
        }
    }
}

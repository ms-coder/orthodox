//! T8.5 adversarial tests for `ofm_core::compare` (F27 directory-compare engine, pure part).
//!
//! Extends the module unit tests + proptests with: 100k entries where half are case-variant
//! duplicates (the case-insensitive hash-pairing stress), mtime tolerance off by ±1 ns in both
//! directions with a custom tolerance, symlink-vs-file and fifo-vs-file name clashes, greedy
//! case-insensitive pairing pinned exactly, extreme i128 mtimes (saturating arithmetic), Other-kind
//! entries in every mode, and degenerate names ("", ".", "..") that must never panic.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::time::{Duration, Instant};

use ofm_core::compare::{
    CompareEntry, CompareKind, CompareMode, CompareOptions, MarkReason, apply_content_result,
    compare,
};

fn file(name: &str, size: u64, mtime_ns: i128) -> CompareEntry {
    CompareEntry {
        name: name.into(),
        kind: CompareKind::File,
        size,
        mtime_ns,
        link_target: None,
    }
}

fn names(result: &ofm_core::compare::CompareResult) -> Vec<OsString> {
    fn side(marks: &[ofm_core::compare::Marked]) -> Vec<OsString> {
        marks.iter().map(|m| m.name.clone()).collect()
    }
    let mut all = side(&result.left_marks);
    all.extend(side(&result.right_marks));
    all
}

fn no_marks(r: &ofm_core::compare::CompareResult) -> bool {
    r.left_marks.is_empty() && r.right_marks.is_empty()
}

// --- Scale: 100k with case-duplicate twins -------------------------------------------------

/// 100k entries, 50% of them case-variant twins ("f0001234" / "F0001234"), matched
/// case-insensitively: the pairing hash map sees every key twice. Identical listings must produce
/// zero marks quickly; mutating half of one side's twins must produce exactly the predicted
/// marks, deterministically.
#[test]
fn hundred_k_case_duplicate_twins() {
    let n = 100_000u32; // 50k base names, each twice: "f0123456" and "F0123456"
    let mk = |i: u32, upper: bool, size: u64| {
        let name = if upper {
            format!("F{i:07}")
        } else {
            format!("f{i:07}")
        };
        file(&name, size, i128::from(i))
    };
    let both = |size_of: &dyn Fn(u32, bool) -> u64| -> Vec<CompareEntry> {
        (0..n / 2)
            .flat_map(|i| {
                [
                    mk(i, false, size_of(i, false)),
                    mk(i, true, size_of(i, true)),
                ]
            })
            .collect()
    };
    let left = both(&|_, _| 100);
    let right_same = both(&|_, _| 100);
    let opts = CompareOptions {
        case_insensitive: true,
        mode: CompareMode::NameSize,
        ..CompareOptions::default()
    };

    let t0 = Instant::now();
    let r = compare(&left, &right_same, &opts);
    let elapsed = t0.elapsed();
    assert!(no_marks(&r), "marks: {:?}", names(&r));
    assert!(elapsed.as_secs() < 5, "took {elapsed:?}");

    // Deterministic: same input, same output.
    let r2 = compare(&left, &right_same, &opts);
    assert_eq!(r, r2);

    // Left side: grow every fourth lowercase entry. Greedy pairing (input order on both sides)
    // matches lowercase-with-lowercase and uppercase-with-uppercase, so each mutation marks
    // exactly one pair with SizeDiffers.
    let changed = both(&|i, upper| if !upper && i % 4 == 0 { 101 } else { 100 });
    let r = compare(&left, &changed, &opts);
    let expected = (0..n / 2).filter(|i| i % 4 == 0).count();
    assert_eq!(r.left_marks.len(), expected, "left marks changed?");
    assert_eq!(r.right_marks.len(), expected, "right marks changed?");
    assert!(
        r.left_marks
            .iter()
            .all(|m| m.reason == MarkReason::SizeDiffers)
    );
    // Marks stay sorted by name on both sides.
    for side in [&r.left_marks, &r.right_marks] {
        let mut sorted = side.clone();
        sorted.sort_by(|a, b| a.name.cmp(&b.name));
        assert_eq!(side.to_vec(), sorted);
    }
}

// --- Mtime tolerance ±1 ns -----------------------------------------------------------------

/// Around a custom tolerance, both directions: diff == tol, tol - 1, tol + 1, and the mirrored
/// negative diffs. Boundary belongs to "same".
#[test]
fn mtime_tolerance_off_by_one_ns_both_directions() {
    let tol_ns: i128 = 1_500_000_000; // 1.5 s
    let opts = CompareOptions {
        mode: CompareMode::NameSizeMtime,
        mtime_tolerance: Duration::from_nanos(tol_ns as u64),
        ..CompareOptions::default()
    };
    let base: i128 = 10_000_000_000;
    for (l, r, expect_newer) in [
        (base, base, false),              // identical
        (base + tol_ns, base, false),     // exactly tolerance: same
        (base + tol_ns - 1, base, false), // inside: same
        (base + tol_ns + 1, base, true),  // outside: left newer
        (base, base + tol_ns, false),     // exactly tolerance: same
        (base, base + tol_ns - 1, false), // inside: same
        (base, base + tol_ns + 1, true),  // outside: left older (right newer)
        (base - tol_ns - 1, base, true),  // negative direction, left older
        (base, base - tol_ns - 1, true),  // negative direction, left newer
    ] {
        let left = vec![file("x", 10, l)];
        let right = vec![file("x", 10, r)];
        let res = compare(&left, &right, &opts);
        if expect_newer {
            assert_eq!(res.left_marks.len(), 1, "({l}, {r})");
            let left_newer = l > r;
            assert_eq!(
                res.left_marks[0].reason,
                if left_newer {
                    MarkReason::NewerHere
                } else {
                    MarkReason::OlderHere
                },
                "({l}, {r})"
            );
            assert_eq!(
                res.right_marks[0].reason,
                if left_newer {
                    MarkReason::OlderHere
                } else {
                    MarkReason::NewerHere
                },
                "({l}, {r})"
            );
        } else {
            assert!(no_marks(&res), "({l}, {r}): {:?}", names(&res));
        }
    }
}

/// i128-extreme mtimes must not overflow or panic (saturating arithmetic) and still classify
/// the direction correctly.
#[test]
fn extreme_mtimes_saturate_without_panic() {
    let opts = CompareOptions::default();
    let big = i128::MAX - 10;
    let cases = [
        (big, 0i128, MarkReason::NewerHere, MarkReason::OlderHere),
        (0, big, MarkReason::OlderHere, MarkReason::NewerHere),
        (-big, 0, MarkReason::OlderHere, MarkReason::NewerHere),
        (0, -big, MarkReason::NewerHere, MarkReason::OlderHere),
        (big, -big, MarkReason::NewerHere, MarkReason::OlderHere),
        (
            i128::MAX,
            i128::MAX,
            MarkReason::NewerHere,
            MarkReason::OlderHere,
        ), // == 0, same
    ];
    for (l, r, l_reason, r_reason) in cases {
        let left = vec![file("x", 1, l)];
        let right = vec![file("x", 1, r)];
        let res = compare(&left, &right, &opts);
        if l == r {
            assert!(no_marks(&res), "({l}, {r})");
        } else {
            assert_eq!(res.left_marks[0].reason, l_reason, "({l}, {r})");
            assert_eq!(res.right_marks[0].reason, r_reason, "({l}, {r})");
        }
    }
}

// --- Kind clashes --------------------------------------------------------------------------

/// A symlink and a regular file with the same name are a TypeDiffers in every mode (the engine
/// distinguishes kinds, and a link is never "equal" to its target file by accident).
#[test]
fn symlink_vs_file_same_name_is_type_differs() {
    let mut l = CompareEntry::new("x", CompareKind::Symlink);
    l.link_target = Some(OsString::from("/etc/hostname"));
    for mode in [
        CompareMode::NameOnly,
        CompareMode::NameSize,
        CompareMode::NameSizeMtime,
        CompareMode::Content,
    ] {
        let opts = CompareOptions {
            mode,
            ..CompareOptions::default()
        };
        let r = compare(
            std::slice::from_ref(&l),
            std::slice::from_ref(&file("x", 0, 0)),
            &opts,
        );
        assert_eq!(r.left_marks[0].reason, MarkReason::TypeDiffers, "{mode:?}");
        assert_eq!(r.right_marks[0].reason, MarkReason::TypeDiffers, "{mode:?}");
    }
}

/// FIFO/socket/device (Other) entries go through the size/mtime branch like files: same-name
/// Other entries with different sizes are SizeDiffers in size modes, invisible in NameOnly.
#[test]
fn other_kind_entries_compare_by_size_like_files() {
    let mk_other = |size: u64| {
        let mut e = CompareEntry::new("fifo", CompareKind::Other);
        e.size = size;
        e
    };
    for mode in [CompareMode::NameOnly, CompareMode::NameSize] {
        let opts = CompareOptions {
            mode,
            ..CompareOptions::default()
        };
        let r = compare(&[mk_other(10)], &[mk_other(11)], &opts);
        if mode == CompareMode::NameOnly {
            assert!(no_marks(&r));
        } else {
            assert_eq!(r.left_marks[0].reason, MarkReason::SizeDiffers);
            assert_eq!(r.right_marks[0].reason, MarkReason::SizeDiffers);
        }
    }
    // Other vs File with the same name is a clash, like dir vs file.
    let r = compare(
        &[mk_other(10)],
        &[file("fifo", 10, 0)],
        &CompareOptions::default(),
    );
    assert_eq!(r.left_marks[0].reason, MarkReason::TypeDiffers);
}

// --- Case-insensitive greedy pairing -------------------------------------------------------

/// When one side holds case-variant duplicates and the other a single entry, the pairing is
/// greedy in input order — pin the exact result so the "50% duplicates" behaviour is a contract,
/// not an accident.
#[test]
fn case_insensitive_greedy_pairing_pinned() {
    let opts = CompareOptions {
        case_insensitive: true,
        mode: CompareMode::NameSize,
        ..CompareOptions::default()
    };
    // Left: "FOO"(size 1) first, "foo"(size 2) second; right: "foo"(size 1).
    // FOO folds to "foo" and pairs greedily with the right entry (sizes equal -> no mark);
    // left "foo" (size 2) then has no partner -> MissingOnOtherSide.
    let left = vec![file("FOO", 1, 0), file("foo", 2, 0)];
    let right = vec![file("foo", 1, 0)];
    let r = compare(&left, &right, &opts);
    assert_eq!(names(&r), vec![OsString::from("foo")], "{r:?}");
    assert_eq!(r.left_marks[0].reason, MarkReason::MissingOnOtherSide);
    // Order flipped: "foo"(2) pairs with right "foo"(1) -> SizeDiffers on both, FOO missing.
    let left = vec![file("foo", 2, 0), file("FOO", 1, 0)];
    let r = compare(&left, &right, &opts);
    assert_eq!(r.left_marks.len(), 2);
    assert_eq!(r.right_marks.len(), 1);
    assert_eq!(r.right_marks[0].reason, MarkReason::SizeDiffers);
    let reasons: Vec<_> = r.left_marks.iter().map(|m| m.reason).collect();
    assert!(reasons.contains(&MarkReason::SizeDiffers));
    assert!(reasons.contains(&MarkReason::MissingOnOtherSide));
}

/// Case-insensitive matching must fold full Unicode for valid UTF-8 (not just ASCII):
/// "STRASSE" lowercase-folds against "strasse"... use a real fold pair: 'É' folds to 'é'.
#[test]
fn unicode_case_folds_for_valid_utf8() {
    let opts = CompareOptions {
        case_insensitive: true,
        mode: CompareMode::NameSize,
        ..CompareOptions::default()
    };
    let left = vec![file("CafÉ.txt", 1, 0)];
    let right = vec![file("café.txt", 1, 0)];
    let r = compare(&left, &right, &opts);
    assert!(no_marks(&r), "{:?}", names(&r));
    // And ß vs SS: full Unicode lowercase does NOT fold ß to ss (that is case-folding, not
    // lowercase) — pin that "straße" does NOT match "STRASSE".
    let left = vec![file("straße", 1, 0)];
    let right = vec![file("STRASSE", 1, 0)];
    let r = compare(&left, &right, &opts);
    assert_eq!(r.left_marks.len(), 1);
    assert_eq!(r.left_marks[0].reason, MarkReason::MissingOnOtherSide);
}

// --- Degenerate names and content results --------------------------------------------------

/// Entries named "", ".", ".." and names that are pure duplicates never panic, pair up or mark
/// deterministically, and every mark names an existing entry.
#[test]
fn degenerate_names_never_panic() {
    let left = vec![
        file("", 1, 0),
        file(".", 1, 0),
        file("..", 1, 0),
        file("x", 1, 0),
        file("x", 1, 0), // duplicate within a side
    ];
    let right = vec![
        file("", 1, 0),
        file(".", 1, 0),
        file("..", 2, 0),
        file("x", 1, 0),
    ];
    let opts = CompareOptions {
        mode: CompareMode::NameSize,
        ..CompareOptions::default()
    };
    let r = compare(&left, &right, &opts);
    // ".." differs in size (2 vs 1) -> one mark each side; the extra left "x" is unmatched.
    // names() concatenates both sides, so ".." appears twice.
    let expect: Vec<OsString> = vec!["x".into(), "..".into(), "..".into()];
    let mut got = names(&r);
    got.sort();
    let mut expect_sorted = expect;
    expect_sorted.sort();
    assert_eq!(got, expect_sorted, "{r:?}");
    // Deterministic.
    assert_eq!(r, compare(&left, &right, &opts));
}

/// apply_content_result with hundreds of differing pairs keeps both mark vectors sorted by name.
#[test]
fn content_results_keep_marks_sorted() {
    let opts = CompareOptions {
        mode: CompareMode::Content,
        ..CompareOptions::default()
    };
    let mut left = Vec::new();
    let mut right = Vec::new();
    // Reverse order on the left to stress the sorted insertion.
    for i in (0..500u32).rev() {
        left.push(file(&format!("c{i:04}"), 10, 0));
        right.push(file(&format!("c{i:04}"), 10, 0));
    }
    let mut r = compare(&left, &right, &opts);
    assert_eq!(r.content_candidates.len(), 500);
    let candidates = std::mem::take(&mut r.content_candidates);
    for pair in candidates {
        apply_content_result(&mut r, pair, false);
    }
    assert_eq!(r.left_marks.len(), 500);
    assert_eq!(r.right_marks.len(), 500);
    for side in [&r.left_marks, &r.right_marks] {
        for w in side.windows(2) {
            assert!(w[0].name < w[1].name, "{side:?}");
        }
        assert!(
            side.iter().all(|m| m.reason == MarkReason::ContentDiffers),
            "{side:?}"
        );
    }
}

/// include_dirs=false hides directories entirely, even from a file/dir name clash: the file
/// then compares against nothing and is reported missing... actually with dirs filtered from
/// both sides the clash disappears — pin that the file is simply MissingOnOtherSide.
#[test]
fn include_dirs_false_dissolves_clash() {
    let opts = CompareOptions {
        include_dirs: false,
        ..CompareOptions::default()
    };
    let left = vec![CompareEntry::new("x", CompareKind::Dir), file("f", 1, 0)];
    let right = vec![file("x", 1, 0), file("f", 1, 0)];
    let r = compare(&left, &right, &opts);
    // The file "x" exists only on the right (the left "x" was a dir and got filtered): the mark
    // lands on the right side as MissingOnOtherSide.
    assert!(r.left_marks.is_empty(), "{r:?}");
    assert_eq!(
        r.right_marks
            .iter()
            .map(|m| m.name.clone())
            .collect::<Vec<_>>(),
        vec![OsString::from("x")],
        "{r:?}"
    );
    assert_eq!(r.right_marks[0].reason, MarkReason::MissingOnOtherSide);
}

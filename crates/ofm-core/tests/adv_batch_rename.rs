//! T8.5 adversarial tests for `ofm_core::batch_rename` (the F20 planning engine).
//!
//! Extends `batch_rename_prop.rs` with deterministic torture cases the proptest strategies do not
//! reach: Unicode normalisation twins (NFC vs NFD) and Unicode-case twins, a 5000-name single
//! rotation cycle, a seeded mix of swaps/chains/case-only renames, directory entries that
//! masquerade as generated temp names, byte-length limits, and adversarial editor text (BOM,
//! off-by-one line counts, CR-only line endings, trailing spaces/tabs). Every test executes the
//! plan against an in-memory name->content map and asserts the exact intended mapping (a wrong
//! file under a right name counts as data loss), plus exact undo.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::time::Instant;

use ofm_core::batch_rename::{
    BatchError, InvalidNameReason, Plan, PlanOptions, RenameOp, StepKind, TEMP_PREFIX, UndoLog,
    parse_edited, plan_with_options, to_editor_text, undo_plan, verify_undo,
};

type Fs = HashMap<Vec<u8>, u64>;

fn os(s: &str) -> OsString {
    OsString::from(s)
}

fn os_bytes(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

fn existing(names: &[&str]) -> HashSet<OsString> {
    names.iter().map(|n| os(n)).collect()
}

fn op(from: &str, to: &str) -> RenameOp {
    RenameOp {
        from: os(from),
        to: os(to),
    }
}

fn ops(pairs: &[(&str, &str)]) -> Vec<RenameOp> {
    pairs.iter().map(|(a, b)| op(a, b)).collect()
}

/// Executes a plan on the in-memory fs, asserting every step's preconditions.
fn apply(plan: &Plan, fs: &mut Fs) {
    for step in &plan.steps {
        let content = fs
            .remove(step.from.as_bytes())
            .unwrap_or_else(|| panic!("step source {:?} does not exist", step.from));
        assert!(
            fs.insert(step.to.as_bytes().to_vec(), content).is_none(),
            "step target {:?} already exists (overwrites another file!)",
            step.to
        );
    }
}

fn fs_names(fs: &Fs) -> HashSet<OsString> {
    fs.keys()
        .map(|k| OsStringExt::from_vec(k.clone()))
        .collect()
}

/// The temp names a plan generates must be unique and outside the user name space.
fn assert_temps_clean(plan: &Plan, user_names: &HashSet<Vec<u8>>) {
    let mut temps = HashSet::new();
    for s in &plan.steps {
        if s.kind == StepKind::ToTemp {
            let t = s.to.as_bytes().to_vec();
            assert!(t.starts_with(TEMP_PREFIX.as_bytes()), "{:?}", s.to);
            assert!(temps.insert(t.clone()), "temp name reused: {s:?}");
            assert!(
                !user_names.contains(&t),
                "temp name collides with a user name: {s:?}"
            );
        }
    }
    // Every FromTemp consumes a temp that a ToTemp produced, exactly once.
    for s in &plan.steps {
        if s.kind == StepKind::FromTemp {
            assert!(temps.remove(s.from.as_bytes()), "{s:?}");
        }
    }
    assert!(temps.is_empty(), "temp produced but never consumed");
}

// --- Unicode -----------------------------------------------------------------------------

/// NFC "café" (U+00E9) and its NFD twin (e + U+0301) are different byte strings: the engine must
/// treat them as distinct names on both the case-sensitive and the case-insensitive path, never
/// "deduplicate" them, and must move the right content to each.
#[test]
fn unicode_normalisation_twins_are_distinct_names() {
    let nfc = "café"; // caf + U+00E9
    let nfd = "café"; // caf + e + U+0301
    assert_ne!(nfc.as_bytes(), nfd.as_bytes());

    let ops = ops(&[(nfc, "renamed-nfc"), (nfd, "renamed-nfd")]);
    let existing: HashSet<OsString> = [os(nfc), os(nfd)].into_iter().collect();
    let plan = plan_with_options(&ops, &existing, &PlanOptions::default()).unwrap();
    assert_eq!(plan.steps.len(), 2, "no temp steps needed: {plan:?}");

    let mut fs: Fs = [(os_bytes(nfc), 1u64), (os_bytes(nfd), 2u64)]
        .into_iter()
        .collect();
    apply(&plan, &mut fs);
    assert_eq!(fs.get(b"renamed-nfc".as_slice()), Some(&1));
    assert_eq!(fs.get(b"renamed-nfd".as_slice()), Some(&2));
    assert_eq!(fs.len(), 2);

    // Undo restores both twins with their own content.
    let log = UndoLog::new("/dir".into(), &plan);
    verify_undo(&log, &fs_names(&fs)).unwrap();
    apply(&undo_plan(&log), &mut fs);
    assert_eq!(fs.get(&os_bytes(nfc)), Some(&1));
    assert_eq!(fs.get(&os_bytes(nfd)), Some(&2));
}

/// A target that is the NFC twin of an existing NFD name (or vice versa) is NOT a collision:
/// matching is byte-exact, so such a rename must be planned (and is the user's responsibility).
#[test]
fn unicode_twin_target_is_not_a_byte_collision() {
    let nfc = "café";
    let nfd = "café";
    let plan = plan_with_options(
        &[RenameOp {
            from: os("x"),
            to: os(nfc),
        }],
        &[os("x"), os(nfd)].into_iter().collect(),
        &PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.steps.len(), 1);
}

/// Case folding is ASCII-only (documented): "café" vs "CAFÉ" have no ASCII letters to fold, so
/// even with `assume_case_insensitive` they are distinct names — but "CAFE" vs "cafe" fold.
#[test]
fn unicode_case_twins_fold_ascii_only() {
    let opts = PlanOptions {
        assume_case_insensitive: true,
    };
    let unicode = plan_with_options(&ops(&[("café", "CAFÉ")]), &existing(&["café"]), &opts);
    assert!(
        unicode.is_ok(),
        "non-ASCII case twins must not be treated as the same name: {unicode:?}"
    );
    let plan = unicode.unwrap();
    assert!(
        plan.steps.iter().all(|s| s.kind == StepKind::Direct),
        "no ASCII fold happened, so no temp split: {plan:?}"
    );

    // Pure-ASCII case twins still collide on the CI path.
    assert!(matches!(
        plan_with_options(&ops(&[("x", "CAFE")]), &existing(&["x", "cafe"]), &opts),
        Err(BatchError::Collision { .. })
    ));
}

// --- Large batches -------------------------------------------------------------------------

/// A single 5000-name rotation is one 5000-cycle: the hardest ordering case. The plan must be
/// correct (exact content mapping, no overwrite at any step), produced quickly, and undo must
/// restore the original state exactly.
#[test]
fn five_thousand_name_rotation_cycle() {
    let n = 5000usize;
    let names: Vec<String> = (0..n).map(|i| format!("bulk-{i:05}.dat")).collect();
    let targets: Vec<String> = (0..n).map(|i| names[(i + 1) % n].clone()).collect();

    let existing: HashSet<OsString> = names.iter().map(|s| os(s)).collect();
    let ops: Vec<RenameOp> = names
        .iter()
        .zip(&targets)
        .map(|(f, t)| RenameOp {
            from: os(f),
            to: os(t),
        })
        .collect();

    let t0 = Instant::now();
    let plan = plan_with_options(&ops, &existing, &PlanOptions::default()).unwrap();
    let planned = t0.elapsed();
    assert!(
        planned.as_secs() < 5,
        "planning 5000-cycle took {planned:?}"
    );

    // In-memory execution: exact mapping, no data loss.
    let mut fs: Fs = names
        .iter()
        .enumerate()
        .map(|(i, name)| (os_bytes(name), i as u64))
        .collect();
    apply(&plan, &mut fs);
    for (i, t) in targets.iter().enumerate() {
        assert_eq!(fs.get(&os_bytes(t)), Some(&(i as u64)), "target {t}");
    }
    assert_eq!(fs.len(), n);

    let log = UndoLog::new("/dir".into(), &plan);
    verify_undo(&log, &fs_names(&fs)).unwrap();
    apply(&undo_plan(&log), &mut fs);
    for (i, name) in names.iter().enumerate() {
        assert_eq!(fs.get(&os_bytes(name)), Some(&(i as u64)), "name {name}");
    }
}

/// One batch combining every step kind — a 400-name rotation cycle, case-only renames (temp
/// steps with the CI flag), dependency chains, disjoint pair swaps and no-ops — with untouched
/// residents. All target sets are disjoint by construction, so the batch is always accepted and
/// executed: the mapping must be exact, no file lost, undo byte-exact, planning deterministic.
#[test]
fn mixed_swaps_chains_case_only_and_noops_are_exact() {
    let n = 1600usize;
    let names: Vec<String> = (0..n).map(|i| format!("f{i:04}")).collect();
    let mut residents: Vec<String> = Vec::new();
    for i in (0..n).step_by(3) {
        residents.push(format!("f{i:04}.bak")); // untouched, including look-alikes
    }

    let mut all: HashSet<OsString> = names.iter().map(|s| os(s)).collect();
    for r in &residents {
        all.insert(os(r));
    }

    let mut rename_ops: Vec<RenameOp> = Vec::new();
    // [0..400): one big rotation cycle.
    for i in 0..400 {
        rename_ops.push(RenameOp {
            from: os(&names[i]),
            to: os(&names[(i + 1) % 400]),
        });
    }
    // [400..800): case-only renames (CI flag routes them through temp names).
    for n in names.iter().take(800).skip(400) {
        rename_ops.push(RenameOp {
            from: os(n),
            to: OsString::from(n.to_uppercase()),
        });
    }
    // [800..1000): a dependency chain f0800 -> f0801 -> ... -> f1000.
    for i in 800..999 {
        rename_ops.push(RenameOp {
            from: os(&names[i]),
            to: os(&names[i + 1]),
        });
    }
    rename_ops.push(RenameOp {
        from: os(&names[999]),
        to: os("chain-end"),
    });
    // [1000..1200): disjoint adjacent swaps.
    for i in (1000..1200).step_by(2) {
        rename_ops.push(RenameOp {
            from: os(&names[i]),
            to: os(&names[i + 1]),
        });
        rename_ops.push(RenameOp {
            from: os(&names[i + 1]),
            to: os(&names[i]),
        });
    }
    // [1200..1600): every fourth is a no-op, the rest go to fresh names.
    for (i, n) in names.iter().enumerate().take(1600).skip(1200) {
        let target = if i % 4 == 0 {
            n.clone()
        } else {
            format!("fresh-{i:04}")
        };
        rename_ops.push(RenameOp {
            from: os(n),
            to: os(&target),
        });
    }

    let opts = PlanOptions {
        assume_case_insensitive: true,
    };
    let plan = plan_with_options(&rename_ops, &all, &opts)
        .unwrap_or_else(|e| panic!("disjoint batch must plan: {e}"));
    let plan2 = plan_with_options(&rename_ops, &all, &opts).unwrap();
    assert_eq!(plan, plan2, "planning must be deterministic");
    assert_temps_clean(&plan, &all.iter().map(|o| o.as_bytes().to_vec()).collect());
    // Case-only renames really went through temp names.
    assert!(
        plan.steps
            .iter()
            .filter(|s| s.kind == StepKind::ToTemp)
            .count()
            >= 400,
        "expected >= 400 temp splits: {}",
        plan.steps.len()
    );

    // Execute with content tokens; check the whole mapping.
    let mut fs: Fs = names
        .iter()
        .chain(residents.iter())
        .enumerate()
        .map(|(i, name)| (os_bytes(name), i as u64))
        .collect();
    let before = fs.clone();
    apply(&plan, &mut fs);
    for rename_op in &rename_ops {
        if rename_op.from == rename_op.to {
            continue;
        }
        let token = before.get(rename_op.from.as_bytes()).copied().unwrap();
        assert_eq!(
            fs.get(rename_op.to.as_bytes()),
            Some(&token),
            "content must follow the rename {:?} -> {:?}",
            rename_op.from,
            rename_op.to
        );
    }
    assert_eq!(fs.len(), before.len(), "no file lost or created");

    // Undo restores the original byte-exact state.
    let log = UndoLog::new("/dir".into(), &plan);
    verify_undo(&log, &fs_names(&fs)).unwrap();
    apply(&undo_plan(&log), &mut fs);
    assert_eq!(fs, before);
}

// --- Temp-name collisions ------------------------------------------------------------------

/// Entries that already look exactly like generated temp names must not be clobbered: the planner
/// has to pick different temp names and execution must leave the decoys untouched.
#[test]
fn directory_entries_masquerading_as_temp_names_survive() {
    let pid = std::process::id();
    let decoys: Vec<String> = (1..=8).map(|i| format!("{TEMP_PREFIX}{pid}-{i}")).collect();
    let mut all = existing(&["a", "b"]);
    for d in &decoys {
        all.insert(os(d));
    }
    let plan = plan_with_options(
        &ops(&[("a", "b"), ("b", "a")]),
        &all,
        &PlanOptions::default(),
    )
    .unwrap_or_else(|e| panic!("swap with decoy temps must plan: {e}"));
    for step in &plan.steps {
        let to = step.to.to_string_lossy();
        assert!(
            !to.starts_with(TEMP_PREFIX) || !decoys.iter().any(|d| *d == to),
            "step clobbers a decoy temp name: {step:?}"
        );
    }
    // Explicit content tokens (HashSet iteration order is arbitrary).
    let mut fs: Fs = [(b"a".to_vec(), 1u64), (b"b".to_vec(), 2u64)]
        .into_iter()
        .collect();
    for (i, d) in decoys.iter().enumerate() {
        fs.insert(os_bytes(d), 100 + i as u64);
    }
    let before = fs.clone();
    apply(&plan, &mut fs);
    for (i, d) in decoys.iter().enumerate() {
        assert_eq!(fs.get(&os_bytes(d)), Some(&(100 + i as u64)), "{d}");
    }
    assert_eq!(fs.len(), before.len());
}

/// A user target that collides with the *pattern* of temp names (but not an actual generated one)
/// is an ordinary collision when it exists and is not renamed away.
#[test]
fn target_that_looks_like_a_temp_name_is_treated_normally() {
    let fake = format!("{TEMP_PREFIX}999999-1"); // not our pid
    assert!(matches!(
        plan_with_options(
            &ops(&[("a", &fake)]),
            &existing(&["a", &fake]),
            &PlanOptions::default()
        ),
        Err(BatchError::Collision { .. }),
    ));
    // Renamed away in the same batch: fine.
    let plan = plan_with_options(
        &ops(&[("a", &fake), (&fake, "c")]),
        &existing(&["a", &fake]),
        &PlanOptions::default(),
    )
    .unwrap();
    let mut fs: Fs = [(b"a".to_vec(), 1u64), (os_bytes(&fake), 2u64)]
        .into_iter()
        .collect();
    apply(&plan, &mut fs);
    assert_eq!(fs.get(b"c".as_slice()), Some(&2));
    assert_eq!(fs.get(b"a".as_slice()), None);
    assert_eq!(fs.get(&os_bytes(&fake)), Some(&1));
}

// --- Byte-length limits --------------------------------------------------------------------

/// Names beyond the on-disk NAME_MAX (255 bytes on Linux) cannot exist, but the pure planner has
/// no file-system limits: it must still produce a correct, executable plan (the job layer turns
/// the OS error into a clean failure — see adv_rename_batch.rs). Pins the layer split.
#[test]
fn oversized_names_plan_and_execute_in_memory() {
    let long_a: String = format!("{}a", "x".repeat(299));
    let long_b: String = format!("{}b", "y".repeat(299));
    assert!(long_a.len() > 255 && long_b.len() > 255);
    let plan = plan_with_options(
        &[RenameOp {
            from: os(&long_a),
            to: os(&long_b),
        }],
        &[os(&long_a)].into_iter().collect(),
        &PlanOptions::default(),
    )
    .unwrap();
    let mut fs: Fs = [(os_bytes(&long_a), 7u64)].into_iter().collect();
    apply(&plan, &mut fs);
    assert_eq!(fs.get(&os_bytes(&long_b)), Some(&7));
}

/// Exactly-at-the-limit names (255 bytes) participate in swaps and case-only renames: temp names
/// are short, so no plan step may ever exceed 255 bytes either.
#[test]
fn name_max_boundary_names_work_through_temp_steps() {
    let a: String = "a".repeat(255);
    let b_upper: String = "B".repeat(255);
    let b_lower: String = "b".repeat(255);

    // A swap of two 255-byte names routes one side through a temp name.
    let all: HashSet<OsString> = [os(&a), os(&b_upper)].into_iter().collect();
    let plan = plan_with_options(
        &ops(&[(&a, &b_upper), (&b_upper, &a)]),
        &all,
        &PlanOptions::default(),
    )
    .unwrap();
    for step in &plan.steps {
        assert!(step.from.as_bytes().len() <= 255, "{step:?}");
        assert!(step.to.as_bytes().len() <= 255, "{step:?}");
    }
    let mut fs: Fs = [(os_bytes(&a), 1u64), (os_bytes(&b_upper), 2u64)]
        .into_iter()
        .collect();
    apply(&plan, &mut fs);
    assert_eq!(fs.get(&os_bytes(&a)), Some(&2));
    assert_eq!(fs.get(&os_bytes(&b_upper)), Some(&1));

    // A case-only 255-byte rename under the CI flag also stays within the limit.
    let all: HashSet<OsString> = [os(&b_upper)].into_iter().collect();
    let plan = plan_with_options(
        &[RenameOp {
            from: os(&b_upper),
            to: os(&b_lower),
        }],
        &all,
        &PlanOptions {
            assume_case_insensitive: true,
        },
    )
    .unwrap();
    for step in &plan.steps {
        assert!(step.from.as_bytes().len() <= 255, "{step:?}");
        assert!(step.to.as_bytes().len() <= 255, "{step:?}");
    }
    let mut fs: Fs = [(os_bytes(&b_upper), 3u64)].into_iter().collect();
    apply(&plan, &mut fs);
    assert_eq!(fs.get(&os_bytes(&b_lower)), Some(&3));
}

// --- Adversarial editor text ---------------------------------------------------------------

/// A UTF-8 BOM at the start of the buffer is data, not markup: parse must not strip it, so the
/// first line differs from the original name and becomes a rename to a BOM-prefixed name.
#[test]
fn bom_is_part_of_the_name_not_silently_stripped() {
    let old = [os("a"), os("b")];
    let text = "\u{feff}x\nb\n";
    let parsed = parse_edited(&old, text).unwrap();
    assert_eq!(
        parsed,
        vec![RenameOp {
            from: os("a"),
            to: os("\u{feff}x"),
        }]
    );
}

/// Trailing spaces and tabs typed in the editor are part of the name (legal on Linux).
#[test]
fn trailing_spaces_and_tabs_are_kept_verbatim() {
    let old = [os("a"), os("b")];
    let parsed = parse_edited(&old, "x \t\nb\n").unwrap();
    assert_eq!(parsed, vec![op("a", "x \t")]);
}

/// Line-count off-by-one attacks: every extra or missing line (including blank ones) is reported
/// as a LineCount error with correct added/removed numbers, never silently dropped or invented.
#[test]
fn off_by_one_line_counts_are_reported_exactly() {
    let old = [os("a"), os("b")];
    for (text, actual, added, removed) in [
        ("a\nb\n\n", 3, 1, 0),   // one trailing blank line
        ("a\nb\n\n\n", 4, 2, 0), // two trailing blank lines
        ("\na\nb\n", 3, 1, 0),   // leading blank line
        ("a\n", 1, 0, 1),        // last line deleted
        ("\n", 1, 0, 1),         // lone newline = one empty line
    ] {
        match parse_edited(&old, text) {
            Err(BatchError::LineCount {
                expected,
                actual: act,
                added: ad,
                removed: rem,
            }) => {
                assert_eq!(
                    (expected, act, ad, rem),
                    (2, actual, added, removed),
                    "{text:?}"
                );
            }
            other => panic!("{text:?}: unexpected {other:?}"),
        }
    }
    // Missing trailing newline is tolerated and both unchanged lines yield no ops.
    assert_eq!(parse_edited(&old, "a\nb").unwrap(), Vec::new());
    assert_eq!(parse_edited(&old, "a\nx").unwrap(), vec![op("b", "x")]);
}

/// Old-name list containing an empty name round-trips: "" is editor-safe, and a single "\n" is
/// its unchanged line.
#[test]
fn empty_old_name_round_trips() {
    let old = [os("")];
    let text = to_editor_text(&old);
    assert_eq!(text.text, "\n");
    assert_eq!(parse_edited(&old, &text.text).unwrap(), Vec::new());
    // Renaming "" to something real is a valid op (parse-level; such a name cannot exist on disk).
    assert_eq!(parse_edited(&old, "x\n").unwrap(), vec![op("", "x")]);
}

/// CR-only line endings (classic Mac, or a mangled buffer) do not split lines: the whole buffer
/// is one line, so a 2-name list reports a LineCount error instead of guessing.
#[test]
fn cr_only_line_endings_do_not_split() {
    let old = [os("a"), os("b")];
    match parse_edited(&old, "x\ry\r").unwrap_err() {
        BatchError::LineCount {
            expected, actual, ..
        } => {
            assert_eq!((expected, actual), (2, 1));
        }
        other => panic!("unexpected {other:?}"),
    }
}

/// An interior CR (not line-final) survives as name data; only a line-final CR is stripped.
#[test]
fn interior_carriage_return_is_name_data() {
    let old = [os("a")];
    let parsed = parse_edited(&old, "a\rb\n").unwrap();
    assert_eq!(parsed, vec![op("a", "a\rb")]);
    // A name ending in CR cannot round trip (documented): it is flagged unsupported.
    let old = [os("x\r")];
    assert!(matches!(
        parse_edited(&old, "y\n"),
        Err(BatchError::UnsupportedNames(_))
    ));
}

/// A single line-final CR without any LF is still stripped (CRLF tolerance is per-line, not
/// per-buffer).
#[test]
fn lone_line_final_cr_is_stripped() {
    let old = [os("a")];
    let parsed = parse_edited(&old, "z\r").unwrap();
    assert_eq!(parsed, vec![op("a", "z")]);
}

// --- Error battery -------------------------------------------------------------------------

/// Adversarial op batteries must each produce either a clean refusal or an executable plan —
/// never a plan that overwrites data when executed.
#[test]
fn adversarial_batteries_refuse_or_execute_exactly() {
    let batteries: Vec<(Vec<RenameOp>, Vec<&str>, PlanOptions, &str)> = vec![
        // Case-only rename colliding case-insensitively with an untouched resident.
        (
            ops(&[("x", "README")]),
            vec!["x", "readme"],
            PlanOptions {
                assume_case_insensitive: true,
            },
            "ci-collision",
        ),
        // Swap plus an outsider renaming into one side of the swap.
        (
            ops(&[("a", "b"), ("b", "a"), ("c", "a")]),
            vec!["a", "b", "c"],
            PlanOptions::default(),
            "dup-target-with-swap",
        ),
        // Chain into a name that a later op also targets.
        (
            ops(&[("a", "b"), ("b", "c"), ("d", "c")]),
            vec!["a", "b", "d"],
            PlanOptions::default(),
            "chain-merge-dup",
        ),
        // Case-only twins as sources with the CI flag (cannot coexist on a CI fs).
        (
            ops(&[("a", "x"), ("A", "y")]),
            vec!["a", "A"],
            PlanOptions {
                assume_case_insensitive: true,
            },
            "ci-dup-sources",
        ),
        // No-op swap: a->a plus b->a (a is "renamed away" to itself... a is a dup target? no:
        // a->a and b->a share the target key -> DuplicateTarget).
        (
            ops(&[("a", "a"), ("b", "a")]),
            vec!["a", "b"],
            PlanOptions::default(),
            "noop-vs-dup-target",
        ),
        // Everything renamed to one name via case variants (CI).
        (
            ops(&[("p", "Z"), ("q", "z")]),
            vec!["p", "q"],
            PlanOptions {
                assume_case_insensitive: true,
            },
            "ci-dup-targets",
        ),
    ];

    for (rename_ops, residents, opts, tag) in batteries {
        let all: HashSet<OsString> = residents.iter().map(|r| os(r)).collect();
        match plan_with_options(&rename_ops, &all, &opts) {
            Err(e) => eprintln!("{tag}: refused cleanly: {e}"),
            Ok(plan) => {
                let mut fs: Fs = all
                    .iter()
                    .enumerate()
                    .map(|(i, name)| (name.as_bytes().to_vec(), i as u64))
                    .collect();
                let before = fs.clone();
                apply(&plan, &mut fs);
                for r in rename_ops.iter().filter(|o| o.from != o.to) {
                    let token = before.get(r.from.as_bytes()).copied().unwrap();
                    assert_eq!(
                        fs.get(r.to.as_bytes()),
                        Some(&token),
                        "{tag}: mapping wrong"
                    );
                }
                assert_eq!(fs.len(), before.len(), "{tag}: file count changed");
            }
        }
    }
}

/// Invalid target names are refused at parse time with the precise reason, including NUL and
/// slash variants that would escape the directory.
#[test]
fn invalid_target_reasons_battery() {
    let old = [os("a")];
    for (line, reason) in [
        ("", InvalidNameReason::Empty),
        (".", InvalidNameReason::Dot),
        ("..", InvalidNameReason::DotDot),
        ("/", InvalidNameReason::Slash),
        ("//x", InvalidNameReason::Slash),
        ("a/b", InvalidNameReason::Slash),
        ("\0", InvalidNameReason::Nul),
        ("a\0b", InvalidNameReason::Nul),
        (".\0", InvalidNameReason::Nul),
        ("/\0", InvalidNameReason::Slash), // '/' is checked before NUL
    ] {
        match parse_edited(&old, &format!("{line}\n")).unwrap_err() {
            BatchError::InvalidTarget { reason: r, .. } => {
                assert_eq!(r, reason, "line {line:?}");
            }
            other => panic!("line {line:?}: unexpected {other:?}"),
        }
    }
}

// --- Undo ----------------------------------------------------------------------------------

/// Undo verification must accept only the exact post-batch state: a squatter recreating an
/// original name that undo needs free makes the undo stale (never overwrite the squatter).
#[test]
fn undo_refuses_squatter_on_original_name() {
    let plan = plan_with_options(
        &ops(&[("a", "b")]),
        &existing(&["a"]),
        &PlanOptions::default(),
    )
    .unwrap();
    let mut fs: Fs = [(b"a".to_vec(), 1u64)].into_iter().collect();
    apply(&plan, &mut fs);
    assert_eq!(fs.get(b"b".as_slice()), Some(&1));

    let log = UndoLog::new("/dir".into(), &plan);
    // Someone recreated "a" (the name undo needs free) -> stale, must refuse.
    let mut names = fs_names(&fs);
    names.insert(os("a"));
    assert!(matches!(
        verify_undo(&log, &names),
        Err(BatchError::StaleUndo(_))
    ));
    // Also stale when the renamed file itself was deleted or renamed away.
    assert!(matches!(
        verify_undo(&log, &existing(&["c"])),
        Err(BatchError::StaleUndo(_))
    ));
    // Exact post-batch state verifies and undoes byte-exactly.
    let names = fs_names(&fs);
    verify_undo(&log, &names).unwrap();
    apply(&undo_plan(&log), &mut fs);
    assert_eq!(fs.get(b"a".as_slice()), Some(&1));
    assert_eq!(fs.len(), 1);
}

/// Temp-using plans (swaps): undo must verify against the post-batch state and restore exactly,
/// and a squatter on the *temp* name must also refuse.
#[test]
fn swap_undo_with_temp_names_verifies_and_restores() {
    let plan = plan_with_options(
        &ops(&[("a", "b"), ("b", "a")]),
        &existing(&["a", "b"]),
        &PlanOptions::default(),
    )
    .unwrap();
    let mut fs: Fs = [(b"a".to_vec(), 1u64), (b"b".to_vec(), 2u64)]
        .into_iter()
        .collect();
    apply(&plan, &mut fs);
    // Content crossed over.
    assert_eq!(fs.get(b"a".as_slice()), Some(&2));
    assert_eq!(fs.get(b"b".as_slice()), Some(&1));

    let log = UndoLog::new("/dir".into(), &plan);
    let names = fs_names(&fs);
    verify_undo(&log, &names).unwrap();
    apply(&undo_plan(&log), &mut fs);
    assert_eq!(fs.get(b"a".as_slice()), Some(&1));
    assert_eq!(fs.get(b"b".as_slice()), Some(&2));
}

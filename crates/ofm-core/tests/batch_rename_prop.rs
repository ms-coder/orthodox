//! Property tests for `ofm_core::batch_rename` (Task T8.3a).
//!
//! Strategies cover permuted renames (swaps, chains, n-cycles), renames to fresh disjoint names,
//! and case-only renames (with and without the case-insensitive option). A simulated in-memory
//! file system (name -> content token) applies plans step by step; the final name set must equal
//! the intended one *and* the content mapping must be correct, and undo must restore the
//! original state exactly.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use ofm_core::batch_rename::{
    Plan, PlanOptions, RenameOp, StepKind, UndoLog, plan_with_options, undo_plan, verify_undo,
};
use proptest::prelude::*;

/// In-memory file system: file name -> content token (the original index proves the rename
/// moved the right file, not just produced the right name set).
type Fs = HashMap<Vec<u8>, u64>;

fn os(s: &str) -> OsString {
    OsString::from(s)
}

fn os_bytes(s: &str) -> Vec<u8> {
    s.as_bytes().to_vec()
}

/// Executes a plan step by step, asserting each step's preconditions hold.
fn apply(plan: &Plan, fs: &mut Fs) {
    for step in &plan.steps {
        let content = fs
            .remove(step.from.as_bytes())
            .unwrap_or_else(|| panic!("step source {:?} does not exist", step.from));
        assert!(
            fs.insert(step.to.as_bytes().to_vec(), content).is_none(),
            "step target {:?} already exists",
            step.to
        );
    }
}

fn fs_names(fs: &Fs) -> HashSet<OsString> {
    fs.keys()
        .map(|k| OsStringExt::from_vec(k.clone()))
        .collect()
}

fn ops(names: &[String], targets: &[String]) -> Vec<RenameOp> {
    names
        .iter()
        .zip(targets)
        .map(|(from, to)| RenameOp {
            from: os(from),
            to: os(to),
        })
        .collect()
}

proptest! {
    /// Any permutation of the selected names (rotations cover swaps, chains and full n-cycles)
    /// must execute to the intended permutation and undo must restore the original state.
    #[test]
    fn permutations_execute_and_undo(
        names in prop::collection::hash_set(
            prop::collection::vec(
                prop::sample::select((b'a'..=b'z').collect::<Vec<u8>>()),
                1..=5
            ).prop_map(|b| String::from_utf8(b).unwrap()),
            1..=8
        ),
        shift in 0..8usize,
    ) {
        let names: Vec<String> = names.into_iter().collect();
        let len = names.len();
        let shift = shift % len;
        let targets: Vec<String> = (0..len)
            .map(|i| names[(i + shift) % len].clone())
            .collect();

        let existing: HashSet<OsString> = names.iter().map(|n| os(n)).collect();
        let plan = plan_with_options(
            &ops(&names, &targets),
            &existing,
            &PlanOptions::default(),
        ).unwrap();

        let mut fs: Fs = names
            .iter()
            .enumerate()
            .map(|(i, n)| (os_bytes(n), i as u64))
            .collect();
        apply(&plan, &mut fs);

        // Names: the rotated set. Contents: target `targets[i]` must hold original index i.
        let expected: Fs = targets
            .iter()
            .enumerate()
            .map(|(i, t)| (os_bytes(t), i as u64))
            .collect();
        assert_eq!(fs, expected);

        // Undo restores the original state exactly.
        let log = UndoLog::new("/dir".into(), &plan);
        verify_undo(&log, &fs_names(&fs)).unwrap();
        let undo = undo_plan(&log);
        apply(&undo, &mut fs);
        let original: Fs = names
            .iter()
            .enumerate()
            .map(|(i, n)| (os_bytes(n), i as u64))
            .collect();
        assert_eq!(fs, original);
    }

    /// Renames to fresh names (disjoint from the existing set) must execute directly.
    #[test]
    fn fresh_targets_execute_and_undo(
        names in prop::collection::hash_set(
            prop::collection::vec(
                prop::sample::select((b'a'..=b'z').collect::<Vec<u8>>()),
                1..=5
            ).prop_map(|b| String::from_utf8(b).unwrap()),
            1..=8
        ),
        targets in prop::collection::hash_set(
            prop::collection::vec(
                prop::sample::select((b'A'..=b'Z').collect::<Vec<u8>>()),
                1..=5
            ).prop_map(|b| String::from_utf8(b).unwrap()),
            1..=8
        ),
    ) {
        let names: Vec<String> = names.into_iter().collect();
        let targets: Vec<String> = targets.into_iter().take(names.len()).collect();
        if targets.len() < names.len() {
            return Ok(());
        }

        let existing: HashSet<OsString> = names.iter().map(|n| os(n)).collect();
        let plan = plan_with_options(
            &ops(&names, &targets),
            &existing,
            &PlanOptions::default(),
        ).unwrap();

        let mut fs: Fs = names
            .iter()
            .enumerate()
            .map(|(i, n)| (os_bytes(n), i as u64))
            .collect();
        apply(&plan, &mut fs);

        let expected: Fs = targets
            .iter()
            .enumerate()
            .map(|(i, t)| (os_bytes(t), i as u64))
            .collect();
        assert_eq!(fs, expected);

        let log = UndoLog::new("/dir".into(), &plan);
        verify_undo(&log, &fs_names(&fs)).unwrap();
        let undo = undo_plan(&log);
        apply(&undo, &mut fs);
        let original: Fs = names
            .iter()
            .enumerate()
            .map(|(i, n)| (os_bytes(n), i as u64))
            .collect();
        assert_eq!(fs, original);
    }

    /// Case-only renames (a.txt -> A.TXT style, distinct case) work directly on a
    /// case-sensitive file system and via temp steps with `assume_case_insensitive`.
    #[test]
    fn case_only_renames(
        names in prop::collection::hash_set(
            prop::collection::vec(
                prop::sample::select((b'a'..=b'z').collect::<Vec<u8>>()),
                1..=5
            ).prop_map(|b| String::from_utf8(b).unwrap()),
            1..=6
        ),
    ) {
        let names: Vec<String> = names.into_iter().collect();
        let targets: Vec<String> = names.iter().map(|n| n.to_uppercase()).collect();

        let existing: HashSet<OsString> = names.iter().map(|n| os(n)).collect();
        let original: Fs = names
            .iter()
            .enumerate()
            .map(|(i, n)| (os_bytes(n), i as u64))
            .collect();

        for ci in [false, true] {
            let options = PlanOptions { assume_case_insensitive: ci };
            let plan = plan_with_options(&ops(&names, &targets), &existing, &options).unwrap();
            if ci {
                // Case-only renames are split into temp steps.
                assert!(
                    plan.steps.iter().any(|s| s.kind == StepKind::ToTemp),
                    "expected temp steps with CI option"
                );
            }
            let mut fs = original.clone();
            apply(&plan, &mut fs);
            let expected: Fs = targets
                .iter()
                .enumerate()
                .map(|(i, t)| (os_bytes(t), i as u64))
                .collect();
            assert_eq!(fs, expected);

            let log = UndoLog::new("/dir".into(), &plan);
            verify_undo(&log, &fs_names(&fs)).unwrap();
            apply(&undo_plan(&log), &mut fs);
            assert_eq!(fs, original);
        }
    }
}

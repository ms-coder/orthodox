//! T8.3b: crash safety of the batch rename undo log (write-ahead) and chunked writes for big
//! batches.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod rename_batch_support;
mod t32_support;

use std::sync::Arc;

use ofm_core::batch_rename::{UndoLog, save_undo_log};
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::rename_batch::chunk_for;
use ofm_jobs::{JobSpec, Outcome};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use rename_batch_support::*;
use t32_support::TestFs;

const FILES: [&str; 5] = ["a", "b", "c", "d", "e"];

fn compare() -> Compare {
    Compare {
        mtime: false,
        ..Compare::all()
    }
}

fn five_steps(e: &Env) -> ofm_core::batch_rename::Plan {
    plan_of(
        &e.work,
        &[("a", "v"), ("b", "w"), ("c", "x"), ("d", "y"), ("e", "z")],
    )
}

#[test]
fn hard_kill_mid_batch_leaves_a_log_that_still_rolls_back() {
    // A panic inside the file system layer is not caught by the job's own cleanup (it unwinds to
    // `run_job`), so the log stays as the write-ahead left it: it lists the step that was about to
    // run. Rollback must recognise that tail as "did not run" and restore exactly.
    let files = ["a", "b", "c", "d"];
    for (pairs, kill_at) in [
        (vec![("a", "w"), ("b", "x"), ("c", "y"), ("d", "z")], 3u64),
        // chain + cycle: the not-run tail has dependent steps
        (vec![("a", "b"), ("b", "c"), ("c", "a"), ("d", "z")], 2),
        (vec![("a", "b"), ("b", "a")], 3),
    ] {
        let e = env(&files);
        let p = plan_of(&e.work, &pairs);
        let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(kill_at, || {
            panic!("simulated kill");
        })));
        let out = run_quiet(JobSpec::RenameBatch(batch(&e, p.clone())), fs);
        let (msg, _) = failed(&out);
        assert!(msg.contains("internal error"), "{msg}");
        // Write-ahead: the interrupted step is listed although it did not run.
        let log = ofm_jobs::load_log(&e.log).unwrap();
        assert_eq!(log.steps.len(), kill_at as usize, "{pairs:?}");
        assert_eq!(log.steps, p.steps[..kill_at as usize].to_vec());

        let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
        assert!(matches!(undo, Outcome::Ok(_)), "{pairs:?}: {undo:?}");
        assert_eq!(report(&undo).files_done, kill_at - 1);
        assert_trees_equal(&e.work, pristine(&files).path(), compare());
        assert!(!e.log.exists());
    }
}

#[test]
fn log_listing_steps_that_never_ran_is_tolerated_but_only_at_the_tail() {
    let e = env(&FILES);
    let p = five_steps(&e);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::RenameNoreplace,
        3,
        FaultErrno::Io,
    )));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p.clone())), fs);
    failed(&out);
    // Make the log list all five steps although only two ran.
    let full = UndoLog::new(e.work.clone(), &p);
    save_undo_log(&full, &e.log).unwrap();
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_eq!(report(&undo).files_done, 2);
    assert_trees_equal(&e.work, pristine(&FILES).path(), compare());

    // Not at the tail: step 2's result vanished (renamed away by someone) -> refused, untouched.
    let e = env(&FILES);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, five_steps(&e))), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    std::fs::rename(e.work.join("w"), e.work.join("elsewhere")).unwrap();
    let before = listing(&e.work);
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    failed(&undo);
    assert_eq!(listing(&e.work), before);
}

#[test]
fn large_batches_use_chunked_log_writes_and_stay_exact_on_stop() {
    assert_eq!(chunk_for(1), 1);
    assert_eq!(chunk_for(100), 1);
    assert_eq!(chunk_for(1000), 10);
    assert_eq!(chunk_for(1_000_000), 1024);

    let names: Vec<String> = (0..300).map(|i| format!("f{i:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let e = env(&refs);
    let pairs: Vec<(String, String)> = names
        .iter()
        .map(|n| (n.clone(), format!("g{}", &n[1..])))
        .collect();
    let pair_refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let p = plan_of(&e.work, &pair_refs);
    // Stop in the middle of a chunk (chunk = 3): the log is trimmed to exactly the done steps.
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::RenameNoreplace,
        101,
        FaultErrno::Io,
    )));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    failed(&out);
    assert_eq!(report(&out).files_done, 100);
    assert_eq!(log_steps(&e), 100);
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_trees_equal(&e.work, pristine(&refs).path(), compare());
}

#[test]
fn large_batch_killed_mid_chunk_rolls_back_with_a_multi_step_tail() {
    let names: Vec<String> = (0..300).map(|i| format!("f{i:03}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let e = env(&refs);
    let pairs: Vec<(String, String)> = names
        .iter()
        .map(|n| (n.clone(), format!("g{}", &n[1..])))
        .collect();
    let pair_refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let p = plan_of(&e.work, &pair_refs);
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(101, || panic!("simulated kill"))));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    failed(&out);
    // The last write-ahead (chunk of 3) listed steps 100..=102 (1-based) ahead of time: 102 are
    // logged, 100 ran, the 101st was about to.
    assert_eq!(log_steps(&e), 102);
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_eq!(report(&undo).files_done, 100);
    assert_trees_equal(&e.work, pristine(&refs).path(), compare());
}

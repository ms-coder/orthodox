//! T8.3b: batch rename data safety (F20, invariant 3): nothing is overwritten, every failure and
//! cancel leaves an exact log, rollback/undo restore exactly, stale plans and logs are refused.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod rename_batch_support;
mod t32_support;

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;

use ofm_core::CancelToken;
use ofm_core::batch_rename::{Plan, Step, StepKind, TEMP_PREFIX, UndoLog, save_undo_log};
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{JobSpec, Outcome, RollbackSpec};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use rename_batch_support::*;
use t32_support::TestFs;

fn compare() -> Compare {
    Compare {
        mtime: false,
        ..Compare::all()
    }
}

const FILES: [&str; 5] = ["a", "b", "c", "d", "e"];

fn direct(from: &str, to: &str) -> Step {
    Step {
        from: OsString::from(from),
        to: OsString::from(to),
        kind: StepKind::Direct,
    }
}

fn manual_plan(steps: Vec<Step>) -> Plan {
    let preview = steps
        .iter()
        .map(|s| (s.from.clone(), s.to.clone()))
        .collect();
    Plan { steps, preview }
}

/// Plan renaming a..e to v..z (5 direct steps).
fn five_steps(e: &Env) -> Plan {
    plan_of(
        &e.work,
        &[("a", "v"), ("b", "w"), ("c", "x"), ("d", "y"), ("e", "z")],
    )
}

fn assert_restored(e: &Env) {
    assert_trees_equal(&e.work, pristine(&FILES).path(), compare());
}

// ---- target exists: never overwritten --------------------------------------------------------

#[test]
fn existing_target_is_caught_before_anything_changes() {
    let e = env(&FILES);
    // A hand-made (stale) plan: `b` exists and is not renamed away.
    let p = manual_plan(vec![direct("a", "x"), direct("c", "b")]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    let (msg, path) = failed(&out);
    assert!(msg.contains("step 2/2"), "{msg}");
    assert_eq!(path, Some(e.work.join("b").as_path()));
    assert_restored(&e);
    assert!(!e.log.exists(), "nothing done, no log");
}

#[test]
fn target_created_after_start_stops_the_job_and_is_never_overwritten() {
    let e = env(&FILES);
    let p = five_steps(&e);
    // On the second step's lstat somebody creates "w" (the target of step 2).
    let work = e.work.clone();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(2, move || {
        std::fs::write(work.join("w"), b"PRECIOUS").unwrap();
    })));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    let (msg, path) = failed(&out);
    assert!(msg.contains("step 2/5") && msg.contains("exists"), "{msg}");
    assert_eq!(path, Some(e.work.join("w").as_path()));
    assert_eq!(report(&out).files_done, 1);
    // Step 1 done, the rest untouched, the intruder intact.
    assert_eq!(
        listing(&e.work),
        expect(&[
            ("v", "a"),
            ("b", "b"),
            ("c", "c"),
            ("d", "d"),
            ("e", "e"),
            ("w", "PRECIOUS")
        ])
    );
    assert_eq!(log_steps(&e), 1);
    // Roll back what was done.
    std::fs::remove_file(e.work.join("w")).unwrap();
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_restored(&e);
}

#[test]
fn replace_fallback_never_overwrites_either() {
    // A file system without RENAME_NOREPLACE (EINVAL): link+unlink fallback must still refuse an
    // existing target.
    let e = env(&FILES);
    let p = manual_plan(vec![direct("a", "x"), direct("c", "d")]);
    let spec = batch(&e, p);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_from(
        FaultOp::RenameNoreplace,
        1,
        FaultErrno::Raw(22),
    )));
    let out = run_quiet(JobSpec::RenameBatch(spec), fs);
    // Preflight already refuses "d" (exists, not renamed): nothing moved.
    failed(&out);
    assert_restored(&e);

    // Same fallback on a good plan works.
    let p = plan_of(&e.work, &[("a", "x"), ("c", "z")]);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_from(
        FaultOp::RenameNoreplace,
        1,
        FaultErrno::Raw(22),
    )));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(e.work.join("x")).unwrap(), b"a");
    assert_eq!(std::fs::read(e.work.join("z")).unwrap(), b"c");
}

// ---- cancel and rollback -------------------------------------------------------------------

#[test]
fn cancel_after_n_steps_then_rollback_restores_exactly() {
    for n in 1..=4u64 {
        let e = env(&FILES);
        let p = five_steps(&e);
        let cancel = CancelToken::new();
        let c = cancel.clone();
        let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(n, move || c.cancel())));
        let (out, rec) = run_cancellable(JobSpec::RenameBatch(batch(&e, p)), fs, cancel);
        assert!(matches!(out, Outcome::Cancelled(_)), "n={n}: {out:?}");
        assert!(rec.lock().unwrap().questions.is_empty());
        assert_eq!(report(&out).files_done, n, "cancel only between steps");
        assert_eq!(log_steps(&e), n as usize);

        let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
        assert!(matches!(undo, Outcome::Ok(_)), "n={n}: {undo:?}");
        assert_restored(&e);
        assert!(!e.log.exists());
    }
}

#[test]
fn cancel_in_the_middle_of_a_swap_logs_the_temp_name_and_rollback_cleans_it() {
    let files = ["a", "b"];
    let e = env(&files);
    let p = plan_of(&e.work, &[("a", "b"), ("b", "a")]);
    assert_eq!(p.steps.len(), 3);
    let cancel = CancelToken::new();
    let c = cancel.clone();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(1, move || c.cancel())));
    let (out, _) = run_cancellable(JobSpec::RenameBatch(batch(&e, p)), fs, cancel);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    // The file sits under the temp name and the log says exactly so.
    let temps: Vec<OsString> = names(&e.work)
        .into_iter()
        .filter(|n| n.to_string_lossy().starts_with(TEMP_PREFIX))
        .collect();
    assert_eq!(temps.len(), 1, "{:?}", names(&e.work));
    let log = ofm_jobs::load_log(&e.log).unwrap();
    assert_eq!(log.steps.len(), 1);
    assert_eq!(log.steps[0].kind, StepKind::ToTemp);
    assert_eq!(log.steps[0].to, temps[0]);
    assert!(
        log.renames.is_empty(),
        "no user-visible rename completed yet"
    );

    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_trees_equal(&e.work, pristine(&files).path(), compare());
}

#[test]
fn cancelled_rollback_keeps_an_accurate_log_and_can_be_resumed() {
    let e = env(&FILES);
    let p = five_steps(&e);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");

    let cancel = CancelToken::new();
    let c = cancel.clone();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(2, move || c.cancel())));
    let (undo, _) = run_cancellable(JobSpec::RenameRollback(rollback_spec(&e)), fs, cancel);
    assert!(matches!(undo, Outcome::Cancelled(_)), "{undo:?}");
    assert_eq!(report(&undo).files_done, 2);
    assert_eq!(log_steps(&e), 3, "five steps, two undone");

    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_restored(&e);
    assert!(!e.log.exists());
}

// ---- identity check ------------------------------------------------------------------------

#[test]
fn source_replaced_between_plan_and_run_stops_the_job() {
    let e = env(&FILES);
    let p = five_steps(&e);
    let mut spec = batch(&e, p.clone());
    spec.identities = identities(&e, &p);
    // Replace "c" by a different file (new inode: create first, then rename over).
    std::fs::write(e.work.join("c.new"), b"IMPOSTOR").unwrap();
    std::fs::rename(e.work.join("c.new"), e.work.join("c")).unwrap();

    let out = run_quiet(JobSpec::RenameBatch(spec), real());
    let (msg, path) = failed(&out);
    assert!(msg.contains("changed since"), "{msg}");
    assert_eq!(path, Some(e.work.join("c").as_path()));
    assert_eq!(report(&out).files_done, 2, "a and b were renamed before");
    assert_eq!(std::fs::read(e.work.join("c")).unwrap(), b"IMPOSTOR");
    assert_eq!(log_steps(&e), 2);
    assert!(!e.work.join("x").exists());
}

#[test]
fn temp_file_swapped_while_parked_is_detected() {
    let files = ["a", "b"];
    let e = env(&files);
    let p = plan_of(&e.work, &[("a", "b"), ("b", "a")]);
    let work = e.work.clone();
    let temp = p
        .steps
        .iter()
        .find(|s| s.kind == StepKind::ToTemp)
        .unwrap()
        .to
        .clone();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(3, move || {
        // lstat #3 is step 3 (temp -> final): swap the parked file.
        let t = work.join(&temp);
        std::fs::write(work.join("imp"), b"IMPOSTOR").unwrap();
        std::fs::rename(work.join("imp"), t).unwrap();
    })));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    let (msg, _) = failed(&out);
    assert!(msg.contains("changed since"), "{msg}");
    assert_eq!(report(&out).files_done, 2);
}

// ---- errors mid-batch ----------------------------------------------------------------------

#[test]
fn permission_denied_mid_batch_stops_reports_and_can_roll_back() {
    let e = env(&FILES);
    let p = five_steps(&e);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::RenameNoreplace,
        3,
        FaultErrno::PermissionDenied,
    )));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs.clone());
    let (msg, path) = failed(&out);
    assert!(msg.contains("step 3/5"), "{msg}");
    assert!(path.is_some());
    assert_eq!(report(&out).files_done, 2);
    assert_eq!(
        fs.calls_of(FaultOp::RenameNoreplace),
        3,
        "stops at the failure"
    );
    assert_eq!(log_steps(&e), 2);
    assert_eq!(
        listing(&e.work),
        expect(&[("v", "a"), ("w", "b"), ("c", "c"), ("d", "d"), ("e", "e")])
    );

    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_restored(&e);
}

#[test]
fn real_permission_denied_on_a_read_only_directory() {
    if is_root() {
        return;
    }
    let e = env(&FILES);
    let p = five_steps(&e);
    std::fs::set_permissions(&e.work, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    std::fs::set_permissions(&e.work, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (msg, _) = failed(&out);
    assert!(msg.contains("step 1/5"), "{msg}");
    assert_eq!(report(&out).files_done, 0);
    assert_restored(&e);
}

#[test]
fn exdev_is_a_clear_error() {
    let e = env(&FILES);
    let p = five_steps(&e);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::RenameNoreplace,
        1,
        FaultErrno::Raw(18),
    )));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    let (msg, _) = failed(&out);
    assert!(msg.contains("EXDEV"), "{msg}");
    assert_restored(&e);
}

#[test]
fn missing_source_stops_before_anything_changes() {
    let e = env(&FILES);
    let p = five_steps(&e);
    std::fs::remove_file(e.work.join("d")).unwrap();
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    let (msg, path) = failed(&out);
    assert!(msg.contains("step 4/5"), "{msg}");
    assert_eq!(path, Some(e.work.join("d").as_path()));
    assert_eq!(report(&out).files_done, 0);
    assert!(names(&e.work).contains(std::ffi::OsStr::new("a")));
}

// ---- the log is incremental ----------------------------------------------------------------

#[test]
fn log_has_exactly_the_completed_steps_whenever_the_job_dies() {
    // "Kill simulation": the k+1-th rename fails (and nothing after runs); the log on disk must
    // list exactly the k steps that were done, including swap temp steps.
    let files = ["a", "b", "c", "d"];
    let pairs = [("a", "b"), ("b", "a"), ("c", "x"), ("d", "y")];
    let total = {
        let e = env(&files);
        plan_of(&e.work, &pairs).steps.len()
    };
    for k in 0..total {
        let e = env(&files);
        let p = plan_of(&e.work, &pairs);
        let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_from(
            FaultOp::RenameNoreplace,
            k as u64 + 1,
            FaultErrno::Io,
        )));
        let out = run_quiet(JobSpec::RenameBatch(batch(&e, p.clone())), fs);
        failed(&out);
        assert_eq!(report(&out).files_done, k as u64);
        let log = ofm_jobs::load_log(&e.log).unwrap();
        assert_eq!(log.steps, p.steps[..k].to_vec(), "k={k}");
        // The log is consistent with the directory: undo restores.
        if k > 0 {
            let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
            assert!(matches!(undo, Outcome::Ok(_)), "k={k}: {undo:?}");
        }
        assert_trees_equal(&e.work, pristine(&files).path(), compare());
    }
}

#[test]
fn log_location_not_writable_fails_before_the_first_rename() {
    let e = env(&FILES);
    let p = five_steps(&e);
    // The log's parent is a regular file.
    let blocker = e.root.path().join("blocker");
    std::fs::write(&blocker, b"x").unwrap();
    let mut spec = batch(&e, p);
    spec.undo_log = blocker.join("undo.toml");
    let out = run_quiet(JobSpec::RenameBatch(spec), real());
    failed(&out);
    assert_restored(&e);
}

#[test]
fn log_write_failure_mid_batch_stops_before_the_unrecorded_step() {
    if is_root() {
        return;
    }
    let e = env(&FILES);
    let p = five_steps(&e);
    // During step 2 the state directory becomes read-only: the write-ahead of step 3 fails, so
    // step 3 never runs.
    let state = e.log.parent().unwrap().to_path_buf();
    let fs = Arc::new(TestFs::real().hooked(on_nth_lstat(2, {
        let state = state.clone();
        move || std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o500)).unwrap()
    })));
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), fs);
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).unwrap();
    failed(&out);
    // Exactly the two recorded steps are applied.
    assert_eq!(log_steps(&e), 2);
    assert_eq!(
        listing(&e.work),
        expect(&[("v", "a"), ("w", "b"), ("c", "c"), ("d", "d"), ("e", "e")])
    );
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_restored(&e);
}

// ---- refusing bad requests -----------------------------------------------------------------

#[test]
fn names_that_are_not_plain_components_are_refused() {
    for bad in ["../evil", "a/b", "", ".", "..", "nul\0byte"] {
        let e = env(&FILES);
        let p = manual_plan(vec![direct("a", bad)]);
        let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
        let (msg, _) = failed(&out);
        assert!(msg.contains("invalid request"), "{bad:?}: {msg}");
        assert_restored(&e);
    }
}

#[test]
fn relative_directory_is_refused() {
    let e = env(&FILES);
    let mut spec = batch(&e, five_steps(&e));
    spec.dir = PathBuf::from("work");
    let out = run_quiet(JobSpec::RenameBatch(spec), real());
    assert!(failed(&out).0.contains("absolute"));
}

#[test]
fn tampered_log_cannot_escape_the_directory() {
    let e = env(&FILES);
    let log = UndoLog {
        dir: e.work.clone(),
        steps: vec![direct("a", "../escaped")],
        renames: vec![],
    };
    save_undo_log(&log, &e.log).unwrap();
    let out = run_quiet(
        JobSpec::RenameRollback(RollbackSpec {
            log: ofm_jobs::load_log(&e.log).unwrap(),
            log_path: e.log.clone(),
        }),
        real(),
    );
    assert!(failed(&out).0.contains("invalid request"));
    assert_restored(&e);
}

#[test]
fn stale_undo_is_refused_and_touches_nothing() {
    let e = env(&FILES);
    let p = five_steps(&e);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // The user renamed one of the files meanwhile.
    std::fs::rename(e.work.join("x"), e.work.join("moved")).unwrap();
    let before = listing(&e.work);
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    let (msg, _) = failed(&undo);
    assert!(msg.contains("no longer applicable"), "{msg}");
    assert_eq!(listing(&e.work), before);
    assert!(e.log.exists(), "log kept");

    // A new file squatting on an original name is refused too (never overwritten).
    std::fs::rename(e.work.join("moved"), e.work.join("x")).unwrap();
    std::fs::write(e.work.join("a"), b"SQUATTER").unwrap();
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Failed { .. }), "{undo:?}");
    assert_eq!(std::fs::read(e.work.join("a")).unwrap(), b"SQUATTER");
}

#[test]
fn rollback_of_a_log_with_nothing_to_undo_is_ok() {
    let e = env(&FILES);
    let log = UndoLog {
        dir: e.work.clone(),
        steps: vec![],
        renames: vec![],
    };
    save_undo_log(&log, &e.log).unwrap();
    let out = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!e.log.exists());
    assert_restored(&e);
}

#[test]
fn titles_name_the_directory() {
    let e = env(&FILES);
    let t = JobSpec::RenameBatch(batch(&e, five_steps(&e))).title();
    assert!(t.starts_with("Rename 5 items in "), "{t}");
}

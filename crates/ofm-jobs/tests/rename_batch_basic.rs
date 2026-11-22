//! T8.3b: batch rename happy paths (F20): simple, swap, cycle, chain, case-only, undo, non-UTF-8,
//! progress and a 1000-file sanity run. Real file system in temp dirs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod rename_batch_support;

use std::collections::HashSet;
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::time::Instant;

use ofm_core::batch_rename::{Plan, RenameOp, Step, StepKind, TEMP_PREFIX, plan};
use ofm_jobs::{JobSpec, Outcome};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::tree::raw_name;
use rename_batch_support::*;

fn has_temp_leftovers(env: &Env) -> bool {
    names(&env.work)
        .iter()
        .any(|n| n.to_string_lossy().starts_with(TEMP_PREFIX))
}

fn compare() -> Compare {
    Compare {
        mtime: false,
        ..Compare::all()
    }
}

#[test]
fn simple_renames_and_log() {
    let files = ["a", "b", "c"];
    let e = env(&files);
    let p = plan_of(&e.work, &[("a", "x"), ("b", "y")]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        listing(&e.work),
        expect(&[("x", "a"), ("y", "b"), ("c", "c")])
    );
    assert_eq!(report(&out).files_done, 2);
    assert_eq!(
        report(&out).created,
        vec![e.work.join("x"), e.work.join("y")]
    );
    let log = ofm_jobs::load_log(&e.log).unwrap();
    assert_eq!(log.steps.len(), 2);
    assert_eq!(log.renames.len(), 2);
    assert_eq!(log.dir, e.work);
    let mode = std::fs::metadata(&e.log).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn empty_plan_is_a_no_op_and_writes_no_log() {
    let e = env(&["a"]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, Plan::default())), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!e.log.exists());
}

#[test]
fn swap_uses_temp_name_and_leaves_none() {
    let e = env(&["a", "b"]);
    let p = plan_of(&e.work, &[("a", "b"), ("b", "a")]);
    assert_eq!(p.steps.len(), 3);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(listing(&e.work), expect(&[("a", "b"), ("b", "a")]));
    assert!(!has_temp_leftovers(&e));
    assert_eq!(report(&out).files_done, 3);
    assert_eq!(log_steps(&e), 3);
}

#[test]
fn three_cycle() {
    let e = env(&["a", "b", "c"]);
    let p = plan_of(&e.work, &[("a", "b"), ("b", "c"), ("c", "a")]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        listing(&e.work),
        expect(&[("b", "a"), ("c", "b"), ("a", "c")])
    );
    assert!(!has_temp_leftovers(&e));
}

#[test]
fn chain_in_dependency_order() {
    let e = env(&["a", "b"]);
    let p = plan_of(&e.work, &[("a", "b"), ("b", "c")]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(listing(&e.work), expect(&[("b", "a"), ("c", "b")]));
}

#[test]
fn case_only_rename_through_temp_steps() {
    let e = env(&["Readme.TXT", "x"]);
    let p = plan_ci(&e.work, &[("Readme.TXT", "readme.txt")], true);
    assert_eq!(
        p.steps.len(),
        2,
        "case-only goes through a temp name: {p:?}"
    );
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        listing(&e.work),
        expect(&[("readme.txt", "Readme.TXT"), ("x", "x")])
    );
}

#[test]
fn directories_and_symlinks_are_renamed_not_followed() {
    let e = env(&["a"]);
    std::fs::create_dir(e.work.join("d")).unwrap();
    std::fs::write(e.work.join("d/inner"), b"in").unwrap();
    std::os::unix::fs::symlink("/nonexistent/target", e.work.join("dangling")).unwrap();
    let p = plan_of(&e.work, &[("d", "d2"), ("dangling", "link2")]);
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(e.work.join("d2/inner")).unwrap(), b"in");
    assert_eq!(
        std::fs::read_link(e.work.join("link2")).unwrap(),
        std::path::Path::new("/nonexistent/target")
    );
    assert!(!e.work.join("dangling").exists() && !e.work.join("d").exists());
}

#[test]
fn undo_of_a_finished_batch_restores_the_tree_exactly() {
    let files = ["a", "b", "c", "d", "e"];
    let e = env(&files);
    // swap + 3-cycle + a plain rename: every step kind.
    let p = plan_of(
        &e.work,
        &[("a", "b"), ("b", "a"), ("c", "d"), ("d", "e"), ("e", "c")],
    );
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_ne!(listing(&e.work), listing(pristine(&files).path()));

    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_trees_equal(&e.work, pristine(&files).path(), compare());
    assert!(!e.log.exists(), "finished undo removes the log");
    assert_eq!(report(&undo).files_done, 7);
}

#[test]
fn rename_back_and_forth_keeps_content_with_names() {
    // do, undo, do again: the log is replaced each time.
    let files = ["a", "b"];
    let e = env(&files);
    for _ in 0..2 {
        let p = plan_of(&e.work, &[("a", "b"), ("b", "a")]);
        let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
        assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
        let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
        assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
        assert_trees_equal(&e.work, pristine(&files).path(), compare());
    }
}

#[test]
fn non_utf8_names_work_and_survive_the_log() {
    let e = env(&[]);
    let bad_a = raw_name(b"caf\xe9-\xff.txt");
    let bad_b = raw_name(b"\xfe\xfd");
    std::fs::write(e.work.join(&bad_a), b"A").unwrap();
    std::fs::write(e.work.join(&bad_b), b"B").unwrap();
    let existing: HashSet<OsString> = names(&e.work);
    let ops = vec![
        RenameOp {
            from: bad_a.clone(),
            to: OsString::from("plain.txt"),
        },
        RenameOp {
            from: bad_b.clone(),
            to: raw_name(b"new-\xc3\x28"),
        },
    ];
    let p = plan(&ops, &existing).unwrap();
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(e.work.join("plain.txt")).unwrap(), b"A");
    assert_eq!(
        std::fs::read(e.work.join(raw_name(b"new-\xc3\x28"))).unwrap(),
        b"B"
    );
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_eq!(std::fs::read(e.work.join(&bad_a)).unwrap(), b"A");
    assert_eq!(std::fs::read(e.work.join(&bad_b)).unwrap(), b"B");
    assert_eq!(names(&e.work).len(), 2);
}

#[test]
fn progress_is_reported_per_step() {
    let e = env(&["a", "b", "c"]);
    let p = plan_of(&e.work, &[("a", "b"), ("b", "a"), ("c", "z")]);
    let n = p.steps.len() as u64;
    let (out, rec) = run_on(JobSpec::RenameBatch(batch(&e, p)), real());
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let rec = rec.lock().unwrap();
    let last = rec.progress.last().unwrap();
    assert_eq!(last.files_total, n);
    assert_eq!(last.files_done, n);
    assert!(last.current.is_some());
}

#[test]
fn thousand_files_with_incremental_log_stay_fast_and_undo_exactly() {
    let names_owned: Vec<String> = (0..1000).map(|i| format!("file-{i:04}.txt")).collect();
    let refs: Vec<&str> = names_owned.iter().map(String::as_str).collect();
    let e = env(&refs);
    // Rename 997 files and rotate the first three among themselves (a cycle with temp steps).
    let mut pairs: Vec<(String, String)> = names_owned[3..]
        .iter()
        .map(|n| (n.clone(), n.replace("file-", "doc-")))
        .collect();
    pairs.push(("file-0000.txt".into(), "file-0001.txt".into()));
    pairs.push(("file-0001.txt".into(), "file-0002.txt".into()));
    pairs.push(("file-0002.txt".into(), "file-0000.txt".into()));
    let pair_refs: Vec<(&str, &str)> = pairs
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let p = plan_of(&e.work, &pair_refs);
    assert!(p.steps.iter().any(|s: &Step| s.kind == StepKind::ToTemp));

    let t0 = Instant::now();
    let out = run_quiet(JobSpec::RenameBatch(batch(&e, p)), real());
    let forward = t0.elapsed();
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!has_temp_leftovers(&e));
    assert_eq!(listing(&e.work).len(), 1000);

    let t1 = Instant::now();
    let undo = run_quiet(JobSpec::RenameRollback(rollback_spec(&e)), real());
    let backward = t1.elapsed();
    assert!(matches!(undo, Outcome::Ok(_)), "{undo:?}");
    assert_trees_equal(&e.work, pristine(&refs).path(), compare());
    eprintln!("1000-file batch: forward {forward:?}, undo {backward:?}");
    assert!(
        forward.as_secs() < 60 && backward.as_secs() < 60,
        "forward {forward:?}, undo {backward:?}"
    );
}

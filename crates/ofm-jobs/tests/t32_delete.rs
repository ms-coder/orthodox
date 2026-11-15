//! T3.2: permanent delete and trash through `run_sync` (F14).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ofm_core::CancelToken;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    ErrorAction, Fs, JobCtx, JobSpec, Outcome, PauseGate, Question, ScriptedSink, SystemClock,
    run_job, run_sync,
};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};
use t32_support::{TestFs, error_policy, no_questions, questions, report, trash_policy};

fn delete(paths: Vec<PathBuf>, to_trash: bool) -> JobSpec {
    JobSpec::Delete { paths, to_trash }
}

fn tree() -> TempDir {
    TreeSpec::new()
        .file("keep.txt", FileSpec::content(b"keep"))
        .dir(
            "victim",
            TreeSpec::new()
                .file("a", FileSpec::content(b"a"))
                .file("b", FileSpec::content(b"b"))
                .dir("sub", TreeSpec::new().file("c", FileSpec::content(b"c"))),
        )
        .build_temp()
        .unwrap()
}

#[test]
fn permanent_delete_removes_the_tree_and_counts_entries() {
    let t = tree();
    let (sink, rec) = no_questions();
    let out = run_sync(
        &delete(vec![t.path().join("victim")], false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("victim").exists());
    assert!(t.path().join("keep.txt").exists());
    // victim, a, b, sub, c
    assert_eq!(report(&out).files_done, 5);
    assert!(questions(&rec).is_empty());
    let last = rec.lock().unwrap().progress.last().cloned().unwrap();
    assert_eq!((last.files_total, last.files_done), (5, 5));
    assert!(!last.planning);
}

#[test]
fn permanent_delete_never_follows_symlinks() {
    let t = TempDir::new().unwrap();
    let outside = TreeSpec::new()
        .file("precious", FileSpec::content(b"data"))
        .dir("d", TreeSpec::new().file("x", FileSpec::content(b"x")))
        .build_temp()
        .unwrap();
    TreeSpec::new()
        .dir(
            "victim",
            TreeSpec::new()
                .symlink("to_dir", outside.path())
                .symlink("to_file", outside.path().join("precious"))
                .symlink("dangling", "/nonexistent/xyz"),
        )
        .symlink("top_link", outside.path())
        .build(t.path())
        .unwrap();

    let (sink, _) = no_questions();
    let out = run_sync(
        &delete(
            vec![t.path().join("victim"), t.path().join("top_link")],
            false,
        ),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("victim").exists());
    assert!(fs::symlink_metadata(t.path().join("top_link")).is_err());
    assert_eq!(fs::read(outside.path().join("precious")).unwrap(), b"data");
    assert_eq!(fs::read(outside.path().join("d/x")).unwrap(), b"x");
}

#[test]
fn odd_names_are_deleted() {
    let t = TempDir::new().unwrap();
    TreeSpec::new()
        .file(raw_name(b"bad-\xff\xfe.txt"), FileSpec::content(b"1"))
        .file("new\nline", FileSpec::content(b"2"))
        .file("-rf", FileSpec::content(b"3"))
        .file("back\\slash", FileSpec::content(b"4"))
        .dir(
            raw_name(b"dir-\xc3("),
            TreeSpec::new().file("x y", FileSpec::size(1)),
        )
        .build(t.path())
        .unwrap();
    let paths: Vec<PathBuf> = fs::read_dir(t.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(paths.len(), 5);
    let (sink, _) = no_questions();
    let out = run_sync(&delete(paths, false), Arc::new(TestFs::real()), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(fs::read_dir(t.path()).unwrap().count(), 0);
}

#[test]
fn missing_path_is_a_warning_not_an_error() {
    let t = tree();
    let (sink, rec) = no_questions();
    let out = run_sync(
        &delete(
            vec![t.path().join("nope"), t.path().join("keep.txt")],
            false,
        ),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let r = report(&out);
    assert_eq!(r.warnings.len(), 1);
    assert_eq!(r.warnings[0].path, t.path().join("nope"));
    assert!(!t.path().join("keep.txt").exists());
    assert!(questions(&rec).is_empty());
}

#[test]
fn invalid_requests_fail_before_any_io() {
    let (sink, _) = no_questions();
    let out = run_sync(&delete(vec![], false), Arc::new(TestFs::real()), sink);
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("nothing")),
        "{out:?}"
    );
    let (sink, _) = no_questions();
    let out = run_sync(
        &delete(vec!["/".into()], false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("refusing")),
        "{out:?}"
    );
}

#[test]
fn per_entry_error_offers_retry_skip_abort() {
    // victim has a, b, sub/c; the 2nd unlink fails.
    let run = |action: ErrorAction| {
        let t = tree();
        let fs = Arc::new(FaultyFs::new(
            Arc::new(TestFs::real()),
            FaultPlan::new().fail_nth(FaultOp::Unlink, 2, FaultErrno::PermissionDenied),
        ));
        let (sink, rec) = error_policy(action);
        let out = run_sync(
            &delete(vec![t.path().join("victim")], false),
            fs.clone(),
            sink,
        );
        (t, out, rec, fs)
    };

    // Skip: exactly one file (and therefore its parents) stay; rmdir is not even attempted on them.
    let (t, out, rec, fs) = run(ErrorAction::Skip);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(report(&out).skipped, 1);
    let survivors: usize = ["a", "b", "sub/c"]
        .iter()
        .filter(|n| t.path().join("victim").join(n).exists())
        .count();
    assert_eq!(survivors, 1);
    assert!(t.path().join("victim").is_dir());
    let qs = questions(&rec);
    assert_eq!(qs.len(), 1);
    assert!(matches!(
        &qs[0],
        Question::Error {
            os_error: Some(13),
            ..
        }
    ));
    // sub or victim rmdir: sub is emptied unless c survived; victim never.
    assert!(fs.calls_of(FaultOp::Rmdir) <= 1);

    // Retry: the second attempt succeeds, everything is gone.
    let (t, out, rec, _) = run(ErrorAction::Retry);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("victim").exists());
    assert_eq!(questions(&rec).len(), 1);

    // Abort: cancelled, no further deletion after the failure.
    let (t, out, _, _) = run(ErrorAction::Abort);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(t.path().join("victim").exists());
}

#[test]
fn unanswered_error_aborts_the_job() {
    let t = tree();
    let fs = Arc::new(FaultyFs::new(
        Arc::new(TestFs::real()),
        FaultPlan::new().fail_from(FaultOp::Unlink, 1, FaultErrno::Io),
    ));
    let (sink, rec) = no_questions();
    let out = run_sync(&delete(vec![t.path().join("victim")], false), fs, sink);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert!(t.path().join("victim/a").exists() || t.path().join("victim/b").exists());
}

#[test]
fn read_only_directory_error_goes_to_the_user_not_to_chmod() {
    let t = TempDir::new().unwrap();
    let ro = t.path().join("ro");
    fs::create_dir(&ro).unwrap();
    fs::write(ro.join("f"), b"x").unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o555)).unwrap();
    if TestFs::real().is_root() {
        fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("SKIP: running as root");
        return;
    }
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(
        &delete(vec![ro.clone()], false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert!(ro.join("f").exists(), "nothing was force-deleted");
    assert_eq!(
        fs::metadata(&ro).unwrap().permissions().mode() & 0o777,
        0o555
    );
    fs::set_permissions(&ro, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn cancel_stops_a_running_delete() {
    let t = TempDir::new().unwrap();
    let mut spec = TreeSpec::new();
    for i in 0..50 {
        spec = spec.file(format!("f{i:02}"), FileSpec::size(1));
    }
    spec.build(t.path()).unwrap();

    let cancel = CancelToken::new();
    let unlinks = Arc::new(AtomicUsize::new(0));
    let (c, n) = (cancel.clone(), Arc::clone(&unlinks));
    let fs = Arc::new(TestFs::real().hooked(move |op| {
        if op == "unlink" && n.fetch_add(1, Ordering::SeqCst) + 1 == 10 {
            c.cancel();
        }
    }));
    let (sink, _) = no_questions();
    let mut ctx = JobCtx::new(
        fs,
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    let out = run_job(&delete(vec![t.path().to_path_buf()], false), &mut ctx);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(unlinks.load(Ordering::SeqCst), 10);
    assert_eq!(fs::read_dir(t.path()).unwrap().count(), 40);
}

#[test]
fn cancel_before_start_deletes_nothing() {
    let t = tree();
    let cancel = CancelToken::new();
    cancel.cancel();
    let (sink, _) = no_questions();
    let mut ctx = JobCtx::new(
        Arc::new(TestFs::real()),
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    let out = run_job(&delete(vec![t.path().join("victim")], false), &mut ctx);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(t.path().join("victim/a").exists());
}

// ---- trash -----------------------------------------------------------------------------------

#[test]
fn trash_moves_items_into_the_private_trash() {
    let t = tree();
    let trash = TempDir::new().unwrap();
    let home = trash.path().join("Trash");
    let (sink, rec) = no_questions();
    let out = run_sync(
        &delete(
            vec![t.path().join("victim"), t.path().join("keep.txt")],
            true,
        ),
        Arc::new(TestFs::with_trash(&home)),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("victim").exists());
    assert!(!t.path().join("keep.txt").exists());
    assert!(home.join("files/victim/sub/c").is_file());
    assert_eq!(fs::read(home.join("files/keep.txt")).unwrap(), b"keep");
    assert!(home.join("info/victim.trashinfo").is_file());
    assert_eq!(report(&out).files_done, 2);
    assert!(questions(&rec).is_empty());
}

#[test]
fn unavailable_trash_asks_then_deletes_or_skips() {
    // permanent = true
    let t = tree();
    let (sink, rec) = trash_policy(true, false);
    let out = run_sync(
        &delete(vec![t.path().join("victim")], true),
        Arc::new(TestFs::trash_unavailable()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("victim").exists());
    assert!(matches!(
        questions(&rec).as_slice(),
        [Question::TrashUnavailable { .. }]
    ));
    assert_eq!(
        report(&out).files_done,
        5,
        "fallback deletion is counted per entry"
    );

    // permanent = false -> skipped, still there
    let t = tree();
    let (sink, rec) = trash_policy(false, false);
    let out = run_sync(
        &delete(vec![t.path().join("victim")], true),
        Arc::new(TestFs::trash_unavailable()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(t.path().join("victim/sub/c").exists());
    assert_eq!(report(&out).skipped, 1);
    assert_eq!(questions(&rec).len(), 1);

    // nobody answers -> abort, nothing deleted
    let t = tree();
    let (sink, _) = no_questions();
    let out = run_sync(
        &delete(vec![t.path().join("victim")], true),
        Arc::new(TestFs::trash_unavailable()),
        sink,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(t.path().join("victim/a").exists());
}

#[test]
fn apply_to_all_asks_the_trash_question_once() {
    let t = tree();
    let (sink, rec) = trash_policy(true, true);
    let out = run_sync(
        &delete(
            vec![t.path().join("victim"), t.path().join("keep.txt")],
            true,
        ),
        Arc::new(TestFs::trash_unavailable()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert!(!t.path().join("victim").exists() && !t.path().join("keep.txt").exists());
}

#[test]
fn trash_io_error_goes_through_retry_skip_abort() {
    let faulty = || {
        Arc::new(FaultyFs::new(
            Arc::new(TestFs::trash_unavailable()),
            FaultPlan::new().fail_nth(FaultOp::Trash, 1, FaultErrno::Io),
        ))
    };
    // Skip: the item stays.
    let t = tree();
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(
        &delete(vec![t.path().join("keep.txt")], true),
        faulty(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(t.path().join("keep.txt").exists());
    assert!(matches!(
        questions(&rec).as_slice(),
        [Question::Error {
            os_error: Some(5),
            ..
        }]
    ));
    assert_eq!(report(&out).skipped, 1);

    // Retry: the second call reaches the (unavailable) trash and asks the other question.
    let t = tree();
    let (sink, rec) = ScriptedSink::new(|q| {
        Some(match q {
            Question::Error { .. } => ofm_jobs::Answer::Error {
                action: ErrorAction::Retry,
                apply_to_all: false,
            },
            _ => ofm_jobs::Answer::Confirm {
                permanent: true,
                apply_to_all: false,
            },
        })
    });
    let out = run_sync(
        &delete(vec![t.path().join("keep.txt")], true),
        faulty(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!t.path().join("keep.txt").exists());
    assert_eq!(questions(&rec).len(), 2);
}

#[test]
fn trashing_a_missing_path_warns() {
    let t = TempDir::new().unwrap();
    let trash = TempDir::new().unwrap();
    let (sink, rec) = no_questions();
    let out = run_sync(
        &delete(vec![t.path().join("gone")], true),
        Arc::new(TestFs::with_trash(&trash.path().join("Trash"))),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(report(&out).warnings.len(), 1);
    assert!(questions(&rec).is_empty());
}

//! T3.4 torture suite — delete (spec F14 side of F17, ADR-0004 "Delete"): permanent delete is
//! depth-first, never follows symlinks, asks on per-entry errors; trash asks before deleting
//! permanently when unavailable. Ignored until T3.2 lands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod torture_common;

use std::path::Path;
use std::sync::Arc;

use ofm_jobs::ScriptedSink;
use ofm_jobs::event::{Answer, ErrorAction, Outcome, Question};
use ofm_jobs::fs::RealFs;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::spec::JobSpec;
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::tree::{FileSpec, TreeSpec};

use torture_common::{TrashUnavailableFs, report_of, run_policy, running_as_root, src_dst};

fn real() -> Arc<dyn ofm_jobs::Fs> {
    Arc::new(RealFs)
}

fn faulty(plan: FaultPlan) -> Arc<FaultyFs> {
    Arc::new(FaultyFs::over_real(plan))
}

fn delete(paths: &[std::path::PathBuf], to_trash: bool) -> JobSpec {
    JobSpec::Delete {
        paths: paths.to_vec(),
        to_trash,
    }
}

#[test]
fn permanent_delete_removes_a_mixed_tree() {
    let (root, outside) = src_dst("del-mixed");
    let spec = TreeSpec::new()
        .file("plain.txt", FileSpec::content(b"x"))
        .file(raw_name(), FileSpec::content(b"non-utf8"))
        .dir(
            "sub",
            TreeSpec::new().file("nested", FileSpec::content(b"n")),
        )
        .dir("empty-dir", TreeSpec::new())
        .symlink("link-to-file", "plain.txt")
        .symlink("link-to-dir", "sub")
        .symlink("broken", "nowhere")
        .symlink("loop-a", "loop-b")
        .symlink("loop-b", "loop-a")
        .file("orig", FileSpec::content(b"hardlink source"))
        .hardlink("twin", "orig");
    spec.build(root.path()).unwrap();
    std::fs::write(outside.path().join("keep.txt"), b"outside").unwrap();

    let (sink, rec) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(&delete(&[root.path().to_path_buf()], false), real(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(
        rec.lock().unwrap().questions.is_empty(),
        "deleting a readable tree asks nothing: {:?}",
        rec.lock().unwrap().questions
    );
    assert!(!root.path().exists(), "the whole tree must be gone");
    // Nothing outside the deleted root was touched.
    assert_eq!(
        std::fs::read(outside.path().join("keep.txt")).unwrap(),
        b"outside"
    );
}

fn raw_name() -> std::ffi::OsString {
    ofm_testkit::tree::raw_name(b"bad\xffname")
}

#[test]
fn delete_never_follows_symlinks_out_of_the_set() {
    let (root, outside) = src_dst("del-links");
    std::fs::create_dir_all(outside.path().join("precious")).unwrap();
    std::fs::write(outside.path().join("precious/data.txt"), b"must survive").unwrap();
    std::fs::create_dir_all(root.path()).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("precious/data.txt"),
        root.path().join("to-file"),
    )
    .unwrap();
    std::os::unix::fs::symlink(outside.path().join("precious"), root.path().join("to-dir"))
        .unwrap();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(&delete(&[root.path().to_path_buf()], false), real(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!root.path().exists());
    // The links were removed, the targets were not followed.
    assert_eq!(
        std::fs::read(outside.path().join("precious/data.txt")).unwrap(),
        b"must survive"
    );
}

#[test]
fn delete_of_a_symlink_itself_unlinks_only_the_link() {
    let (root, _) = src_dst("del-link-only");
    std::fs::write(root.path().join("real.txt"), b"target data").unwrap();
    std::os::unix::fs::symlink("real.txt", root.path().join("alias")).unwrap();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(&delete(&[root.path().join("alias")], false), real(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!root.path().join("alias").exists(), "the link is gone");
    assert_eq!(
        std::fs::read(root.path().join("real.txt")).unwrap(),
        b"target data"
    );
}

#[test]
fn delete_read_only_dir_reports_errors_and_does_not_chmod() {
    if running_as_root() {
        eprintln!("SKIP: running as root, mode 0o500 does not block deletion");
        return;
    }
    let (root, _) = src_dst("del-ro");
    let sub = root.path().join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("file.txt"), b"stuck").unwrap();
    std::fs::write(root.path().join("free.txt"), b"ok").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o500)).unwrap();

    let (out, _rec) = run_policy(
        &delete(&[root.path().to_path_buf()], false),
        real(),
        ofm_jobs::event::ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // The engine must not chmod the directory open behind the user's back.
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        std::fs::symlink_metadata(&sub).unwrap().mode() & 0o7777,
        0o500,
        "read-only dir mode must not be changed"
    );
    assert_eq!(std::fs::read(sub.join("file.txt")).unwrap(), b"stuck");
    assert!(report_of(&out, "del-ro").skipped >= 1);

    // Restore so the TempDir cleanup can remove the tree.
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn delete_missing_path_is_a_warning_not_an_error() {
    let (root, _) = src_dst("del-missing");
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(root.path().join("here.txt"), b"x").unwrap();
    let missing = root.path().join("no/such/thing.txt");

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &delete(&[root.path().join("here.txt"), missing], false),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let rep = report_of(&out, "del-missing");
    assert!(
        !rep.warnings.is_empty(),
        "the missing path must produce a warning, got {rep:?}"
    );
    assert!(!root.path().join("here.txt").exists());
}

#[test]
fn delete_retries_past_a_transient_unlink_error() {
    let (root, _) = src_dst("del-retry");
    std::fs::create_dir_all(root.path()).unwrap();
    for n in ["a", "b"] {
        std::fs::write(root.path().join(n), b"data").unwrap();
    }
    // First unlink fails with EIO, the retry (2nd call) succeeds.
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::Unlink, 1, FaultErrno::Io));
    let (sink, _) = ScriptedSink::new(move |q| match q {
        Question::Error { .. } => Some(Answer::Error {
            action: ErrorAction::Retry,
            apply_to_all: false,
        }),
        Question::Conflict { .. } => None,
        Question::TrashUnavailable { .. } => None,
    });
    let out = ofm_jobs::run_sync(&delete(&[root.path().to_path_buf()], false), fs, sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!root.path().exists(), "everything deleted after the retry");
}

#[test]
fn delete_skip_all_leaves_failed_items_in_place() {
    if running_as_root() {
        eprintln!("SKIP: running as root, EACCES faults would not occur the same way");
        return;
    }
    let (root, _) = src_dst("del-skipall");
    std::fs::create_dir_all(root.path()).unwrap();
    for n in ["first", "second", "third"] {
        std::fs::write(root.path().join(n), b"d").unwrap();
    }
    // First unlink works, every later one fails: exactly one file is gone, two are skipped.
    // (Walk order is the engine's choice, so which one is gone must not be pinned.)
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::Unlink, 2, FaultErrno::PermissionDenied));
    let (out, _rec) = run_policy(
        &delete(&[root.path().to_path_buf()], false),
        fs,
        ofm_jobs::event::ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let mut gone = 0;
    let mut kept = 0;
    for n in ["first", "second", "third"] {
        match std::fs::read(root.path().join(n)) {
            Ok(b) => {
                assert_eq!(b, b"d", "{n} must be intact");
                kept += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => gone += 1,
            Err(e) => panic!("{n}: {e}"),
        }
    }
    assert_eq!(gone, 1, "exactly the first unlinked file is deleted");
    assert_eq!(kept, 2);
    assert!(report_of(&out, "del-skipall").skipped >= 1);
}

#[test]
fn delete_abort_on_error_keeps_what_was_not_deleted() {
    let (root, _) = src_dst("del-abort");
    std::fs::create_dir_all(root.path()).unwrap();
    for n in ["a", "b"] {
        std::fs::write(root.path().join(n), b"data").unwrap();
    }
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::Unlink, 1, FaultErrno::Io));
    let (out, _rec) = run_policy(
        &delete(&[root.path().to_path_buf()], false),
        fs,
        ofm_jobs::event::ConflictAnswer::Skip,
        ErrorAction::Abort,
        false,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(std::fs::read(root.path().join("a")).unwrap(), b"data");
    assert_eq!(std::fs::read(root.path().join("b")).unwrap(), b"data");
}

#[test]
fn delete_rmdir_error_skips_the_dir() {
    let (root, _) = src_dst("del-rmdir");
    std::fs::create_dir_all(root.path().join("d")).unwrap();
    std::fs::write(root.path().join("d/f"), b"x").unwrap();

    // The file unlink succeeds; the rmdir of "d" fails with EIO -> skip.
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::Rmdir, 1, FaultErrno::Io));
    let (out, _rec) = run_policy(
        &delete(&[root.path().to_path_buf()], false),
        fs,
        ofm_jobs::event::ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(
        root.path().join("d").is_dir(),
        "the dir survives, now empty"
    );
    assert_eq!(std::fs::read_dir(root.path().join("d")).unwrap().count(), 0);
    assert!(report_of(&out, "del-rmdir").skipped >= 1);
}

#[test]
fn trash_io_error_never_deletes_permanently() {
    let (root, _) = src_dst("trash-io");
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(root.path().join("a.txt"), b"stay").unwrap();

    let fs = faulty(FaultPlan::new().fail_from(FaultOp::Trash, 1, FaultErrno::Io));
    let (out, rec) = run_policy(
        &delete(&[root.path().join("a.txt")], true),
        fs,
        ofm_jobs::event::ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(root.path().join("a.txt")).unwrap(),
        b"stay",
        "a failing trash must never permanently delete"
    );
    let rec = rec.lock().unwrap();
    assert!(
        rec.questions
            .iter()
            .any(|q| matches!(q, Question::Error { .. })),
        "the trash failure must be reported as an error question: {:?}",
        rec.questions
    );
}

#[test]
fn trash_unavailable_asking_skip_leaves_files() {
    let (root, _) = src_dst("trash-ask-skip");
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(root.path().join("a.txt"), b"keep").unwrap();
    std::fs::write(root.path().join("b.txt"), b"keep too").unwrap();

    let (sink, rec) = ScriptedSink::new(|q| match q {
        Question::TrashUnavailable { .. } => Some(Answer::Confirm {
            permanent: false,
            apply_to_all: true,
        }),
        Question::Error { .. } => Some(Answer::Error {
            action: ErrorAction::Skip,
            apply_to_all: true,
        }),
        Question::Conflict { .. } => None,
    });
    let out = ofm_jobs::run_sync(
        &delete(
            &[root.path().join("a.txt"), root.path().join("b.txt")],
            true,
        ),
        Arc::new(TrashUnavailableFs::new()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(root.path().join("a.txt")).unwrap(), b"keep");
    assert_eq!(
        std::fs::read(root.path().join("b.txt")).unwrap(),
        b"keep too"
    );
    let rec = rec.lock().unwrap();
    assert_eq!(
        rec.questions.len(),
        1,
        "'apply to all' must ask once: {:?}",
        rec.questions
    );
    assert!(matches!(
        rec.questions[0],
        Question::TrashUnavailable { .. }
    ));
    drop(rec);
    assert_eq!(report_of(&out, "trash-skip").skipped, 2);
}

#[test]
fn trash_unavailable_confirmed_permanent_deletes() {
    let (root, _) = src_dst("trash-ask-del");
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(root.path().join("a.txt"), b"gone").unwrap();

    let (sink, rec) = ScriptedSink::new(|q| match q {
        Question::TrashUnavailable { .. } => Some(Answer::Confirm {
            permanent: true,
            apply_to_all: true,
        }),
        Question::Error { .. } => Some(Answer::Error {
            action: ErrorAction::Skip,
            apply_to_all: true,
        }),
        Question::Conflict { .. } => None,
    });
    let out = ofm_jobs::run_sync(
        &delete(&[root.path().join("a.txt")], true),
        Arc::new(TrashUnavailableFs::new()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(
        !root.path().join("a.txt").exists(),
        "permanent delete after confirmation"
    );
    assert!(
        rec.lock()
            .unwrap()
            .questions
            .iter()
            .all(|q| matches!(q, Question::TrashUnavailable { .. })),
        "no other question expected"
    );
}

#[test]
fn delete_pre_cancelled_touches_nothing() {
    let (root, _) = src_dst("del-precancel");
    std::fs::create_dir_all(root.path().join("d")).unwrap();
    std::fs::write(root.path().join("d/f"), b"x").unwrap();

    let out =
        torture_common::run_pre_cancelled(&delete(&[root.path().to_path_buf()], false), real());
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(std::fs::read(root.path().join("d/f")).unwrap(), b"x");
}

#[test]
fn delete_cancelled_at_first_progress_keeps_the_tree() {
    let (root, _) = src_dst("del-cancel0");
    std::fs::create_dir_all(root.path().join("d")).unwrap();
    std::fs::write(root.path().join("d/f"), b"x").unwrap();

    let (out, _sink) =
        torture_common::run_cancel_at(&delete(&[root.path().to_path_buf()], false), real(), 0);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(std::fs::read(root.path().join("d/f")).unwrap(), b"x");
}

#[test]
fn delete_cancelled_midway_leaves_a_consistent_subset() {
    // Cancel from another thread while a wide tree is being deleted: whatever remains is a
    // proper subset of the original tree (deleted items are simply gone, nothing half-removed).
    let (root, _) = src_dst("del-midcancel");
    let mut spec = TreeSpec::new();
    for i in 0..400 {
        spec = spec.file(format!("f{i:03}"), FileSpec::content(b"payload"));
    }
    spec.build(root.path()).unwrap();

    let cancel = ofm_core::CancelToken::new();
    let (sink, _) = ScriptedSink::refusing();
    let mut ctx = ofm_jobs::JobCtx::new(
        real(),
        cancel.clone(),
        ofm_jobs::PauseGate::new(),
        Box::new(sink),
        std::sync::Arc::new(ofm_jobs::SystemClock::new()),
    );
    let path = root.path().to_path_buf();
    let worker = std::thread::spawn(move || ofm_jobs::run_job(&delete(&[path], false), &mut ctx));
    std::thread::sleep(std::time::Duration::from_millis(3));
    cancel.cancel();
    let out = worker.join().unwrap();
    assert!(
        matches!(out, Outcome::Cancelled(_) | Outcome::Ok(_)),
        "{out:?}"
    );
    if root.path().exists() {
        // Whatever survived is intact, one file per original name, no temp leftovers.
        for i in 0..400 {
            let p = root.path().join(format!("f{i:03}"));
            assert_absent_or_content(&p);
        }
        assert!(torture_common::part_files_in(root.path()).is_empty());
    }
}

fn assert_absent_or_content(path: &Path) {
    match std::fs::read(path) {
        Ok(b) => assert_eq!(b, b"payload", "{} must be intact", path.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => panic!("{} unreadable: {e}", path.display()),
    }
}

#[test]
fn trashed_file_lands_in_a_temporary_xdg_trash() {
    // Needs the real trash implementation (T3.2). XDG_DATA_HOME points at a temp dir so the
    // real trash is never touched; the temp dir and the source share a filesystem, so the
    // freedesktop home trash is used.
    let (root, xdg) = src_dst("trash-happy");
    std::fs::create_dir_all(root.path()).unwrap();
    std::fs::write(root.path().join("away.txt"), b"to trash").unwrap();

    // Env changes are process-global: this is the only env-touching test in this binary.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let old = std::env::var("XDG_DATA_HOME").ok();
    // SAFETY: single-threaded with respect to every other test in this binary (ENV_LOCK, and
    // no other test here reads XDG_DATA_HOME or calls the trash crate); restored below.
    unsafe { std::env::set_var("XDG_DATA_HOME", xdg.path()) };
    let (sink, rec) = ScriptedSink::policy(
        ofm_jobs::event::ConflictAnswer::Skip,
        ErrorAction::Abort,
        false,
    );
    let out = ofm_jobs::run_sync(&delete(&[root.path().join("away.txt")], true), real(), sink);
    match old {
        Some(v) => unsafe { std::env::set_var("XDG_DATA_HOME", v) },
        None => unsafe { std::env::remove_var("XDG_DATA_HOME") },
    }

    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!root.path().join("away.txt").exists());
    let questions = rec.lock().unwrap().questions.clone();
    if questions
        .iter()
        .any(|q| matches!(q, Question::TrashUnavailable { .. }))
    {
        eprintln!("SKIP: trash unavailable in this environment (asked instead)");
        return;
    }
    let files = xdg.path().join("Trash/files");
    let info = xdg.path().join("Trash/info");
    assert!(
        files.join("away.txt").is_file() || files.join("away_2.txt").is_file(),
        "trashed file must land in {}/Trash/files",
        xdg.path().display()
    );
    assert!(
        info.join("away.txt.trashinfo").is_file(),
        "a matching .trashinfo must exist"
    );
}

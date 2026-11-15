//! T3.6a regression tests for review findings M4 (the temp file's `close()` result was ignored, so
//! a deferred write error on NFS/FUSE could let a cross-device move unlink its source) and M5 (an
//! overwrite published an unsynced file when `sync` was off).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t36a_support;

use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ConflictAnswer, ErrorAction};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t36a_support::*;

#[test]
fn m4_close_error_fails_the_copy_and_removes_the_temp() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(s.join("f"), b"payload").unwrap();
    let plan = FaultPlan::new().fail_nth(FaultOp::FileClose, 1, FaultErrno::Io);
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(
        fs.calls_of(FaultOp::FileClose),
        1,
        "the temp is closed with a check"
    );
    let errs = error_questions(&rec);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].1, Some(5));
    assert!(names(&d).is_empty(), "nothing published, temp removed");
    assert_eq!(read(&s.join("f")), b"payload");
}

#[test]
fn m4_close_error_then_retry_publishes() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(s.join("f"), b"payload").unwrap();
    let plan = FaultPlan::new().fail_nth(FaultOp::FileClose, 1, FaultErrno::Io);
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, _) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Retry,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(names(&d), vec!["f"]);
    assert_eq!(read(&d.join("f")), b"payload");
}

#[test]
fn m4_cross_device_move_keeps_the_source_after_a_deferred_close_error() {
    let Some(pair) = ofm_testkit::crossdev::two_filesystems() else {
        eprintln!("SKIP: no second file system");
        return;
    };
    let src = pair.first.path().join("src");
    let dst = pair.second.path().join("dst");
    std::fs::create_dir_all(src.join("dir")).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(src.join("f"), b"precious").unwrap();
    std::fs::write(src.join("dir/g"), b"also precious").unwrap();
    // Every close of a written temp fails (FUSE/NFS report the deferred write error there).
    let plan = FaultPlan::new().fail_from(FaultOp::FileClose, 1, FaultErrno::Io);
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, rec) = run_policy(
        &move_spec(&[src.join("f"), src.join("dir")], &dst, opts()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    // Asked once; "skip" applies to all, so the second failure is skipped without a question.
    assert_eq!(error_questions(&rec).len(), 1);
    assert_eq!(report_of(&o).skipped, 2);
    assert_eq!(read(&src.join("f")), b"precious", "source kept");
    assert_eq!(read(&src.join("dir/g")), b"also precious", "source kept");
    assert!(!dst.join("f").exists());
    assert!(!dst.join("dir/g").exists());
    assert_no_temps(&dst);
}

#[test]
fn m5_overwrite_is_fsynced_before_it_replaces_the_existing_file() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(s.join("f"), b"new content").unwrap();
    std::fs::write(d.join("f"), b"old").unwrap();
    // sync = false (default). The first fsync fails: if it happens before the rename (as it
    // must), the existing file is never replaced by an unsynced copy.
    let plan = FaultPlan::new().fail_nth(FaultOp::FileSync, 1, FaultErrno::Io);
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(
        fs.calls_of(FaultOp::FileSync),
        1,
        "fsync despite sync = false"
    );
    assert_eq!(fs.calls_of(FaultOp::RenameReplace), 0, "fsync came first");
    assert_eq!(error_questions(&rec).len(), 1);
    assert_eq!(read(&d.join("f")), b"old");
    assert_eq!(names(&d), vec!["f"]);
}

#[test]
fn m5_overwrite_syncs_file_and_directory() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(s.join("f"), b"new content").unwrap();
    std::fs::write(s.join("fresh"), b"no conflict").unwrap();
    std::fs::write(d.join("f"), b"old").unwrap();
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new()));
    let (o, _) = run_policy(
        &copy_spec(&[s.join("f"), s.join("fresh")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(read(&d.join("f")), b"new content");
    assert_eq!(read(&d.join("fresh")), b"no conflict");
    // Only the overwrite is synced (sync = false): one file fsync, one directory fsync.
    assert_eq!(fs.calls_of(FaultOp::FileSync), 1);
    assert_eq!(fs.calls_of(FaultOp::SyncDir), 1);
    assert_eq!(fs.calls_of(FaultOp::RenameReplace), 1);
}

#[test]
fn m5_symlink_overwrite_syncs_the_directory() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    std::os::unix::fs::symlink("new-target", s.join("l")).unwrap();
    std::os::unix::fs::symlink("old-target", d.join("l")).unwrap();
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new()));
    let (o, _) = run_policy(
        &copy_spec(&[s.join("l")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(
        std::fs::read_link(d.join("l")).unwrap(),
        std::path::Path::new("new-target")
    );
    assert_eq!(fs.calls_of(FaultOp::SyncDir), 1);
}

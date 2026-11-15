//! T3.6a regression tests for review finding M3: `renameat2(RENAME_NOREPLACE)` is not implemented
//! on NFS and many FUSE file systems (ntfs-3g, sshfs, gvfs: EINVAL/ENOSYS/EOPNOTSUPP). The engine
//! must fall back to `link` + `unlink` (atomic, never replaces) and, where hard links are not
//! available either (FAT), to "lstat says absent, then rename".
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t36a_support;

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ConflictAnswer, ConflictKind, ErrorAction, Question};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t36a_support::*;

const EPERM: i32 = 1;
const EINVAL: i32 = 22;
const ENOSYS: i32 = 38;
const EOPNOTSUPP: i32 = 95;

fn no_noreplace(errno: i32) -> FaultPlan {
    FaultPlan::new().fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(errno))
}

fn fixture() -> (TempDir, std::path::PathBuf, std::path::PathBuf) {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    (t, s, d)
}

#[test]
fn m3_copy_falls_back_to_link_when_noreplace_is_unsupported() {
    for errno in [EINVAL, ENOSYS, EOPNOTSUPP] {
        let (_t, s, d) = fixture();
        std::fs::write(s.join("f"), b"data").unwrap();
        std::os::unix::fs::symlink("f", s.join("l")).unwrap();
        let fs = Arc::new(FaultyFs::over_real(no_noreplace(errno)));
        let (o, rec) = run_policy(
            &copy_spec(&[s.join("f"), s.join("l")], &d, opts()),
            fs.clone(),
            ConflictAnswer::Skip,
            ErrorAction::Skip,
        );
        assert!(o.is_success(), "errno {errno}: {o:?}");
        assert!(
            questions(&rec).is_empty(),
            "errno {errno}: no endless retry/skip"
        );
        assert_eq!(read(&d.join("f")), b"data");
        assert_eq!(
            std::fs::read_link(d.join("l")).unwrap(),
            std::path::Path::new("f")
        );
        assert!(fs.calls_of(FaultOp::HardLink) >= 2, "file and symlink temp");
        assert_eq!(fs.calls_of(FaultOp::RenameReplace), 0, "no blind rename");
        assert_eq!(names(&d), vec!["f", "l"]);
        assert_eq!(std::fs::metadata(d.join("f")).unwrap().len(), 4);
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(d.join("f")).unwrap().nlink(),
            1,
            "temp unlinked"
        );
        assert_no_temps(&d);
    }
}

#[test]
fn m3_link_fallback_never_replaces_a_name_that_appeared_meanwhile() {
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"new").unwrap();
    let intruder = d.join("f");
    let fired = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&fired);
    // Another program creates d/f right before the engine links its temp there.
    let hook = HookFs::new().hook(move |op, _, to| {
        if op == "hard_link" && to == Some(intruder.as_path()) && !flag.swap(true, Ordering::SeqCst)
        {
            std::fs::write(&intruder, b"intruder").unwrap();
        }
        None
    });
    let fs = Arc::new(FaultyFs::new(Arc::new(hook), no_noreplace(EINVAL)));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert!(fired.load(Ordering::SeqCst));
    // EEXIST from link = conflict as before: asked, answered Skip.
    assert!(matches!(
        questions(&rec).as_slice(),
        [Question::Conflict {
            kind: ConflictKind::Replaceable,
            ..
        }]
    ));
    assert_eq!(read(&d.join("f")), b"intruder");
    assert_no_temps(&d);
}

#[test]
fn m3_without_hard_links_falls_back_to_checked_rename() {
    // FAT: no RENAME_NOREPLACE through FUSE, link() is EPERM.
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"data").unwrap();
    std::fs::write(s.join("g"), b"gg").unwrap();
    std::fs::write(d.join("g"), b"old").unwrap();
    let plan = no_noreplace(EINVAL).fail_from(FaultOp::HardLink, 1, FaultErrno::Raw(EPERM));
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f"), s.join("g")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(read(&d.join("f")), b"data");
    assert_eq!(read(&d.join("g")), b"old", "existing g untouched");
    assert_eq!(questions(&rec).len(), 1, "only the g conflict");
    assert!(fs.calls_of(FaultOp::RenameReplace) >= 1);
    assert_no_temps(&d);
}

#[test]
fn m3_checked_rename_fallback_detects_a_name_that_appeared_meanwhile() {
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"new").unwrap();
    let intruder = d.join("f");
    let fired = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&fired);
    let hook = HookFs::new().hook(move |op, _, to| {
        if op == "hard_link" && to == Some(intruder.as_path()) {
            if !flag.swap(true, Ordering::SeqCst) {
                std::fs::write(&intruder, b"intruder").unwrap();
            }
            return Some(io::Error::from_raw_os_error(EPERM));
        }
        None
    });
    let fs = Arc::new(FaultyFs::new(Arc::new(hook), no_noreplace(EOPNOTSUPP)));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert_eq!(read(&d.join("f")), b"intruder");
    assert_no_temps(&d);
}

#[test]
fn m3_same_device_move_falls_back_to_link_and_rename() {
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"file").unwrap();
    std::fs::create_dir_all(s.join("dir/sub")).unwrap();
    std::fs::write(s.join("dir/sub/x"), b"x").unwrap();
    std::os::unix::fs::symlink("nowhere", s.join("l")).unwrap();
    let fs = Arc::new(FaultyFs::over_real(no_noreplace(EINVAL)));
    let (o, rec) = run_policy(
        &move_spec(&[s.join("f"), s.join("dir"), s.join("l")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert!(questions(&rec).is_empty());
    assert_eq!(read(&d.join("f")), b"file");
    assert_eq!(read(&d.join("dir/sub/x")), b"x");
    assert_eq!(
        std::fs::read_link(d.join("l")).unwrap(),
        std::path::Path::new("nowhere")
    );
    assert!(names(&s).is_empty(), "sources moved");
    assert!(fs.calls_of(FaultOp::HardLink) >= 2);
}

#[test]
fn m3_same_device_move_into_an_existing_name_is_still_a_conflict() {
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"new").unwrap();
    std::fs::write(d.join("f"), b"old").unwrap();
    let fs = Arc::new(FaultyFs::over_real(no_noreplace(EINVAL)));
    let (o, rec) = run_policy(
        &move_spec(&[s.join("f")], &d, opts()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert_eq!(read(&d.join("f")), b"old");
    assert_eq!(read(&s.join("f")), b"new");
}

#[test]
fn m3_rename_existing_uses_the_fallback_too() {
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"new").unwrap();
    std::fs::write(d.join("f"), b"old").unwrap();
    let fs = Arc::new(FaultyFs::over_real(no_noreplace(ENOSYS)));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs,
        ConflictAnswer::RenameExisting,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(questions(&rec).len(), 1, "only the conflict, no errors");
    assert_eq!(read(&d.join("f")), b"new");
    assert_eq!(read(&d.join("f (1)")), b"old");
}

#[test]
fn m3_real_errors_of_noreplace_are_not_masked() {
    // EACCES is a real error: asked, not "fallen back".
    let (_t, s, d) = fixture();
    std::fs::write(s.join("f"), b"x").unwrap();
    let plan = FaultPlan::new().fail_nth(FaultOp::RenameNoreplace, 1, FaultErrno::PermissionDenied);
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(error_questions(&rec).len(), 1);
    assert_eq!(fs.calls_of(FaultOp::HardLink), 0);
    assert!(names(&d).is_empty());
}

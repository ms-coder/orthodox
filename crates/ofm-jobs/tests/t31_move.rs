//! T3.1: move (F13). Same file system = `rename_noreplace`; cross-device = copy, fsync, verify,
//! then delete each source item. Cross-device is exercised twice: simulated (`FaultyFs` makes the
//! first rename fail with EXDEV, which forces the copy path on one file system) and, when the
//! machine has two file systems (`two_filesystems()`, e.g. /tmp and /dev/shm), for real.
//! Every failure case asserts that nothing is lost: each file exists whole at the source or at
//! the destination.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t31_support;

use std::ffi::OsString;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    ConflictAnswer, ConflictKind, CopyOpts, ErrorAction, Fs, Outcome, Question, RealFs,
};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::crossdev::two_filesystems;
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec, raw_name};
use t31_support::*;

const EXDEV: i32 = 18;

/// Adds faults to a plan.
type PlanFn = fn(FaultPlan) -> FaultPlan;

fn tree() -> TreeSpec {
    TreeSpec::new()
        .file("a.txt", FileSpec::content(b"alpha").mode(0o640))
        .file("big.bin", FileSpec::content(&noise(2_500_000, 9)))
        .file(raw_name(b"bad-\xff"), FileSpec::content(b"non-utf8"))
        .file("new\nline", FileSpec::content(b"nl"))
        .file("-rf", FileSpec::content(b"dash"))
        .file(
            "sparse",
            FileSpec::sparse_ranges(32 << 20, &[(8 << 20, b"x")]),
        )
        .dir(
            "sub",
            DirSpec::new(
                TreeSpec::new()
                    .file("c", FileSpec::content(b"c"))
                    .dir("deeper", TreeSpec::new().file("d", FileSpec::content(b"d"))),
            )
            .mode(0o750)
            .mtime_ns(1_300_000_000_123_456_789),
        )
        .symlink("link", "a.txt")
        .symlink("dangling", "nowhere")
}

/// Rename fails once with EXDEV (the move must take the cross-device path), plus `extra` faults.
fn exdev_then(extra: PlanFn) -> Arc<FaultyFs> {
    let plan = FaultPlan::new().fail_nth(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(EXDEV));
    Arc::new(FaultyFs::over_real(extra(plan)))
}

fn none(p: FaultPlan) -> FaultPlan {
    p
}

fn ino(p: &Path) -> u64 {
    std::fs::symlink_metadata(p).unwrap().ino()
}

#[test]
fn same_fs_move_is_an_atomic_rename() {
    let reference = tree().build_temp().unwrap();
    let src = TempDir::new().unwrap();
    tree().build(&src.path().join("t")).unwrap();
    let before = ino(&src.path().join("t/big.bin"));
    let dst = TempDir::new().unwrap();
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new()));
    let (out, rec) = run_with(
        &move_spec(&[src.path().join("t")], dst.path(), CopyOpts::default()),
        fs.clone(),
        |_| None,
    );
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert_trees_equal(reference.path(), &dst.path().join("t"), Compare::all());
    assert!(!src.path().join("t").exists());
    assert_eq!(
        ino(&dst.path().join("t/big.bin")),
        before,
        "renamed, not copied"
    );
    assert_eq!(fs.calls_of(FaultOp::CreateExcl), 0, "no data copied");
    assert_eq!(report_of(&out).created, vec![dst.path().join("t")]);
}

#[test]
fn rename_in_place_with_a_literal_mask() {
    let src = TreeSpec::new()
        .file("old name.txt", FileSpec::content(b"x"))
        .file("taken", FileSpec::content(b"t"))
        .build_temp()
        .unwrap();
    let i = ino(&src.path().join("old name.txt"));
    let opts = |m: &str| CopyOpts {
        rename_mask: Some(m.into()),
        ..CopyOpts::default()
    };
    let (out, _) = run_quiet(&move_spec(
        &[src.path().join("old name.txt")],
        src.path(),
        opts("--new\nname"),
    ));
    assert!(out.is_success(), "{out:?}");
    assert_eq!(ino(&src.path().join("--new\nname")), i);
    assert_eq!(
        report_of(&out).created,
        vec![src.path().join("--new\nname")]
    );
    // Renaming onto an existing name asks; "skip" keeps both untouched.
    let (out, rec) = run_policy(
        &move_spec(&[src.path().join("--new\nname")], src.path(), opts("taken")),
        Arc::new(RealFs),
        ConflictAnswer::Skip,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert_eq!(std::fs::read(src.path().join("taken")).unwrap(), b"t");
    assert!(src.path().join("--new\nname").exists());
    // Same name: same file; overwrite refused (skip), rename-new gives a numbered name.
    let (out, rec) = run_policy(
        &move_spec(&[src.path().join("taken")], src.path(), CopyOpts::default()),
        Arc::new(RealFs),
        ConflictAnswer::RenameNew,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    assert!(matches!(
        &questions(&rec)[0],
        Question::Conflict {
            kind: ConflictKind::SameFile,
            ..
        }
    ));
    assert_eq!(
        names_in(src.path()),
        vec![OsString::from("--new\nname"), "taken (1)".into()]
    );
}

#[test]
fn move_conflicts_overwrite_or_keep_the_source() {
    for (answer, expect_dst, src_kept) in [
        (ConflictAnswer::Overwrite, &b"new"[..], false),
        (ConflictAnswer::Skip, &b"old"[..], true),
        (ConflictAnswer::OverwriteIfNewer, &b"old"[..], true),
    ] {
        let src = TreeSpec::new()
            .file("f", FileSpec::content(b"new"))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new()
            .file("f", FileSpec::content(b"old"))
            .build_temp()
            .unwrap();
        let (out, _) = run_policy(
            &move_spec(&[src.path().join("f")], dst.path(), CopyOpts::default()),
            Arc::new(RealFs),
            answer,
            ErrorAction::Abort,
        );
        assert!(out.is_success(), "{answer:?}: {out:?}");
        assert_eq!(
            std::fs::read(dst.path().join("f")).unwrap(),
            expect_dst,
            "{answer:?}"
        );
        assert_eq!(src.path().join("f").exists(), src_kept, "{answer:?}");
    }
}

#[test]
fn moving_a_directory_onto_a_directory_merges() {
    let src = TreeSpec::new()
        .dir(
            "d",
            TreeSpec::new()
                .file("a", FileSpec::content(b"a"))
                .file("b", FileSpec::content(b"src-b"))
                .dir("sub", TreeSpec::new().file("c", FileSpec::content(b"c"))),
        )
        .build_temp()
        .unwrap();
    let dst = TreeSpec::new()
        .dir(
            "d",
            TreeSpec::new()
                .file("b", FileSpec::content(b"dst-b"))
                .dir("sub", TreeSpec::new().file("x", FileSpec::content(b"x"))),
        )
        .build_temp()
        .unwrap();
    let (out, rec) = run_policy(
        &move_spec(&[src.path().join("d")], dst.path(), CopyOpts::default()),
        Arc::new(RealFs),
        ConflictAnswer::Skip,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    assert_eq!(questions(&rec).len(), 1, "only the file conflict is asked");
    let d = dst.path().join("d");
    assert_eq!(std::fs::read(d.join("a")).unwrap(), b"a");
    assert_eq!(std::fs::read(d.join("b")).unwrap(), b"dst-b");
    assert_eq!(std::fs::read(d.join("sub/c")).unwrap(), b"c");
    assert!(d.join("sub/x").exists());
    // The skipped file stays at the source; emptied directories are removed.
    assert_eq!(names_in(&src.path().join("d")), vec![OsString::from("b")]);
}

#[test]
fn moving_into_itself_is_refused() {
    let src = tree().build_temp().unwrap();
    let before = all_paths(src.path());
    let (out, _) = run_quiet(&move_spec(
        &[src.path().join("sub")],
        &src.path().join("sub/deeper"),
        CopyOpts::default(),
    ));
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    assert_eq!(all_paths(src.path()), before);
}

#[test]
fn simulated_cross_device_move_copies_syncs_and_deletes_sources() {
    let reference = tree().build_temp().unwrap();
    let src = TempDir::new().unwrap();
    tree().build(&src.path().join("t")).unwrap();
    let dst = TempDir::new().unwrap();
    let fs = exdev_then(none);
    let (out, rec) = run_with(
        &move_spec(&[src.path().join("t")], dst.path(), CopyOpts::default()),
        fs.clone(),
        |_| None,
    );
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert!(
        report_of(&out).warnings.is_empty(),
        "{:?}",
        report_of(&out).warnings
    );
    assert_trees_equal(reference.path(), &dst.path().join("t"), Compare::all());
    assert!(
        !src.path().join("t").exists(),
        "sources deleted after the copy"
    );
    // 8 regular files, each fsynced, plus directory syncs.
    assert_eq!(fs.calls_of(FaultOp::FileSync), 8);
    assert!(fs.calls_of(FaultOp::SyncDir) >= 9);
    assert!(fs.calls_of(FaultOp::Unlink) >= 9);
    let sparse = std::fs::metadata(dst.path().join("t/sparse")).unwrap();
    assert!(
        sparse.blocks() * 512 < sparse.len() / 4,
        "sparse stays sparse"
    );
    let p = rec.lock().unwrap().progress.last().cloned().unwrap();
    assert_eq!(p.bytes_done, p.bytes_total);
}

#[test]
fn cross_device_failures_never_lose_data() {
    let reference = tree().build_temp().unwrap();
    let plans: [(&str, PlanFn); 6] = [
        ("enospc data", |p| {
            p.fail_nth(FaultOp::CopyRangeFrom, 2, FaultErrno::NoSpace)
        }),
        ("eio fsync", |p| {
            p.fail_nth(FaultOp::FileSync, 3, FaultErrno::Io)
        }),
        ("eio publish", |p| {
            p.fail_nth(FaultOp::RenameNoreplace, 4, FaultErrno::Io)
        }),
        ("eio dir sync", |p| {
            p.fail_nth(FaultOp::SyncDir, 2, FaultErrno::Io)
        }),
        ("enospc create", |p| {
            p.fail_nth(FaultOp::CreateExcl, 3, FaultErrno::NoSpace)
        }),
        ("eio mkdir", |p| {
            p.fail_nth(FaultOp::Mkdir, 2, FaultErrno::Io)
        }),
    ];
    for (what, plan) in plans {
        for answer in [ErrorAction::Abort, ErrorAction::Skip] {
            let src = TempDir::new().unwrap();
            tree().build(&src.path().join("t")).unwrap();
            let dst = TempDir::new().unwrap();
            let (out, _) = run_policy(
                &move_spec(&[src.path().join("t")], dst.path(), CopyOpts::default()),
                exdev_then(plan),
                ConflictAnswer::Abort,
                answer,
            );
            if what == "eio dir sync" {
                // Not an error question: the copy is published, its durability is not proven, so
                // the source is kept and the job reports a warning.
                assert!(out.is_success(), "{what}: {out:?}");
                let w = report_of(&out).warnings;
                assert!(w.iter().any(|w| w.message.contains("source kept")), "{w:?}");
            } else if answer == ErrorAction::Abort {
                assert!(matches!(out, Outcome::Cancelled(_)), "{what}: {out:?}");
            } else {
                assert!(out.is_success(), "{what}: {out:?}");
            }
            assert_nothing_lost(
                reference.path(),
                &src.path().join("t"),
                &dst.path().join("t"),
            );
            assert_files_complete(reference.path(), &dst.path().join("t"));
            assert_no_temps(dst.path());
        }
    }
}

#[test]
fn cross_device_cancel_at_any_point_never_loses_data() {
    let reference = tree().build_temp().unwrap();
    for at in [0, 1, 100_000, 1_200_000, 2_600_000, 2_600_020, 40_000_000] {
        let src = TempDir::new().unwrap();
        tree().build(&src.path().join("t")).unwrap();
        let dst = TempDir::new().unwrap();
        let (out, _) = run_cancel_at(
            &move_spec(&[src.path().join("t")], dst.path(), CopyOpts::default()),
            exdev_then(none),
            at,
        );
        assert!(
            matches!(out, Outcome::Cancelled(_) | Outcome::Ok(_)),
            "@{at}: {out:?}"
        );
        assert_nothing_lost(
            reference.path(),
            &src.path().join("t"),
            &dst.path().join("t"),
        );
        assert_files_complete(reference.path(), &dst.path().join("t"));
        assert_no_temps(dst.path());
    }
}

#[test]
fn failed_source_deletion_keeps_the_source_and_the_copy() {
    let src = TreeSpec::new()
        .file("f", FileSpec::content(b"data"))
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    // Unlink #1 is the source deletion (the temp was renamed, not unlinked).
    let fs = exdev_then(|p| p.fail_nth(FaultOp::Unlink, 1, FaultErrno::PermissionDenied));
    let (out, rec) = run_policy(
        &move_spec(&[src.path().join("f")], dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Abort,
        ErrorAction::Skip,
    );
    assert!(out.is_success(), "{out:?}");
    assert!(matches!(
        &questions(&rec)[..],
        [Question::Error {
            os_error: Some(13),
            ..
        }]
    ));
    assert_eq!(std::fs::read(src.path().join("f")).unwrap(), b"data");
    assert_eq!(std::fs::read(dst.path().join("f")).unwrap(), b"data");
}

#[test]
fn real_cross_device_move() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second file system available for a real cross-device move");
        return;
    };
    let reference = tree().build_temp().unwrap();
    let src_root = pair.first.path().join("t");
    tree().build(&src_root).unwrap();
    let (out, rec) = run_quiet(&move_spec(
        std::slice::from_ref(&src_root),
        pair.second.path(),
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert_trees_equal(
        reference.path(),
        &pair.second.path().join("t"),
        Compare::all(),
    );
    assert!(!src_root.exists());

    // Back again, cancelled half-way: nothing lost.
    let back = pair.first.path().join("back");
    std::fs::create_dir(&back).unwrap();
    let (out, _) = run_cancel_at(
        &move_spec(&[pair.second.path().join("t")], &back, CopyOpts::default()),
        Arc::new(RealFs) as Arc<dyn Fs>,
        1_000_000,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_nothing_lost(
        reference.path(),
        &pair.second.path().join("t"),
        &back.join("t"),
    );
    assert_files_complete(reference.path(), &back.join("t"));
    assert_no_temps(&back);
}

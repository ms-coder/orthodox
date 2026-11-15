//! T3.2: chmod/chown (F19) and size calculation (F19, F3 on a directory).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ofm_core::CancelToken;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    ErrorAction, Fs, JobCtx, JobSpec, ModeChange, Outcome, OwnerChange, PauseGate, Question,
    SizeTotals, SystemClock, run_job, run_sync,
};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec, raw_name};
use t32_support::{TestFs, error_policy, no_questions, questions, report};

fn perm(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().mode() & 0o7777
}

fn chmod(paths: &[&Path], set: u32, clear: u32, recursive: bool) -> JobSpec {
    JobSpec::SetAttrs {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
        mode: Some(ModeChange { set, clear }),
        owner: None,
        recursive,
    }
}

fn size_of(paths: &[&Path]) -> JobSpec {
    JobSpec::CalcSize {
        paths: paths.iter().map(|p| p.to_path_buf()).collect(),
    }
}

fn is_root() -> bool {
    TestFs::real().is_root()
}

// ---- chmod -----------------------------------------------------------------------------------

#[test]
fn chmod_set_and_clear_bits() {
    let t = TempDir::new().unwrap();
    let f = t.path().join("f");
    fs::write(&f, b"x").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();

    let (sink, rec) = no_questions();
    let out = run_sync(
        &chmod(&[&f], 0o111, 0o044, false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(perm(&f), 0o711);
    assert!(questions(&rec).is_empty());

    // set-id and sticky bits are part of the mask.
    let (sink, _) = no_questions();
    let out = run_sync(
        &chmod(&[&f], 0o4000, 0, false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(perm(&f), 0o4711);
    let (sink, _) = no_questions();
    run_sync(
        &chmod(&[&f], 0, 0o4000, false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert_eq!(perm(&f), 0o711);
}

#[test]
fn explicit_execute_bit_on_a_file_is_honoured_when_not_recursive() {
    let t = TempDir::new().unwrap();
    let f = t.path().join("plain");
    fs::write(&f, b"x").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
    let (sink, _) = no_questions();
    run_sync(
        &chmod(&[&f], 0o100, 0, false),
        Arc::new(TestFs::real()),
        sink,
    );
    assert_eq!(perm(&f), 0o700);
}

#[test]
fn recursive_chmod_does_not_make_plain_files_executable() {
    let t = TempDir::new().unwrap();
    TreeSpec::new()
        .dir(
            "top",
            DirSpec::new(
                TreeSpec::new()
                    .file("plain", FileSpec::content(b"p").mode(0o600))
                    .file("prog", FileSpec::content(b"#!/bin/sh").mode(0o700))
                    .dir(
                        "sub",
                        DirSpec::new(
                            TreeSpec::new().file("deep", FileSpec::content(b"d").mode(0o600)),
                        )
                        .mode(0o700),
                    ),
            )
            .mode(0o700),
        )
        .build(t.path())
        .unwrap();
    let top = t.path().join("top");

    // like `chmod -R go+rX`
    let (sink, rec) = no_questions();
    let out = run_sync(
        &chmod(&[&top], 0o055, 0, true),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(perm(&top), 0o755);
    assert_eq!(perm(&top.join("sub")), 0o755);
    assert_eq!(perm(&top.join("plain")), 0o644, "no x on plain files");
    assert_eq!(perm(&top.join("sub/deep")), 0o644);
    assert_eq!(
        perm(&top.join("prog")),
        0o755,
        "already executable: x follows"
    );
    assert!(questions(&rec).is_empty());
    assert_eq!(report(&out).files_done, 5);

    // `clear` is applied as given everywhere.
    let (sink, _) = no_questions();
    run_sync(
        &chmod(&[&top], 0, 0o022, true),
        Arc::new(TestFs::real()),
        sink,
    );
    assert_eq!(perm(&top.join("plain")), 0o644);
    let (sink, _) = no_questions();
    run_sync(
        &chmod(&[&top], 0, 0o007, true),
        Arc::new(TestFs::real()),
        sink,
    );
    assert_eq!(perm(&top.join("sub")), 0o750);
    assert_eq!(perm(&top.join("plain")), 0o640);
}

#[test]
fn chmod_skips_symlinks_and_never_follows_them() {
    let t = TempDir::new().unwrap();
    let outside = TreeSpec::new()
        .file("o", FileSpec::content(b"o").mode(0o600))
        .build_temp()
        .unwrap();
    fs::set_permissions(outside.path(), fs::Permissions::from_mode(0o700)).unwrap();
    TreeSpec::new()
        .dir(
            "d",
            TreeSpec::new()
                .symlink("to_dir", outside.path())
                .symlink("to_file", outside.path().join("o")),
        )
        .symlink("top", outside.path().join("o"))
        .build(t.path())
        .unwrap();

    let (sink, rec) = no_questions();
    let out = run_sync(
        &chmod(
            &[&t.path().join("d"), &t.path().join("top")],
            0o077,
            0,
            true,
        ),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(perm(&outside.path().join("o")), 0o600);
    assert_eq!(perm(outside.path()), 0o700);
    assert!(questions(&rec).is_empty());
}

#[test]
fn unchanged_objects_cost_no_syscalls() {
    let t = TempDir::new().unwrap();
    let f = t.path().join("f");
    fs::write(&f, b"x").unwrap();
    fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
    let fs_ = Arc::new(FaultyFs::new(Arc::new(TestFs::real()), FaultPlan::new()));
    let (sink, _) = no_questions();
    let out = run_sync(
        &JobSpec::SetAttrs {
            paths: vec![f.clone()],
            mode: Some(ModeChange {
                set: 0o644,
                clear: 0,
            }),
            owner: Some(OwnerChange {
                uid: Some(fs::metadata(&f).unwrap().uid()),
                gid: Some(fs::metadata(&f).unwrap().gid()),
            }),
            recursive: false,
        },
        fs_.clone(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(fs_.calls_of(FaultOp::SetMode), 0);
    assert_eq!(fs_.calls_of(FaultOp::SetOwner), 0);
}

#[test]
fn chmod_errors_are_reported_per_item_and_missing_paths_warn() {
    let t = TempDir::new().unwrap();
    let files: Vec<PathBuf> = (0..3).map(|i| t.path().join(format!("f{i}"))).collect();
    for f in &files {
        fs::write(f, b"x").unwrap();
        fs::set_permissions(f, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let fs_ = Arc::new(FaultyFs::new(
        Arc::new(TestFs::real()),
        FaultPlan::new().fail_nth(FaultOp::SetMode, 2, FaultErrno::PermissionDenied),
    ));
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let refs: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let mut paths = refs.clone();
    let missing = t.path().join("missing");
    paths.push(&missing);
    let out = run_sync(&chmod(&paths, 0o044, 0, false), fs_, sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let changed: Vec<u32> = files.iter().map(|f| perm(f)).collect();
    assert_eq!(changed.iter().filter(|m| **m == 0o644).count(), 2);
    assert_eq!(changed.iter().filter(|m| **m == 0o600).count(), 1);
    assert_eq!(questions(&rec).len(), 1);
    assert_eq!(report(&out).warnings.len(), 1);
}

#[test]
fn recursive_chmod_can_open_a_locked_directory() {
    if is_root() {
        eprintln!("SKIP: running as root");
        return;
    }
    let t = TempDir::new().unwrap();
    let d = t.path().join("locked");
    fs::create_dir(&d).unwrap();
    fs::write(d.join("f"), b"x").unwrap();
    fs::set_permissions(d.join("f"), fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&d, fs::Permissions::from_mode(0o000)).unwrap();
    let (sink, rec) = no_questions();
    let out = run_sync(
        &chmod(&[&d], 0o600 | 0o100, 0, true),
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(perm(&d), 0o700);
    assert_eq!(perm(&d.join("f")), 0o600, "file had no x: stays without");
    assert!(questions(&rec).is_empty());
}

#[test]
fn nothing_to_change_is_invalid() {
    let (sink, _) = no_questions();
    let out = run_sync(
        &JobSpec::SetAttrs {
            paths: vec!["/tmp".into()],
            mode: None,
            owner: Some(OwnerChange {
                uid: None,
                gid: None,
            }),
            recursive: false,
        },
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
}

// ---- chown -----------------------------------------------------------------------------------

#[test]
fn chown_failure_without_privilege_is_reported_per_item() {
    let t = TempDir::new().unwrap();
    let files: Vec<PathBuf> = (0..3).map(|i| t.path().join(format!("f{i}"))).collect();
    for f in &files {
        fs::write(f, b"x").unwrap();
    }
    let me = fs::metadata(&files[0]).unwrap().uid();
    // Real call as a normal user fails with EPERM; inject it so root runs behave the same.
    let fs_ = Arc::new(FaultyFs::new(
        Arc::new(TestFs::real()),
        FaultPlan::new().fail_from(FaultOp::SetOwner, 1, FaultErrno::Raw(1)),
    ));
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(
        &JobSpec::SetAttrs {
            paths: files.clone(),
            mode: None,
            owner: Some(OwnerChange {
                uid: Some(me + 1),
                gid: None,
            }),
            recursive: false,
        },
        fs_,
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let qs = questions(&rec);
    assert_eq!(qs.len(), 3, "one question per item");
    for (q, f) in qs.iter().zip(&files) {
        assert!(
            matches!(q, Question::Error { path, os_error: Some(1), .. } if path == f),
            "{q:?}"
        );
    }
    assert_eq!(report(&out).skipped, 3);
    assert!(files.iter().all(|f| fs::metadata(f).unwrap().uid() == me));
}

#[test]
fn chown_uses_lchown_and_real_chown_works_as_root() {
    if !is_root() {
        eprintln!("SKIP: real chown needs root");
        return;
    }
    let t = TempDir::new().unwrap();
    fs::write(t.path().join("target"), b"x").unwrap();
    std::os::unix::fs::symlink("target", t.path().join("l")).unwrap();
    let (sink, _) = no_questions();
    let out = run_sync(
        &JobSpec::SetAttrs {
            paths: vec![t.path().join("l")],
            mode: None,
            owner: Some(OwnerChange {
                uid: Some(12345),
                gid: Some(12346),
            }),
            recursive: false,
        },
        Arc::new(TestFs::real()),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let l = fs::symlink_metadata(t.path().join("l")).unwrap();
    assert_eq!((l.uid(), l.gid()), (12345, 12346));
    assert_ne!(fs::metadata(t.path().join("target")).unwrap().uid(), 12345);
}

// ---- size ------------------------------------------------------------------------------------

fn size_tree() -> TempDir {
    TreeSpec::new()
        .file("a", FileSpec::size(1000))
        .file("twin", FileSpec::content(b"0123456789"))
        .hardlink("twin_link", "twin")
        .file(raw_name(b"bad-\xff"), FileSpec::size(5))
        .file("new\nline", FileSpec::size(7))
        .dir(
            "d",
            TreeSpec::new()
                .file("b", FileSpec::size(2000))
                .dir("e", TreeSpec::new().file("c", FileSpec::size(30)))
                .dir("empty", TreeSpec::new()),
        )
        .build_temp()
        .unwrap()
}

#[test]
fn counts_files_dirs_bytes_and_dedupes_hardlinks() {
    let t = size_tree();
    let (sink, rec) = no_questions();
    let out = run_sync(&size_of(&[t.path()]), Arc::new(TestFs::real()), sink);
    let Outcome::Size(s) = out else {
        panic!("{out:?}")
    };
    // a, twin (+twin_link once), bad, newline, b, c
    assert_eq!(s.files, 6);
    // root, d, e, empty
    assert_eq!(s.dirs, 4);
    assert_eq!(s.bytes, 1000 + 10 + 5 + 7 + 2000 + 30);
    assert!(s.disk_bytes >= s.bytes.min(4096), "{s:?}");
    assert!(questions(&rec).is_empty());
    // Running totals were published; the final snapshot has the exact totals.
    let last = rec.lock().unwrap().progress.last().cloned().unwrap();
    assert_eq!((last.files_total, last.bytes_total), (6, s.bytes));
    assert!(!last.planning);
}

#[test]
fn several_roots_share_the_hardlink_set_and_symlinks_are_not_followed() {
    let t = size_tree();
    let big = TreeSpec::new()
        .file("huge", FileSpec::size(1_000_000))
        .build_temp()
        .unwrap();
    std::os::unix::fs::symlink(big.path(), t.path().join("d/to_big")).unwrap();
    let (sink, _) = no_questions();
    let out = run_sync(
        &size_of(&[
            &t.path().join("twin"),
            &t.path().join("twin_link"),
            &t.path().join("d"),
        ]),
        Arc::new(TestFs::real()),
        sink,
    );
    let Outcome::Size(s) = out else {
        panic!("{out:?}")
    };
    // twin + twin_link once; d: b, c, and the symlink as an entry of its own.
    assert_eq!(s.files, 1 + 3);
    assert_eq!(s.dirs, 3);
    assert!(
        s.bytes < 100_000,
        "symlink target must not be counted: {s:?}"
    );
}

#[test]
fn sparse_files_report_apparent_and_allocated_size() {
    let t = TreeSpec::new()
        .file("hole", FileSpec::sparse(64 << 20))
        .build_temp()
        .unwrap();
    let (sink, _) = no_questions();
    let out = run_sync(&size_of(&[t.path()]), Arc::new(TestFs::real()), sink);
    let Outcome::Size(s) = out else {
        panic!("{out:?}")
    };
    assert_eq!(s.bytes, 64 << 20);
    let allocated = fs::metadata(t.path().join("hole")).unwrap().blocks() * 512;
    if allocated < s.bytes {
        assert!(s.disk_bytes < s.bytes, "{s:?}");
    }
}

#[test]
fn empty_and_missing_inputs() {
    let (sink, _) = no_questions();
    let out = run_sync(&size_of(&[]), Arc::new(TestFs::real()), sink);
    assert_eq!(out, Outcome::Size(SizeTotals::default()));

    let t = TempDir::new().unwrap();
    let (sink, rec) = no_questions();
    let out = run_sync(
        &size_of(&[&t.path().join("gone")]),
        Arc::new(TestFs::real()),
        sink,
    );
    assert_eq!(out, Outcome::Size(SizeTotals::default()));
    assert!(questions(&rec).is_empty());
}

#[test]
fn unreadable_directory_is_a_warning_not_an_error() {
    let t = size_tree();
    // Injected, so it also holds when the tests run as root.
    let fs_ = Arc::new(FaultyFs::new(
        Arc::new(TestFs::real()),
        FaultPlan::new().fail_nth(FaultOp::ReadDir, 2, FaultErrno::PermissionDenied),
    ));
    let (sink, rec) = no_questions();
    let mut ctx = JobCtx::simple(fs_, sink);
    let out = run_job(&size_of(&[t.path()]), &mut ctx);
    assert!(matches!(out, Outcome::Size(_)), "{out:?}");
    assert!(questions(&rec).is_empty());
    // The warning is lost with Outcome::Size (no report); the job must still have succeeded and
    // counted everything it could read.
    let Outcome::Size(s) = out else { return };
    assert!(s.files >= 3 && s.dirs >= 1, "{s:?}");
}

#[test]
fn cancel_stops_the_walk() {
    let t = TempDir::new().unwrap();
    let mut spec = TreeSpec::new();
    for i in 0..200 {
        spec = spec.file(format!("f{i:03}"), FileSpec::size(10));
    }
    spec.build(t.path()).unwrap();

    let cancel = CancelToken::new();
    let lstats = Arc::new(AtomicUsize::new(0));
    let (c, n) = (cancel.clone(), Arc::clone(&lstats));
    let fs_ = Arc::new(TestFs::real().hooked(move |op| {
        if op == "lstat" && n.fetch_add(1, Ordering::SeqCst) + 1 == 20 {
            c.cancel();
        }
    }));
    let (sink, _) = no_questions();
    let mut ctx = JobCtx::new(
        fs_,
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    let out = run_job(&size_of(&[t.path()]), &mut ctx);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(
        lstats.load(Ordering::SeqCst),
        20,
        "no stat after the cancel"
    );
}

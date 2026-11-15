//! T3.1: copy behaviour (F13, F16, F17) on the real file system: metadata, sparse files,
//! symlinks, special files, names, rename masks, copy into the same directory, conflicts.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t31_support;

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use ofm_jobs::{
    ConflictAnswer, ConflictKind, CopyOpts, ErrorAction, JobError, Outcome, Question, RealFs,
};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{DirSpec, FileSpec, TreeSpec, raw_name};
use t31_support::*;

fn sample_tree(xattrs: bool) -> TreeSpec {
    let mut f = FileSpec::content(&noise(300_000, 1)).mode(0o640);
    let mut d = DirSpec::new(
        TreeSpec::new()
            .file(
                "inner.txt",
                FileSpec::content(b"inner\n").mtime_ns(1_234_567_890_123_456_789),
            )
            .dir("empty", TreeSpec::new()),
    )
    .mode(0o750)
    .mtime_ns(1_111_111_111_222_333_444);
    if xattrs {
        f = f.xattr("user.ofm.test", b"value");
        d = d.xattr("user.ofm.dir", b"d");
    }
    TreeSpec::new()
        .file("data.bin", f)
        .file("exec.sh", FileSpec::content(b"#!/bin/sh\n").mode(0o755))
        .file("empty.txt", FileSpec::content(b""))
        .file(
            "big.bin",
            FileSpec::content(&noise(3 * 1024 * 1024 + 17, 2)),
        )
        .dir("sub", d)
        .symlink("rel-link", "data.bin")
        .symlink("abs-link", "/etc/hostname")
        .symlink("broken-link", "does/not/exist")
        .symlink("loop-link", "loop-link")
}

#[test]
fn copies_tree_with_metadata_content_and_links() {
    let probe = TempDir::new().unwrap();
    let xattrs = xattrs_supported(probe.path());
    let spec = sample_tree(xattrs);
    let src = TempDir::new().unwrap();
    spec.build(&src.path().join("tree")).unwrap();
    let dst = TempDir::new().unwrap();
    let (out, rec) = run_quiet(&copy_spec(
        &[src.path().join("tree")],
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(matches!(&out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty());
    let cmp = Compare {
        xattrs,
        ..Compare::all()
    };
    assert_trees_equal(&src.path().join("tree"), &dst.path().join("tree"), cmp);
    assert_no_temps(dst.path());
    let r = report_of(&out);
    assert_eq!(r.created, vec![dst.path().join("tree")]);
    // 4 regular files + sub/inner.txt + 4 links
    assert_eq!(r.files_done, 9);
    assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    // Final progress is exact.
    let p = rec.lock().unwrap().progress.last().cloned().unwrap();
    assert!(!p.planning);
    assert_eq!((p.files_done, p.files_total), (9, 9));
    assert_eq!(p.bytes_done, p.bytes_total);
}

#[test]
fn read_write_fallback_produces_identical_copy() {
    let src = sample_tree(false).build_temp().unwrap();
    let dst = TempDir::new().unwrap();
    let (out, _) = run_policy(
        &copy_spec(&[src.path().to_path_buf()], dst.path(), CopyOpts::default()),
        Tiers::read_write_only(),
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    let name = src.path().file_name().unwrap();
    assert_trees_equal(
        src.path(),
        &dst.path().join(name),
        Compare {
            xattrs: false,
            ..Compare::all()
        },
    );
}

#[test]
fn sparse_files_stay_sparse() {
    let size = 64 << 20;
    let spec = TreeSpec::new()
        .file(
            "sparse.img",
            FileSpec::sparse_ranges(size, &[(1 << 20, b"hello"), (40 << 20, &noise(70_000, 3))]),
        )
        .file("hole-only.img", FileSpec::sparse(size));
    let src = spec.build_temp().unwrap();
    for fs in [
        Arc::new(RealFs) as Arc<dyn ofm_jobs::Fs>,
        Tiers::read_write_only(),
    ] {
        let dst = TempDir::new().unwrap();
        let (out, _) = run_policy(
            &copy_spec(
                &[
                    src.path().join("sparse.img"),
                    src.path().join("hole-only.img"),
                ],
                dst.path(),
                CopyOpts::default(),
            ),
            fs,
            ConflictAnswer::Abort,
            ErrorAction::Abort,
        );
        assert!(out.is_success(), "{out:?}");
        for n in ["sparse.img", "hole-only.img"] {
            let a = std::fs::metadata(src.path().join(n)).unwrap();
            let b = std::fs::metadata(dst.path().join(n)).unwrap();
            assert_eq!(b.len(), size);
            assert!(
                b.blocks() * 512 < size / 4,
                "{n}: {} blocks allocated (source {})",
                b.blocks(),
                a.blocks()
            );
            assert_eq!(
                std::fs::read(src.path().join(n)).unwrap(),
                std::fs::read(dst.path().join(n)).unwrap()
            );
        }
        let r = report_of(&out);
        assert_eq!(r.bytes_done, 2 * size, "holes count as progress");
    }
}

#[test]
fn follow_symlinks_copies_targets_and_keeps_dangling_links() {
    let src = TreeSpec::new()
        .file("f", FileSpec::content(b"target"))
        .dir("d", TreeSpec::new().file("x", FileSpec::content(b"x")))
        .symlink("to-f", "f")
        .symlink("to-d", "d")
        .symlink("dangling", "nope")
        .symlink("self", "self")
        .symlink("up", ".")
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let opts = CopyOpts {
        follow_symlinks: true,
        ..CopyOpts::default()
    };
    let (out, _) = run_quiet(&copy_spec(&[src.path().to_path_buf()], dst.path(), opts));
    assert!(out.is_success(), "{out:?}");
    let root = dst.path().join(src.path().file_name().unwrap());
    let md = std::fs::symlink_metadata(root.join("to-f")).unwrap();
    assert!(md.is_file());
    assert_eq!(std::fs::read(root.join("to-f")).unwrap(), b"target");
    assert!(
        std::fs::symlink_metadata(root.join("to-d"))
            .unwrap()
            .is_dir()
    );
    assert_eq!(std::fs::read(root.join("to-d/x")).unwrap(), b"x");
    assert!(
        std::fs::symlink_metadata(root.join("dangling"))
            .unwrap()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(root.join("self"))
            .unwrap()
            .is_symlink()
    );
    // `up` points at the source root itself: a loop, skipped with a warning.
    assert!(!root.join("up").exists());
    assert!(
        report_of(&out)
            .warnings
            .iter()
            .any(|w| w.message.contains("loop"))
    );
}

#[test]
fn special_files_are_skipped_with_a_warning_and_hardlinks_become_files() {
    let src = TreeSpec::new()
        .file("a", FileSpec::content(b"a"))
        .hardlink("a2", "a")
        .build_temp()
        .unwrap();
    let _sock = std::os::unix::net::UnixListener::bind(src.path().join("sock")).unwrap();
    let dst = TempDir::new().unwrap();
    let (out, _) = run_quiet(&copy_spec(
        &[src.path().to_path_buf()],
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    let root = dst.path().join(src.path().file_name().unwrap());
    assert!(!root.join("sock").exists());
    let r = report_of(&out);
    assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
    assert_eq!(r.warnings[0].path, src.path().join("sock"));
    let (x, y) = (
        std::fs::metadata(root.join("a")).unwrap(),
        std::fs::metadata(root.join("a2")).unwrap(),
    );
    assert_ne!(x.ino(), y.ino(), "hardlinks are copied as separate files");
    assert_eq!(std::fs::read(root.join("a2")).unwrap(), b"a");
}

#[test]
fn odd_names_copy_and_temp_names_fit() {
    let long = OsString::from_vec(vec![b'L'; 255]);
    let spec = TreeSpec::new()
        .file(raw_name(b"bad-\xff\xfe.txt"), FileSpec::content(b"1"))
        .file("new\nline", FileSpec::content(b"2"))
        .file("-rf", FileSpec::content(b"3"))
        .file("--help", FileSpec::content(b"4"))
        .file("back\\slash", FileSpec::content(b"5"))
        .file(" lead and trail ", FileSpec::content(b"6"))
        .file(long.clone(), FileSpec::content(&noise(5000, 4)))
        .dir(
            raw_name(b"dir-\x80"),
            TreeSpec::new().file("x", FileSpec::content(b"7")),
        );
    let src = spec.build_temp().unwrap();
    let dst = TempDir::new().unwrap();
    let sources: Vec<_> = names_in(src.path())
        .into_iter()
        .map(|n| src.path().join(n))
        .collect();
    let (out, _) = run_quiet(&copy_spec(&sources, dst.path(), CopyOpts::default()));
    assert!(out.is_success(), "{out:?}");
    assert_trees_equal(src.path(), dst.path(), Compare::all());
    assert_no_temps(dst.path());
    assert_eq!(report_of(&out).created.len(), sources.len());
}

#[test]
fn deep_nesting_and_many_files() {
    let mut t = TreeSpec::new().file("leaf", FileSpec::content(b"deep"));
    for i in 0..200 {
        t = TreeSpec::new().dir(format!("{}", i % 10), t);
    }
    let mut many = TreeSpec::new();
    for i in 0..500 {
        many = many.file(
            format!("f{i:04}"),
            FileSpec::content(format!("{i}").as_bytes()),
        );
    }
    let src = TreeSpec::new()
        .dir("deep", t)
        .dir("many", many)
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let (out, _) = run_quiet(&copy_spec(
        &[src.path().join("deep"), src.path().join("many")],
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    assert_trees_equal(src.path(), dst.path(), Compare::all());
}

#[test]
fn read_only_source_dir_is_copied_and_keeps_its_mode() {
    let src = TreeSpec::new()
        .dir(
            "ro",
            DirSpec::new(TreeSpec::new().file("f", FileSpec::content(b"x").mode(0o444)))
                .mode(0o555),
        )
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let (out, _) = run_quiet(&copy_spec(
        &[src.path().join("ro")],
        dst.path(),
        CopyOpts {
            sync: true,
            ..CopyOpts::default()
        },
    ));
    assert!(out.is_success(), "{out:?}");
    assert_trees_equal(src.path(), dst.path(), Compare::all());
    for d in [src.path().join("ro"), dst.path().join("ro")] {
        std::fs::set_permissions(&d, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
}

#[test]
fn rename_mask_applies_to_top_level_names_only() {
    let src = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("b.txt", FileSpec::content(b"b"))
        .dir(
            "d.txt",
            TreeSpec::new().file("in.txt", FileSpec::content(b"i")),
        )
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let opts = CopyOpts {
        rename_mask: Some("*.bak".into()),
        ..CopyOpts::default()
    };
    let srcs: Vec<_> = ["a.txt", "b.txt", "d.txt"]
        .iter()
        .map(|n| src.path().join(n))
        .collect();
    let (out, _) = run_quiet(&copy_spec(&srcs, dst.path(), opts));
    assert!(out.is_success(), "{out:?}");
    assert_eq!(
        names_in(dst.path()),
        vec![OsString::from("a.bak"), "b.bak".into(), "d.bak".into()]
    );
    assert!(dst.path().join("d.bak/in.txt").is_file());
    assert_eq!(
        report_of(&out).created,
        vec![
            dst.path().join("a.bak"),
            dst.path().join("b.bak"),
            dst.path().join("d.bak")
        ]
    );
}

#[test]
fn invalid_requests_fail_before_any_io() {
    let src = TreeSpec::new()
        .dir("d", TreeSpec::new().dir("sub", TreeSpec::new()))
        .build_temp()
        .unwrap();
    let before = all_paths(src.path());
    for (dest, mask) in [
        (src.path().join("d/sub"), None),
        (src.path().join("d"), None),
        (src.path().join("x"), Some("a/b")),
        (src.path().join("x"), Some("[x]")),
        (src.path().join("x"), Some("..")),
    ] {
        let opts = CopyOpts {
            rename_mask: mask.map(String::from),
            ..CopyOpts::default()
        };
        let (out, rec) = run_quiet(&copy_spec(&[src.path().join("d")], &dest, opts));
        assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
        assert!(questions(&rec).is_empty());
    }
    assert_eq!(all_paths(src.path()), before, "nothing was created");
    // The error message names the path.
    let (out, _) = run_quiet(&copy_spec(
        &[src.path().join("d")],
        &src.path().join("d/sub"),
        CopyOpts::default(),
    ));
    let Outcome::Failed { error, path, .. } = out else {
        panic!()
    };
    assert!(error.contains("inside source"), "{error}");
    assert_eq!(path, Some(src.path().join("d/sub")));
    assert!(JobError::DestInsideSource("/x".into()).path().is_some());
}

#[test]
fn missing_destination_directory_is_created() {
    let src = TreeSpec::new()
        .file("a", FileSpec::content(b"a"))
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let dest = dst.path().join("new/deeper");
    let (out, _) = run_quiet(&copy_spec(
        &[src.path().join("a")],
        &dest,
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    assert_eq!(std::fs::read(dest.join("a")).unwrap(), b"a");
}

// ---- same directory (F13) and conflicts (F16) ---------------------------------------------------

#[test]
fn copy_into_same_directory_asks_and_renames_new() {
    let src = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .dir("d", TreeSpec::new().file("x", FileSpec::content(b"x")))
        .build_temp()
        .unwrap();
    let srcs = vec![src.path().join("a.txt"), src.path().join("d")];
    let spec = copy_spec(&srcs, src.path(), CopyOpts::default());
    // Overwrite is refused for the same file: degrades to skip, nothing changes.
    let (out, rec) = run_policy(
        &spec,
        Arc::new(RealFs),
        ConflictAnswer::Overwrite,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).iter().all(|q| matches!(
        q,
        Question::Conflict {
            kind: ConflictKind::SameFile,
            ..
        }
    )));
    assert_eq!(
        names_in(src.path()),
        vec![OsString::from("a.txt"), "d".into()]
    );
    // RenameNew makes "name (1)" copies.
    let (out, rec) = run_policy(
        &spec,
        Arc::new(RealFs),
        ConflictAnswer::RenameNew,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    assert_eq!(questions(&rec).len(), 1, "apply-to-all answers both");
    assert_eq!(
        names_in(src.path()),
        vec![
            OsString::from("a (1).txt"),
            "a.txt".into(),
            "d".into(),
            "d (1)".into()
        ]
    );
    assert_eq!(std::fs::read(src.path().join("a (1).txt")).unwrap(), b"a");
    assert_eq!(std::fs::read(src.path().join("d (1)/x")).unwrap(), b"x");
    assert_eq!(
        report_of(&out).created,
        vec![src.path().join("a (1).txt"), src.path().join("d (1)")]
    );
    // Nobody answers: cancelled, nothing new.
    let (out, _) = run_quiet(&spec);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(names_in(src.path()).len(), 4);
}

#[test]
fn conflict_answers_replace_only_the_chosen_files() {
    const OLD: i128 = 1_000_000_000_000_000_000;
    const NEW: i128 = 1_500_000_000_000_000_000;
    let src = TreeSpec::new()
        .file("newer", FileSpec::content(b"src-newer").mtime_ns(NEW))
        .file("older", FileSpec::content(b"src-older").mtime_ns(OLD))
        .file("same", FileSpec::content(b"src-same").mtime_ns(OLD))
        .file("fresh", FileSpec::content(b"fresh"))
        .build_temp()
        .unwrap();
    let dest_spec = TreeSpec::new()
        .file("newer", FileSpec::content(b"dst-newer").mtime_ns(OLD))
        .file("older", FileSpec::content(b"dst-older").mtime_ns(NEW))
        .file("same", FileSpec::content(b"dst-same").mtime_ns(OLD));
    let srcs: Vec<_> = names_in(src.path())
        .into_iter()
        .map(|n| src.path().join(n))
        .collect();

    // "Overwrite if newer" for all: only `newer` replaced.
    let dst = dest_spec.build_temp().unwrap();
    let (out, rec) = run_with(
        &copy_spec(&srcs, dst.path(), CopyOpts::default()),
        Arc::new(RealFs),
        |_| conflict(ConflictAnswer::OverwriteIfNewer, true),
    );
    assert!(out.is_success(), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    let read = |n: &str| std::fs::read(dst.path().join(n)).unwrap();
    assert_eq!(read("newer"), b"src-newer");
    assert_eq!(read("older"), b"dst-older");
    assert_eq!(read("same"), b"dst-same");
    assert_eq!(read("fresh"), b"fresh");
    assert_eq!(report_of(&out).skipped, 2);
    assert_no_temps(dst.path());

    // Per-file answers.
    let dst = dest_spec.build_temp().unwrap();
    let (out, rec) = run_with(
        &copy_spec(&srcs, dst.path(), CopyOpts::default()),
        Arc::new(RealFs),
        |q| match q {
            Question::Conflict { source, .. } => {
                match source.path.file_name().and_then(|n| n.to_str()) {
                    Some("newer") => conflict(ConflictAnswer::Skip, false),
                    Some("older") => conflict(ConflictAnswer::Overwrite, false),
                    Some("same") => conflict(ConflictAnswer::RenameExisting, false),
                    _ => None,
                }
            }
            _ => None,
        },
    );
    assert!(out.is_success(), "{out:?}");
    assert_eq!(questions(&rec).len(), 3);
    let read = |n: &str| std::fs::read(dst.path().join(n)).unwrap();
    assert_eq!(read("newer"), b"dst-newer");
    assert_eq!(read("older"), b"src-older");
    assert_eq!(read("same"), b"src-same");
    assert_eq!(read("same (1)"), b"dst-same");
    // The overwritten file is a new inode (atomic rename over it, never written in place).
    assert_eq!(names_in(dst.path()).len(), 5);

    // Pre-decided policy (CopyOpts::on_conflict): no question at all.
    let dst = dest_spec.build_temp().unwrap();
    let opts = CopyOpts {
        on_conflict: Some(ConflictAnswer::Skip),
        ..CopyOpts::default()
    };
    let (out, rec) = run_quiet(&copy_spec(&srcs, dst.path(), opts));
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert_eq!(
        std::fs::read(dst.path().join("older")).unwrap(),
        b"dst-older"
    );
    assert_eq!(report_of(&out).skipped, 3);
}

#[test]
fn overwrite_replaces_atomically_with_a_new_inode() {
    let src = TreeSpec::new()
        .file("f", FileSpec::content(b"new content"))
        .build_temp()
        .unwrap();
    let dst = TreeSpec::new()
        .file("f", FileSpec::content(b"old"))
        .build_temp()
        .unwrap();
    // A hardlink to the old destination must keep the old data: proves no in-place write.
    std::fs::hard_link(dst.path().join("f"), dst.path().join("old-link")).unwrap();
    let (out, _) = run_policy(
        &copy_spec(&[src.path().join("f")], dst.path(), CopyOpts::default()),
        Arc::new(RealFs),
        ConflictAnswer::Overwrite,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    assert_eq!(std::fs::read(dst.path().join("f")).unwrap(), b"new content");
    assert_eq!(std::fs::read(dst.path().join("old-link")).unwrap(), b"old");
}

#[test]
fn type_mismatch_never_replaces_and_dirs_merge_silently() {
    let src = TreeSpec::new()
        .file("x", FileSpec::content(b"file"))
        .dir(
            "d",
            TreeSpec::new()
                .file("new", FileSpec::content(b"n"))
                .file("both", FileSpec::content(b"src")),
        )
        .build_temp()
        .unwrap();
    let dst = TreeSpec::new()
        .dir("x", TreeSpec::new().file("keep", FileSpec::content(b"k")))
        .dir(
            "d",
            TreeSpec::new()
                .file("old", FileSpec::content(b"o"))
                .file("both", FileSpec::content(b"dst")),
        )
        .build_temp()
        .unwrap();
    let (out, rec) = run_policy(
        &copy_spec(
            &[src.path().join("x"), src.path().join("d")],
            dst.path(),
            CopyOpts::default(),
        ),
        Arc::new(RealFs),
        ConflictAnswer::Overwrite,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    let qs = questions(&rec);
    // x: type mismatch (asked, overwrite refused -> skipped); d: merged without a question;
    // d/both: a normal conflict, but "overwrite all" was not remembered from a refused answer.
    assert!(matches!(
        &qs[0],
        Question::Conflict {
            kind: ConflictKind::TypeMismatch,
            ..
        }
    ));
    assert!(dst.path().join("x/keep").is_file());
    assert_eq!(std::fs::read(dst.path().join("d/new")).unwrap(), b"n");
    assert_eq!(std::fs::read(dst.path().join("d/old")).unwrap(), b"o");
    assert_eq!(std::fs::read(dst.path().join("d/both")).unwrap(), b"src");
    assert!(qs.iter().all(
        |q| !matches!(q, Question::Conflict { source, .. } if source.path == src.path().join("d"))
    ));
}

#[test]
fn unreadable_source_offers_skip_and_continues() {
    let src = TreeSpec::new()
        .file("a", FileSpec::content(b"a"))
        .file("secret", FileSpec::content(b"s").mode(0o000))
        .file("z", FileSpec::content(b"z"))
        .build_temp()
        .unwrap();
    if std::fs::read(src.path().join("secret")).is_ok() {
        eprintln!("SKIP: running with CAP_DAC_OVERRIDE (root?)");
        return;
    }
    let dst = TempDir::new().unwrap();
    let srcs: Vec<_> = ["a", "secret", "z"]
        .iter()
        .map(|n| src.path().join(n))
        .collect();
    let (out, rec) = run_policy(
        &copy_spec(&srcs, dst.path(), CopyOpts::default()),
        Arc::new(RealFs),
        ConflictAnswer::Abort,
        ErrorAction::Skip,
    );
    assert!(out.is_success(), "{out:?}");
    let qs = questions(&rec);
    assert!(
        matches!(&qs[0], Question::Error { path, os_error: Some(13), .. } if *path == src.path().join("secret"))
    );
    assert_eq!(names_in(dst.path()), vec![OsString::from("a"), "z".into()]);
    assert_eq!(report_of(&out).skipped, 1);
    assert_no_temps(dst.path());
}

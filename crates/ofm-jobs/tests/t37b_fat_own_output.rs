//! T3.7b regression tests for review finding N2 (MAJOR, data loss on FAT/exFAT sticks).
//!
//! On vfat/exFAT the kernel assigns a NEW `iunique()` inode number when an in-memory inode is
//! evicted under page-cache pressure. The own-output protection of T3.6 B1 compares
//! `(dev, ino)` and therefore misses: in a large photo import to a stick, a published
//! `IMG.JPG` can be renumbered, and a later `img.jpg` (same name, different case, a different
//! source) counts as `Replaceable` — an overwrite-all / newer-only policy then destroys the
//! first source's copy (and, on a move, that source was already deleted).
//!
//! The tests run the engine over [`FatFs`](t37b_support::FatFs), which simulates exactly that:
//! case-insensitive names and a renumbered inode for every published entry looked up under a
//! different case spelling. They must keep both files' contents, for copy AND (cross-device)
//! move, with `Overwrite`, `OverwriteIfNewer` and `RenameNew` policies, and for directories
//! (`Photos` vs `photos`, which must be asked about, not silently merged).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t37b_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_jobs::{ConflictAnswer, ConflictKind, ErrorAction};
use ofm_testkit::temp::TempDir;
use t37b_support::*;

/// Two source folders (a "list panel" selection) and one "FAT stick": `a/IMG.JPG` ("upper",
/// older) and `b/img.jpg` ("lower!", newer) both land on `dst` as one case-insensitive name.
fn photo_import() -> (TempDir, PathBuf, PathBuf, PathBuf) {
    let t = TempDir::new().unwrap();
    let (a, b, dst) = (
        t.path().join("a"),
        t.path().join("b"),
        t.path().join("stick"),
    );
    for p in [&a, &b, &dst] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(a.join("IMG.JPG"), b"upper").unwrap();
    std::fs::write(b.join("img.jpg"), b"lower!").unwrap();
    set_mtime(&a.join("IMG.JPG"), 1_000_000);
    set_mtime(&b.join("img.jpg"), 2_000_000);
    (t, a, b, dst)
}

/// Both files' contents must still exist somewhere after the job.
fn assert_nothing_lost(a: &Path, b: &Path, dst: &Path, moved: bool) {
    assert_eq!(read(&dst.join("img.jpg")), b"upper", "first copy kept");
    assert_eq!(read(&b.join("img.jpg")), b"lower!", "refused source kept");
    if !moved {
        assert_eq!(read(&a.join("IMG.JPG")), b"upper");
    }
    assert_no_temps(dst);
}

#[test]
fn n2_unstable_ino_copy_overwrite_policies_keep_own_output() {
    for answer in [ConflictAnswer::Overwrite, ConflictAnswer::OverwriteIfNewer] {
        let (_t, a, b, dst) = photo_import();
        let fs = Arc::new(FatFs::new(&dst));
        let srcs = [a.join("IMG.JPG"), b.join("img.jpg")];
        let (o, rec) = run_policy(
            &copy_spec(&srcs, &dst, opts()),
            fs,
            answer,
            ErrorAction::Skip,
        );
        let what = format!("copy/{answer:?}");
        assert!(o.is_success(), "{what}: {o:?}");
        // The second item collides with the job's own output: asked as non-overwritable.
        assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile], "{what}");
        assert_eq!(names(&dst), vec!["img.jpg"], "{what}");
        assert_nothing_lost(&a, &b, &dst, false);
        assert_eq!(report_of(&o).skipped, 1, "{what}");
    }
}

#[test]
fn n2_unstable_ino_cross_device_move_overwrite_policies_keep_both() {
    // The stick is another device (the wrapper answers EXDEV for the source-side renames), so
    // this exercises the cross-device half of the move: the copy engine.
    for answer in [ConflictAnswer::Overwrite, ConflictAnswer::OverwriteIfNewer] {
        let (_t, a, b, dst) = photo_import();
        let fs = Arc::new(FatFs::new(&dst));
        let srcs = [a.join("IMG.JPG"), b.join("img.jpg")];
        let (o, rec) = run_policy(
            &move_spec(&srcs, &dst, opts()),
            fs,
            answer,
            ErrorAction::Skip,
        );
        let what = format!("move/{answer:?}");
        assert!(o.is_success(), "{what}: {o:?}");
        assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile], "{what}");
        assert_eq!(names(&dst), vec!["img.jpg"], "{what}");
        assert_nothing_lost(&a, &b, &dst, true);
        assert!(
            !a.join("IMG.JPG").exists(),
            "{what}: first source was moved (its copy is on the stick)"
        );
        assert_eq!(report_of(&o).skipped, 1, "{what}");
    }
}

#[test]
fn n2_unstable_ino_rename_new_keeps_both() {
    for kind in ["copy", "move"] {
        let (_t, a, b, dst) = photo_import();
        let fs = Arc::new(FatFs::new(&dst));
        let srcs = [a.join("IMG.JPG"), b.join("img.jpg")];
        let spec = if kind == "move" {
            move_spec(&srcs, &dst, opts())
        } else {
            copy_spec(&srcs, &dst, opts())
        };
        let (o, rec) = run_policy(&spec, fs, ConflictAnswer::RenameNew, ErrorAction::Skip);
        let what = format!("{kind}/rename-new");
        assert!(o.is_success(), "{what}: {o:?}");
        assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile], "{what}");
        assert_eq!(names(&dst), vec!["img (1).jpg", "img.jpg"], "{what}");
        assert_eq!(read(&dst.join("img.jpg")), b"upper", "{what}");
        assert_eq!(read(&dst.join("img (1).jpg")), b"lower!", "{what}");
        assert_no_temps(&dst);
        if kind == "move" {
            assert!(!a.join("IMG.JPG").exists(), "{what}: moved");
            assert!(
                !b.join("img.jpg").exists(),
                "{what}: moved under a new name"
            );
        }
    }
}

#[test]
fn n2_rename_new_records_final_names_for_later_case_variants() {
    // A third source that case-folds onto the FIRST one's name must collide with it (under
    // "rename new" it gets the next free number), not overwrite it.
    let (_t, a, b, dst) = photo_import();
    let c = _t.path().join("c");
    std::fs::create_dir_all(&c).unwrap();
    std::fs::write(c.join("Img.JPG"), b"third").unwrap();
    let fs = Arc::new(FatFs::new(&dst));
    let srcs = [a.join("IMG.JPG"), b.join("img.jpg"), c.join("Img.JPG")];
    let (o, rec) = run_policy(
        &copy_spec(&srcs, &dst, opts()),
        fs,
        ConflictAnswer::RenameNew,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    // Asked once: the answer applies to all (the second collision reuses it).
    assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile]);
    assert_eq!(names(&dst), vec!["img (1).jpg", "img (2).jpg", "img.jpg"]);
    assert_eq!(read(&dst.join("img.jpg")), b"upper");
    assert_eq!(read(&dst.join("img (1).jpg")), b"lower!");
    assert_eq!(read(&dst.join("img (2).jpg")), b"third");
}

#[test]
fn n2_unstable_ino_directories_are_not_silently_merged() {
    // Two source folders `Photos` and `photos` onto the stick: the second must collide with
    // the directory this job created (a renumbered inode would make it a silent merge, whose
    // children then fight case-insensitively one by one).
    let t = TempDir::new().unwrap();
    let (a, b, dst) = (
        t.path().join("a"),
        t.path().join("b"),
        t.path().join("stick"),
    );
    for (p, d, child, content) in [
        (&a, "Photos", "x", &b"first"[..]),
        (&b, "photos", "y", &b"second, longer"[..]),
    ] {
        std::fs::create_dir_all(p.join(d)).unwrap();
        std::fs::write(p.join(d).join(child), content).unwrap();
    }
    std::fs::create_dir_all(&dst).unwrap();
    let fs = Arc::new(FatFs::new(&dst));
    let srcs = [a.join("Photos"), b.join("photos")];
    let (o, rec) = run_policy(
        &copy_spec(&srcs, &dst, opts()),
        fs,
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile]);
    assert_eq!(names(&dst), vec!["photos"]);
    // Different children: a silent merge would have copied `y` into the first folder.
    assert_eq!(names(&dst.join("photos")), vec!["x"]);
    assert_eq!(read(&dst.join("photos").join("x")), b"first");
    assert_eq!(read(&b.join("photos").join("y")), b"second, longer");
    assert_eq!(report_of(&o).skipped, 1);
    assert_no_temps(&dst);
}

#[test]
fn n2_case_variant_of_a_preexisting_file_is_still_replaceable() {
    // The new name check must not block legitimate overwrites: a file the JOB did not publish
    // (it was on the stick before) is `Replaceable` even when its name case-folds onto the
    // name of a later source.
    let (_t, _a, b, dst) = photo_import();
    // No IMG.JPG source this time: the stick already holds the user's file (a FAT stick stores
    // the name in whatever case; the wrapper folds to lower case on disk).
    std::fs::write(dst.join("img.jpg"), b"user's file").unwrap();
    let fs = Arc::new(FatFs::new(&dst));
    let srcs = [b.join("img.jpg")];
    let (o, rec) = run_policy(
        &copy_spec(&srcs, &dst, opts()),
        fs,
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(conflict_kinds(&rec), vec![ConflictKind::Replaceable]);
    assert_eq!(names(&dst), vec!["img.jpg"]);
    assert_eq!(read(&dst.join("img.jpg")), b"lower!");
    assert_eq!(read(&b.join("img.jpg")), b"lower!");
}

//! T3.6a regression tests for review finding B1 (a job overwrote an item it had created itself)
//! and m3 ("rename existing" renamed the source's own parent directory).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t36a_support;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ofm_jobs::{
    Answer, ConflictAnswer, ConflictKind, CopyOpts, ErrorAction, Outcome, Question, RealFs,
};
use ofm_testkit::temp::TempDir;
use t36a_support::*;

fn conflict_kinds(rec: &Rec) -> Vec<ConflictKind> {
    questions(rec)
        .into_iter()
        .filter_map(|q| match q {
            Question::Conflict { kind, .. } => Some(kind),
            _ => None,
        })
        .collect()
}

// ---- plan time: duplicate target names --------------------------------------------------

#[test]
fn b1_mask_collision_is_refused_before_any_io() {
    for kind in ["copy", "move"] {
        let t = TempDir::new().unwrap();
        let (s, d) = (t.path().join("s"), t.path().join("d"));
        std::fs::create_dir_all(&s).unwrap();
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(s.join("a.txt"), b"TXT").unwrap();
        std::fs::write(s.join("a.md"), b"MD").unwrap();
        let srcs = [s.join("a.txt"), s.join("a.md")];
        let spec = if kind == "move" {
            move_spec(&srcs, &d, with_mask("*.bak"))
        } else {
            copy_spec(&srcs, &d, with_mask("*.bak"))
        };
        let (o, rec) = run_policy(
            &spec,
            Arc::new(RealFs),
            ConflictAnswer::Overwrite,
            ErrorAction::Skip,
        );
        assert!(
            matches!(&o, Outcome::Failed { error, .. } if error.contains("a.bak")),
            "{kind}: {o:?}"
        );
        assert!(questions(&rec).is_empty(), "{kind}");
        assert_eq!(read(&s.join("a.txt")), b"TXT", "{kind}");
        assert_eq!(read(&s.join("a.md")), b"MD", "{kind}");
        assert!(names(&d).is_empty(), "{kind}: nothing written");
    }
}

#[test]
fn b1_same_name_from_two_folders_is_refused() {
    let t = TempDir::new().unwrap();
    let (a, b, d) = (t.path().join("a"), t.path().join("b"), t.path().join("d"));
    for p in [&a, &b, &d] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(a.join("f"), b"A").unwrap();
    std::fs::write(b.join("f"), b"BB").unwrap();
    let (o, _) = run_policy(
        &move_spec(&[a.join("f"), b.join("f")], &d, opts()),
        Arc::new(RealFs),
        ConflictAnswer::OverwriteIfSizeDiffers,
        ErrorAction::Skip,
    );
    assert!(matches!(o, Outcome::Failed { .. }), "{o:?}");
    assert_eq!(read(&a.join("f")), b"A");
    assert_eq!(read(&b.join("f")), b"BB");
    assert!(names(&d).is_empty());
}

#[test]
fn b1_same_name_from_two_folders_cross_device_is_refused() {
    let Some(pair) = ofm_testkit::crossdev::two_filesystems() else {
        eprintln!("SKIP: no second file system");
        return;
    };
    let (a, b) = (pair.first.path().join("a"), pair.first.path().join("b"));
    let d = pair.second.path().join("d");
    for p in [&a, &b, &d] {
        std::fs::create_dir_all(p).unwrap();
    }
    std::fs::write(a.join("f"), b"AAAA").unwrap();
    std::fs::write(b.join("f"), b"B").unwrap();
    let (o, _) = run_policy(
        &move_spec(&[a.join("f"), b.join("f")], &d, opts()),
        Arc::new(RealFs),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(matches!(o, Outcome::Failed { .. }), "{o:?}");
    assert_eq!(read(&a.join("f")), b"AAAA");
    assert_eq!(read(&b.join("f")), b"B");
}

// ---- run time: collisions with the job's own output (case-insensitive destination) -------

struct Fat {
    _t: TempDir,
    src: PathBuf,
    dst: PathBuf,
}

/// `src/IMG.JPG` ("upper", older) and `src/img.jpg` ("lower", newer); `dst` is "FAT".
fn fat_fixture() -> Fat {
    let t = TempDir::new().unwrap();
    let (src, dst) = (t.path().join("src"), t.path().join("dst"));
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(src.join("IMG.JPG"), b"upper").unwrap();
    std::fs::write(src.join("img.jpg"), b"lower!").unwrap();
    set_mtime(&src.join("IMG.JPG"), 1_000_000);
    set_mtime(&src.join("img.jpg"), 2_000_000);
    Fat { _t: t, src, dst }
}

#[test]
fn b1_case_insensitive_collision_never_overwrites_own_output() {
    for (kind, answer) in [
        ("copy", ConflictAnswer::OverwriteIfNewer),
        ("copy", ConflictAnswer::Overwrite),
        ("move", ConflictAnswer::OverwriteIfNewer),
        ("move", ConflictAnswer::OverwriteIfSizeDiffers),
    ] {
        let f = fat_fixture();
        let fs = Arc::new(HookFs::new().case_insensitive_below(&f.dst));
        let srcs = [f.src.join("IMG.JPG"), f.src.join("img.jpg")];
        let spec = if kind == "move" {
            move_spec(&srcs, &f.dst, opts())
        } else {
            copy_spec(&srcs, &f.dst, opts())
        };
        let (o, rec) = run_policy(&spec, fs, answer, ErrorAction::Skip);
        let what = format!("{kind}/{answer:?}");
        assert!(o.is_success(), "{what}: {o:?}");
        // The second item collided with the first one's output: asked as non-overwritable.
        assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile], "{what}");
        assert_eq!(names(&f.dst), vec!["img.jpg"], "{what}");
        assert_eq!(
            read(&f.dst.join("img.jpg")),
            b"upper",
            "{what}: first copy kept"
        );
        // Nothing lost: the refused source is still there.
        assert_eq!(read(&f.src.join("img.jpg")), b"lower!", "{what}");
        if kind == "copy" {
            assert_eq!(read(&f.src.join("IMG.JPG")), b"upper", "{what}");
        }
        assert_eq!(report_of(&o).skipped, 1, "{what}");
        assert_no_temps(&f.dst);
    }
}

#[test]
fn b1_own_output_collision_with_rename_new_keeps_both() {
    // RenameExisting on the job's own output behaves like RenameNew (never renames it).
    for kind in ["copy", "move"] {
        for answer in [ConflictAnswer::RenameNew, ConflictAnswer::RenameExisting] {
            let f = fat_fixture();
            let fs = Arc::new(HookFs::new().case_insensitive_below(&f.dst));
            let srcs = [f.src.join("IMG.JPG"), f.src.join("img.jpg")];
            let spec = if kind == "move" {
                move_spec(&srcs, &f.dst, opts())
            } else {
                copy_spec(&srcs, &f.dst, opts())
            };
            let (o, _) = run_policy(&spec, fs, answer, ErrorAction::Skip);
            assert!(o.is_success(), "{kind}/{answer:?}: {o:?}");
            assert_eq!(names(&f.dst), vec!["img (1).jpg", "img.jpg"]);
            assert_eq!(read(&f.dst.join("img.jpg")), b"upper");
            assert_eq!(read(&f.dst.join("img (1).jpg")), b"lower!");
        }
    }
}

#[test]
fn b1_own_output_is_asked_even_with_a_silent_overwrite_policy() {
    // `[general] confirm_overwrite = false` submits `on_conflict = Some(Overwrite)`.
    let f = fat_fixture();
    let fs = Arc::new(HookFs::new().case_insensitive_below(&f.dst));
    let opts = CopyOpts {
        on_conflict: Some(ConflictAnswer::Overwrite),
        ..CopyOpts::default()
    };
    let asked = Arc::new(AtomicU32::new(0));
    let seen = Arc::clone(&asked);
    let (o, rec) = run_with(
        &move_spec(
            &[f.src.join("IMG.JPG"), f.src.join("img.jpg")],
            &f.dst,
            opts,
        ),
        fs,
        move |q| {
            seen.fetch_add(1, Ordering::SeqCst);
            assert!(matches!(
                q,
                Question::Conflict {
                    kind: ConflictKind::SameFile,
                    ..
                }
            ));
            Some(Answer::Conflict {
                action: ConflictAnswer::RenameNew,
                apply_to_all: false,
            })
        },
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(asked.load(Ordering::SeqCst), 1);
    assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile]);
    assert_eq!(read(&f.dst.join("img.jpg")), b"upper");
    assert_eq!(read(&f.dst.join("img (1).jpg")), b"lower!");
    assert!(names(&f.src).is_empty(), "both moved");
}

#[test]
fn b1_case_insensitive_directories_are_not_merged_silently() {
    // src/D/x and src/d/x onto "FAT": d must not be merged into the D this job just created.
    let t = TempDir::new().unwrap();
    let (src, dst) = (t.path().join("src"), t.path().join("dst"));
    std::fs::create_dir_all(src.join("D")).unwrap();
    std::fs::create_dir_all(src.join("d")).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(src.join("D/x"), b"first").unwrap();
    std::fs::write(src.join("d/x"), b"second, longer").unwrap();
    let fs = Arc::new(HookFs::new().case_insensitive_below(&dst));
    let (o, rec) = run_policy(
        &copy_spec(&[src.join("D"), src.join("d")], &dst, opts()),
        fs,
        ConflictAnswer::OverwriteIfSizeDiffers,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile]);
    assert_eq!(names(&dst), vec!["d"]);
    assert_eq!(read(&dst.join("d/x")), b"first");
}

#[test]
fn b1_cross_device_move_onto_case_insensitive_dest_keeps_every_source() {
    let Some(pair) = ofm_testkit::crossdev::two_filesystems() else {
        eprintln!("SKIP: no second file system");
        return;
    };
    let src = pair.first.path().join("src");
    let dst = pair.second.path().join("dst");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::create_dir_all(&dst).unwrap();
    std::fs::write(src.join("IMG.JPG"), b"upper").unwrap();
    std::fs::write(src.join("img.jpg"), b"lower!").unwrap();
    let fs = Arc::new(HookFs::new().case_insensitive_below(&dst));
    let (o, rec) = run_policy(
        &move_spec(&[src.join("IMG.JPG"), src.join("img.jpg")], &dst, opts()),
        fs,
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(conflict_kinds(&rec), vec![ConflictKind::SameFile]);
    assert_eq!(read(&dst.join("img.jpg")), b"upper");
    assert!(!src.join("IMG.JPG").exists(), "first source moved");
    assert_eq!(read(&src.join("img.jpg")), b"lower!", "refused source kept");
    assert_no_temps(&dst);
}

// ---- m3: "rename existing" must not rename an ancestor of the source ---------------------

#[test]
fn m3_rename_existing_refuses_an_ancestor_of_the_source() {
    for kind in ["copy", "move"] {
        // Copy/move /p/d/d (a file) into /p: the existing /p/d is the source's own parent.
        let t = TempDir::new().unwrap();
        let p = t.path().join("p");
        std::fs::create_dir_all(p.join("d")).unwrap();
        std::fs::write(p.join("d/d"), b"inner").unwrap();
        let srcs = [p.join("d/d")];
        let spec = if kind == "move" {
            move_spec(&srcs, &p, opts())
        } else {
            copy_spec(&srcs, &p, opts())
        };
        let (o, rec) = run_policy(
            &spec,
            Arc::new(RealFs),
            ConflictAnswer::RenameExisting,
            ErrorAction::Skip,
        );
        assert!(o.is_success(), "{kind}: {o:?}");
        assert_eq!(conflict_kinds(&rec), vec![ConflictKind::TypeMismatch]);
        assert_eq!(names(&p), vec!["d"], "{kind}: the user's d was not renamed");
        assert_eq!(read(&p.join("d/d")), b"inner", "{kind}");
        let r = report_of(&o);
        assert_eq!(r.skipped, 1, "{kind}");
        assert!(!r.warnings.is_empty(), "{kind}: refusal is reported");
    }
}

#[test]
fn m3_rename_existing_still_works_for_unrelated_items() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(d.join("x")).unwrap();
    std::fs::write(s.join("x"), b"new").unwrap();
    let (o, _) = run_policy(
        &copy_spec(&[s.join("x")], &d, opts()),
        Arc::new(RealFs),
        ConflictAnswer::RenameExisting,
        ErrorAction::Skip,
    );
    assert!(o.is_success(), "{o:?}");
    assert_eq!(names(&d), vec!["x", "x (1)"]);
    assert_eq!(read(&d.join("x")), b"new");
    assert!(d.join("x (1)").is_dir());
}

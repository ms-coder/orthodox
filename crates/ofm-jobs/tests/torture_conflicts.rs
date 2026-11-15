//! T3.4 torture suite — conflict questions and overwrite decisions (spec F16/F17, ADR-0006
//! question protocol). Only the files the user chose are replaced; skip leaves the destination
//! untouched; conditional answers are evaluated centrally; rename answers use `name (N).ext`;
//! overwrite is refused for type mismatches and same-file conflicts. Ignored until T3.1 lands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod torture_common;

use std::ffi::OsString;
use std::sync::Arc;

use ofm_jobs::ScriptedSink;
use ofm_jobs::event::{Answer, ConflictAnswer, ConflictKind, ErrorAction, Outcome, Question};
use ofm_jobs::fs::RealFs;
use ofm_jobs::spec::CopyOpts;
use ofm_testkit::tree::DEFAULT_MTIME_NS;

use torture_common::{answers_by_name, report_of, src_dst, write_file};

fn real() -> Arc<dyn ofm_jobs::Fs> {
    Arc::new(RealFs)
}

fn copy_one(
    src_dir: &std::path::Path,
    name: &str,
    dst_dir: &std::path::Path,
    opts: CopyOpts,
    sink: ScriptedSink,
) -> Outcome {
    ofm_jobs::run_sync(
        &torture_common::copy_spec(&[src_dir.join(name)], dst_dir, opts),
        real(),
        sink,
    )
}

/// Sink that answers conflicts from a script (in question order) with per-answer apply_to_all.
fn scripted(
    conflicts: Vec<(ConflictAnswer, bool)>,
) -> (ScriptedSink, Arc<std::sync::Mutex<ofm_jobs::Recording>>) {
    let mut reversed = conflicts;
    reversed.reverse();
    let queue = Arc::new(std::sync::Mutex::new(reversed));
    ScriptedSink::new(move |q| match q {
        Question::Conflict { .. } => {
            let mut q = queue.lock().unwrap();
            q.pop().map(|(action, all)| Answer::Conflict {
                action,
                apply_to_all: all,
            })
        }
        Question::Error { .. } => Some(Answer::Error {
            action: ErrorAction::Skip,
            apply_to_all: false,
        }),
        Question::TrashUnavailable { .. } => None,
    })
}

#[test]
#[ignore = "T3.1"]
fn only_the_files_the_user_chose_are_replaced() {
    let (src, dst) = src_dst("cf-interactive");
    write_file(&src.path().join("a.txt"), b"NEW-A", 0o644, 0);
    write_file(&src.path().join("b.txt"), b"NEW-B", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"OLD-A", 0o644, 0);
    write_file(&dst.path().join("b.txt"), b"OLD-B", 0o644, 0);

    // Answer per file name (order-independent): skip a, overwrite b, no apply-to-all.
    let map = vec![
        (OsString::from("a.txt"), ConflictAnswer::Skip, false),
        (OsString::from("b.txt"), ConflictAnswer::Overwrite, false),
    ];
    let (sink, rec) = answers_by_name(&map, ErrorAction::Skip);
    let opts = CopyOpts::default();
    let out = ofm_jobs::run_sync(
        &torture_common::copy_spec(
            &[src.path().join("a.txt"), src.path().join("b.txt")],
            dst.path(),
            opts,
        ),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("a.txt")).unwrap(),
        b"OLD-A",
        "skip keeps existing"
    );
    assert_eq!(
        std::fs::read(dst.path().join("b.txt")).unwrap(),
        b"NEW-B",
        "chosen file replaced"
    );
    assert_eq!(report_of(&out, "cf-interactive").skipped, 1);
    let questions = rec.lock().unwrap().questions.clone();
    assert_eq!(questions.len(), 2, "both conflicts asked");
    for q in &questions {
        let Question::Conflict { kind, existing, .. } = q else {
            panic!("expected conflict questions, got {q:?}")
        };
        assert_eq!(*kind, ConflictKind::Replaceable);
        assert_eq!(
            existing.path,
            dst.path().join(existing.path.file_name().unwrap())
        );
    }
}

#[test]
#[ignore = "T3.1"]
fn overwrite_apply_to_all_replaces_everything_and_asks_once() {
    let (src, dst) = src_dst("cf-ow-all");
    for n in ["a.txt", "b.txt", "c.txt"] {
        write_file(&src.path().join(n), format!("NEW-{n}").as_bytes(), 0o644, 0);
        write_file(&dst.path().join(n), format!("OLD-{n}").as_bytes(), 0o644, 0);
    }

    let (out, rec) = torture_common::run_policy(
        &torture_common::copy_spec(
            &["a.txt", "b.txt", "c.txt"]
                .iter()
                .map(|n| src.path().join(n))
                .collect::<Vec<_>>(),
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    for n in ["a.txt", "b.txt", "c.txt"] {
        assert_eq!(
            std::fs::read(dst.path().join(n)).unwrap(),
            format!("NEW-{n}").as_bytes(),
            "{n} must be replaced"
        );
    }
    assert_eq!(
        rec.lock().unwrap().questions.len(),
        1,
        "apply-to-all asks once"
    );
}

#[test]
#[ignore = "T3.1"]
fn pre_decided_skip_policy_asks_nothing() {
    let (src, dst) = src_dst("cf-predecided");
    write_file(&src.path().join("a.txt"), b"NEW", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"OLD", 0o644, 0);

    let opts = CopyOpts {
        on_conflict: Some(ConflictAnswer::Skip),
        ..CopyOpts::default()
    };
    let (sink, rec) = ScriptedSink::refusing();
    let out = copy_one(src.path(), "a.txt", dst.path(), opts, sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(dst.path().join("a.txt")).unwrap(), b"OLD");
    assert!(
        rec.lock().unwrap().questions.is_empty(),
        "a pre-decided policy must not ask"
    );
}

#[test]
#[ignore = "T3.1"]
fn overwrite_if_newer_uses_strictly_newer_mtime() {
    let (src, dst) = src_dst("cf-newer");
    let day = 86_400_000_000_000i128;
    // newer: source beats destination; older: destination wins; equal: not "strictly newer".
    write_file(
        &src.path().join("newer.txt"),
        b"SRC",
        0o644,
        DEFAULT_MTIME_NS + 2 * day,
    );
    write_file(
        &dst.path().join("newer.txt"),
        b"DST",
        0o644,
        DEFAULT_MTIME_NS,
    );
    write_file(
        &src.path().join("older.txt"),
        b"SRC",
        0o644,
        DEFAULT_MTIME_NS,
    );
    write_file(
        &dst.path().join("older.txt"),
        b"DST",
        0o644,
        DEFAULT_MTIME_NS + 3 * day,
    );
    write_file(
        &src.path().join("same.txt"),
        b"SRC",
        0o644,
        DEFAULT_MTIME_NS + 5 * day,
    );
    write_file(
        &dst.path().join("same.txt"),
        b"DST",
        0o644,
        DEFAULT_MTIME_NS + 5 * day,
    );

    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(
            &["newer.txt", "older.txt", "same.txt"]
                .iter()
                .map(|n| src.path().join(n))
                .collect::<Vec<_>>(),
            dst.path(),
            CopyOpts {
                on_conflict: Some(ConflictAnswer::OverwriteIfNewer),
                ..CopyOpts::default()
            },
        ),
        real(),
        ConflictAnswer::Abort,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("newer.txt")).unwrap(),
        b"SRC",
        "newer source replaces"
    );
    assert_eq!(
        std::fs::read(dst.path().join("older.txt")).unwrap(),
        b"DST",
        "older source skipped"
    );
    assert_eq!(
        std::fs::read(dst.path().join("same.txt")).unwrap(),
        b"DST",
        "equal mtime is not newer"
    );
}

#[test]
#[ignore = "T3.1"]
fn overwrite_if_size_differs_compares_sizes() {
    let (src, dst) = src_dst("cf-sizediff");
    write_file(&src.path().join("differs.txt"), b"123456", 0o644, 0);
    write_file(&dst.path().join("differs.txt"), b"12345678", 0o644, 0);
    write_file(&src.path().join("equal.txt"), b"abc", 0o644, 0);
    write_file(&dst.path().join("equal.txt"), b"xyz", 0o644, 0);

    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(
            &[src.path().join("differs.txt"), src.path().join("equal.txt")],
            dst.path(),
            CopyOpts {
                on_conflict: Some(ConflictAnswer::OverwriteIfSizeDiffers),
                ..CopyOpts::default()
            },
        ),
        real(),
        ConflictAnswer::Abort,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("differs.txt")).unwrap(),
        b"123456",
        "size differs -> replace"
    );
    assert_eq!(
        std::fs::read(dst.path().join("equal.txt")).unwrap(),
        b"xyz",
        "same size -> skip"
    );
}

#[test]
#[ignore = "T3.1"]
fn rename_new_uses_name_n_ext_scheme() {
    let (src, dst) = src_dst("cf-renamenew");
    write_file(&src.path().join("a.txt"), b"INCOMING", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"EXISTING", 0o644, 0);
    write_file(&src.path().join(".bashrc"), b"INCOMING-DOT", 0o644, 0);
    write_file(&dst.path().join(".bashrc"), b"EXISTING-DOT", 0o644, 0);
    write_file(&src.path().join("archive.tar.gz"), b"INCOMING-GZ", 0o644, 0);
    write_file(&dst.path().join("archive.tar.gz"), b"EXISTING-GZ", 0o644, 0);

    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(
            &["a.txt", ".bashrc", "archive.tar.gz"]
                .iter()
                .map(|n| src.path().join(n))
                .collect::<Vec<_>>(),
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        ConflictAnswer::RenameNew,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // ADR-0006: first free `name (N).ext` (N >= 1): `a (1).txt`, `.bashrc (1)`,
    // `a.tar.gz` -> `a.tar (1).gz`.
    assert_eq!(
        std::fs::read(dst.path().join("a.txt")).unwrap(),
        b"EXISTING"
    );
    assert_eq!(
        std::fs::read(dst.path().join("a (1).txt")).unwrap(),
        b"INCOMING"
    );
    assert_eq!(
        std::fs::read(dst.path().join(".bashrc")).unwrap(),
        b"EXISTING-DOT"
    );
    assert_eq!(
        std::fs::read(dst.path().join(".bashrc (1)")).unwrap(),
        b"INCOMING-DOT"
    );
    assert_eq!(
        std::fs::read(dst.path().join("archive.tar.gz")).unwrap(),
        b"EXISTING-GZ"
    );
    assert_eq!(
        std::fs::read(dst.path().join("archive.tar (1).gz")).unwrap(),
        b"INCOMING-GZ"
    );
    // The report names the renamed destinations.
    let created = report_of(&out, "cf-renamenew").created;
    assert!(
        created.contains(&dst.path().join("a (1).txt")),
        "{created:?}"
    );
}

#[test]
#[ignore = "T3.1"]
fn rename_new_finds_the_next_free_number() {
    let (src, dst) = src_dst("cf-renamenew2");
    write_file(&src.path().join("a.txt"), b"INCOMING", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"EXISTING", 0o644, 0);
    write_file(&dst.path().join("a (1).txt"), b"FIRST FREE TAKEN", 0o644, 0);

    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::RenameNew,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("a (1).txt")).unwrap(),
        b"FIRST FREE TAKEN"
    );
    assert_eq!(
        std::fs::read(dst.path().join("a (2).txt")).unwrap(),
        b"INCOMING"
    );
}

#[test]
#[ignore = "T3.1"]
fn rename_existing_moves_the_old_file_out_of_the_way() {
    let (src, dst) = src_dst("cf-renameexisting");
    write_file(&src.path().join("a.txt"), b"INCOMING", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"EXISTING", 0o644, 0);

    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::RenameExisting,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("a.txt")).unwrap(),
        b"INCOMING",
        "new file under the original name"
    );
    assert_eq!(
        std::fs::read(dst.path().join("a (1).txt")).unwrap(),
        b"EXISTING",
        "existing file renamed away"
    );
}

#[test]
#[ignore = "T3.1"]
fn file_onto_existing_directory_is_a_type_mismatch() {
    let (src, dst) = src_dst("cf-typemismatch");
    write_file(&src.path().join("a"), b"FILE", 0o644, 0);
    std::fs::create_dir_all(dst.path().join("a")).unwrap();
    write_file(&dst.path().join("a/inner.txt"), b"TREE", 0o644, 0);

    // Overwrite is forbidden here: even an Overwrite policy must degrade to skip + warning,
    // never replace a directory with a file.
    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(&[src.path().join("a")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(dst.path().join("a").is_dir(), "the directory survives");
    assert_eq!(
        std::fs::read(dst.path().join("a/inner.txt")).unwrap(),
        b"TREE"
    );
    let rep = report_of(&out, "cf-typemismatch");
    assert!(rep.skipped >= 1);
    assert!(
        !rep.warnings.is_empty(),
        "a forbidden answer degrades to skip WITH a warning"
    );
}

#[test]
#[ignore = "T3.1"]
fn type_mismatch_is_asked_with_the_right_kind() {
    let (src, dst) = src_dst("cf-askkind");
    write_file(&src.path().join("a"), b"FILE", 0o644, 0);
    std::fs::create_dir_all(dst.path().join("a")).unwrap();

    let (sink, rec) = scripted(vec![(ConflictAnswer::Skip, false)]);
    let out = copy_one(src.path(), "a", dst.path(), CopyOpts::default(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let questions = rec.lock().unwrap().questions.clone();
    assert_eq!(questions.len(), 1);
    match &questions[0] {
        Question::Conflict { kind, .. } => assert_eq!(*kind, ConflictKind::TypeMismatch),
        other => panic!("expected a conflict question, got {other:?}"),
    }
}

#[test]
#[ignore = "T3.1"]
fn directory_onto_existing_directory_merges_without_asking() {
    let (src, dst) = src_dst("cf-merge");
    std::fs::create_dir_all(src.path().join("sub")).unwrap();
    std::fs::write(src.path().join("sub/new.txt"), b"from source").unwrap();
    std::fs::create_dir_all(dst.path().join("sub")).unwrap();
    std::fs::write(dst.path().join("sub/old.txt"), b"from dest").unwrap();

    let (sink, rec) = ScriptedSink::refusing();
    let out = copy_one(src.path(), "sub", dst.path(), CopyOpts::default(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("sub/new.txt")).unwrap(),
        b"from source"
    );
    assert_eq!(
        std::fs::read(dst.path().join("sub/old.txt")).unwrap(),
        b"from dest"
    );
    assert!(
        rec.lock().unwrap().questions.is_empty(),
        "dir-into-dir merges silently"
    );
}

#[test]
#[ignore = "T3.1"]
fn copy_into_own_directory_asks_and_renames_new() {
    // F13: copying a file into the directory it already lives in. The conflict is SameFile;
    // Overwrite is refused; RenameNew writes `a (1).txt`.
    let (src, _dst) = src_dst("cf-samedir");
    write_file(&src.path().join("a.txt"), b"ORIGINAL", 0o644, 0);

    let (sink, rec) = scripted(vec![(ConflictAnswer::RenameNew, false)]);
    let out = copy_one(src.path(), "a.txt", src.path(), CopyOpts::default(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(src.path().join("a.txt")).unwrap(),
        b"ORIGINAL"
    );
    assert_eq!(
        std::fs::read(src.path().join("a (1).txt")).unwrap(),
        b"ORIGINAL"
    );
    let questions = rec.lock().unwrap().questions.clone();
    assert_eq!(questions.len(), 1);
    match &questions[0] {
        Question::Conflict { kind, .. } => assert_eq!(*kind, ConflictKind::SameFile),
        other => panic!("expected a conflict question, got {other:?}"),
    }
}

#[test]
#[ignore = "T3.1"]
fn same_inode_detected_through_hardlinked_destination() {
    // The destination file is a hard link to the source: same dev+ino -> SameFile conflict,
    // an Overwrite policy must degrade to skip and leave the data intact.
    let (src, dst) = src_dst("cf-sameino");
    write_file(&src.path().join("a.txt"), b"SHARED INODE", 0o644, 0);
    std::fs::hard_link(src.path().join("a.txt"), dst.path().join("a.txt")).unwrap();

    let (out, _rec) = torture_common::run_policy(
        &torture_common::copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("a.txt")).unwrap(),
        b"SHARED INODE",
        "the same file must never overwrite itself"
    );
    assert_eq!(
        std::fs::read(src.path().join("a.txt")).unwrap(),
        b"SHARED INODE",
        "and of course the source survives"
    );
    let rep = report_of(&out, "cf-sameino");
    assert!(rep.skipped >= 1);
    assert!(!rep.warnings.is_empty(), "refused answer warns");
}

#[test]
#[ignore = "T3.1"]
fn conflict_abort_cancels_and_keeps_completed_work() {
    let (src, dst) = src_dst("cf-abort");
    write_file(&src.path().join("first.txt"), b"done before", 0o644, 0);
    write_file(&src.path().join("clash.txt"), b"CLASH", 0o644, 0);
    write_file(&dst.path().join("clash.txt"), b"PRECIOUS", 0o644, 0);

    // first.txt copies cleanly; the clash asks; Abort ends the job as Cancelled.
    let (sink, _rec) = scripted(vec![(ConflictAnswer::Abort, false)]);
    let out = ofm_jobs::run_sync(
        &torture_common::copy_spec(
            &[src.path().join("first.txt"), src.path().join("clash.txt")],
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("first.txt")).unwrap(),
        b"done before"
    );
    assert_eq!(
        std::fs::read(dst.path().join("clash.txt")).unwrap(),
        b"PRECIOUS"
    );
    torture_common::assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn conflict_for_a_symlink_pair_is_replaceable() {
    let (src, dst) = src_dst("cf-symlink");
    std::os::unix::fs::symlink("somewhere-old", dst.path().join("link")).unwrap();

    let (sink, rec) = scripted(vec![(ConflictAnswer::Overwrite, false)]);
    std::os::unix::fs::symlink("somewhere-new", src.path().join("link")).unwrap();
    let out = copy_one(src.path(), "link", dst.path(), CopyOpts::default(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read_link(dst.path().join("link")).unwrap(),
        std::path::PathBuf::from("somewhere-new"),
        "the link itself is replaced"
    );
    let questions = rec.lock().unwrap().questions.clone();
    assert_eq!(questions.len(), 1);
    match &questions[0] {
        Question::Conflict { kind, .. } => assert_eq!(*kind, ConflictKind::Replaceable),
        other => panic!("expected a conflict question, got {other:?}"),
    }
}

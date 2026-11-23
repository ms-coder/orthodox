//! T6.2 extract job: the conflict protocol (same as copy, ADR-0006) and "never write through a
//! symlink that already lies in the destination".
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::os::unix::fs::symlink;
use std::time::{Duration, SystemTime};

use extract_support::*;
use ofm_jobs::event::{ConflictKind, Question};
use ofm_jobs::{ConflictAnswer, ErrorAction, JobCtx, ScriptedSink};

const OLD: &[u8] = b"old content";

fn archive_with(tmp: &Tmp) -> MemArchive {
    let mut a = MemArchive::new(&tmp.archive_file("a.zip", 20_000));
    a.file("a.txt", b"new a");
    a.file("b.txt", b"new b");
    a.file("c.txt", b"new c");
    a
}

fn pre_existing(dst: &std::path::Path) {
    for n in ["a.txt", "b.txt", "c.txt"] {
        std::fs::write(dst.join(n), OLD).unwrap();
    }
}

fn set_mtime(p: &std::path::Path, t: SystemTime) {
    std::fs::File::options()
        .write(true)
        .open(p)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

#[test]
fn skip_keeps_existing_and_counts() {
    let tmp = Tmp::new("c-skip");
    let dst = tmp.dst();
    pre_existing(&dst);
    let mut a = archive_with(&tmp);
    a.file("fresh.txt", b"fresh");
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Skip);
    let rep = run.ok();
    assert_eq!(read(&dst.join("a.txt")), OLD);
    assert_eq!(read(&dst.join("fresh.txt")), b"fresh");
    assert_eq!(rep.skipped, 3);
    assert_eq!(
        run.questions(),
        1,
        "apply-to-all remembered after the first answer"
    );
    assert_no_part_files(&dst);
}

#[test]
fn overwrite_replaces_atomically_with_new_data_and_metadata() {
    let tmp = Tmp::new("c-over");
    let dst = tmp.dst();
    pre_existing(&dst);
    let mut a = archive_with(&tmp);
    a.mode(0o600);
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Overwrite);
    run.ok();
    assert_eq!(read(&dst.join("a.txt")), b"new a");
    assert_eq!(read(&dst.join("c.txt")), b"new c");
    assert_eq!(perm(&dst.join("c.txt")), 0o600);
    assert_eq!(mtime_ns(&dst.join("a.txt")), MTIME);
    assert_no_part_files(&dst);
}

#[test]
fn pre_decided_policy_asks_nothing() {
    let tmp = Tmp::new("c-pre");
    let dst = tmp.dst();
    pre_existing(&dst);
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let o = ofm_jobs::ExtractOpts {
        on_conflict: Some(ConflictAnswer::RenameNew),
        ..opts()
    };
    // `run_plain` refuses every question: success proves none was asked.
    let run = run_plain(&mut r, &[], &dst, &o);
    run.ok();
    assert_eq!(run.questions(), 0);
    assert_eq!(read(&dst.join("a.txt")), OLD);
    assert_eq!(read(&dst.join("a (1).txt")), b"new a");
    assert_eq!(read(&dst.join("c (1).txt")), b"new c");
}

#[test]
fn rename_new_keeps_both() {
    let tmp = Tmp::new("c-rn");
    let dst = tmp.dst();
    pre_existing(&dst);
    std::fs::write(dst.join("a (1).txt"), b"taken").unwrap();
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::RenameNew);
    let rep = run.ok();
    assert_eq!(read(&dst.join("a.txt")), OLD);
    assert_eq!(read(&dst.join("a (1).txt")), b"taken");
    assert_eq!(read(&dst.join("a (2).txt")), b"new a");
    assert!(
        rep.created.contains(&dst.join("a (2).txt")),
        "{:?}",
        rep.created
    );
    assert_no_part_files(&dst);
}

#[test]
fn rename_existing_moves_the_old_one_aside() {
    let tmp = Tmp::new("c-re");
    let dst = tmp.dst();
    pre_existing(&dst);
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::RenameExisting);
    run.ok();
    assert_eq!(read(&dst.join("a.txt")), b"new a");
    assert_eq!(read(&dst.join("a (1).txt")), OLD);
    assert_no_part_files(&dst);
}

#[test]
fn overwrite_if_newer_compares_archive_mtime_with_the_existing_file() {
    let tmp = Tmp::new("c-newer");
    let dst = tmp.dst();
    pre_existing(&dst);
    // Archive mtime is 2000-01-01. a: existing is newer (now); b: existing is older (1990).
    set_mtime(
        &dst.join("b.txt"),
        SystemTime::UNIX_EPOCH + Duration::from_secs(631_152_000),
    );
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::OverwriteIfNewer);
    run.ok();
    assert_eq!(read(&dst.join("a.txt")), OLD, "existing is newer: kept");
    assert_eq!(
        read(&dst.join("b.txt")),
        b"new b",
        "archive entry is newer: replaced"
    );
}

#[test]
fn overwrite_if_size_differs() {
    let tmp = Tmp::new("c-size");
    let dst = tmp.dst();
    std::fs::write(dst.join("a.txt"), b"new a").unwrap(); // same size, kept
    std::fs::write(dst.join("b.txt"), b"x").unwrap(); // different, replaced
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let run = run_policy(
        &mut r,
        &[],
        &dst,
        &opts(),
        ConflictAnswer::OverwriteIfSizeDiffers,
    );
    run.ok();
    assert_eq!(read(&dst.join("b.txt")), b"new b");
}

#[test]
fn abort_cancels_and_leaves_everything_as_it_was() {
    let tmp = Tmp::new("c-abort");
    let dst = tmp.dst();
    pre_existing(&dst);
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Abort);
    assert!(is_cancelled(&run), "{:?}", run.result);
    assert_eq!(read(&dst.join("a.txt")), OLD);
    assert_no_part_files(&dst);
}

#[test]
fn the_question_names_the_archive_entry_and_the_existing_file() {
    let tmp = Tmp::new("c-q");
    let dst = tmp.dst();
    pre_existing(&dst);
    let arch = tmp.archive_file("q.zip", 20_000);
    let mut a = MemArchive::new(&arch);
    a.file("a.txt", b"12345");
    let mut r = a.reader();
    let (sink, rec) = ScriptedSink::new(|_| conflict(ConflictAnswer::Skip, false));
    let run = run_with_ctx(
        JobCtx::simple(std::sync::Arc::new(ofm_jobs::RealFs), sink),
        rec,
        &mut r,
        &[],
        &dst,
        &opts(),
    );
    run.ok();
    let qs = question_conflicts(&run.rec);
    let [
        Question::Conflict {
            source,
            existing,
            kind,
        },
    ] = &qs[..]
    else {
        panic!("{qs:?}")
    };
    assert_eq!(source.path, arch.join("a.txt"));
    assert_eq!((source.size, source.mtime_ns), (5, MTIME));
    assert_eq!(existing.path, dst.join("a.txt"));
    assert_eq!(existing.size, OLD.len() as u64);
    assert_eq!(*kind, ConflictKind::Replaceable);
}

#[test]
fn per_item_answers_are_honoured() {
    let tmp = Tmp::new("c-each");
    let dst = tmp.dst();
    pre_existing(&dst);
    let a = archive_with(&tmp);
    let mut r = a.reader();
    let mut n = 0;
    let (sink, rec) = ScriptedSink::new(move |_| {
        n += 1;
        match n {
            1 => conflict(ConflictAnswer::Overwrite, false),
            2 => conflict(ConflictAnswer::Skip, false),
            _ => conflict(ConflictAnswer::RenameNew, false),
        }
    });
    let run = run_with_ctx(
        JobCtx::simple(std::sync::Arc::new(ofm_jobs::RealFs), sink),
        rec,
        &mut r,
        &[],
        &dst,
        &opts(),
    );
    run.ok();
    assert_eq!(run.questions(), 3);
    assert_eq!(read(&dst.join("a.txt")), b"new a");
    assert_eq!(read(&dst.join("b.txt")), OLD);
    assert_eq!(read(&dst.join("c.txt")), OLD);
    assert_eq!(read(&dst.join("c (1).txt")), b"new c");
}

// ---------------------------------------------------------------------------- directories

#[test]
fn existing_directories_are_merged_without_asking() {
    let tmp = Tmp::new("c-merge");
    let dst = tmp.dst();
    std::fs::create_dir_all(dst.join("d")).unwrap();
    std::fs::write(dst.join("d/keep.txt"), b"keep").unwrap();
    std::fs::set_permissions(
        dst.join("d"),
        std::os::unix::fs::PermissionsExt::from_mode(0o711),
    )
    .unwrap();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.dir("d").mode(0o700).mtime(MTIME);
    a.file("d/new.txt", b"new");
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    assert_eq!(read(&dst.join("d/keep.txt")), b"keep");
    assert_eq!(read(&dst.join("d/new.txt")), b"new");
    assert_eq!(
        perm(&dst.join("d")),
        0o711,
        "an existing directory keeps its own mode"
    );
    assert_ne!(mtime_ns(&dst.join("d")), MTIME);
}

#[test]
fn a_directory_where_a_file_goes_is_a_type_mismatch_never_overwritten() {
    let tmp = Tmp::new("c-tm1");
    let dst = tmp.dst();
    std::fs::create_dir_all(dst.join("f")).unwrap();
    std::fs::write(dst.join("f/inner"), b"precious").unwrap();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.file("f", b"i am a file");
    let mut r = a.reader();
    // Overwrite is forbidden for a type mismatch: it degrades to skip + warning.
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Overwrite);
    run.ok();
    assert_eq!(read(&dst.join("f/inner")), b"precious");
    assert!(
        run.report
            .warnings
            .iter()
            .any(|w| w.message.contains("not allowed"))
    );

    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::RenameNew);
    run.ok();
    assert_eq!(read(&dst.join("f (1)")), b"i am a file");
    assert_eq!(read(&dst.join("f/inner")), b"precious");
}

#[test]
fn a_file_where_a_directory_is_needed_skips_the_whole_subtree_with_one_question() {
    let tmp = Tmp::new("c-tm2");
    let dst = tmp.dst();
    std::fs::write(dst.join("d"), b"i am a file").unwrap();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.dir("d");
    a.file("d/one", b"1")
        .file("d/two", b"2")
        .file("d/three", b"3");
    a.file("other", b"o");
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Skip);
    run.ok();
    assert_eq!(read(&dst.join("d")), b"i am a file");
    assert_eq!(read(&dst.join("other")), b"o");
    assert_eq!(run.questions(), 1, "the skipped directory is remembered");
    assert_no_part_files(&dst);

    // "Rename new": the directory lands under a new name, with all its children.
    let mut r = a.reader();
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::RenameNew);
    run.ok();
    assert_eq!(read(&dst.join("d (1)/two")), b"2");
    assert_eq!(read(&dst.join("d")), b"i am a file");
}

// ------------------------------------------------------- symlinks that already exist in dst

#[test]
fn an_existing_symlink_to_an_outside_directory_is_never_written_through() {
    let tmp = Tmp::new("c-sym1");
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("existing"), b"outside data").unwrap();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.dir("link");
    a.file("link/evil.txt", b"payload");
    a.file("link/existing", b"overwritten?!");
    let before = snapshot(&outside);
    for (i, answer) in [
        ConflictAnswer::Skip,
        ConflictAnswer::Overwrite,
        ConflictAnswer::OverwriteIfNewer,
        ConflictAnswer::OverwriteIfSizeDiffers,
        ConflictAnswer::RenameExisting,
        ConflictAnswer::RenameNew,
    ]
    .into_iter()
    .enumerate()
    {
        let dst = tmp.path().join(format!("dst{i}"));
        std::fs::create_dir_all(&dst).unwrap();
        symlink(&outside, dst.join("link")).unwrap();
        let mut r = a.reader();
        let run = run_policy(&mut r, &[], &dst, &opts(), answer);
        run.ok();
        assert_eq!(
            snapshot(&outside),
            before,
            "{answer:?} wrote through the link"
        );
        assert_eq!(read(&outside.join("existing")), b"outside data");
        assert_no_part_files(&outside);
        assert_no_part_files(&dst);
        match answer {
            ConflictAnswer::RenameNew => {
                assert_eq!(read(&dst.join("link (1)/evil.txt")), b"payload");
                assert!(
                    std::fs::symlink_metadata(dst.join("link"))
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
            }
            ConflictAnswer::RenameExisting => {
                // The link itself was moved aside; the archive's directory is a real one.
                assert!(
                    std::fs::symlink_metadata(dst.join("link (1)"))
                        .unwrap()
                        .file_type()
                        .is_symlink()
                );
                assert_eq!(read(&dst.join("link/evil.txt")), b"payload");
            }
            _ => assert!(
                std::fs::symlink_metadata(dst.join("link"))
                    .unwrap()
                    .file_type()
                    .is_symlink()
            ),
        }
    }
}

#[test]
fn an_existing_symlink_in_the_way_of_a_file_is_replaced_itself_not_its_target() {
    let tmp = Tmp::new("c-sym2");
    let dst = tmp.dst();
    let target = tmp.path().join("victim.txt");
    std::fs::write(&target, b"victim").unwrap();
    symlink(&target, dst.join("f.txt")).unwrap();
    // A dangling link pointing at a name outside that does not exist yet.
    let missing = tmp.path().join("must-not-appear.txt");
    symlink(&missing, dst.join("g.txt")).unwrap();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.file("f.txt", b"payload f").file("g.txt", b"payload g");

    // Skip: nothing changes, and certainly nothing is created at the dangling target.
    let mut r = a.reader();
    run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Skip).ok();
    assert_absent(&missing);
    assert_eq!(read(&target), b"victim");
    assert!(
        std::fs::symlink_metadata(dst.join("g.txt"))
            .unwrap()
            .file_type()
            .is_symlink()
    );

    // Overwrite: the link itself is replaced by a regular file (rename), the target is untouched.
    let mut r = a.reader();
    run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Overwrite).ok();
    assert_eq!(read(&target), b"victim");
    assert_absent(&missing);
    let md = std::fs::symlink_metadata(dst.join("f.txt")).unwrap();
    assert!(md.is_file() && !md.file_type().is_symlink());
    assert_eq!(read(&dst.join("f.txt")), b"payload f");
    assert_no_part_files(&dst);
}

#[test]
fn a_symlink_the_job_created_is_not_a_path_for_later_entries() {
    let tmp = Tmp::new("c-sym3");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.dir("real");
    a.symlink("ln", "real");
    a.file("ln/through.txt", b"x"); // the index flags this one (ThroughSymlink)
    a.file("dummy.txt", b"dummy");
    let mut r = a.reader();
    // A lying reader hands out `ln/through.txt` as a safe entry anyway.
    r.tamper = Some(Box::new(|e| {
        if e.path == std::path::Path::new("dummy.txt") {
            vec![e.clone(), forged(e, "ln/through.txt")]
        } else {
            vec![e.clone()]
        }
    }));
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::Skip);
    run.ok();
    assert_absent(&dst.join("real/through.txt"));
    assert!(
        std::fs::symlink_metadata(dst.join("ln"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

// ------------------------------------------------------------------- duplicates and races

#[test]
fn a_repeated_name_goes_through_the_conflict_protocol() {
    let tmp = Tmp::new("c-dup");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.file("dup.txt", b"first");
    let mut r = a.reader();
    r.tamper = Some(Box::new(|e| vec![e.clone(), e.clone()]));
    let run = run_policy(&mut r, &[], &dst, &opts(), ConflictAnswer::RenameNew);
    run.ok();
    assert_eq!(read(&dst.join("dup.txt")), b"first");
    assert_eq!(read(&dst.join("dup (1).txt")), b"first");

    // The job's own output is never overwritten: "overwrite" degrades to skip + warning.
    let dst2 = tmp.path().join("dst2");
    let mut r = a.reader();
    r.tamper = Some(Box::new(|e| vec![e.clone(), e.clone()]));
    let run = run_policy(&mut r, &[], &dst2, &opts(), ConflictAnswer::Overwrite);
    run.ok();
    assert_eq!(snapshot(&dst2).len(), 1);
    assert!(
        run.report
            .warnings
            .iter()
            .any(|w| w.message.contains("not allowed"))
    );
}

#[test]
fn a_name_created_by_someone_else_during_extraction_is_not_overwritten() {
    let tmp = Tmp::new("c-race");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.file("race.txt", &bytes(3, 50_000));
    a.file("after.txt", b"after");
    let mut r = a.reader();
    let racer = dst.join("race.txt");
    r.on_read = Some(std::sync::Arc::new(move |delivered| {
        if delivered == 4096 && !racer.exists() {
            std::fs::write(&racer, b"somebody else's file").unwrap();
        }
    }));
    let run = run_plain(&mut r, &[], &dst, &opts());
    run.ok();
    assert_eq!(read(&dst.join("race.txt")), b"somebody else's file");
    assert_eq!(read(&dst.join("after.txt")), b"after");
    assert!(
        run.warnings()
            .iter()
            .any(|w| w.contains("appeared while extracting"))
    );
    assert_no_part_files(&dst);
}

#[test]
fn error_skip_answers_are_not_needed_for_corrupt_entries() {
    // A corrupt entry is a warning, never a question (the stream cannot be retried).
    let tmp = Tmp::new("c-noq");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("a.tar", 20_000));
    a.file_with("bad.bin", 9000, Data::Corrupt(vec![1; 5000]));
    a.file("good.txt", b"good");
    let mut r = a.reader();
    let run = run_on(
        std::sync::Arc::new(ofm_jobs::RealFs),
        &mut r,
        &[],
        &dst,
        &opts(),
        ConflictAnswer::Skip,
        ErrorAction::Abort,
    );
    run.ok();
    assert_eq!(run.questions(), 0);
}

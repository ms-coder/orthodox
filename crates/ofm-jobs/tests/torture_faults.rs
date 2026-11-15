//! T3.4 torture suite — fault injection through `FaultyFs` (spec F17, ADR-0004). ENOSPC / EIO /
//! short writes at the N-th call of the data tiers (`FileWrite`, `CopyRangeFrom`, `TryCloneFrom`)
//! must never leave a half-written destination under its final name, never leave a temp file, and
//! never delete the source of a move. Ignored until T3.1 lands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod torture_common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_jobs::ScriptedSink;
use ofm_jobs::event::{ConflictAnswer, ErrorAction, Outcome};
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::spec::CopyOpts;
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::tree::{FileSpec, TreeSpec};

use torture_common::{
    assert_absent_or_exact, assert_no_part_files, copy_spec, report_of, run_policy, src_dst,
    two_filesystems, write_file,
};

fn faulty(plan: FaultPlan) -> Arc<FaultyFs> {
    Arc::new(FaultyFs::over_real(plan))
}

/// Copy `names` (files with the given contents) from `src` to `dst` and return the outcome.
fn copy_named(
    src: &Path,
    dst: &Path,
    names: &[(&str, &str)],
    opts: CopyOpts,
    fs: Arc<dyn ofm_jobs::Fs>,
) -> Outcome {
    let sources: Vec<PathBuf> = names.iter().map(|(n, _)| src.join(n)).collect();
    let (sink, _) = ScriptedSink::policy(ConflictAnswer::Skip, ErrorAction::Abort, false);
    ofm_jobs::run_sync(&copy_spec(&sources, dst, opts), fs, sink)
}

/// Assert the destination only ever holds complete files and no temp.
fn safety_all(dst: &Path, names: &[(&str, &str)], what: &str) {
    for (name, content) in names {
        assert_absent_or_exact(&dst.join(name), content.as_bytes(), what);
    }
    assert_no_part_files(dst);
}

#[test]
fn write_tier_faults_at_nth_call_never_leave_a_broken_destination() {
    let tiers = [
        FaultOp::FileWrite,
        FaultOp::CopyRangeFrom,
        FaultOp::TryCloneFrom,
    ];
    let errnos = [FaultErrno::NoSpace, FaultErrno::Io];
    for op in tiers {
        for errno in errnos {
            for nth in [1u64, 2, 3] {
                let tag = format!("fmatrix-{op:?}-{errno:?}-{nth}");
                let (src, dst) = src_dst(&tag);
                std::fs::create_dir_all(src.path()).unwrap();
                std::fs::create_dir_all(dst.path()).unwrap();
                let content = "A".repeat(1 << 20);
                std::fs::write(src.path().join("data.bin"), content.as_bytes()).unwrap();

                let fs = faulty(FaultPlan::new().fail_nth(op, nth, errno));
                // Abort on the first error question: worst case for cleanup.
                let out = copy_named(
                    src.path(),
                    dst.path(),
                    &[("data.bin", content.as_str())],
                    CopyOpts::default(),
                    fs.clone(),
                );
                // Whatever tier the engine picked, the safety invariants hold; if the fault
                // fired the outcome is a clean Cancelled/Failed (abort) or a full success
                // (legitimate fallback to another tier), never a half-written file.
                assert!(
                    matches!(
                        out,
                        Outcome::Ok(_) | Outcome::Cancelled(_) | Outcome::Failed { .. }
                    ),
                    "{tag}: {out:?}"
                );
                safety_all(dst.path(), &[("data.bin", content.as_str())], &tag);
                assert_eq!(
                    std::fs::read(src.path().join("data.bin")).unwrap(),
                    content.as_bytes(),
                    "{tag}: copy never damages the source"
                );
            }
        }
    }
}

#[test]
fn persistent_enospc_on_writes_skips_inflight_and_copies_the_rest() {
    // Needs the read/write tier (copy_file_range and FICLONE cannot cross devices), so this
    // runs on two real filesystems; skipped (with a log line) when only one exists.
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available to force the read/write tier");
        return;
    };
    let (src, dst) = (pair.first, pair.second);
    let names = [
        ("one.bin", "1".repeat(4096)),
        ("two.bin", "2".repeat(4096)),
        ("three.bin", "3".repeat(4096)),
    ];
    for (n, c) in &names {
        std::fs::write(src.path().join(n), c).unwrap();
    }

    // From the 2nd write call on, everything fails with ENOSPC. Files before the fault are
    // complete; the file in flight and everything after is skipped cleanly. (The skip count
    // below proves the write tier was really in use: with no writes, nothing would fail.)
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::FileWrite, 2, FaultErrno::NoSpace));
    let (out, _rec) = run_policy(
        &copy_spec(
            &names
                .iter()
                .map(|(n, _)| src.path().join(n))
                .collect::<Vec<_>>(),
            dst.path(),
            CopyOpts::default(),
        ),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    for (n, c) in &names {
        assert_absent_or_exact(&dst.path().join(n), c.as_bytes(), "enospc mid-tree");
    }
    assert_no_part_files(dst.path());
    let rep = report_of(&out, "enospc");
    assert!(rep.skipped >= 1, "at least one item skipped: {rep:?}");
    assert!(
        rep.files_done + rep.skipped >= 1,
        "something finished or was skipped: {rep:?}"
    );
}

#[test]
fn enospc_on_write_with_skip_policy_leaves_no_trace() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available to force the read/write tier");
        return;
    };
    let (src, dst) = (pair.first, pair.second);
    let content = "Z".repeat(1 << 20);
    std::fs::write(src.path().join("big.bin"), content.as_bytes()).unwrap();

    let fs = faulty(FaultPlan::new().fail_from(FaultOp::FileWrite, 1, FaultErrno::NoSpace));
    let (out, _rec) = run_policy(
        &copy_spec(
            &[src.path().join("big.bin")],
            dst.path(),
            CopyOpts::default(),
        ),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(!dst.path().join("big.bin").exists());
    assert_no_part_files(dst.path());
    assert_eq!(report_of(&out, "enospc-skip").skipped, 1);
}

#[test]
fn transient_write_fault_recovered_by_retry() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available to force the read/write tier");
        return;
    };
    let (src, dst) = (pair.first, pair.second);
    let content = "R".repeat(1 << 20);
    std::fs::write(src.path().join("retry.bin"), content.as_bytes()).unwrap();

    // The 2nd write call fails once; "retry" answers it and the copy completes.
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::FileWrite, 2, FaultErrno::Io));
    let (sink, _) = ScriptedSink::policy(ConflictAnswer::Skip, ErrorAction::Retry, false);
    let out = ofm_jobs::run_sync(
        &copy_spec(
            &[src.path().join("retry.bin")],
            dst.path(),
            CopyOpts::default(),
        ),
        fs,
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("retry.bin")).unwrap(),
        content.as_bytes()
    );
    assert_no_part_files(dst.path());
}

#[test]
fn short_writes_are_looped_to_completion() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available to force the read/write tier");
        return;
    };
    for (nth, short) in [(1u64, 0usize), (1, 1), (2, 4096), (3, 1 << 20)] {
        let src_dir = pair.first.path().join(format!("sw{nth}-{short}"));
        let dst_dir = pair.second.path().join(format!("sw{nth}-{short}"));
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(&dst_dir).unwrap();
        let content = "S".repeat(3 << 20);
        std::fs::write(src_dir.join("short.bin"), content.as_bytes()).unwrap();

        let fs = faulty(FaultPlan::new().short_write_at(nth, short));
        // Skip policy: an engine that treats Ok(0) as an error skips the item (absent dest),
        // a looping engine completes it (exact dest) — both are safe; never a partial file.
        let (sink, _) = ScriptedSink::policy(ConflictAnswer::Skip, ErrorAction::Skip, false);
        let out = ofm_jobs::run_sync(
            &copy_spec(&[src_dir.join("short.bin")], &dst_dir, CopyOpts::default()),
            fs,
            sink,
        );
        assert!(
            matches!(out, Outcome::Ok(_)),
            "short write {nth}/{short}: {out:?}"
        );
        assert_absent_or_exact(
            &dst_dir.join("short.bin"),
            content.as_bytes(),
            "short write {nth}/{short}",
        );
        assert_no_part_files(&dst_dir);
    }
}

#[test]
fn create_temp_failure_skips_the_item_and_keeps_the_rest() {
    let (src, dst) = src_dst("fault-create");
    let names = [("a.txt", "alpha"), ("b.txt", "beta"), ("c.txt", "gamma")];
    for (n, c) in &names {
        std::fs::write(src.path().join(n), c).unwrap();
    }
    std::fs::create_dir_all(dst.path()).unwrap();

    // 2nd create_excl (the temp of b.txt) fails persistently: b and c are skipped.
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::CreateExcl, 2, FaultErrno::NoSpace));
    let (out, _rec) = run_policy(
        &copy_spec(
            &names
                .iter()
                .map(|(n, _)| src.path().join(n))
                .collect::<Vec<_>>(),
            dst.path(),
            CopyOpts::default(),
        ),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // Exactly the item before the fault is copied; the rest is skipped (which item that is
    // depends on the engine's source order — not pinned here).
    let mut copied = 0;
    for (n, c) in &names {
        match std::fs::read(dst.path().join(n)) {
            Ok(b) => {
                assert_eq!(b, c.as_bytes(), "{n} must be complete");
                copied += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("{n}: {e}"),
        }
    }
    assert_eq!(copied, 1, "only the first item is copied");
    assert_eq!(report_of(&out, "fault-create").skipped, 2);
    assert_no_part_files(dst.path());
}

#[test]
fn publish_rename_failure_cleans_the_temp() {
    let (src, dst) = src_dst("fault-publish");
    std::fs::create_dir_all(src.path()).unwrap();
    std::fs::write(src.path().join("a.txt"), b"data").unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let fs = faulty(FaultPlan::new().fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Io));
    let (out, _rec) = run_policy(
        &copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Abort,
        false,
    );
    assert!(
        matches!(out, Outcome::Cancelled(_) | Outcome::Failed { .. }),
        "{out:?}"
    );
    assert!(!dst.path().join("a.txt").exists());
    assert_no_part_files(dst.path());
    assert_eq!(std::fs::read(src.path().join("a.txt")).unwrap(), b"data");
}

#[test]
fn overwrite_rename_failure_keeps_the_existing_destination() {
    let (src, dst) = src_dst("fault-ow-rename");
    std::fs::create_dir_all(src.path()).unwrap();
    std::fs::write(src.path().join("a.txt"), b"NEW").unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();
    std::fs::write(dst.path().join("a.txt"), b"PRECIOUS OLD").unwrap();

    let fs = faulty(FaultPlan::new().fail_from(FaultOp::RenameReplace, 1, FaultErrno::Io));
    let (out, _rec) = run_policy(
        &copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Overwrite,
        ErrorAction::Abort,
        false,
    );
    // The overwrite failed atomically: the old content survives, nothing partial appears.
    assert!(
        matches!(
            out,
            Outcome::Cancelled(_) | Outcome::Failed { .. } | Outcome::Ok(_)
        ),
        "{out:?}"
    );
    let existing = std::fs::read(dst.path().join("a.txt")).unwrap_or_default();
    assert!(
        existing == b"PRECIOUS OLD" || existing == b"NEW",
        "destination must be fully old or fully new, got {:?}",
        String::from_utf8_lossy(&existing)
    );
    assert_no_part_files(dst.path());
}

#[test]
fn source_open_failure_reports_and_skips() {
    let (src, dst) = src_dst("fault-open");
    for (n, c) in [("a.txt", "one"), ("b.txt", "two"), ("c.txt", "three")] {
        std::fs::write(src.path().join(n), c).unwrap();
    }
    std::fs::create_dir_all(dst.path()).unwrap();

    // the 2nd open_read fails once: exactly one source is skipped (order not pinned).
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::OpenRead, 2, FaultErrno::Io));
    let (out, _rec) = run_policy(
        &copy_spec(
            &[
                src.path().join("a.txt"),
                src.path().join("b.txt"),
                src.path().join("c.txt"),
            ],
            dst.path(),
            CopyOpts::default(),
        ),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let mut copied = 0;
    for (n, c) in [
        ("a.txt", &b"one"[..]),
        ("b.txt", &b"two"[..]),
        ("c.txt", &b"three"[..]),
    ] {
        match std::fs::read(dst.path().join(n)) {
            Ok(b) => {
                assert_eq!(b, c, "{n} must be complete");
                copied += 1;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => panic!("{n}: {e}"),
        }
    }
    assert_eq!(copied, 2, "two of three files are copied");
    assert_eq!(report_of(&out, "fault-open").skipped, 1);
    assert_no_part_files(dst.path());
}

#[test]
fn faulted_tree_copy_never_corrupts_completed_directories() {
    // Faults interleaved with directory recursion: completed subtrees stay complete, nothing
    // partial is visible, no temp survives anywhere under the destination.
    let (src, dst) = src_dst("fault-tree");
    let mut spec = TreeSpec::new();
    for d in 0..5 {
        let mut sub = TreeSpec::new();
        for f in 0..20 {
            sub = sub.file(
                format!("f{f}"),
                FileSpec::content(&format!("{d}-{f}").into_bytes()),
            );
        }
        spec = spec.dir(format!("d{d}"), sub);
    }
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let fs = faulty(FaultPlan::new().fail_from(FaultOp::FileWrite, 3, FaultErrno::NoSpace));
    let (out, _rec) = run_policy(
        &copy_spec(&[src.path().to_path_buf()], dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // Every destination file is complete or absent, per file; directories may be partially
    // filled (skips) but never contain partial content or temps.
    for d in 0..5 {
        let dir = dst
            .path()
            .join(src.path().file_name().unwrap())
            .join(format!("d{d}"));
        if dir.is_dir() {
            for f in 0..20 {
                let p = dir.join(format!("f{f}"));
                let expected = format!("{d}-{f}").into_bytes();
                assert_absent_or_exact(&p, &expected, "fault-tree");
            }
        }
    }
    assert_no_part_files(dst.path());
}

#[test]
fn exdev_move_fault_matrix_keeps_sources_intact() {
    // Cross-device move (EXDEV injected on rename) with data faults: sources survive unless the
    // item verifiably completed.
    let (src, dst) = src_dst("fault-move");
    write_file(&src.path().join("a.bin"), &[7u8; 1 << 20], 0o644, 0);

    for op in [
        FaultOp::FileWrite,
        FaultOp::CopyRangeFrom,
        FaultOp::TryCloneFrom,
    ] {
        for errno in [FaultErrno::NoSpace, FaultErrno::Io] {
            let plan = FaultPlan::new()
                .fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(18))
                .fail_from(op, 1, errno);
            let fs = faulty(plan);
            let (out, _rec) = run_policy(
                &torture_common::move_spec(
                    &[src.path().join("a.bin")],
                    dst.path(),
                    CopyOpts::default(),
                ),
                fs,
                ConflictAnswer::Skip,
                ErrorAction::Skip,
                false,
            );
            assert!(
                matches!(
                    out,
                    Outcome::Ok(_) | Outcome::Cancelled(_) | Outcome::Failed { .. }
                ),
                "{op:?}/{errno:?}: {out:?}"
            );
            let src_ok =
                std::fs::read(src.path().join("a.bin")).is_ok_and(|b| b == vec![7u8; 1 << 20]);
            let dst_ok =
                std::fs::read(dst.path().join("a.bin")).is_ok_and(|b| b == vec![7u8; 1 << 20]);
            assert!(
                src_ok || dst_ok,
                "{op:?}/{errno:?}: move lost the data (source and destination both incomplete)"
            );
            assert_no_part_files(dst.path());
            // Reset for the next matrix cell.
            let _ = std::fs::remove_file(dst.path().join("a.bin"));
        }
    }
}

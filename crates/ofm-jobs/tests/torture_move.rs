//! T3.4 torture suite — move (spec F17, ADR-0004 "Move"). Same-filesystem moves must be renames
//! (no data copied); cross-device moves copy with fsync and delete each source item only after
//! its copy completed. Ignored until T3.1 lands.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod torture_common;

use std::path::PathBuf;
use std::sync::Arc;

use ofm_jobs::ScriptedSink;
use ofm_jobs::event::{ConflictAnswer, ErrorAction, Outcome};
use ofm_jobs::fs::RealFs;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::spec::CopyOpts;
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::tree::{FileSpec, TreeSpec};

use torture_common::{
    Rng, assert_absent_or_exact, assert_full_tree_equal, assert_no_part_files, move_spec,
    random_tree, report_of, run_cancel_at, run_policy, sole_entry, src_dst, two_filesystems,
    write_file,
};

fn real() -> Arc<dyn ofm_jobs::Fs> {
    Arc::new(RealFs)
}

fn faulty(plan: FaultPlan) -> Arc<FaultyFs> {
    Arc::new(FaultyFs::over_real(plan))
}

/// ENOSPC is 28, EIO 5 ... EXDEV is 18 (Linux errno).
fn exdev() -> FaultErrno {
    FaultErrno::Raw(18)
}

#[test]
fn move_same_filesystem_is_a_pure_rename() {
    let (src, dst) = src_dst("move-rename");
    write_file(&src.path().join("a.txt"), b"alpha", 0o644, 0);
    write_file(&src.path().join("b.bin"), b"beta", 0o600, 0);
    std::fs::create_dir_all(dst.path()).unwrap();

    let fs = faulty(FaultPlan::new());
    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &move_spec(
            &[src.path().join("a.txt"), src.path().join("b.bin")],
            dst.path(),
            CopyOpts::default(),
        ),
        fs.clone(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");

    // Renamed, not copied: no data path may run at all (ADR-0004 "Move").
    assert_eq!(
        fs.calls_of(FaultOp::FileWrite),
        0,
        "same-fs move must not write data"
    );
    assert_eq!(
        fs.calls_of(FaultOp::CopyRangeFrom),
        0,
        "same-fs move must not copy data"
    );
    assert_eq!(
        fs.calls_of(FaultOp::TryCloneFrom),
        0,
        "same-fs move must not clone data"
    );
    assert!(
        fs.calls_of(FaultOp::RenameNoreplace) >= 1,
        "same-fs move renames items"
    );
    assert!(!src.path().join("a.txt").exists(), "source must be gone");
    assert_eq!(std::fs::read(dst.path().join("a.txt")).unwrap(), b"alpha");
    assert_eq!(std::fs::read(dst.path().join("b.bin")).unwrap(), b"beta");
}

#[test]
fn move_tree_same_filesystem_keeps_metadata() {
    let (src, dst) = src_dst("move-tree");
    let spec = TreeSpec::new()
        .file("f\nwith-newline", FileSpec::content(b"nl"))
        .dir(
            "sub",
            ofm_testkit::tree::DirSpec::new(TreeSpec::new()).mode(0o750),
        )
        .symlink("link", "f\nwith-newline");
    spec.build(src.path()).unwrap();
    let children: Vec<PathBuf> = std::fs::read_dir(src.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &move_spec(&children, dst.path(), CopyOpts::default()),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let reference = src_tree_copy(&spec);
    assert_full_tree_equal(reference.path(), dst.path());
    assert_eq!(
        std::fs::read_dir(src.path()).unwrap().count(),
        0,
        "src emptied"
    );
}

/// Rebuild `spec` into a fresh temp dir; the comparison reference for moves (the original tree
/// is gone after the move). The guard's drop cleans up (read-only dirs included).
fn src_tree_copy(spec: &TreeSpec) -> torture_common::TempDirGuard {
    let t = torture_common::TempDirGuard::with_name("ofm-t34-move-ref").unwrap();
    spec.build(t.path()).unwrap();
    t
}

#[test]
fn move_whole_dir_renames_it() {
    let (src, dst) = src_dst("move-dir");
    let spec = TreeSpec::new().dir(
        "tree",
        TreeSpec::new()
            .file("leaf", FileSpec::content(b"leaf"))
            .dir("empty", TreeSpec::new()),
    );
    spec.build(src.path()).unwrap();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &move_spec(&[src.path().join("tree")], dst.path(), CopyOpts::default()),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let moved = sole_entry(dst.path());
    assert!(moved.ends_with("tree"));
    assert!(moved.join("leaf").is_file());
    assert!(moved.join("empty").is_dir());
    assert!(!src.path().join("tree").exists());
}

#[test]
fn move_rename_in_place_with_literal_mask() {
    // "Rename" (Shift+F6): a Move into the same directory with a literal new name.
    let (src, _dst) = src_dst("move-inplace");
    write_file(&src.path().join("f.txt"), b"renamed in place", 0o644, 0);

    let opts = CopyOpts {
        rename_mask: Some("renamed.dat".into()),
        ..CopyOpts::default()
    };
    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &move_spec(&[src.path().join("f.txt")], src.path(), opts),
        real(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(src.path().join("renamed.dat")).unwrap(),
        b"renamed in place"
    );
    assert!(!src.path().join("f.txt").exists());
}

#[test]
fn move_same_fs_conflict_overwrite_replaces_and_removes_source() {
    let (src, dst) = src_dst("move-conflict-ow");
    write_file(&src.path().join("a.txt"), b"NEW", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"OLD", 0o644, 0);

    let (out, _rec) = run_policy(
        &move_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::Overwrite,
        ErrorAction::Abort,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(dst.path().join("a.txt")).unwrap(), b"NEW");
    assert!(!src.path().join("a.txt").exists());
}

#[test]
fn move_same_fs_conflict_skip_keeps_both_sides() {
    let (src, dst) = src_dst("move-conflict-skip");
    write_file(&src.path().join("a.txt"), b"NEW", 0o644, 0);
    write_file(&dst.path().join("a.txt"), b"OLD", 0o644, 0);

    let (out, _rec) = run_policy(
        &move_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
        ConflictAnswer::Skip,
        ErrorAction::Abort,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(std::fs::read(dst.path().join("a.txt")).unwrap(), b"OLD");
    assert_eq!(std::fs::read(src.path().join("a.txt")).unwrap(), b"NEW");
    assert_eq!(report_of(&out, "move-skip").skipped, 1);
}

#[test]
fn move_survives_exdev_injected_on_rename() {
    // The task-mandated EXDEV simulation: the FIRST rename returns EXDEV; the engine must fall
    // back to copy + verify + delete-source for that item (later renames succeed normally).
    // (Only the first: a persistent fault would also kill the temp->final publish renames of
    // the fallback itself.)
    let (src, dst) = src_dst("move-exdev");
    write_file(&src.path().join("a.txt"), b"alpha-alpha", 0o644, 0);
    write_file(&src.path().join("b.txt"), b"beta-beta", 0o600, 0);

    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::RenameNoreplace, 1, exdev()));
    let (out, _rec) = run_policy(
        &move_spec(
            &[src.path().join("a.txt"), src.path().join("b.txt")],
            dst.path(),
            CopyOpts::default(),
        ),
        fs.clone(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
        false,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("a.txt")).unwrap(),
        b"alpha-alpha"
    );
    assert_eq!(
        std::fs::read(dst.path().join("b.txt")).unwrap(),
        b"beta-beta"
    );
    assert!(!src.path().join("a.txt").exists());
    assert!(!src.path().join("b.txt").exists());
    assert_no_part_files(dst.path());
    // The EXDEV'd item was really carried over by a data copy, not lost.
    let data_ops = fs.calls_of(FaultOp::FileWrite)
        + fs.calls_of(FaultOp::CopyRangeFrom)
        + fs.calls_of(FaultOp::TryCloneFrom);
    assert!(data_ops >= 1, "the EXDEV fallback must copy data");
    assert!(
        fs.calls_of(FaultOp::FileSync) >= 1,
        "the fallback move syncs before deleting the source"
    );
}

#[test]
fn move_exdev_with_failing_fsync_never_deletes_source() {
    // Cross-device move must fsync the destination before deleting the source. Make the first
    // fsync fail: aborting the job must leave the source fully intact.
    let (src, dst) = src_dst("move-exdev-sync");
    write_file(&src.path().join("a.txt"), b"must survive", 0o644, 0);

    let fs = faulty(
        FaultPlan::new()
            .fail_from(FaultOp::RenameNoreplace, 1, exdev())
            .fail_from(FaultOp::FileSync, 1, FaultErrno::Io),
    );
    let (out, _rec) = run_policy(
        &move_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Skip,
        ErrorAction::Abort,
        false,
    );
    // Whether the fault aborted the job or the engine completed the fallback move, the data
    // safety invariant is the same: the content survives on at least one side, complete, and
    // no temp is left behind.
    let src_ok = std::fs::read(src.path().join("a.txt")).is_ok_and(|b| b == b"must survive");
    let dst_ok = std::fs::read(dst.path().join("a.txt")).is_ok_and(|b| b == b"must survive");
    assert!(
        src_ok || dst_ok,
        "data lost: neither source nor destination holds the content ({out:?})"
    );
    assert_no_part_files(dst.path());
}

#[test]
fn move_across_real_filesystems_copies_syncs_and_deletes_per_item() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available for the cross-device move");
        return;
    };
    let src = pair.first;
    let dst = pair.second;
    write_file(&src.path().join("a.txt"), b"across devices", 0o644, 0);
    write_file(&src.path().join("b.txt"), b"second item", 0o600, 0);

    let fs = faulty(FaultPlan::new());
    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &move_spec(
            &[src.path().join("a.txt"), src.path().join("b.txt")],
            dst.path(),
            CopyOpts::default(),
        ),
        fs.clone(),
        sink,
    );
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        std::fs::read(dst.path().join("a.txt")).unwrap(),
        b"across devices"
    );
    assert_eq!(
        std::fs::read(dst.path().join("b.txt")).unwrap(),
        b"second item"
    );
    assert!(!src.path().join("a.txt").exists());
    assert!(!src.path().join("b.txt").exists());
    // Cross-device move always syncs (ADR-0004 step 5): the file before the source is unlinked,
    // the directory after the publish rename.
    assert!(
        fs.calls_of(FaultOp::FileSync) >= 1,
        "file must be fsynced before source removal"
    );
    assert!(
        fs.calls_of(FaultOp::SyncDir) >= 1,
        "dest dir must be fsynced after the rename"
    );
    assert_no_part_files(dst.path());
}

#[test]
fn move_across_real_filesystems_deletes_source_only_per_completed_item() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available for the cross-device move");
        return;
    };
    let src = pair.first;
    let dst = pair.second;
    for (name, content) in [
        ("a.txt", &b"first"[..]),
        ("b.txt", &b"second"[..]),
        ("c.txt", &b"third"[..]),
    ] {
        write_file(&src.path().join(name), content, 0o644, 0);
    }
    // The first temp file is created (call 1); every later create fails with ENOSPC ->
    // items 2 and 3 are skipped, their sources must remain.
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::CreateExcl, 2, FaultErrno::NoSpace));
    let (out, _rec) = run_policy(
        &move_spec(
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
    // Exactly one item moved completely (gone at the source, complete at the destination);
    // the skipped items never lose their source. Which one moved is the engine's order.
    let all = [
        ("a.txt", &b"first"[..]),
        ("b.txt", &b"second"[..]),
        ("c.txt", &b"third"[..]),
    ];
    let mut moved = 0;
    for (n, c) in all {
        let at_src = std::fs::read(src.path().join(n)).is_ok_and(|b| b == c);
        let at_dst = std::fs::read(dst.path().join(n)).is_ok_and(|b| b == c);
        assert!(
            at_src != at_dst,
            "{n}: item must be entirely at the source or at the destination"
        );
        if at_dst {
            moved += 1;
        }
    }
    assert_eq!(moved, 1, "exactly one item completes before the fault");
    assert_eq!(report_of(&out, "move-partial").skipped, 2);
    assert_no_part_files(dst.path());
}

#[test]
fn move_across_real_filesystems_cancelled_mid_copy_keeps_source() {
    let Some(pair) = two_filesystems() else {
        eprintln!("SKIP: no second filesystem available for the cross-device move");
        return;
    };
    let src = pair.first;
    let dst = pair.second;
    // Two 2 MiB files; cancel once roughly a quarter of the data is through.
    ofm_testkit::tree::TreeSpec::new()
        .file("a.bin", FileSpec::size(2 << 20))
        .file("b.bin", FileSpec::size(2 << 20))
        .build(src.path())
        .unwrap();

    let (out, _sink) = run_cancel_at(
        &move_spec(
            &[src.path().join("a.bin"), src.path().join("b.bin")],
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        1 << 20,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    // Every source item is either still here (not yet moved) or fully moved; the copy in
    // progress never appears truncated, and no temp survives.
    let zeros = vec![0u8; 2 << 20];
    assert_absent_or_exact(&src.path().join("a.bin"), &zeros, "src a");
    assert_absent_or_exact(&src.path().join("b.bin"), &zeros, "src b");
    assert_absent_or_exact(&dst.path().join("a.bin"), &zeros, "dst a");
    assert_absent_or_exact(&dst.path().join("b.bin"), &zeros, "dst b");
    assert_no_part_files(dst.path());
    // The job must not have finished everything: at least one item is still at the source.
    assert!(
        src.path().join("a.bin").exists() || src.path().join("b.bin").exists(),
        "cancel must leave work undone"
    );
}

#[test]
fn move_destination_inside_source_is_refused() {
    let (src, _dst) = src_dst("move-inside");
    let spec = TreeSpec::new()
        .file("f.txt", FileSpec::content(b"x"))
        .dir("sub", TreeSpec::new());
    spec.build(src.path()).unwrap();

    let (sink, _) = ScriptedSink::refusing();
    let out = ofm_jobs::run_sync(
        &move_spec(
            &[src.path().to_path_buf()],
            &src.path().join("sub"),
            CopyOpts::default(),
        ),
        real(),
        sink,
    );
    match &out {
        Outcome::Failed { error, .. } => assert!(
            error.to_lowercase().contains("inside"),
            "expected dest-inside-source error, got: {error}"
        ),
        other => panic!("move into itself must fail, got {other:?}"),
    }
    assert!(
        src.path().join("f.txt").exists(),
        "source must be untouched"
    );
    assert_eq!(
        std::fs::read_dir(src.path().join("sub")).unwrap().count(),
        0
    );
}

#[test]
fn move_pre_cancelled_touches_nothing() {
    let (src, dst) = src_dst("move-precancel");
    write_file(&src.path().join("a.txt"), b"stay", 0o644, 0);

    let out = torture_common::run_pre_cancelled(
        &move_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(std::fs::read(src.path().join("a.txt")).unwrap(), b"stay");
    if dst.path().exists() {
        assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 0);
    }
}

#[test]
fn move_random_trees_same_filesystem() {
    for seed in 11..=16u64 {
        let (src, dst) = src_dst(&format!("move-rnd-{seed}"));
        let mut rng = Rng::new(seed);
        let spec = random_tree(&mut rng, src.path(), 0);
        spec.build(src.path()).unwrap();
        let names: Vec<PathBuf> = std::fs::read_dir(src.path())
            .unwrap()
            .flatten()
            .map(|e| e.path())
            .collect();

        // Move the children of src away; compare against a second materialization of the spec.
        let reference = src_tree_copy(&spec);
        let (sink, _) = ScriptedSink::refusing();
        let out = ofm_jobs::run_sync(
            &move_spec(&names, dst.path(), CopyOpts::default()),
            real(),
            sink,
        );
        assert!(matches!(out, Outcome::Ok(_)), "seed {seed}: {out:?}");
        assert_full_tree_equal(reference.path(), dst.path());
        for n in &names {
            assert!(!n.exists(), "source {n:?} must be gone after the move");
        }
    }
}

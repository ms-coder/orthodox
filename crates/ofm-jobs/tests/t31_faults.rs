//! T3.1: failure and cancel paths of copy (invariant 3). Faults come from `FaultyFs` (ENOSPC, EIO,
//! short writes at the N-th call); cancellation from a sink that cancels at a byte offset. In every
//! case: no file under a final name is half-written, no temp file remains, sources are untouched,
//! and an existing destination keeps its old content unless it was atomically replaced.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t31_support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    Answer, ConflictAnswer, CopyOpts, ErrorAction, Fs, JobEngine, JobEvent, Outcome, Question,
    RealFs, RunMode,
};
use ofm_testkit::compare::{Compare, assert_trees_equal};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec};
use t31_support::*;

const MIB: usize = 1 << 20;

fn big_src() -> TempDir {
    TreeSpec::new()
        .file("big", FileSpec::content(&noise(6 * MIB + 7, 11)))
        .build_temp()
        .unwrap()
}

/// Both data tiers: kernel `copy_file_range` (tmpfs, ext4) and plain read/write.
fn tiers() -> Vec<(&'static str, Arc<dyn Fs>)> {
    vec![
        ("kernel", Arc::new(RealFs)),
        ("read/write", Tiers::read_write_only()),
    ]
}

fn faulty(inner: Arc<dyn Fs>, plan: FaultPlan) -> Arc<FaultyFs> {
    Arc::new(FaultyFs::new(inner, plan))
}

/// Faults on the n-th data transfer call of either tier.
fn data_fault(n: u64, errno: FaultErrno) -> FaultPlan {
    FaultPlan::new()
        .fail_nth(FaultOp::FileWrite, n, errno)
        .fail_nth(FaultOp::CopyRangeFrom, n, errno)
}

#[test]
fn cancel_at_any_byte_offset_leaves_no_final_and_no_temp() {
    let src = big_src();
    let total = (6 * MIB + 7) as u64;
    let offsets = [
        0,
        1,
        4096,
        MIB as u64 - 1,
        MIB as u64,
        MIB as u64 + 1,
        2_621_440,
        5 * MIB as u64 + 12_345,
        total,
    ];
    for (tier, fs) in tiers() {
        for at in offsets {
            let dst = TempDir::new().unwrap();
            let spec = copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default());
            let (out, seen) = run_cancel_at(&spec, Arc::clone(&fs), at);
            assert!(
                matches!(out, Outcome::Cancelled(_)),
                "{tier} @{at}: {out:?}"
            );
            assert!(
                names_in(dst.path()).is_empty(),
                "{tier} @{at}: dest not clean"
            );
            // Cancellation is observed within one 1 MiB chunk.
            let max = seen.iter().map(|p| p.bytes_done).max().unwrap_or(0);
            assert!(
                max <= at + MIB as u64,
                "{tier} @{at}: copied {max} after cancel"
            );
            assert_eq!(
                std::fs::read(src.path().join("big")).unwrap().len(),
                total as usize
            );
        }
    }
}

#[test]
fn cancel_in_a_tree_keeps_completed_files_whole() {
    let mut spec = TreeSpec::new();
    for i in 0..12 {
        spec = spec.file(format!("f{i:02}"), FileSpec::content(&noise(700_000, i)));
    }
    let src = TreeSpec::new().dir("t", spec).build_temp().unwrap();
    for at in [1, 1_500_000, 4_000_000, 7_000_000] {
        let dst = TempDir::new().unwrap();
        let (out, _) = run_cancel_at(
            &copy_spec(&[src.path().join("t")], dst.path(), CopyOpts::default()),
            Arc::new(RealFs),
            at,
        );
        assert!(
            matches!(&out, Outcome::Cancelled(r) if r.created == vec![dst.path().join("t")]),
            "{out:?}"
        );
        assert_no_temps(dst.path());
        assert_files_complete(&src.path().join("t"), &dst.path().join("t"));
        assert!(names_in(&dst.path().join("t")).len() < 12);
    }
}

#[test]
fn cancel_while_copying_a_sparse_file() {
    let src = TreeSpec::new()
        .file(
            "s",
            FileSpec::sparse_ranges(
                256 << 20,
                &[(0, &noise(3 * MIB, 5)), (100 << 20, &noise(3 * MIB, 6))],
            ),
        )
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let (out, _) = run_cancel_at(
        &copy_spec(&[src.path().join("s")], dst.path(), CopyOpts::default()),
        Arc::new(RealFs),
        2 * MIB as u64,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(names_in(dst.path()).is_empty());
}

#[test]
fn enospc_abort_leaves_nothing_and_skip_continues() {
    for (tier, inner) in tiers() {
        let src = big_src();
        let dst = TempDir::new().unwrap();
        let fs = faulty(Arc::clone(&inner), data_fault(3, FaultErrno::NoSpace));
        let (out, rec) = run_policy(
            &copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default()),
            fs.clone(),
            ConflictAnswer::Abort,
            ErrorAction::Abort,
        );
        assert!(matches!(out, Outcome::Cancelled(_)), "{tier}: {out:?}");
        let qs = questions(&rec);
        assert!(
            matches!(&qs[..], [Question::Error { os_error: Some(28), path, .. }] if *path == dst.path().join("big")),
            "{tier}: {qs:?}"
        );
        assert!(names_in(dst.path()).is_empty(), "{tier}");

        // Skip: the failing file is skipped, the next one is copied.
        let src = TreeSpec::new()
            .file("a", FileSpec::content(&noise(3 * MIB, 1)))
            .file("b", FileSpec::content(&noise(10, 2)))
            .build_temp()
            .unwrap();
        let dst = TempDir::new().unwrap();
        // Only the tier in use: FaultyFs also counts the declined copy_range_from probes.
        let op = if tier == "kernel" {
            FaultOp::CopyRangeFrom
        } else {
            FaultOp::FileWrite
        };
        let fs = faulty(inner, FaultPlan::new().fail_nth(op, 2, FaultErrno::NoSpace));
        let (out, _) = run_policy(
            &copy_spec(
                &[src.path().join("a"), src.path().join("b")],
                dst.path(),
                CopyOpts::default(),
            ),
            fs,
            ConflictAnswer::Abort,
            ErrorAction::Skip,
        );
        assert!(out.is_success(), "{tier}: {out:?}");
        assert_eq!(
            names_in(dst.path()),
            vec![std::ffi::OsString::from("b")],
            "{tier}"
        );
        assert_eq!(report_of(&out).skipped, 1);
    }
}

#[test]
fn eio_retry_restarts_the_file_and_succeeds() {
    for (tier, inner) in tiers() {
        let src = big_src();
        let dst = TempDir::new().unwrap();
        let plan = data_fault(2, FaultErrno::Io).fail_nth(FaultOp::FileRead, 2, FaultErrno::Io);
        let fs = faulty(inner, plan);
        let mut retries = 0;
        let (out, rec) = run_with(
            &copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default()),
            fs,
            move |q| {
                assert!(
                    matches!(
                        q,
                        Question::Error {
                            os_error: Some(5),
                            ..
                        }
                    ),
                    "{q:?}"
                );
                retries += 1;
                assert!(retries < 5);
                error(ErrorAction::Retry)
            },
        );
        assert!(out.is_success(), "{tier}: {out:?}");
        assert!(!questions(&rec).is_empty(), "{tier}");
        assert_trees_equal(src.path(), dst.path(), Compare::all());
    }
}

#[test]
fn short_writes_are_completed() {
    let src = big_src();
    let dst = TempDir::new().unwrap();
    let plan = FaultPlan::new()
        .short_write_at(1, 1)
        .short_write_at(2, 4095)
        .short_write_at(5, 0)
        .short_write_at(6, 100_000);
    let fs = faulty(Tiers::read_write_only(), plan);
    let (out, rec) = run_with(
        &copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default()),
        fs.clone(),
        |_| error(ErrorAction::Retry),
    );
    assert!(out.is_success(), "{out:?}");
    // The zero-length write is an error (WriteZero); retry restarts the file.
    assert_eq!(questions(&rec).len(), 1);
    assert_trees_equal(src.path(), dst.path(), Compare::all());
    assert!(fs.calls_of(FaultOp::FileWrite) >= 8);
}

#[test]
fn failed_overwrite_keeps_the_old_destination() {
    let src = big_src();
    for plan in [
        data_fault(4, FaultErrno::NoSpace),
        FaultPlan::new().fail_nth(FaultOp::RenameReplace, 1, FaultErrno::Io),
        FaultPlan::new().fail_nth(FaultOp::FileSync, 1, FaultErrno::Io),
        FaultPlan::new().fail_nth(FaultOp::CreateExcl, 1, FaultErrno::NoSpace),
    ] {
        for (tier, inner) in tiers() {
            let dst = TreeSpec::new()
                .file("big", FileSpec::content(b"precious old data"))
                .build_temp()
                .unwrap();
            let fs = faulty(inner, plan.clone());
            let (out, _) = run_policy(
                &copy_spec(
                    &[src.path().join("big")],
                    dst.path(),
                    CopyOpts {
                        sync: true,
                        ..CopyOpts::default()
                    },
                ),
                fs,
                ConflictAnswer::Overwrite,
                ErrorAction::Abort,
            );
            assert!(
                matches!(out, Outcome::Cancelled(_)),
                "{tier} {plan:?}: {out:?}"
            );
            assert_eq!(
                std::fs::read(dst.path().join("big")).unwrap(),
                b"precious old data",
                "{tier} {plan:?}"
            );
            assert_eq!(names_in(dst.path()).len(), 1, "{tier} {plan:?}: temp left");
        }
    }
}

#[test]
fn publish_failure_removes_the_temp_even_if_the_first_unlink_fails() {
    let src = big_src();
    let dst = TempDir::new().unwrap();
    let plan = FaultPlan::new()
        .fail_nth(FaultOp::RenameNoreplace, 1, FaultErrno::Io)
        .fail_nth(FaultOp::Unlink, 1, FaultErrno::Io);
    let fs = faulty(Arc::new(RealFs), plan);
    let (out, _) = run_policy(
        &copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default()),
        fs.clone(),
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(names_in(dst.path()).is_empty());
    assert_eq!(fs.calls_of(FaultOp::Unlink), 2);

    // A temp that cannot be removed at all is reported, never silently left.
    let dst = TempDir::new().unwrap();
    let plan = FaultPlan::new()
        .fail_nth(FaultOp::RenameNoreplace, 1, FaultErrno::Io)
        .fail_from(FaultOp::Unlink, 1, FaultErrno::Io);
    let (out, _) = run_policy(
        &copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default()),
        faulty(Arc::new(RealFs), plan),
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    let r = report_of(&out);
    assert!(
        r.warnings
            .iter()
            .any(|w| w.message.contains("temporary file")),
        "{r:?}"
    );
    assert!(!dst.path().join("big").exists());
}

#[test]
fn metadata_failures_are_warnings_not_errors() {
    let src = TreeSpec::new()
        .file("a", FileSpec::content(b"a").mode(0o640))
        .file("b", FileSpec::content(b"b").mode(0o600))
        .build_temp()
        .unwrap();
    let dst = TempDir::new().unwrap();
    let plan = FaultPlan::new()
        .fail_from(FaultOp::FileSetMode, 1, FaultErrno::PermissionDenied)
        .fail_nth(FaultOp::FileSetTimes, 1, FaultErrno::Io);
    let (out, rec) = run_quiet_on(
        &copy_spec(
            &[src.path().join("a"), src.path().join("b")],
            dst.path(),
            CopyOpts::default(),
        ),
        faulty(Arc::new(RealFs), plan),
    );
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).is_empty());
    let r = report_of(&out);
    // Deduplicated: one warning per kind, not one per file.
    assert_eq!(r.warnings.len(), 2, "{:?}", r.warnings);
    assert_eq!(std::fs::read(dst.path().join("b")).unwrap(), b"b");
    // A file whose mode could not be set stays at the safe temp mode 0600.
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(dst.path().join("a"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

fn run_quiet_on(spec: &ofm_jobs::JobSpec, fs: Arc<dyn Fs>) -> (Outcome, Rec) {
    run_with(spec, fs, |_| None)
}

#[test]
fn engine_with_faulty_fs_asks_over_events_and_cleans_up() {
    let src = big_src();
    let dst = TempDir::new().unwrap();
    let engine = JobEngine::with_fs(faulty(Arc::new(RealFs), data_fault(2, FaultErrno::NoSpace)));
    let rx = engine.events();
    let id = engine.submit(
        copy_spec(&[src.path().join("big")], dst.path(), CopyOpts::default()),
        RunMode::Queued,
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let outcome = loop {
        assert!(Instant::now() < deadline, "timeout");
        let Ok(ev) = rx.try_recv() else {
            std::thread::sleep(Duration::from_millis(2));
            continue;
        };
        match ev {
            JobEvent::Ask {
                id: qid,
                question: Question::Error { os_error, .. },
                reply,
            } => {
                assert_eq!((qid, os_error), (id, Some(28)));
                reply.answer(Answer::Error {
                    action: ErrorAction::Abort,
                    apply_to_all: false,
                });
            }
            JobEvent::Finished { id: fid, outcome } => {
                assert_eq!(fid, id);
                break outcome;
            }
            _ => {}
        }
    };
    assert!(matches!(outcome, Outcome::Cancelled(_)), "{outcome:?}");
    assert!(names_in(dst.path()).is_empty());
}

#[test]
fn sparse_copy_without_seek_data_falls_back_to_a_dense_copy() {
    let src = TreeSpec::new()
        .file(
            "s",
            FileSpec::sparse_ranges(8 << 20, &[(3 << 20, &noise(5000, 7))]),
        )
        .build_temp()
        .unwrap();
    // EINVAL from the very first SEEK_DATA: dense copy, identical content.
    let dst = TempDir::new().unwrap();
    let fs = faulty(
        Arc::new(RealFs),
        FaultPlan::new().fail_nth(FaultOp::SeekData, 1, FaultErrno::Raw(22)),
    );
    let (out, rec) = run_quiet_on(
        &copy_spec(&[src.path().join("s")], dst.path(), CopyOpts::default()),
        fs,
    );
    assert!(out.is_success(), "{out:?}");
    assert!(questions(&rec).is_empty());
    assert_trees_equal(src.path(), dst.path(), Compare::all());

    // A real error in the middle of the extent walk: asked, abort leaves nothing.
    let dst = TempDir::new().unwrap();
    let fs = faulty(
        Arc::new(RealFs),
        FaultPlan::new().fail_nth(FaultOp::SeekData, 2, FaultErrno::Io),
    );
    let (out, rec) = run_policy(
        &copy_spec(&[src.path().join("s")], dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(matches!(
        &questions(&rec)[..],
        [Question::Error {
            os_error: Some(5),
            ..
        }]
    ));
    assert!(names_in(dst.path()).is_empty());
}

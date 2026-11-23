//! T6.2 extract job: cancel at many offsets and injected file system faults. The invariants:
//! no `.ofm-part` temp file survives, no final name holds truncated data, the job fails with the
//! path of the failing operation, directories get their final mode even when the job stops.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use extract_support::*;
use ofm_core::CancelToken;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ConflictAnswer, ErrorAction, ExtractOpts, JobError};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};

const F1: usize = 50_000;
const F2: usize = 200_000;
const F3: usize = 30_000;

fn tree(tmp: &Tmp) -> MemArchive {
    let mut a = MemArchive::new(&tmp.archive_file("t.zip", 100_000));
    a.dir("d").mode(0o750).mtime(MTIME);
    a.file("d/f1", &bytes(1, F1));
    a.file("d/f2", &bytes(2, F2)).mode(0o600);
    a.file("d/f3", &bytes(3, F3));
    a.file("last.txt", b"last");
    a
}

fn assert_exact_or_absent(dst: &std::path::Path) {
    assert_absent_or_exact(&dst.join("d/f1"), &bytes(1, F1));
    assert_absent_or_exact(&dst.join("d/f2"), &bytes(2, F2));
    assert_absent_or_exact(&dst.join("d/f3"), &bytes(3, F3));
    assert_absent_or_exact(&dst.join("last.txt"), b"last");
}

/// Like [`assert_exact_or_absent`] for a destination where `last.txt` already held `old`.
fn assert_exact_or_absent_with_old_last(dst: &std::path::Path, old: &[u8]) {
    assert_absent_or_exact(&dst.join("d/f1"), &bytes(1, F1));
    assert_absent_or_exact(&dst.join("d/f2"), &bytes(2, F2));
    assert_absent_or_exact(&dst.join("d/f3"), &bytes(3, F3));
    assert_eq!(read(&dst.join("last.txt")), old);
}

// -------------------------------------------------------------------------------- cancel

#[test]
fn cancel_at_many_offsets_never_leaves_temp_files_or_truncated_finals() {
    let total = (F1 + F2 + F3) as u64;
    let mut offsets: Vec<u64> = vec![0, 1, 100, 4095, 4096, 4097, 8192, 10_000, 49_999];
    offsets.extend([50_000, 50_001, 65_536, 100_000, 249_999, 250_000, 250_001]);
    offsets.extend([total - 1]);
    for off in offsets {
        let tmp = Tmp::new("cancel");
        let dst = tmp.dst();
        let a = tree(&tmp);
        let mut r = a.reader();
        let cancel = CancelToken::new();
        let c = cancel.clone();
        r.on_read = Some(Arc::new(move |delivered| {
            if delivered >= off {
                c.cancel();
            }
        }));
        let run = run_cancellable(&cancel, &mut r, &[], &dst, &opts());
        assert!(is_cancelled(&run), "offset {off}: {:?}", run.result);
        assert_no_part_files(&dst);
        assert_exact_or_absent(&dst);
        // The directory got its final mode and time although the job stopped.
        if dst.join("d").exists() {
            assert_eq!(perm(&dst.join("d")), 0o750, "offset {off}");
            assert_eq!(mtime_ns(&dst.join("d")), MTIME, "offset {off}");
        }
        // Files that were completed are in the report's counters.
        let done = [dst.join("d/f1"), dst.join("d/f2"), dst.join("d/f3")]
            .iter()
            .filter(|p| p.exists())
            .count() as u64;
        assert!(run.report.files_done >= done, "offset {off}");
    }
}

#[test]
fn cancel_is_noticed_within_one_chunk_of_a_big_file() {
    let tmp = Tmp::new("cancel1m");
    let dst = tmp.dst();
    let big = 6 << 20;
    let mut a = MemArchive::new(&tmp.archive_file("big.zip", 10_000_000));
    a.file_with("big.bin", big, Data::Zeros(big));
    let mut r = a.reader();
    r.chunk = 1 << 20; // the reader hands out 1 MiB per read: the coarsest allowed granularity
    let cancel = CancelToken::new();
    let max_delivered = Arc::new(AtomicU64::new(0));
    let (c, m) = (cancel.clone(), Arc::clone(&max_delivered));
    r.on_read = Some(Arc::new(move |delivered| {
        m.store(delivered, Ordering::SeqCst);
        if delivered >= 1 << 20 {
            c.cancel();
        }
    }));
    let run = run_cancellable(&cancel, &mut r, &[], &dst, &opts());
    assert!(is_cancelled(&run));
    // The read that saw the cancel may still deliver its chunk; the next checkpoint stops it.
    assert!(max_delivered.load(Ordering::SeqCst) <= 2 << 20);
    assert_no_part_files(&dst);
    assert_absent(&dst.join("big.bin"));
}

#[test]
fn a_cancel_before_the_start_does_nothing() {
    let tmp = Tmp::new("cancel0");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let mut r = a.reader();
    let cancel = CancelToken::new();
    cancel.cancel();
    let run = run_cancellable(&cancel, &mut r, &[], &dst, &opts());
    assert!(is_cancelled(&run));
    assert!(snapshot(&dst).is_empty());
}

#[test]
fn nobody_answering_a_conflict_cancels_cleanly() {
    let tmp = Tmp::new("noanswer");
    let dst = tmp.dst();
    std::fs::write(dst.join("last.txt"), b"mine").unwrap();
    let a = tree(&tmp);
    let mut r = a.reader();
    let run = run_plain(&mut r, &[], &dst, &opts()); // refusing sink: the first question aborts
    assert!(is_cancelled(&run), "{:?}", run.result);
    assert_no_part_files(&dst);
    assert_exact_or_absent_with_old_last(&dst, b"mine");
}

// -------------------------------------------------------------------------------- faults

fn faulty(plan: FaultPlan) -> Arc<FaultyFs> {
    Arc::new(FaultyFs::over_real(plan))
}

fn run_faulty(fs: &Arc<FaultyFs>, a: &MemArchive, dst: &std::path::Path, o: &ExtractOpts) -> Run {
    let mut r = a.reader();
    run_on(
        fs.clone(),
        &mut r,
        &[],
        dst,
        o,
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    )
}

#[test]
fn enospc_and_eio_at_every_write_stop_the_job_cleanly() {
    // Baseline: how many data writes does a clean run perform?
    let tmp = Tmp::new("f-base");
    let a = tree(&tmp);
    let base = faulty(FaultPlan::new());
    run_faulty(&base, &a, &tmp.dst(), &opts()).ok();
    let writes = base.calls_of(FaultOp::FileWrite);
    assert!(writes >= 4, "{writes}");
    let step = (writes / 12).max(1);
    for errno in [FaultErrno::NoSpace, FaultErrno::Io] {
        let mut nth = 1;
        while nth <= writes {
            let tmp = Tmp::new("f-write");
            let dst = tmp.dst();
            let fs = faulty(FaultPlan::new().fail_nth(FaultOp::FileWrite, nth, errno));
            let run = run_faulty(&fs, &a, &dst, &opts());
            match &run.result {
                Err(JobError::Io { path, source }) => {
                    assert!(path.starts_with(&dst), "{path:?}");
                    assert_eq!(source.raw_os_error(), errno.to_io_error().raw_os_error());
                }
                other => panic!("write {nth}/{errno:?}: {other:?}"),
            }
            assert_no_part_files(&dst);
            assert_exact_or_absent(&dst);
            nth += step;
        }
    }
}

#[test]
fn enospc_mid_file_keeps_earlier_files_and_directory_metadata() {
    let tmp = Tmp::new("f-mid");
    let dst = tmp.dst();
    let a = tree(&tmp);
    // d/f1 needs 13 writes of 4096; fail somewhere in d/f2.
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::FileWrite, 20, FaultErrno::NoSpace));
    let run = run_faulty(&fs, &a, &dst, &opts());
    assert!(
        matches!(run.result, Err(JobError::Io { .. })),
        "{:?}",
        run.result
    );
    assert_eq!(
        read(&dst.join("d/f1")),
        bytes(1, F1),
        "completed files stay"
    );
    assert_absent(&dst.join("d/f2"));
    assert_absent(&dst.join("d/f3"));
    assert_absent(&dst.join("last.txt"));
    assert_no_part_files(&dst);
    assert_eq!(perm(&dst.join("d")), 0o750);
    assert_eq!(run.report.files_done, 1);
}

#[test]
fn short_writes_are_completed() {
    let tmp = Tmp::new("f-short");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let fs = faulty(FaultPlan::new().short_write_at(2, 7).short_write_at(5, 1));
    run_faulty(&fs, &a, &dst, &opts()).ok();
    assert_eq!(read(&dst.join("d/f2")), bytes(2, F2));
    assert_no_part_files(&dst);
}

#[test]
fn fsync_and_close_errors_fail_the_job_and_remove_the_temp() {
    for (op, sync) in [(FaultOp::FileSync, true), (FaultOp::FileClose, false)] {
        let tmp = Tmp::new("f-sync");
        let dst = tmp.dst();
        let a = tree(&tmp);
        let fs = faulty(FaultPlan::new().fail_nth(op, 2, FaultErrno::Io));
        let o = ExtractOpts { sync, ..opts() };
        let run = run_faulty(&fs, &a, &dst, &o);
        assert!(
            matches!(run.result, Err(JobError::Io { .. })),
            "{op:?}: {:?}",
            run.result
        );
        assert_no_part_files(&dst);
        assert_exact_or_absent(&dst);
        assert_absent(&dst.join("d/f2"));
    }
}

#[test]
fn a_full_disk_when_creating_the_temp_file_stops_the_job() {
    let tmp = Tmp::new("f-create");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::CreateExcl, 2, FaultErrno::NoSpace));
    let run = run_faulty(&fs, &a, &dst, &opts());
    assert!(
        matches!(run.result, Err(JobError::Io { .. })),
        "{:?}",
        run.result
    );
    assert_eq!(read(&dst.join("d/f1")), bytes(1, F1));
    assert_no_part_files(&dst);
}

#[test]
fn a_permission_error_on_one_file_is_a_warning_for_that_file_only() {
    let tmp = Tmp::new("f-eacces");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let fs =
        faulty(FaultPlan::new().fail_nth(FaultOp::CreateExcl, 2, FaultErrno::PermissionDenied));
    let run = run_faulty(&fs, &a, &dst, &opts());
    let rep = run.ok();
    assert_eq!(read(&dst.join("d/f1")), bytes(1, F1));
    assert_absent(&dst.join("d/f2"));
    assert_eq!(read(&dst.join("d/f3")), bytes(3, F3));
    assert_eq!(read(&dst.join("last.txt")), b"last");
    assert_eq!(rep.skipped, 1);
    assert!(
        rep.warnings.iter().any(|w| w.path.ends_with("d/f2")),
        "{:?}",
        rep.warnings
    );
    assert_no_part_files(&dst);
}

#[test]
fn a_failing_rename_removes_the_temp_and_publishes_nothing() {
    let tmp = Tmp::new("f-rename");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let fs = faulty(FaultPlan::new().fail_nth(
        FaultOp::RenameNoreplace,
        1,
        FaultErrno::PermissionDenied,
    ));
    let run = run_faulty(&fs, &a, &dst, &opts());
    run.ok();
    assert_absent(&dst.join("d/f1"));
    assert_eq!(read(&dst.join("d/f2")), bytes(2, F2));
    assert_no_part_files(&dst);
}

#[test]
fn file_systems_without_renameat2_noreplace_fall_back_safely() {
    // No RENAME_NOREPLACE (NFS, FUSE): link+unlink; also without hard links: checked rename.
    for plan in [
        FaultPlan::new().fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(95)),
        FaultPlan::new()
            .fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(95))
            .fail_from(FaultOp::HardLink, 1, FaultErrno::Raw(1)),
    ] {
        let tmp = Tmp::new("f-fallback");
        let dst = tmp.dst();
        std::fs::write(dst.join("last.txt"), b"existing").unwrap();
        let a = tree(&tmp);
        let fs = faulty(plan);
        let mut r = a.reader();
        let run = run_on(
            fs.clone(),
            &mut r,
            &[],
            &dst,
            &opts(),
            ConflictAnswer::Skip,
            ErrorAction::Skip,
        );
        run.ok();
        assert_exact_or_absent_with_old_last(&dst, b"existing");
        assert_eq!(read(&dst.join("d/f2")), bytes(2, F2));
        assert_no_part_files(&dst);
    }
}

#[test]
fn metadata_failures_are_warnings_and_the_data_still_arrives() {
    let tmp = Tmp::new("f-meta");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let fs = faulty(
        FaultPlan::new()
            .fail_from(FaultOp::FileSetMode, 1, FaultErrno::Raw(1))
            .fail_from(FaultOp::FileSetTimes, 1, FaultErrno::Raw(1))
            .fail_from(FaultOp::SetMode, 1, FaultErrno::Raw(1))
            .fail_from(FaultOp::SetTimes, 1, FaultErrno::Raw(1))
            .fail_from(FaultOp::SyncDir, 1, FaultErrno::Io),
    );
    let o = ExtractOpts {
        sync: true,
        ..opts()
    };
    let run = run_faulty(&fs, &a, &dst, &o);
    let rep = run.ok();
    assert_eq!(read(&dst.join("d/f2")), bytes(2, F2));
    assert!(rep.warnings.len() >= 3, "{:?}", rep.warnings);
    assert!(
        rep.warnings.len() <= 8,
        "warnings are deduplicated: {:?}",
        rep.warnings
    );
    assert_no_part_files(&dst);
}

#[test]
fn a_directory_that_cannot_be_created_skips_its_subtree_with_one_warning() {
    let tmp = Tmp::new("f-mkdir");
    let dst = tmp.dst();
    let a = tree(&tmp);
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::Mkdir, 1, FaultErrno::PermissionDenied));
    let run = run_faulty(&fs, &a, &dst, &opts());
    let rep = run.ok();
    assert_absent(&dst.join("d"));
    assert_eq!(read(&dst.join("last.txt")), b"last");
    assert_eq!(rep.warnings.len(), 1, "{:?}", rep.warnings);

    let tmp = Tmp::new("f-mkdir2");
    let dst = tmp.dst();
    let fs = faulty(FaultPlan::new().fail_nth(FaultOp::Mkdir, 1, FaultErrno::NoSpace));
    let run = run_faulty(&fs, &a, &dst, &opts());
    assert!(matches!(run.result, Err(JobError::Io { .. })));
}

#[test]
fn a_hard_link_falls_back_to_a_copy_where_links_are_unsupported() {
    let tmp = Tmp::new("f-hl");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("h.tar", 20_000));
    a.file("orig", b"shared data");
    a.hardlink("link", "orig");
    let fs = faulty(FaultPlan::new().fail_from(FaultOp::HardLink, 1, FaultErrno::Raw(1)));
    let run = run_faulty(&fs, &a, &dst, &opts());
    run.ok();
    assert_eq!(read(&dst.join("link")), b"shared data");
    assert_ne!(
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(dst.join("link")).unwrap()),
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(dst.join("orig")).unwrap())
    );
    assert_no_part_files(&dst);
}

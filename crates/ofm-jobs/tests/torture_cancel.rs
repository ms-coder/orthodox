//! T3.4 torture suite — cancellation (spec F17, ADR-0004 step 7). A cancelled copy/move/delete
//! never leaves a half-written file under the final name and never leaves a `.ofm-part-*` temp;
//! whatever completed before the cancel stays. Cancel points are driven deterministically through
//! a sink that watches the job's own byte counters (see `torture_common::run_cancel_at`).
//! Ignored until T3.1 (copy/move) and T3.2 (delete) land.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod torture_common;

use std::path::PathBuf;
use std::sync::Arc;

use ofm_jobs::event::Outcome;
use ofm_jobs::fs::RealFs;
use ofm_jobs::spec::CopyOpts;
use ofm_testkit::tree::{FileSpec, TreeSpec};

use torture_common::{
    assert_absent_or_exact, assert_no_part_files, copy_spec, move_spec, run_cancel_at, src_dst,
    write_file,
};

fn real() -> Arc<dyn ofm_jobs::Fs> {
    Arc::new(RealFs)
}

const MIB: u64 = 1 << 20;

#[test]
#[ignore = "T3.1"]
fn pre_cancelled_copy_creates_nothing() {
    let (src, dst) = src_dst("cxl-precopy");
    write_file(&src.path().join("a.txt"), b"payload", 0o644, 0);

    let out = torture_common::run_pre_cancelled(
        &copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default()),
        real(),
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(std::fs::read(src.path().join("a.txt")).unwrap(), b"payload");
    if dst.path().exists() {
        assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 0);
        assert_no_part_files(dst.path());
    }
}

#[test]
#[ignore = "T3.1"]
fn cancel_during_planning_stops_before_any_file() {
    // at = 0: the very first progress snapshot (planning phase) cancels.
    let (src, dst) = src_dst("cxl-plan");
    TreeSpec::new()
        .file("a.txt", FileSpec::content(b"aaa"))
        .file("b.txt", FileSpec::content(b"bbb"))
        .build(src.path())
        .unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();

    let (out, sink) = run_cancel_at(
        &copy_spec(
            &[src.path().join("a.txt"), src.path().join("b.txt")],
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        0,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(sink.fired(), "the cancel must have fired");
    assert_eq!(
        std::fs::read_dir(dst.path()).unwrap().count(),
        0,
        "nothing created"
    );
    assert_no_part_files(dst.path());
}

#[test]
#[ignore = "T3.1"]
fn cancel_at_many_byte_offsets_of_a_single_file() {
    const SIZE: u64 = 8 * MIB;
    // A cancel that fires while the job is still planning (at = 0) must stop it before ANY
    // file exists. Mid-file offsets: whether the engine observes the cancel before publishing
    // depends on its data-path granularity (a one-shot FICLONE / whole-file copy has no
    // mid-file checkpoints), so the guaranteed properties are Cancelled-or-Ok, destination
    // absent or byte-exact, no temp left. Between-item cancellation is pinned separately by
    // `cancel_between_files_keeps_completed_work` below.
    let content = vec![0x5Au8; SIZE as usize];

    {
        let (src, dst) = src_dst("cxl-off-planning");
        std::fs::write(src.path().join("big.bin"), &content).unwrap();
        std::fs::create_dir_all(dst.path()).unwrap();

        let (out, sink) = run_cancel_at(
            &copy_spec(
                &[src.path().join("big.bin")],
                dst.path(),
                CopyOpts::default(),
            ),
            real(),
            0,
        );
        assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
        assert!(sink.fired(), "the cancel must have fired");
        assert!(
            !dst.path().join("big.bin").exists(),
            "planning-phase cancel must not create the file"
        );
        assert_no_part_files(dst.path());
    }

    for at in [
        1u64,
        MIB - 1,
        MIB,
        2 * MIB + 123_456,
        4 * MIB,
        6 * MIB,
        7 * MIB,
        SIZE - 1,
        SIZE,
        SIZE + 5 * MIB,
    ] {
        let (src, dst) = src_dst(&format!("cxl-off-{at}"));
        std::fs::write(src.path().join("big.bin"), &content).unwrap();
        std::fs::create_dir_all(dst.path()).unwrap();

        let (out, _sink) = run_cancel_at(
            &copy_spec(
                &[src.path().join("big.bin")],
                dst.path(),
                CopyOpts::default(),
            ),
            real(),
            at,
        );
        // Cancel may arrive too late (Ok) or in time (Cancelled); the file is complete or absent.
        assert!(
            matches!(out, Outcome::Cancelled(_) | Outcome::Ok(_)),
            "at {at}: {out:?}"
        );
        assert_absent_or_exact(&dst.path().join("big.bin"), &content, &format!("at {at}"));
        assert_no_part_files(dst.path());
        assert_eq!(
            std::fs::read(src.path().join("big.bin")).unwrap(),
            content,
            "at {at}: source intact"
        );
    }
}

#[test]
#[ignore = "T3.1"]
fn cancel_between_files_keeps_completed_items() {
    // Three 2 MiB files; cancel once ~3 MiB are through: the first file (or the first two) is
    // complete and published, the rest never appears partially.
    let (src, dst) = src_dst("cxl-multi");
    let mut spec = TreeSpec::new();
    for name in ["one.bin", "two.bin", "three.bin"] {
        spec = spec.file(name, FileSpec::size(2 * MIB));
    }
    spec.build(src.path()).unwrap();
    std::fs::create_dir_all(dst.path()).unwrap();
    let zeros = vec![0u8; 2 * MIB as usize];

    let (out, _sink) = run_cancel_at(
        &copy_spec(
            &[
                src.path().join("one.bin"),
                src.path().join("two.bin"),
                src.path().join("three.bin"),
            ],
            dst.path(),
            CopyOpts::default(),
        ),
        real(),
        3 * MIB,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_absent_or_exact(&dst.path().join("one.bin"), &zeros, "one");
    assert_absent_or_exact(&dst.path().join("two.bin"), &zeros, "two");
    assert_absent_or_exact(&dst.path().join("three.bin"), &zeros, "three");
    // The cancel did something: the last file is never reached.
    assert!(
        !dst.path().join("three.bin").exists(),
        "cancel must stop before the last item"
    );
    assert_no_part_files(dst.path());
    // Sources are never touched by a copy cancel.
    assert_eq!(
        std::fs::read(src.path().join("three.bin")).unwrap(),
        zeros,
        "source intact"
    );
}

#[test]
#[ignore = "T3.1"]
fn cancel_racing_a_copy_from_another_thread_is_always_safe() {
    // Wall-clock (not byte-counter) cancellation: whatever the interleaving, the destination
    // only ever contains complete files and no temp files.
    for seed in 1..=6u64 {
        let (src, dst) = src_dst(&format!("cxl-race-{seed}"));
        let mut spec = TreeSpec::new();
        for i in 0..8 {
            spec = spec.file(format!("f{i}.bin"), FileSpec::size(MIB));
        }
        spec.build(src.path()).unwrap();
        std::fs::create_dir_all(dst.path()).unwrap();

        let cancel = ofm_core::CancelToken::new();
        let names: Vec<PathBuf> = (0..8)
            .map(|i| src.path().join(format!("f{i}.bin")))
            .collect();
        let spec = copy_spec(&names, dst.path(), CopyOpts::default());
        let (sink, _) = ofm_jobs::ScriptedSink::refusing();
        let mut ctx = ofm_jobs::JobCtx::new(
            real(),
            cancel.clone(),
            ofm_jobs::PauseGate::new(),
            Box::new(sink),
            Arc::new(ofm_jobs::SystemClock::new()),
        );
        let worker = std::thread::spawn(move || ofm_jobs::run_job(&spec, &mut ctx));
        std::thread::sleep(std::time::Duration::from_millis(seed));
        cancel.cancel();
        let out = worker.join().unwrap();
        assert!(
            matches!(out, Outcome::Cancelled(_) | Outcome::Ok(_)),
            "seed {seed}: {out:?}"
        );
        let zeros = vec![0u8; MIB as usize];
        for i in 0..8 {
            assert_absent_or_exact(
                &dst.path().join(format!("f{i}.bin")),
                &zeros,
                &format!("seed {seed}"),
            );
        }
        assert_no_part_files(dst.path());
    }
}

#[test]
#[ignore = "T3.1"]
fn cancel_while_paused_returns_promptly() {
    let (src, dst) = src_dst("cxl-pause");
    write_file(&src.path().join("a.txt"), b"paused work", 0o644, 0);
    std::fs::create_dir_all(dst.path()).unwrap();

    let cancel = ofm_core::CancelToken::new();
    let pause = ofm_jobs::PauseGate::new();
    pause.pause();
    let (sink, _) = ofm_jobs::ScriptedSink::refusing();
    let mut ctx = ofm_jobs::JobCtx::new(
        real(),
        cancel.clone(),
        pause.clone(),
        Box::new(sink),
        Arc::new(ofm_jobs::SystemClock::new()),
    );
    let spec = copy_spec(&[src.path().join("a.txt")], dst.path(), CopyOpts::default());
    let worker = std::thread::spawn(move || ofm_jobs::run_job(&spec, &mut ctx));
    // The job sits in its first checkpoint; cancelling while paused must wake and end it.
    cancel.cancel();
    let out = worker.join().expect("worker must not hang while paused");
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(!dst.path().join("a.txt").exists());
    assert_no_part_files(dst.path());
    assert_eq!(
        std::fs::read(src.path().join("a.txt")).unwrap(),
        b"paused work"
    );
}

#[test]
#[ignore = "T3.1"]
fn cancel_of_a_move_keeps_sources_until_items_complete() {
    // Same-filesystem move: cancel lands between renames; whatever is left stays at the source.
    let (src, dst) = src_dst("cxl-move");
    for n in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        write_file(&src.path().join(n), n.as_bytes(), 0o644, 0);
    }
    std::fs::create_dir_all(dst.path()).unwrap();

    let cancel = ofm_core::CancelToken::new();
    let names: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt", "d.txt"]
        .iter()
        .map(|n| src.path().join(n))
        .collect();
    let spec = move_spec(&names, dst.path(), CopyOpts::default());
    let (sink, _) = ofm_jobs::ScriptedSink::refusing();
    let mut ctx = ofm_jobs::JobCtx::new(
        real(),
        cancel.clone(),
        ofm_jobs::PauseGate::new(),
        Box::new(sink),
        Arc::new(ofm_jobs::SystemClock::new()),
    );
    let worker = std::thread::spawn(move || ofm_jobs::run_job(&spec, &mut ctx));
    std::thread::sleep(std::time::Duration::from_millis(1));
    cancel.cancel();
    let out = worker.join().unwrap();
    assert!(
        matches!(out, Outcome::Cancelled(_) | Outcome::Ok(_)),
        "{out:?}"
    );
    // Each item is entirely at the source or entirely at the destination; nothing vanishes.
    for n in ["a.txt", "b.txt", "c.txt", "d.txt"] {
        let content = n.as_bytes();
        assert_absent_or_exact(&src.path().join(n), content, "move-cancel src");
        assert_absent_or_exact(&dst.path().join(n), content, "move-cancel dst");
        assert!(
            src.path().join(n).exists() || dst.path().join(n).exists(),
            "{n} disappeared during a cancelled move"
        );
    }
    assert_no_part_files(dst.path());
}

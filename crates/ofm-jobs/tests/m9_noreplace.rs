//! M9 N4: an NFS client retransmits `link` after a lost reply and gets `EEXIST` for the link it
//! made itself. That must count as success, not as a conflict that leaves the item at the source.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use ofm_core::Location;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{CopyOpts, JobSpec, Outcome, ScriptedSink, run_sync};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;

const EEXIST: i32 = 17;
const EINVAL: i32 = 22;

fn retransmitting() -> Arc<FaultyFs> {
    Arc::new(FaultyFs::over_real(
        FaultPlan::new()
            .fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(EINVAL))
            .lose_reply_nth(FaultOp::HardLink, 1, FaultErrno::Raw(EEXIST)),
    ))
}

/// A move or copy of `<tmp>/s/f` into `<tmp>/d`; both end with `d/f` and nothing asked.
fn run(is_move: bool) -> TempDir {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir(&s).unwrap();
    std::fs::create_dir(&d).unwrap();
    std::fs::write(s.join("f"), b"data").unwrap();
    let sources = vec![Location::Local(s.join("f"))];
    let (dest_dir, opts) = (d.clone(), CopyOpts::default());
    let spec = if is_move {
        JobSpec::Move {
            sources,
            dest_dir,
            opts,
        }
    } else {
        JobSpec::Copy {
            sources,
            dest_dir,
            opts,
        }
    };
    let (sink, rec) = ScriptedSink::refusing();
    let out = run_sync(&spec, retransmitting(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(rec.lock().unwrap().questions.is_empty());
    assert_eq!(std::fs::read(d.join("f")).unwrap(), b"data");
    // No temp file and no numbered twin.
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1);
    t
}

#[test]
fn n4_move_with_a_lost_link_reply_does_not_leave_the_item_at_the_source() {
    let t = run(true);
    assert!(!t.path().join("s/f").exists());
}

#[test]
fn n4_copy_with_a_lost_link_reply_publishes_once() {
    let t = run(false);
    assert!(t.path().join("s/f").exists());
}

/// The checked close reporting `EINTR` is a failed item: nothing is published, the temp file is
/// gone and the source stays (the user may retry or skip).
#[test]
fn an_interrupted_close_publishes_nothing() {
    const EINTR: i32 = 4;
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir(&s).unwrap();
    std::fs::create_dir(&d).unwrap();
    std::fs::write(s.join("f"), b"data").unwrap();
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::FileClose,
        1,
        FaultErrno::Raw(EINTR),
    )));
    let (sink, rec) = ScriptedSink::refusing();
    let spec = JobSpec::Copy {
        sources: vec![Location::Local(s.join("f"))],
        dest_dir: d.clone(),
        opts: CopyOpts::default(),
    };
    let out = run_sync(&spec, fs, sink);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(rec.lock().unwrap().questions.len(), 1);
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 0);
    assert!(s.join("f").exists());
}

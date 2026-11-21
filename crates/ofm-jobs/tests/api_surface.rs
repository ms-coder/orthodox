//! T3.0 baseline for T3.4: proves the public test seams are reachable from an integration test
//! (`FaultyFs` through the `testing` feature, `ScriptedSink`, `run_sync`) and documents how a torture
//! test is wired. T3.4 owns `tests/**` from here on: extend or replace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{Fs, JobSpec, Outcome, ScriptedSink, run_sync};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;

#[test]
fn faulty_fs_is_usable_from_integration_tests() {
    let t = TempDir::new().unwrap();
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::Mkdir,
        1,
        FaultErrno::NoSpace,
    )));
    assert_eq!(
        fs.mkdir(&t.path().join("d"), 0o700)
            .unwrap_err()
            .raw_os_error(),
        Some(28)
    );
    fs.mkdir(&t.path().join("d"), 0o700).unwrap();
    assert_eq!(fs.calls_of(FaultOp::Mkdir), 2);
}

#[test]
fn run_sync_wires_spec_fs_and_sink() {
    let (sink, rec) = ScriptedSink::refusing();
    let out = run_sync(
        &JobSpec::Extract {
            archive: "/x.zip".into(),
            entries: vec![],
            dest_dir: "/y".into(),
        },
        Arc::new(FaultyFs::over_real(FaultPlan::new())),
        sink,
    );
    assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    assert!(rec.lock().unwrap().questions.is_empty());
}

/// The pattern T3.4 follows for every operation (ignored until T3.1 lands).
#[test]
#[ignore = "T3.1"]
fn copy_roundtrip_pattern() {}

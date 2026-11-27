//! FIX-M6M7 #9: matching an entry against the user's selection must not scan the whole selection
//! per entry, and keeps its meaning (outermost selection wins, duplicates are harmless, each
//! selected item lands directly in the destination).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::Duration;

use extract_support::*;
use ofm_core::CancelToken;
use ofm_jobs::progress::SystemClock;
use ofm_jobs::{ConflictAnswer, ErrorAction, JobCtx, PauseGate, RealFs, ScriptedSink};

/// 100 000 selected names. With a scan of the selection per entry this is 1e10 path checks (hours);
/// the watchdog cancels the job after a generous two minutes. The entries are special files, which
/// are matched against the selection like any other and then skipped, so the disk is not involved.
#[test]
fn a_huge_selection_is_matched_in_linear_time() {
    const N: usize = 100_000;
    let tmp = Tmp::new("sel-huge");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("huge.tar", 20_000));
    let selection: Vec<PathBuf> = (0..N).map(|i| PathBuf::from(format!("f{i:06}"))).collect();
    for name in &selection {
        a.other(name.to_str().unwrap());
    }
    let mut r = a.reader();

    let cancel = CancelToken::new();
    let (finished, watch) = mpsc::channel::<()>();
    let token = cancel.clone();
    let watchdog = std::thread::spawn(move || {
        if watch.recv_timeout(Duration::from_secs(120)) == Err(mpsc::RecvTimeoutError::Timeout) {
            token.cancel();
        }
    });
    let (sink, rec) = ScriptedSink::policy(ConflictAnswer::Skip, ErrorAction::Skip, false);
    let ctx = JobCtx::new(
        Arc::new(RealFs),
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    let run = run_ctx_paths(ctx, rec, &mut r, &selection, &dst, &opts());
    drop(finished);
    watchdog.join().unwrap();

    let rep = run.ok();
    assert_eq!(rep.skipped, N as u64);
    assert!(
        rep.warnings
            .iter()
            .all(|w| w.message.contains("special file")),
        "an entry was not matched by its selection"
    );
    assert!(snapshot(&dst).is_empty());
}

#[test]
fn nested_and_duplicate_selections_keep_their_meaning() {
    let tmp = Tmp::new("sel-nested");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("n.zip", 20_000));
    a.file("a/b/c/deep", b"deep");
    a.file("a/b/side", b"side");
    a.file("a/other", b"not selected");
    a.file("x/y/z", b"z");
    a.file("x/yy", b"not selected");
    a.file("top", b"top");
    let mut r = a.reader();
    // `a/b` is outer to `a/b/c` (listed first and twice); `./x//y/` normalises to `x/y`.
    let run = run_plain(
        &mut r,
        &["a/b/c", "a/b", "a/b", "./x//y/", "top"],
        &dst,
        &opts(),
    );
    let rep = run.ok();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    let names: Vec<PathBuf> = snapshot(&dst).into_iter().map(|(p, _)| p).collect();
    let want = ["b", "b/c", "b/c/deep", "b/side", "top", "y", "y/z"];
    assert_eq!(names, want.map(|p| Path::new(p).to_path_buf()));
}

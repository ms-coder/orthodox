//! T8.1b: the read-only content comparison job (F27 content mode).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ofm_core::CancelToken;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    CompareContentOpts, Fs, JobCtx, JobEngine, JobEvent, JobSpec, Outcome, PairResult, PauseGate,
    RealFs, ResultSink, RunMode, ScriptedSink, SkipReason, SystemClock, run_job, run_sync,
};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;

type Collected = Arc<Mutex<Vec<(usize, PairResult)>>>;

fn collector() -> (CompareContentOpts, Collected) {
    let got: Collected = Arc::default();
    let g = Arc::clone(&got);
    let opts = CompareContentOpts {
        on_result: Some(ResultSink::new(move |i, r| {
            g.lock().unwrap().push((i, r.clone()));
        })),
        ..CompareContentOpts::default()
    };
    (opts, got)
}

fn noise(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed) ^ ((i >> 8) as u8))
        .collect()
}

fn put(dir: &Path, name: &str, data: &[u8]) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

fn mkfifo(p: &Path) {
    rustix::fs::mknodat(
        rustix::fs::CWD,
        p,
        rustix::fs::FileType::Fifo,
        rustix::fs::Mode::from_raw_mode(0o600),
        0,
    )
    .unwrap();
}

fn compare_on(fs: Arc<dyn Fs>, pairs: Vec<(PathBuf, PathBuf)>) -> (Outcome, Vec<PairResult>) {
    let (opts, got) = collector();
    let (sink, rec) = ScriptedSink::refusing();
    let out = run_sync(&JobSpec::CompareContent { pairs, opts }, fs, sink);
    assert!(rec.lock().unwrap().questions.is_empty());
    let mut v = got.lock().unwrap().clone();
    // Results arrive in pair order, exactly once each.
    assert!(v.iter().enumerate().all(|(n, (i, _))| n == *i), "{v:?}");
    (out, v.drain(..).map(|(_, r)| r).collect())
}

fn compare(pairs: Vec<(PathBuf, PathBuf)>) -> (Outcome, Vec<PairResult>) {
    compare_on(Arc::new(RealFs), pairs)
}

fn one(a: &Path, b: &Path) -> PairResult {
    let (out, mut r) = compare(vec![(a.to_path_buf(), b.to_path_buf())]);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(r.len(), 1);
    r.remove(0)
}

#[test]
fn equal_files_across_several_chunks() {
    let t = TempDir::new().unwrap();
    let data = noise(700_000, 1);
    let a = put(t.path(), "a", &data);
    let b = put(t.path(), "b", &data);
    assert_eq!(one(&a, &b), PairResult::Equal);
    assert_eq!(one(&b, &a), PairResult::Equal);
}

#[test]
fn differ_in_the_last_byte_of_a_5_mib_file() {
    let t = TempDir::new().unwrap();
    let mut data = noise(5 << 20, 2);
    let a = put(t.path(), "a", &data);
    let last = data.len() - 1;
    data[last] ^= 0x01;
    let b = put(t.path(), "b", &data);
    assert_eq!(one(&a, &b), PairResult::Differs);
    // and the first byte
    data[last] ^= 0x01;
    data[0] ^= 0x80;
    let c = put(t.path(), "c", &data);
    assert_eq!(one(&a, &c), PairResult::Differs);
}

#[test]
fn size_race_files_that_no_longer_have_the_same_size_differ() {
    // The engine reported same-size candidates; by the time we look, one has grown.
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", &noise(100_000, 3));
    let b = put(t.path(), "b", &noise(100_001, 3));
    assert_eq!(one(&a, &b), PairResult::Differs);
}

#[test]
fn empty_files_are_equal() {
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", b"");
    let b = put(t.path(), "b", b"");
    assert_eq!(one(&a, &b), PairResult::Equal);
    // empty against non-empty
    let c = put(t.path(), "c", b"x");
    assert_eq!(one(&a, &c), PairResult::Differs);
}

#[test]
fn sparse_files_are_compared_by_content() {
    let t = TempDir::new().unwrap();
    let make = |name: &str, tail: Option<u8>| {
        let p = t.path().join(name);
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(8 << 20).unwrap();
        if let Some(b) = tail {
            use std::os::unix::fs::FileExt;
            f.write_all_at(&[b], (8 << 20) - 1).unwrap();
        }
        p
    };
    let (a, b, c) = (make("a", None), make("b", None), make("c", Some(7)));
    // A dense file of zeros equals the sparse one.
    let dense = put(t.path(), "dense", &vec![0u8; 8 << 20]);
    assert_eq!(one(&a, &b), PairResult::Equal);
    assert_eq!(one(&a, &dense), PairResult::Equal);
    assert_eq!(one(&a, &c), PairResult::Differs);
}

#[test]
fn several_pairs_report_in_order_and_progress_reaches_the_total() {
    let t = TempDir::new().unwrap();
    let a1 = put(t.path(), "a1", &noise(300_000, 4));
    let b1 = put(t.path(), "b1", &noise(300_000, 4));
    let a2 = put(t.path(), "a2", &noise(300_000, 5));
    let b2 = put(t.path(), "b2", &noise(300_000, 6));
    let (opts, got) = collector();
    let (sink, rec) = ScriptedSink::refusing();
    let out = run_sync(
        &JobSpec::CompareContent {
            pairs: vec![(a1, b1), (a2, b2)],
            opts,
        },
        Arc::new(RealFs),
        sink,
    );
    let Outcome::Ok(report) = out else {
        panic!("{out:?}")
    };
    assert_eq!(report.files_done, 2);
    assert_eq!(report.bytes_done, 600_000);
    let v: Vec<_> = got.lock().unwrap().iter().map(|(_, r)| r.clone()).collect();
    assert_eq!(v, vec![PairResult::Equal, PairResult::Differs]);
    let rec = rec.lock().unwrap();
    let last = rec.progress.last().unwrap();
    assert_eq!((last.files_done, last.files_total), (2, 2));
    assert_eq!((last.bytes_done, last.bytes_total), (600_000, 600_000));
}

#[test]
fn same_inode_is_equal_without_reading() {
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", &noise(1000, 7));
    let l = t.path().join("hard");
    std::fs::hard_link(&a, &l).unwrap();
    assert_eq!(one(&a, &l), PairResult::Equal);
}

#[test]
fn symlinks_compare_targets_and_are_never_followed() {
    let t = TempDir::new().unwrap();
    let file = put(t.path(), "file", b"same");
    let file2 = put(t.path(), "file2", b"same");
    let mk = |name: &str, target: &str| {
        let p = t.path().join(name);
        std::os::unix::fs::symlink(target, &p).unwrap();
        p
    };
    let (l1, l2, l3) = (mk("l1", "file"), mk("l2", "file"), mk("l3", "other"));
    let dangling = mk("dang", "nowhere");
    let dangling2 = mk("dang2", "nowhere");
    assert_eq!(one(&l1, &l2), PairResult::Equal);
    assert_eq!(one(&l1, &l3), PairResult::Differs);
    assert_eq!(one(&dangling, &dangling2), PairResult::Equal);
    // A link against the regular file it points to is not "equal": links are not followed.
    assert_eq!(one(&l1, &file2), PairResult::Differs);
    assert_eq!(one(&file, &l1), PairResult::Differs);
}

#[test]
fn directories_are_skipped() {
    let t = TempDir::new().unwrap();
    let f = put(t.path(), "f", b"x");
    let d = t.path().join("d");
    std::fs::create_dir(&d).unwrap();
    assert_eq!(one(&d, &f), PairResult::Skipped(SkipReason::Directory));
    assert_eq!(one(&f, &d), PairResult::Skipped(SkipReason::Directory));
}

#[test]
fn fifo_is_skipped_and_never_opened() {
    let t = TempDir::new().unwrap();
    let fifo = t.path().join("fifo");
    mkfifo(&fifo);
    let fifo2 = t.path().join("fifo2");
    mkfifo(&fifo2);
    let f = put(t.path(), "f", b"x");
    let pairs = vec![(fifo.clone(), fifo2), (fifo.clone(), f.clone()), (f, fifo)];
    // Run on a thread: a bug here would block in open(2) forever.
    let (tx, rx) = mpsc::channel();
    let faulty = Arc::new(FaultyFs::over_real(FaultPlan::new()));
    let fs: Arc<dyn Fs> = faulty.clone();
    std::thread::spawn(move || {
        let _ = tx.send(compare_on(fs, pairs));
    });
    let (out, results) = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("compare blocked on a FIFO");
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(
        results,
        vec![PairResult::Skipped(SkipReason::SpecialFile); 3]
    );
    assert_eq!(
        faulty.calls_of(FaultOp::OpenRead),
        0,
        "FIFO must not be opened"
    );
}

#[test]
fn fifo_swapped_in_after_lstat_is_refused_by_open_not_blocking() {
    // `RealFs::open_read` is O_NONBLOCK + fstat: calling it directly on a FIFO must return.
    let t = TempDir::new().unwrap();
    let fifo = t.path().join("fifo");
    mkfifo(&fifo);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let r = RealFs.open_read(&fifo).map(|_| ()).map_err(|e| e.kind());
        let _ = tx.send(r);
    });
    let r = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("open blocked");
    assert!(r.is_err());
}

#[test]
fn unreadable_file_is_an_error_entry_not_a_job_failure() {
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", b"secret");
    let b = put(t.path(), "b", b"secret");
    let ok1 = put(t.path(), "ok1", b"fine");
    let ok2 = put(t.path(), "ok2", b"fine");
    // EACCES injected on the very first open: portable (also for root).
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::OpenRead,
        1,
        FaultErrno::PermissionDenied,
    )));
    let (out, results) = compare_on(fs, vec![(a.clone(), b), (ok1, ok2)]);
    let Outcome::Ok(report) = out else {
        panic!("{out:?}")
    };
    assert_eq!(report.warnings.len(), 1);
    assert_eq!(report.warnings[0].path, a);
    let PairResult::Error(e) = &results[0] else {
        panic!("{results:?}")
    };
    assert_eq!((e.path.as_path(), e.os_error), (a.as_path(), Some(13)));
    assert_eq!(results[1], PairResult::Equal);
}

#[test]
fn chmod_000_file_is_an_error_entry() {
    use std::os::unix::fs::PermissionsExt;
    if rustix::process::geteuid().is_root() {
        return; // root reads everything
    }
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", b"secret");
    let b = put(t.path(), "b", b"secret");
    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o000)).unwrap();
    let r = one(&a, &b);
    std::fs::set_permissions(&b, std::fs::Permissions::from_mode(0o600)).unwrap();
    let PairResult::Error(e) = r else {
        panic!("{r:?}")
    };
    assert_eq!(e.path, b);
    assert_eq!(e.os_error, Some(13));
}

#[test]
fn missing_file_is_an_error_entry() {
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", b"x");
    let gone = t.path().join("gone");
    let r = one(&a, &gone);
    assert!(matches!(&r, PairResult::Error(e) if e.path == gone && e.os_error == Some(2)));
}

#[test]
fn faulty_read_error_is_reported_per_pair_and_later_pairs_still_run() {
    let t = TempDir::new().unwrap();
    let data = noise(50_000, 9);
    let a = put(t.path(), "a", &data);
    let b = put(t.path(), "b", &data);
    let c = put(t.path(), "c", &data);
    let d = put(t.path(), "d", &data);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::FileRead,
        1,
        FaultErrno::Io,
    )));
    let (out, results) = compare_on(fs, vec![(a.clone(), b), (c, d)]);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let PairResult::Error(e) = &results[0] else {
        panic!("{results:?}")
    };
    assert_eq!((e.path.as_path(), e.os_error), (a.as_path(), Some(5)));
    assert_eq!(results[1], PairResult::Equal);
}

#[test]
fn read_error_on_the_right_side_names_the_right_path() {
    let t = TempDir::new().unwrap();
    let data = noise(50_000, 9);
    let a = put(t.path(), "a", &data);
    let b = put(t.path(), "b", &data);
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_nth(
        FaultOp::FileRead,
        2,
        FaultErrno::Io,
    )));
    let (_, results) = compare_on(fs, vec![(a, b.clone())]);
    assert!(
        matches!(&results[0], PairResult::Error(e) if e.path == b),
        "{results:?}"
    );
}

#[test]
fn non_utf8_names_work() {
    let t = TempDir::new().unwrap();
    let na = OsStr::from_bytes(b"\xff\xfe-left.bin");
    let nb = OsStr::from_bytes(b"\xc3\x28-right.bin");
    let a = t.path().join(na);
    let b = t.path().join(nb);
    let c = t.path().join(OsStr::from_bytes(b"\x80-diff.bin"));
    std::fs::write(&a, noise(20_000, 1)).unwrap();
    std::fs::write(&b, noise(20_000, 1)).unwrap();
    std::fs::write(&c, noise(20_000, 2)).unwrap();
    let (_, r) = compare(vec![(a.clone(), b), (a, c)]);
    assert_eq!(r, vec![PairResult::Equal, PairResult::Differs]);
}

#[test]
fn compare_never_writes() {
    // Every mutating Fs call is fatal: the job must only read.
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", &noise(10_000, 1));
    let b = put(t.path(), "b", &noise(10_000, 1));
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new()));
    let (_, r) = compare_on(fs.clone(), vec![(a, b)]);
    assert_eq!(r, vec![PairResult::Equal]);
    for op in [
        FaultOp::CreateExcl,
        FaultOp::Mkdir,
        FaultOp::RenameNoreplace,
        FaultOp::RenameReplace,
        FaultOp::Unlink,
        FaultOp::Rmdir,
        FaultOp::Symlink,
        FaultOp::HardLink,
        FaultOp::SetMode,
        FaultOp::SetTimes,
        FaultOp::SetOwner,
        FaultOp::FileWrite,
    ] {
        assert_eq!(fs.calls_of(op), 0, "{op:?}");
    }
    assert_eq!(fs.calls_of(FaultOp::OpenRead), 2);
}

fn cancellable_ctx(sink: impl ofm_jobs::JobSink + 'static) -> (JobCtx, CancelToken) {
    let token = CancelToken::new();
    let ctx = JobCtx::new(
        Arc::new(RealFs),
        token.clone(),
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    (ctx, token)
}

#[test]
fn cancel_between_files_keeps_delivered_results_and_reports_cancelled() {
    let t = TempDir::new().unwrap();
    let data = noise(100_000, 3);
    let mut pairs = Vec::new();
    for i in 0..5 {
        pairs.push((
            put(t.path(), &format!("a{i}"), &data),
            put(t.path(), &format!("b{i}"), &data),
        ));
    }
    let (sink, _) = ScriptedSink::refusing();
    let (mut ctx, token) = cancellable_ctx(sink);
    let seen = Arc::new(AtomicUsize::new(0));
    let (s2, t2) = (Arc::clone(&seen), token.clone());
    let opts = CompareContentOpts {
        on_result: Some(ResultSink::new(move |i, _| {
            s2.fetch_add(1, Ordering::SeqCst);
            if i == 1 {
                t2.cancel();
            }
        })),
        ..CompareContentOpts::default()
    };
    let out = run_job(&JobSpec::CompareContent { pairs, opts }, &mut ctx);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(seen.load(Ordering::SeqCst), 2);
}

#[test]
fn cancel_before_start_reports_cancelled_without_results() {
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", b"x");
    let b = put(t.path(), "b", b"x");
    let (sink, _) = ScriptedSink::refusing();
    let (mut ctx, token) = cancellable_ctx(sink);
    token.cancel();
    let (opts, got) = collector();
    let out = run_job(
        &JobSpec::CompareContent {
            pairs: vec![(a, b)],
            opts,
        },
        &mut ctx,
    );
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert!(got.lock().unwrap().is_empty());
}

#[test]
fn panicking_result_callback_becomes_a_failed_outcome() {
    let t = TempDir::new().unwrap();
    let a = put(t.path(), "a", b"x");
    let b = put(t.path(), "b", b"x");
    let opts = CompareContentOpts {
        on_result: Some(ResultSink::new(|_, _| panic!("boom"))),
        ..CompareContentOpts::default()
    };
    let (sink, _) = ScriptedSink::refusing();
    let out = run_sync(
        &JobSpec::CompareContent {
            pairs: vec![(a, b)],
            opts,
        },
        Arc::new(RealFs),
        sink,
    );
    assert!(
        matches!(&out, Outcome::Failed { error, .. } if error.contains("boom")),
        "{out:?}"
    );
}

#[test]
fn spec_metadata() {
    let (opts, _) = collector();
    let spec = JobSpec::CompareContent {
        pairs: vec![("/a".into(), "/b".into()); 3],
        opts,
    };
    assert!(spec.is_read_only());
    assert_eq!(spec.title(), "Compare contents of 3 files");
}

#[test]
fn runs_through_the_engine_and_streams_results_before_finish() {
    let t = TempDir::new().unwrap();
    let data = noise(200_000, 5);
    let mut pairs = Vec::new();
    for i in 0..4 {
        let mut other = data.clone();
        if i % 2 == 1 {
            other[100] ^= 1;
        }
        pairs.push((
            put(t.path(), &format!("l{i}"), &data),
            put(t.path(), &format!("r{i}"), &other),
        ));
    }
    let (tx, rx) = mpsc::channel::<(usize, PairResult)>();
    let tx = Mutex::new(tx);
    let opts = CompareContentOpts {
        on_result: Some(ResultSink::new(move |i, r| {
            let _ = tx.lock().unwrap().send((i, r.clone()));
        })),
        ..CompareContentOpts::default()
    };
    let engine = JobEngine::new();
    let events = engine.events();
    let id = engine.submit(JobSpec::CompareContent { pairs, opts }, RunMode::Parallel);
    let deadline = Instant::now() + Duration::from_secs(30);
    let outcome = loop {
        assert!(Instant::now() < deadline, "timeout");
        match events.try_recv() {
            Ok(JobEvent::Finished { id: fid, outcome }) => {
                assert_eq!(fid, id);
                break outcome;
            }
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    assert!(matches!(outcome, Outcome::Ok(_)), "{outcome:?}");
    let got: Vec<_> = rx.try_iter().collect();
    assert_eq!(
        got,
        vec![
            (0, PairResult::Equal),
            (1, PairResult::Differs),
            (2, PairResult::Equal),
            (3, PairResult::Differs)
        ]
    );
}

//! T3.6a regression tests for review findings m5 (a FIFO swapped in between `lstat` and `open`
//! blocked `open_read` forever, so even cancel could not end the job) and m6 (EPERM, ETXTBSY and
//! an EIO before the first byte from FICLONE/`copy_file_range` must fall back to read/write, like
//! coreutils, instead of failing the item).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t36a_support;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ConflictAnswer, ErrorAction, Fs, RealFs};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t36a_support::*;

const EPERM: i32 = 1;
const EIO: i32 = 5;
const ETXTBSY: i32 = 26;

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

/// Run `f` on a thread; fail the test if it does not finish in time. `unblock` runs on timeout
/// so the stuck thread can end (opens the FIFO for writing).
fn within<T: Send + 'static>(
    secs: u64,
    unblock: impl FnOnce(),
    f: impl FnOnce() -> T + Send + 'static,
) -> T {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(f());
    });
    match rx.recv_timeout(Duration::from_secs(secs)) {
        Ok(v) => v,
        Err(_) => {
            unblock();
            panic!("blocked for more than {secs} s");
        }
    }
}

fn release_fifo(p: &Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(0o4000) // O_NONBLOCK
        .open(p);
}

#[test]
fn m5_open_read_of_a_fifo_does_not_block() {
    let t = TempDir::new().unwrap();
    let fifo = t.path().join("fifo");
    mkfifo(&fifo);
    let (p1, p2) = (fifo.clone(), fifo.clone());
    let res = within(
        5,
        || release_fifo(&p2),
        move || {
            (
                RealFs.open_read(&p1).map(|_| ()).map_err(|e| e.kind()),
                RealFs
                    .open_read_follow(&p1)
                    .map(|_| ())
                    .map_err(|e| e.kind()),
            )
        },
    );
    assert!(res.0.is_err(), "a FIFO is not a copy source");
    assert!(res.1.is_err(), "a FIFO is not a copy source");
}

#[test]
fn m5_open_read_still_reads_regular_files_blocking() {
    let t = TempDir::new().unwrap();
    let p = t.path().join("f");
    std::fs::write(&p, b"abc").unwrap();
    let mut f = RealFs.open_read(&p).unwrap();
    let mut buf = [0u8; 8];
    assert_eq!(f.read(&mut buf).unwrap(), 3);
    assert_eq!(&buf[..3], b"abc");
    // O_NONBLOCK was cleared again after the open.
    let fl = rustix::fs::fcntl_getfl(f.as_fd()).unwrap();
    assert!(!fl.contains(rustix::fs::OFlags::NONBLOCK));
}

#[test]
fn m5_fifo_swapped_in_after_lstat_does_not_hang_the_copy() {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    let victim = s.join("f");
    std::fs::write(&victim, b"regular").unwrap();
    std::fs::write(s.join("g"), b"next").unwrap();
    let swapped = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&swapped);
    let v = victim.clone();
    // Between the engine's lstat (regular file) and its open, f becomes a FIFO.
    let fs = Arc::new(HookFs::new().hook(move |op, p, _| {
        if op == "open_read" && p == v.as_path() && !flag.swap(true, Ordering::SeqCst) {
            std::fs::remove_file(&v).unwrap();
            mkfifo(&v);
        }
        None
    }));
    let spec = copy_spec(&[victim.clone(), s.join("g")], &d, opts());
    let (o, rec) = within(
        10,
        || release_fifo(&victim),
        move || run_policy(&spec, fs, ConflictAnswer::Skip, ErrorAction::Skip),
    );
    assert!(swapped.load(Ordering::SeqCst));
    assert!(o.is_success(), "{o:?}");
    assert_eq!(error_questions(&rec).len(), 1, "the FIFO is reported");
    assert_eq!(names(&d), vec!["g"]);
    assert_no_temps(&d);
}

fn copy_with(plan: FaultPlan, size: usize) -> (ofm_jobs::Outcome, Rec, Arc<FaultyFs>, TempDir) {
    let t = TempDir::new().unwrap();
    let (s, d) = (t.path().join("s"), t.path().join("d"));
    std::fs::create_dir_all(&s).unwrap();
    std::fs::create_dir_all(&d).unwrap();
    let data: Vec<u8> = (0..size).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(s.join("f"), &data).unwrap();
    let fs = Arc::new(FaultyFs::over_real(plan));
    let (o, rec) = run_policy(
        &copy_spec(&[s.join("f")], &d, opts()),
        fs.clone(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    if o.is_success() && d.join("f").exists() {
        assert_eq!(read(&d.join("f")), data, "copy is exact");
    }
    (o, rec, fs, t)
}

#[test]
fn m6_eperm_and_etxtbsy_from_kernel_copies_fall_back_to_read_write() {
    for errno in [EPERM, ETXTBSY] {
        let plan = FaultPlan::new()
            .fail_from(FaultOp::TryCloneFrom, 1, FaultErrno::Raw(errno))
            .fail_from(FaultOp::CopyRangeFrom, 1, FaultErrno::Raw(errno));
        let (o, rec, fs, t) = copy_with(plan, 3 << 20);
        assert!(o.is_success(), "errno {errno}: {o:?}");
        assert!(questions(&rec).is_empty(), "errno {errno}: no question");
        assert!(t.path().join("d/f").exists(), "errno {errno}");
        assert!(fs.calls_of(FaultOp::FileWrite) >= 3, "read/write tier used");
    }
}

#[test]
fn m6_eio_before_the_first_byte_falls_back() {
    let plan = FaultPlan::new()
        .fail_nth(FaultOp::TryCloneFrom, 1, FaultErrno::Io)
        .fail_nth(FaultOp::CopyRangeFrom, 1, FaultErrno::Io);
    let (o, rec, _fs, t) = copy_with(plan, 3 << 20);
    assert!(o.is_success(), "{o:?}");
    assert!(questions(&rec).is_empty());
    assert!(t.path().join("d/f").exists());
}

#[test]
fn m6_eio_after_kernel_copied_data_is_a_real_error() {
    let plan = FaultPlan::new().fail_nth(FaultOp::CopyRangeFrom, 2, FaultErrno::Io);
    let (o, rec, fs, t) = copy_with(plan, 3 << 20);
    if fs.calls_of(FaultOp::CopyRangeFrom) < 2 {
        eprintln!("SKIP: copy_file_range not used on this file system");
        return;
    }
    assert!(o.is_success(), "{o:?}");
    let errs = error_questions(&rec);
    assert_eq!(errs.len(), 1, "{errs:?}");
    assert_eq!(errs[0].1, Some(EIO));
    assert!(!t.path().join("d/f").exists(), "skipped, temp removed");
    assert!(names(&t.path().join("d")).is_empty());
}

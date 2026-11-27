//! FIX-M6M7 #5: anchored extraction keeps a bounded number of descriptors on wide and deep
//! archives. Its own test binary with a single test, so the process-wide count is not disturbed.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use extract_support::*;

fn open_fds() -> usize {
    std::fs::read_dir("/proc/self/fd").unwrap().count()
}

#[test]
fn wide_and_deep_archives_do_not_accumulate_descriptors() {
    let tmp = Tmp::new("fds");
    let dst = tmp.dst();
    let mut a = MemArchive::new(&tmp.archive_file("w.zip", 100_000));
    for i in 0..400 {
        a.file(format!("w{i:03}/sub/f"), b"x");
    }
    let deep = vec!["n"; 200].join("/");
    a.file(format!("{deep}/leaf"), b"leaf");
    // Back to early directories (reopened after eviction), and hard links across them.
    for i in 0..50 {
        a.file(format!("w{i:03}/sub/g"), b"y");
        a.hardlink(&format!("w{:03}/h{i}", 399 - i), &format!("w{i:03}/sub/g"));
    }
    a.file(format!("{deep}/leaf2"), b"leaf2");
    let mut r = a.reader();
    let before = open_fds();
    let max = Arc::new(AtomicUsize::new(0));
    let m = Arc::clone(&max);
    r.on_read = Some(Arc::new(move |_| {
        m.fetch_max(open_fds(), Ordering::SeqCst);
    }));
    let run = run_plain(&mut r, &[], &dst, &opts());
    let rep = run.ok();
    assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
    assert_eq!(read(&dst.join(format!("{deep}/leaf2"))), b"leaf2");
    assert_eq!(read(&dst.join("w399/h0")), b"y");
    assert_eq!(read(&dst.join("w000/sub/g")), b"y");
    let peak = max.load(Ordering::SeqCst);
    assert!(peak > before, "{peak} {before}");
    assert!(peak <= before + 40, "peak {peak}, before {before}");
    assert_eq!(open_fds(), before, "descriptors leaked");
}

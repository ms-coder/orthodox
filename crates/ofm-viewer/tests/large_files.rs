//! Large-file behaviour: instant open of a huge sparse file, fast first-screen read, and a
//! bench-style indexing run. The 1 GiB benchmark is `#[ignore]`d — run explicitly with
//! `cargo test -p ofm-viewer -- --ignored`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::atomic::AtomicU64;
use std::time::Instant;

use ofm_core::CancelToken;
use ofm_viewer::{Document, SourceKind, index_document};

const SPARSE_LEN: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB
const FIRST_SCREEN: usize = 64 * 1024;

#[test]
fn sparse_10gib_opens_and_reads_first_screen_fast() {
    let dir = match tempfile::tempdir() {
        Ok(d) => d,
        Err(_) => return,
    };
    let p = dir.path().join("sparse.bin");
    let f = match std::fs::File::create(&p) {
        Ok(f) => f,
        Err(_) => return,
    };
    if f.set_len(SPARSE_LEN).is_err() {
        eprintln!("filesystem does not support sparse files; skipping");
        return;
    }
    // Drop a recognizable pattern at the start so the read is real work.
    drop(f);
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .and_then(|mut f| std::io::Write::write_all(&mut f, b"header\n"))
        .ok();

    let t0 = Instant::now();
    let doc = Document::open(&p).unwrap();
    let open_ms = t0.elapsed().as_millis();
    assert_eq!(doc.source_kind(), SourceKind::Mapped);
    assert_eq!(doc.len(), SPARSE_LEN);
    assert!(
        open_ms < 500,
        "open took {open_ms} ms (spec: instant, must not read the file)"
    );

    let t1 = Instant::now();
    let screen = doc.read_range(0, FIRST_SCREEN).unwrap();
    let read_ms = t1.elapsed().as_millis();
    assert_eq!(screen.len(), FIRST_SCREEN);
    assert_eq!(&screen[..7], b"header\n");
    assert!(
        read_ms < 500,
        "first-screen read took {read_ms} ms (spec: < 100 ms; generous bound for CI)"
    );
}

#[test]
fn truncated_sparse_file_does_not_fault() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("trunc.bin");
    let f = std::fs::File::create(&p).unwrap();
    f.set_len(SPARSE_LEN).unwrap();
    drop(f);
    let doc = Document::open(&p).unwrap();
    // Truncate to 1 byte after mapping: reads must short-read, never SIGBUS.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(1)
        .unwrap();
    let got = doc.read_range(0, FIRST_SCREEN).unwrap();
    assert!(
        got.len() <= 1,
        "expected short read, got {} bytes",
        got.len()
    );
}

/// Bench-style: index a 1 GiB sparse file of short lines. Ignored in normal runs.
#[test]
#[ignore = "bench: indexes a 1 GiB file"]
fn bench_index_1gib() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.txt");
    let f = std::fs::File::create(&p).unwrap();
    const GIB: u64 = 1024 * 1024 * 1024;
    f.set_len(GIB).unwrap();
    drop(f);
    // Materialize one line every 4 KiB so the file is mostly holes but indexable.
    let line = b"The quick brown fox jumps over the lazy dog.\n"; // 46 bytes
    {
        let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
        let mut off = 0u64;
        while off < GIB {
            use std::os::unix::fs::FileExt;
            f.write_all_at(line, off).unwrap();
            off += 4096;
        }
    }
    let doc = Document::open(&p).unwrap();
    let progress = AtomicU64::new(0);
    let t0 = Instant::now();
    let idx = index_document(&doc, &CancelToken::new(), &progress).unwrap();
    let secs = t0.elapsed().as_secs_f64();
    eprintln!(
        "indexed 1 GiB in {secs:.2}s: {} lines, {:.0} MiB/s",
        idx.line_count(),
        1024.0 / secs
    );
    assert!(idx.is_complete());
}

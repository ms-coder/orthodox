//! Listing benchmark: 100k entries, first batch and full listing (target < 50 ms / < 1 s).

use std::path::Path;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use ofm_core::{CancelToken, Location};
use ofm_vfs::{ListEvent, Vfs, local::LocalVfs};

fn populate(dir: &Path, n: usize) {
    for i in 0..n {
        // Errors here would invalidate the benchmark; abort loudly.
        if let Err(e) = std::fs::write(dir.join(format!("file-{i:06}.dat")), b"") {
            panic!("fixture creation failed: {e}");
        }
    }
}

fn bench(c: &mut Criterion) {
    let tmp = match tempfile::tempdir() {
        Ok(t) => t,
        Err(e) => panic!("tempdir: {e}"),
    };
    populate(tmp.path(), 100_000);
    let loc = Location::Local(tmp.path().to_path_buf());
    let vfs = LocalVfs::new();

    // One-off report of the two numbers the task cares about.
    let start = Instant::now();
    let mut first = None;
    vfs.list(&loc, &CancelToken::new(), &mut |ev| {
        if matches!(ev, ListEvent::Batch(_)) && first.is_none() {
            first = Some(start.elapsed());
        }
    });
    eprintln!(
        "100k listing: first batch {first:?}, full {:?}",
        start.elapsed()
    );

    c.bench_function("list_100k_full", |b| {
        b.iter(|| {
            let mut n = 0usize;
            vfs.list(&loc, &CancelToken::new(), &mut |ev| {
                if let ListEvent::Batch(v) = ev {
                    n += v.len();
                }
            });
            n
        });
    });
}

criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(10);
    targets = bench
}
criterion_main!(benches);

//! Criterion benchmark for PLAN T1.1: sorting 100k entries must stay well under 50 ms.
//! `cargo bench -p ofm-core` runs it.

use std::hint::black_box;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use ofm_core::entry::{Entry, EntryKind};
use ofm_core::panel::{PanelModel, SortKey, SortSpec};

fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    *state
}

/// 100k synthetic entries with realistic-ish names, sizes and mtimes (no fs access).
fn make_entries(n: usize) -> Vec<Entry> {
    let mut state: u64 = 0x853c_49e6_748f_ea9b;
    (0..n)
        .map(|i| {
            let r = lcg(&mut state);
            let ext = match r % 5 {
                0 => "txt",
                1 => "rs",
                2 => "png",
                3 => "md",
                _ => "tar.gz",
            };
            // Bijective scramble of 0..n: arrival order is not the sorted order (like readdir order).
            let name = format!("file_{:06}.{ext}", (i * 37_139) % n);
            let kind = if r.is_multiple_of(10) {
                EntryKind::Dir
            } else {
                EntryKind::File
            };
            let mut e = Entry::new(name, kind);
            e.size = r % (1 << 40);
            e.mtime_ns = i128::from(r % (1 << 34));
            e
        })
        .collect()
}

/// Full re-sort of 100k entries (what `set_sort` does): the T1.1 budget case. One benchmark per
/// key; each iteration alternates between two specs so every call really re-sorts.
fn bench_resort_100k(c: &mut Criterion) {
    let mut m = PanelModel::new();
    m.begin_load(false);
    m.apply_batch(make_entries(100_000));
    m.finish_load();
    for (label, key) in [
        ("name", SortKey::Name),
        ("ext", SortKey::Ext),
        ("size", SortKey::Size),
        ("mtime", SortKey::Mtime),
    ] {
        let specs = [
            SortSpec {
                key,
                reverse: false,
                dirs_first: true,
            },
            SortSpec {
                key,
                reverse: true,
                dirs_first: true,
            },
        ];
        // Start from a different key so the first iteration also re-sorts.
        let mut i = 0usize;
        c.bench_function(&format!("panel_resort_100k_{label}"), |b| {
            b.iter(|| {
                i = (i + 1) % specs.len();
                m.set_sort(black_box(specs[i]))
            })
        });
    }
}

/// Calibration: sorting 100k random `u64` on this machine (lower bound for any 100k sort here).
fn bench_baseline_u64(c: &mut Criterion) {
    let mut state = 1u64;
    let data: Vec<u64> = (0..100_000).map(|_| lcg(&mut state)).collect();
    c.bench_function("baseline_sort_100k_u64", |b| {
        b.iter_batched(
            || data.clone(),
            |mut v| {
                v.sort_unstable();
                v
            },
            BatchSize::LargeInput,
        )
    });
}

/// Streaming 100k entries in 512-entry batches (the VFS batch size): must not degrade into
/// repeated full re-sorts.
fn bench_stream_100k(c: &mut Criterion) {
    let entries = make_entries(100_000);
    c.bench_function("panel_stream_100k_batches_of_512", |b| {
        b.iter_batched(
            || entries.clone(),
            |entries| {
                let mut m = PanelModel::new();
                m.begin_load(false);
                for chunk in entries.chunks(512) {
                    m.apply_batch(chunk.to_vec());
                }
                m.finish_load();
            },
            BatchSize::PerIteration,
        )
    });
}

criterion_group!(
    benches,
    bench_baseline_u64,
    bench_resort_100k,
    bench_stream_100k
);
criterion_main!(benches);

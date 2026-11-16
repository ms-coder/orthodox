//! Property tests for ofm-viewer (proptest).
//!
//! - Random bytes never panic in decode / hex / line-index / search.
//! - Search finds a needle placed at every offset around a 4 MiB chunk boundary.
//! - `LineIndex::line_start` agrees with a naive reference split.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ofm_core::CancelToken;
use ofm_viewer::encoding::{Encoding, align_down_to_boundary, align_up_to_boundary, decode_range};
use ofm_viewer::hex::hex_rows;
use ofm_viewer::line_index::{CHUNK_SIZE, LineIndex};
use ofm_viewer::search::{Needle, PreparedNeedle, SearchOptions, find_next, find_prev};
use ofm_viewer::{Document, SourceKind};
use proptest::prelude::*;

fn naive_starts(data: &[u8]) -> Vec<u64> {
    let mut starts = vec![0u64];
    let mut i = 0usize;
    while i < data.len() {
        match data[i] {
            b'\n' => {
                i += 1;
                if i < data.len() {
                    starts.push(i as u64);
                }
            }
            b'\r' => {
                if i + 1 < data.len() && data[i + 1] == b'\n' {
                    i += 2;
                } else {
                    i += 1;
                }
                if i < data.len() {
                    starts.push(i as u64);
                }
            }
            _ => i += 1,
        }
    }
    if data.is_empty() {
        starts.clear();
    }
    starts
}

fn doc_from(data: &[u8]) -> (tempfile::TempDir, Document) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("prop.bin");
    std::fs::write(&p, data).unwrap();
    let doc = Document::open(&p).unwrap();
    (dir, doc)
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn random_bytes_never_panic(
        data in prop::collection::vec(any::<u8>(), 0..8192),
        enc_i in 0usize..7,
        cut in 0usize..8192,
    ) {
        let enc = Encoding::ALL[enc_i % Encoding::ALL.len()];
        let cut = cut.min(data.len());
        let _ = decode_range(enc, &data[..cut]);
        let _ = decode_range(enc, &data);
        let _ = align_down_to_boundary(enc, &data, cut);
        let _ = align_up_to_boundary(enc, &data, cut);
        let _ = hex_rows(&data, 0, 0, data.len() / 16 + 2);
        let mut idx = LineIndex::new();
        for c in data.chunks(257) {
            idx.index_chunk(c);
        }
        idx.finish();
        for n in 0..idx.line_count() {
            let _ = idx.line_start(n);
            let _ = idx.line_range(n);
        }
        let (_dir, doc) = doc_from(&data);
        let needle_len = cut.min(4).min(data.len());
        let needle = PreparedNeedle::Bytes(vec![data[..needle_len].to_vec()]);
        let _ = find_next(&doc, &needle, 0, &SearchOptions::strict(), &CancelToken::new());
    }

    #[test]
    fn line_index_matches_naive_split(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        for chunk in [1usize, 3, 7, 64, 1024] {
            let mut idx = LineIndex::new();
            for c in data.chunks(chunk) {
                idx.index_chunk(c);
            }
            idx.finish();
            let want = naive_starts(&data);
            prop_assert_eq!(idx.line_count(), want.len(), "chunk={}", chunk);
            for (n, s) in want.iter().enumerate() {
                prop_assert_eq!(idx.line_start(n), Some(*s), "chunk={} line={}", chunk, n);
            }
        }
    }

    /// A needle placed at every offset around the 4 MiB indexing (and 1 MiB search) chunk
    /// boundaries must be found exactly there.
    #[test]
    fn search_finds_needle_at_chunk_boundaries(off in (CHUNK_SIZE as u64 - 40)..(CHUNK_SIZE as u64 + 40)) {
        let needle = b"UNIQUENEEDLE";
        let mut data = vec![b'x'; CHUNK_SIZE + 1024];
        let off = off as usize;
        data[off..off + needle.len()].copy_from_slice(needle);
        let (_dir, doc) = doc_from(&data);
        let n = Needle::Text("uniqueneedle".into()) // also exercises case folding
            .prepare(Encoding::Utf8, &SearchOptions::default())
            .unwrap();
        let m = find_next(&doc, &n, 0, &SearchOptions::default(), &CancelToken::new())
            .unwrap()
            .unwrap();
        prop_assert_eq!(m.offset, off as u64);
        // Backward from just past the needle finds it too.
        let m = find_prev(&doc, &n, off as u64 + 1, &SearchOptions::default(), &CancelToken::new())
            .unwrap()
            .unwrap();
        prop_assert_eq!(m.offset, off as u64);
    }

    #[test]
    fn hex_rows_cover_all_bytes(data in prop::collection::vec(any::<u8>(), 0..4096)) {
        let rows = hex_rows(&data, 0, 0, data.len() / 16 + 2);
        let flat: Vec<u8> = rows.iter().flat_map(|r| r.bytes.iter().copied()).collect();
        prop_assert_eq!(flat, data);
        for r in &rows {
            prop_assert_eq!(r.text.chars().count(), r.bytes.len());
        }
    }
}

#[test]
fn empty_document_is_mapped_or_buffered_but_never_panics() {
    let (_dir, doc) = doc_from(b"");
    assert_eq!(doc.len(), 0);
    let n = Needle::Text("x".into())
        .prepare(Encoding::Utf8, &SearchOptions::strict())
        .unwrap();
    assert!(
        find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
            .unwrap()
            .is_none()
    );
}

#[test]
fn source_kind_is_sane() {
    let (_dir, doc) = doc_from(b"hello");
    assert_eq!(doc.source_kind(), SourceKind::Mapped);
}

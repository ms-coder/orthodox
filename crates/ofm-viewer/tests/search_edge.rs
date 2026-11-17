//! Search depth tests complementing `edge_cases.rs` (task T5.3a, spec F21):
//! very long needles straddling the 1 MiB search window, and wrap-around /
//! from-past-EOF semantics. Everything else search-related lives in
//! `edge_cases.rs` and `property.rs` — not duplicated here.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ofm_core::CancelToken;
use ofm_viewer::search::{Needle, PreparedNeedle, SEARCH_CHUNK, SearchError, SearchOptions};
use ofm_viewer::{Document, Encoding, find_next, find_prev};

fn doc_with(data: &[u8]) -> (tempfile::TempDir, Document) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("search-edge.bin");
    std::fs::write(&p, data).unwrap();
    (dir, Document::open(&p).unwrap())
}

fn prep(needle: Needle, opts: &SearchOptions) -> PreparedNeedle {
    needle.prepare(Encoding::Utf8, opts).unwrap()
}

fn no_cancel() -> CancelToken {
    CancelToken::new()
}

#[test]
fn long_needle_straddling_search_chunk_boundary() {
    // SEARCH_CHUNK is 1 MiB; a 400-byte needle starting 200 bytes before the boundary
    // spans it. The overlap guarantees boundary-spanning matches for needles up to
    // overlap length — exercise text, hex and regex needles, forward and backward.
    let needle: Vec<u8> = {
        let mut v = b"STRADDLE[".to_vec();
        v.extend(std::iter::repeat_n(b'~', 382));
        v.push(b']');
        v
    };
    assert_eq!(needle.len(), 392);
    let start = SEARCH_CHUNK - 200;
    let mut data = vec![b'x'; SEARCH_CHUNK * 2];
    data[start..start + needle.len()].copy_from_slice(&needle);
    let (_d, doc) = doc_with(&data);

    let text = String::from_utf8(needle.clone()).unwrap();
    let strict = SearchOptions::strict();

    // Text needle (forward + backward).
    let n = prep(Needle::Text(text.clone()), &strict);
    let m = find_next(&doc, &n, 0, &strict, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, start as u64);
    assert_eq!(m.len, needle.len());
    let m = find_prev(&doc, &n, doc.len(), &strict, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, start as u64);

    // Hex needle.
    let n = prep(Needle::Hex(needle.clone()), &strict);
    let m = find_next(&doc, &n, 0, &strict, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, start as u64);

    // Regex needle matching the same 392 bytes (~{382} quantifier spans the boundary).
    let pat = "STRADDLE\\[~{382}\\]".to_string();
    let n = prep(Needle::Regex(pat), &strict);
    let m = find_next(&doc, &n, 0, &strict, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, start as u64);
    assert_eq!(m.len, needle.len());
    let m = find_prev(&doc, &n, doc.len(), &strict, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, start as u64);

    // A search starting one byte inside the match must not re-report it.
    assert!(
        find_next(&doc, &n, start as u64 + 1, &strict, &no_cancel())
            .unwrap()
            .is_none()
    );
}

#[test]
fn from_beyond_eof_and_wrap_semantics() {
    let (_d, doc) = doc_with(b"ab cd ab");
    let n = prep(Needle::Text("ab".into()), &SearchOptions::strict());
    let wrap = SearchOptions {
        case_sensitive: true,
        wrap: true,
    };
    // Forward with wrap from past the last occurrence restarts at the top.
    let m = find_next(&doc, &n, u64::MAX, &wrap, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 0);
    // Without wrap: nothing at/after EOF.
    assert!(
        find_next(&doc, &n, u64::MAX, &SearchOptions::strict(), &no_cancel())
            .unwrap()
            .is_none()
    );
    // Backward with wrap from 0 wraps to the bottom (last match).
    let m = find_prev(&doc, &n, 0, &wrap, &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 6);
    // Forward from the exact start of a match finds it (offset >= from is inclusive).
    let m = find_next(&doc, &n, 6, &SearchOptions::strict(), &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 6);
    // Backward from the exact start of a match skips it.
    let m = find_prev(&doc, &n, 6, &SearchOptions::strict(), &no_cancel())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 0);
}

#[test]
fn empty_needle_and_invalid_regex_are_errors_not_panics() {
    // Integration-level guard: the same errors the unit tests assert, through the
    // public path and across all needle kinds.
    let opts = SearchOptions::strict();
    for needle in [
        Needle::Text(String::new()),
        Needle::Hex(Vec::new()),
        Needle::Regex(String::new()),
    ] {
        let err = needle.prepare(Encoding::Utf8, &opts).unwrap_err();
        assert!(matches!(err, SearchError::EmptyNeedle), "{err}");
    }
    for bad in ["(unclosed", "[a-", "*star"] {
        let res = Needle::Regex(bad.into()).prepare(Encoding::Utf8, &opts);
        assert!(matches!(res, Err(SearchError::Regex(_))), "pattern {bad:?}");
    }
}

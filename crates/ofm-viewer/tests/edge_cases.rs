//! Edge-case tests for the ofm-viewer backend (spec F21): tiny and pathological
//! files, line endings, encodings, wrap, search around chunk/file boundaries,
//! documents that grow or shrink while open, non-UTF-8 names, sparse files and
//! permissions. Everything here is deterministic and well under 5 s per test.
//!
//! Overlap with existing suites is avoided: encoding *detection* fixtures live in
//! `encoding_fixtures.rs`, huge-file open/bench cases in `large_files.rs`, and
//! property tests in `property.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::sync::atomic::AtomicU64;
use std::time::Instant;

use ofm_core::CancelToken;
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};
use ofm_viewer::search::SEARCH_CHUNK;
use ofm_viewer::{
    CHUNK_SIZE, DEFAULT_TAB_WIDTH, Document, Encoding, LineIndex, Needle, Preview, PreviewProvider,
    SearchOptions, SourceKind, TextPreviewProvider, decode_range, detect_encoding, find_next,
    find_prev, goto_line, goto_offset, hex_rows_from, index_document, slice_columns, wrap_line,
};

fn write_doc(name: &str, data: &[u8]) -> (tempfile::TempDir, Document) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join(name);
    std::fs::write(&p, data).unwrap();
    (dir, Document::open(&p).unwrap())
}

fn index_of(doc: &Document) -> LineIndex {
    index_document(doc, &CancelToken::new(), &AtomicU64::new(0)).unwrap()
}

fn prepared(
    needle: Needle,
    encoding: Encoding,
    opts: &SearchOptions,
) -> ofm_viewer::PreparedNeedle {
    needle.prepare(encoding, opts).unwrap()
}

/// Collect every match offset of `needle` in `doc`, forward, no wrap.
fn find_all(doc: &Document, needle: &ofm_viewer::PreparedNeedle) -> Vec<u64> {
    let mut out = Vec::new();
    let mut from = 0u64;
    while let Some(m) = find_next(
        doc,
        needle,
        from,
        &SearchOptions::strict(),
        &CancelToken::new(),
    )
    .unwrap()
    {
        out.push(m.offset);
        from = m.offset + 1;
    }
    out
}

// ---------------------------------------------------------------------------
// Tiny files and line endings
// ---------------------------------------------------------------------------

#[test]
fn tiny_files_index_search_and_preview() {
    type Case<'a> = (&'a [u8], usize, &'a [(u64, u64)]);
    let cases: &[Case<'_>] = &[
        (b"", 0, &[]),
        (b"x", 1, &[(0, 1)]),
        (b"a\nb", 2, &[(0, 1), (2, 3)]),
        (b"a\n", 1, &[(0, 1)]),  // trailing newline: no phantom line
        (b"\n", 1, &[(0, 0)]),   // one empty line
        (b"\r\n", 1, &[(0, 0)]), // CRLF with empty content        (b"\r", 1, &[(0, 0)]),   // lone CR with empty content
        (b"\r\n\r\n", 2, &[(0, 0), (2, 2)]),
    ];
    for (data, lines, ranges) in cases {
        let (_d, doc) = write_doc("tiny", data);
        let idx = index_of(&doc);
        assert!(idx.is_complete(), "data={data:?}");
        assert_eq!(idx.line_count(), *lines, "data={data:?}");
        for (n, (s, e)) in ranges.iter().enumerate() {
            assert_eq!(idx.line_range(n), Some(*s..*e), "data={data:?} line={n}");
        }
        // Line ranges tile the file from 0; the last line's content ends at EOF
        // unless the file ends with a terminator (content excludes terminators).
        if *lines > 0 {
            assert_eq!(idx.line_range(0).unwrap().start, 0);
            let ends_with_break = data.last().is_some_and(|b| *b == b'\n' || *b == b'\r');
            if !ends_with_break {
                assert_eq!(idx.line_range(lines - 1).unwrap().end, data.len() as u64);
            }
        }
    }

    // Search and preview behaviour on the extremes.
    let dir = tempfile::tempdir().unwrap();
    let empty_p = dir.path().join("empty");
    std::fs::write(&empty_p, b"").unwrap();
    let empty = Document::open(&empty_p).unwrap();
    let n = prepared(
        Needle::Text("x".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    assert!(
        find_next(&empty, &n, 0, &SearchOptions::strict(), &CancelToken::new())
            .unwrap()
            .is_none()
    );
    let provider = TextPreviewProvider::default();
    assert!(matches!(
        provider.preview(&empty_p, &CancelToken::new()),
        Err(ofm_viewer::PreviewError::Empty)
    ));

    let one_p = dir.path().join("one.txt");
    std::fs::write(&one_p, b"x").unwrap();
    let _one = Document::open(&one_p).unwrap(); // keep the file open while previewing
    let prev = TextPreviewProvider::default()
        .preview(&one_p, &CancelToken::new())
        .unwrap();
    match prev {
        Preview::Text {
            lines,
            encoding,
            total_lines,
        } => {
            assert_eq!(lines, vec!["x"]);
            assert_eq!(encoding, Encoding::Utf8);
            assert_eq!(total_lines, Some(1));
        }
        other => panic!("unexpected preview: {other:?}"),
    }
}

#[test]
fn mixed_line_endings_decode_exactly() {
    // "one\r\ntwo\rthree\nfour" — CRLF, lone CR, LF, and an unterminated last line.
    let data = b"one\r\ntwo\rthree\nfour";
    let (_d, doc) = write_doc("mixed.txt", data);
    let idx = index_of(&doc);
    let want: &[(u64, u64)] = &[(0, 3), (5, 8), (9, 14), (15, 19)];
    assert_eq!(idx.line_count(), want.len());
    for (n, (s, e)) in want.iter().enumerate() {
        assert_eq!(idx.line_range(n), Some(*s..*e));
    }
    // Decoded contents are terminator-free.
    let texts: Vec<String> = (0..idx.line_count())
        .map(|n| {
            let r = idx.line_range(n).unwrap();
            let bytes = doc.read_range(r.start, (r.end - r.start) as usize).unwrap();
            decode_range(Encoding::Utf8, &bytes).into_owned()
        })
        .collect();
    assert_eq!(texts, vec!["one", "two", "three", "four"]);
    // line_containing agrees at every boundary, including past EOF (clamps to last).
    assert_eq!(idx.line_containing(0), 0);
    assert_eq!(idx.line_containing(3), 0); // the CR itself belongs to line 0
    assert_eq!(idx.line_containing(4), 0); // ... and so does its paired LF
    assert_eq!(idx.line_containing(5), 1);
    assert_eq!(idx.line_containing(18), 3);
    assert_eq!(idx.line_containing(10_000), 3);
}

// ---------------------------------------------------------------------------
// Pathological content: long lines, NULs, invalid UTF-8, BOM
// ---------------------------------------------------------------------------

#[test]
fn long_single_line_10mb_wraps_by_display_columns() {
    const LEN: usize = 10_000_000; // divisible by 80
    let (_d, doc) = write_doc("long.txt", &vec![b'x'; LEN]);
    let t0 = Instant::now();
    let idx = index_of(&doc);
    eprintln!("indexed 10 MB no-newline file in {:?}", t0.elapsed());
    assert_eq!(idx.line_count(), 1);
    assert_eq!(idx.line_range(0), Some(0..LEN as u64));

    let t1 = Instant::now();
    let bytes = doc.read_range(0, LEN).unwrap();
    let text = decode_range(Encoding::Utf8, &bytes);
    let segs = wrap_line(&text, 80, DEFAULT_TAB_WIDTH);
    assert_eq!(segs.len(), LEN / 80);
    // Byte-exact tiling: segments reassemble the line, columns advance by 80.
    for (i, s) in segs.iter().enumerate() {
        assert_eq!(s.start_byte, i * 80);
        assert_eq!(s.end_byte - s.start_byte, 80);
        assert_eq!(s.end_col - s.start_col, 80);
    }
    let joined: String = segs
        .iter()
        .map(|s| &text[s.start_byte..s.end_byte])
        .collect();
    assert_eq!(joined, text);
    // Horizontal-scroll slicing of the same line agrees with the bytes.
    let seg = slice_columns(&text, 1000, 80, DEFAULT_TAB_WIDTH).unwrap();
    assert_eq!(&text[seg.start_byte..seg.end_byte], &text[1000..1080]);
    assert_eq!((seg.start_col, seg.end_col), (1000, 1080));
    assert!(slice_columns(&text, LEN, 80, DEFAULT_TAB_WIDTH).is_none());
    eprintln!("decode+wrap+slice in {:?}", t1.elapsed());
}

#[test]
fn nul_and_control_bytes_in_text_mode() {
    // NUL bytes are valid UTF-8 but control-heavy: detection must not claim UTF-8,
    // indexing must still split on LF, hex view must render '.' for controls.
    let data = b"a\0b\nc\0\x01d\n";
    let (_d, doc) = write_doc("nuls.bin", data);
    let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
    assert_ne!(detect_encoding(&head).encoding, Encoding::Utf8);
    let idx = index_of(&doc);
    assert_eq!(idx.line_count(), 2);
    assert_eq!(idx.line_range(0), Some(0..3));
    assert_eq!(idx.line_range(1), Some(4..8));
    // Decoded text keeps the NULs as chars (lossless for valid UTF-8 controls).
    let r = idx.line_range(0).unwrap();
    let bytes = doc.read_range(r.start, (r.end - r.start) as usize).unwrap();
    assert_eq!(decode_range(Encoding::Utf8, &bytes), "a\0b");
    // Search is byte-based and works across NULs regardless of display.
    let n = prepared(
        Needle::Text("b\nc".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 2);
    // Hex view of the same bytes: controls become '.'.
    let rows = hex_rows_from(&doc, 0, 1).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].bytes, data);
    assert_eq!(rows[0].text, "a.b.c..d.");
}

#[test]
fn invalid_utf8_lossy_display_but_byte_exact_offsets() {
    let data = b"ab\xFFcd\xC0\xC1ef";
    let (_d, doc) = write_doc("bad.txt", data);
    let bytes = doc.read_range(0, data.len()).unwrap();
    let text = decode_range(Encoding::Utf8, &bytes);
    assert!(text.starts_with("ab"));
    assert!(text.contains('\u{FFFD}'));
    assert!(text.ends_with("ef"));
    // Search offsets stay byte-exact even though display is lossy.
    let n = prepared(
        Needle::Text("cd".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!((m.offset, m.len), (3, 2));
    let n = prepared(
        Needle::Text("ef".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 7);
}

#[test]
fn utf8_bom_end_to_end() {
    let mut data = vec![0xEF, 0xBB, 0xBF];
    data.extend_from_slice(b"first\nsecond\n");
    let (_d, doc) = write_doc("bom.txt", &data);
    let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
    let det = detect_encoding(&head);
    assert_eq!(det.encoding, Encoding::Utf8);
    assert_eq!(det.bom_len, 3);

    let idx = index_of(&doc);
    assert_eq!(idx.line_count(), 2);
    // Line 0's range still starts at 0 (the BOM is part of it); the caller skips
    // bom_len when rendering the first line.
    assert_eq!(idx.line_range(0), Some(0..8));
    assert_eq!(idx.line_range(1), Some(9..15));
    let r = idx.line_range(0).unwrap();
    let bytes = doc.read_range(r.start, (r.end - r.start) as usize).unwrap();
    assert_eq!(decode_range(Encoding::Utf8, &bytes[det.bom_len..]), "first");
    // Decoding the raw file also strips the UTF-8 BOM (encoding_rs removes leading
    // BOMs for UTF-8 just like UTF-16); callers that need the raw bytes use read_range.
    let whole = doc.read_range(0, data.len()).unwrap();
    assert_eq!(decode_range(Encoding::Utf8, &whole), "first\nsecond\n");
    // Search offsets are raw-file offsets: "first" sits behind the BOM.
    let n = prepared(
        Needle::Text("first".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 3);
}

// ---------------------------------------------------------------------------
// Encodings: search in legacy encodings, wide/combining wrap
// ---------------------------------------------------------------------------

#[test]
fn search_in_legacy_single_byte_encodings() {
    let ru = "Привет, мир! Это проверка поиска.";
    let tail = " HELLO"; // exercises ASCII case variants
    // Cyrillic encodings: round-trip the Russian text. (windows-1252 cannot
    // encode Cyrillic at all — encoding_rs would emit HTML entities — so its
    // Latin-only case is tested separately below.)
    let cases: [(Encoding, &str); 3] = [
        (Encoding::Windows1251, "windows-1251"),
        (Encoding::Ibm866, "ibm866"),
        (Encoding::Koi8R, "koi8-r"),
    ];
    for (enc, label) in cases {
        let rs = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap();
        let text = format!("{ru}{tail}");
        let (encoded, _, _) = rs.encode(&text);
        let (_d, doc) = write_doc("legacy.txt", &encoded);
        let bytes = doc.read_range(0, encoded.len()).unwrap();
        assert_eq!(decode_range(enc, &bytes), text, "{label}");

        // Russian word found at its exact byte offset in this encoding.
        let needle_ru = "поиска";
        let (raw, _, _) = rs.encode(needle_ru);
        let want = encoded
            .windows(raw.len())
            .position(|w| w == raw.as_ref())
            .unwrap() as u64;
        let n = prepared(
            Needle::Text(needle_ru.into()),
            enc,
            &SearchOptions::strict(),
        );
        let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (want, raw.len()), "{label}");

        // Case-insensitive ASCII folding works inside any single-byte encoding.
        // " HELLO": the space leads at len-6, the match itself starts one byte later.
        let n = prepared(Needle::Text("hello".into()), enc, &SearchOptions::default());
        let m = find_next(&doc, &n, 0, &SearchOptions::default(), &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(
            m.offset,
            encoded.len() as u64 - tail.len() as u64 + 1,
            "{label}"
        );
    }

    // Latin-only text round-trips through the windows-1252 superset.
    {
        let latin = "Grüße! Straße, café, naïve — 100% jalapeño.";
        let rs = encoding_rs::Encoding::for_label(b"windows-1252").unwrap();
        let (encoded, _, _) = rs.encode(latin);
        let (_d, doc) = write_doc("latin_full.txt", &encoded);
        let bytes = doc.read_range(0, encoded.len()).unwrap();
        assert_eq!(decode_range(Encoding::Iso8859_1, &bytes), latin);
        let n = prepared(
            Needle::Text("jalapeño".into()),
            Encoding::Iso8859_1,
            &SearchOptions::strict(),
        );
        let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
            .unwrap()
            .unwrap();
        let want = encoded
            .windows(b"jalape\xF1".len())
            .position(|w| w == b"jalape\xF1")
            .unwrap() as u64;
        assert_eq!(m.offset, want);
    }

    // Latin-1 high-half bytes search byte-exactly (é = 0xE9).
    let (_d, doc) = write_doc("latin.txt", b"caf\xE9 na\xEFve");
    let n = prepared(
        Needle::Text("café".into()),
        Encoding::Iso8859_1,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!((m.offset, m.len), (0, 4));
    assert_eq!(decode_range(Encoding::Iso8859_1, b"caf\xE9"), "café");
}

#[test]
fn wide_and_combining_chars_in_wrap() {
    use unicode_width::UnicodeWidthChar;
    // CJK wide chars (2 cols) mixed with a combining acute (0 cols) and ASCII.
    let line = "ab日本\u{0301}cd e\u{0301}f語".repeat(25);
    let (_d, doc) = write_doc("cjk.txt", line.as_bytes());
    let idx = index_of(&doc);
    assert_eq!(idx.line_count(), 1);
    let bytes = doc.read_range(0, line.len()).unwrap();
    assert_eq!(decode_range(Encoding::Utf8, &bytes), line);

    for width in [1, 3, 7, 10, 24] {
        let segs = wrap_line(&line, width, DEFAULT_TAB_WIDTH);
        assert!(!segs.is_empty());
        // Segments tile the line byte-exactly.
        let joined: String = segs
            .iter()
            .map(|s| &line[s.start_byte..s.end_byte])
            .collect();
        assert_eq!(joined, line, "width={width}");
        for (i, s) in segs.iter().enumerate() {
            let cols = s.end_col - s.start_col;
            let text = &line[s.start_byte..s.end_byte];
            // A segment is at most `width` columns — unless its significant (non
            // zero-width) content is a single wide char that cannot fit at all
            // (it may carry trailing combining marks).
            let significant: Vec<char> = text
                .chars()
                .filter(|c| c.width().unwrap_or(0) > 0)
                .collect();
            let single_too_wide =
                significant.len() == 1 && significant[0].width().unwrap_or(0) > width;
            assert!(
                cols <= width || single_too_wide,
                "width={width} seg={i} cols={cols} text={text:?}"
            );
            if i > 0 {
                // Never break before a zero-width (combining) char: it would render
                // detached from its base.
                let first = line[s.start_byte..].chars().next().unwrap();
                assert!(
                    first.width().unwrap_or(0) > 0,
                    "width={width} seg={i} first={first:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Search: chunk boundaries, file edges, overlap, errors
// ---------------------------------------------------------------------------

/// A ~2 MiB filler file with needles around the 1 MiB search-chunk boundary.
fn boundary_doc() -> (tempfile::TempDir, Document) {
    const N0: &[u8] = b"NEEDLE0";
    const N1: &[u8] = b"NEEDLE1";
    const N2: &[u8] = b"NEEDLE2";
    const N3: &[u8] = b"NEEDLE3";
    let mut data = vec![b'a'; 2 * SEARCH_CHUNK + 512];
    data[..N0.len()].copy_from_slice(N0);
    // Straddles the first chunk boundary: starts 3 bytes before it.
    data[SEARCH_CHUNK - 3..SEARCH_CHUNK - 3 + N1.len()].copy_from_slice(N1);
    data[SEARCH_CHUNK + 10..SEARCH_CHUNK + 10 + N2.len()].copy_from_slice(N2);
    // Ends exactly at EOF.
    let n3_at = data.len() - N3.len();
    data[n3_at..].copy_from_slice(N3);
    write_doc("boundary.bin", &data)
}

#[test]
fn text_search_straddling_chunk_boundaries() {
    let (_d, doc) = boundary_doc();
    let strict = SearchOptions::strict();
    let n = prepared(Needle::Text("NEEDLE".into()), Encoding::Utf8, &strict);

    let m = find_next(&doc, &n, 0, &strict, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 0); // match at file start
    let m = find_next(&doc, &n, 1, &strict, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, SEARCH_CHUNK as u64 - 3); // straddling the boundary
    let m = find_next(&doc, &n, SEARCH_CHUNK as u64, &strict, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, SEARCH_CHUNK as u64 + 10); // just past the boundary
    let m = find_next(&doc, &n, m.offset + 1, &strict, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, doc.len() - 7); // ends exactly at EOF
    assert!(
        find_next(&doc, &n, doc.len(), &strict, &CancelToken::new())
            .unwrap()
            .is_none()
    );

    // Backward across the boundary: the straddling match starts before `from`.
    let m = find_prev(
        &doc,
        &n,
        SEARCH_CHUNK as u64 + 10,
        &strict,
        &CancelToken::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(m.offset, SEARCH_CHUNK as u64 - 3);
    let m = find_prev(&doc, &n, doc.len(), &strict, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, doc.len() - 7);

    // Case-insensitive variant finds the same straddling offset.
    let n = prepared(
        Needle::Text("needle1".into()),
        Encoding::Utf8,
        &SearchOptions::default(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::default(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, SEARCH_CHUNK as u64 - 3);
}

#[test]
fn hex_and_regex_search_boundaries() {
    // Hex needle straddling the boundary.
    let mut data = vec![0x55u8; 2 * SEARCH_CHUNK + 64];
    data[SEARCH_CHUNK - 1] = 0xDE;
    data[SEARCH_CHUNK] = 0xAD;
    let (_d, doc) = write_doc("hexboundary.bin", &data);
    let n = prepared(
        Needle::Hex(vec![0xDE, 0xAD]),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!((m.offset, m.len), (SEARCH_CHUNK as u64 - 1, 2));

    // Regex match straddling the boundary, forward and backward, case-insensitive.
    let mut data = vec![b'b'; 2 * SEARCH_CHUNK + 64];
    data[SEARCH_CHUNK - 4..SEARCH_CHUNK - 4 + 7].copy_from_slice(b"NEEDLE7");
    data[2 * SEARCH_CHUNK - 3..2 * SEARCH_CHUNK - 3 + 7].copy_from_slice(b"NEEDLE8");
    let (_d, doc) = write_doc("regexboundary.bin", &data);
    let n = prepared(
        Needle::Regex(r"needle[0-9]".into()),
        Encoding::Utf8,
        &SearchOptions::default(),
    );
    let opts = SearchOptions::default(); // case-insensitive regex
    let m = find_next(&doc, &n, 0, &opts, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, SEARCH_CHUNK as u64 - 4);
    let m = find_next(&doc, &n, m.offset + 1, &opts, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 2 * SEARCH_CHUNK as u64 - 3);
    let m = find_prev(&doc, &n, doc.len(), &opts, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 2 * SEARCH_CHUNK as u64 - 3);
    let m = find_prev(&doc, &n, m.offset, &opts, &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, SEARCH_CHUNK as u64 - 4);
}

#[test]
fn overlapping_matches_and_file_edges() {
    let (_d, doc) = write_doc("overlap.txt", b"aaaaaa");
    let n = prepared(
        Needle::Text("aa".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    assert_eq!(find_all(&doc, &n), vec![0, 1, 2, 3, 4]);

    // Match at the very first and very last byte.
    let (_d, doc) = write_doc("edges.txt", b"zz");
    let n = prepared(
        Needle::Text("z".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    assert_eq!(find_all(&doc, &n), vec![0, 1]);
    let m = find_prev(&doc, &n, 1, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 0); // backward from a match start skips it
    // Hex needle at EOF-2.
    let (_d, doc) = write_doc("hexend.bin", &[0x01, 0x02, 0xDE, 0xAD]);
    let n = prepared(
        Needle::Hex(vec![0xDE, 0xAD]),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_prev(
        &doc,
        &n,
        doc.len(),
        &SearchOptions::strict(),
        &CancelToken::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!((m.offset, m.len), (2, 2));
}

#[test]
fn search_no_match_and_needle_errors() {
    let (_d, doc) = write_doc("plain.txt", b"hello world");
    let n = prepared(
        Needle::Text("xyz".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    assert!(
        find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
            .unwrap()
            .is_none()
    );
    assert!(
        find_prev(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
            .unwrap()
            .is_none()
    );

    // Empty needles are an error for every needle kind, never a panic.
    let empties: [Needle; 3] = [
        Needle::Text(String::new()),
        Needle::Hex(Vec::new()),
        Needle::Regex(String::new()),
    ];
    for needle in empties {
        let desc = format!("{needle:?}");
        assert!(
            matches!(
                needle.prepare(Encoding::Utf8, &SearchOptions::strict()),
                Err(ofm_viewer::SearchError::EmptyNeedle)
            ),
            "needle={desc}"
        );
    }
    // Invalid regexes are compile errors, never panics.
    for pattern in ["(unclosed", "[a-", "*foo", "(?P<bad"] {
        assert!(
            matches!(
                Needle::Regex(pattern.into()).prepare(Encoding::Utf8, &SearchOptions::strict()),
                Err(ofm_viewer::SearchError::Regex(_))
            ),
            "pattern={pattern}"
        );
    }
}

// ---------------------------------------------------------------------------
// Navigation, growth/shrink while open
// ---------------------------------------------------------------------------

#[test]
fn goto_beyond_eof_clamps_or_returns_none() {
    let (_d, doc) = write_doc("lines.txt", b"a\nb\nc\n");
    let idx = index_of(&doc);
    assert_eq!(idx.line_count(), 3);
    assert_eq!(goto_line(&idx, 0), Some(0));
    assert_eq!(goto_line(&idx, 2), Some(4));
    assert_eq!(goto_line(&idx, 3), None); // past the last line
    assert_eq!(goto_line(&idx, usize::MAX), None);
    assert_eq!(goto_offset(&doc, 2), 2);
    assert_eq!(goto_offset(&doc, u64::MAX), doc.len());
}

#[test]
fn growing_file_while_open_reads_via_pread() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("growing.txt");
    std::fs::write(&p, b"abc").unwrap();
    let doc = Document::open(&p).unwrap();
    assert_eq!(doc.source_kind(), SourceKind::File);
    assert_eq!(doc.len(), 3);

    // Another process/appends: the mapping stays, the file grows.
    std::fs::OpenOptions::new()
        .append(true)
        .open(&p)
        .unwrap()
        .write_all(b"defgh")
        .unwrap();
    assert_eq!(doc.current_file_len().unwrap(), 8);
    // Reads beyond the mapped length take the pread path and see the new data.
    assert_eq!(doc.read_range(3, 5).unwrap(), b"defgh");
    assert_eq!(doc.read_range(0, 3).unwrap(), b"abc");
    // doc.len() is the open-time snapshot; indexing reads through to the new EOF.
    assert_eq!(doc.len(), 3);
    let idx = index_of(&doc);
    assert_eq!(idx.line_range(0), Some(0..8));
}

#[test]
fn truncated_file_indexes_and_reads_without_fault() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("shrink.txt");
    // Three indexing chunks of "abc\n" lines, then truncate into the second chunk.
    let line = b"abc\n";
    let total = 3 * CHUNK_SIZE;
    {
        let mut f = std::fs::File::create(&p).unwrap();
        for _ in 0..total / line.len() {
            f.write_all(line).unwrap();
        }
    }
    let truncated = CHUNK_SIZE as u64 + 100;
    std::fs::OpenOptions::new()
        .write(true)
        .open(&p)
        .unwrap()
        .set_len(truncated)
        .unwrap();

    let doc = Document::open(&p).unwrap();
    let idx = index_of(&doc);
    assert!(idx.is_complete());
    assert_eq!(idx.indexed_len(), truncated);
    assert_eq!(idx.line_count(), (truncated / 4) as usize); // all lines complete
    // Reads past the new end short-read instead of faulting.
    let tail = doc.read_range(CHUNK_SIZE as u64, 4096).unwrap();
    assert_eq!(tail.len(), 100);
    assert_eq!(doc.read_range(truncated, 16).unwrap(), b"");
}

// ---------------------------------------------------------------------------
// Names, permissions, sparse files
// ---------------------------------------------------------------------------

#[test]
fn non_utf8_filename_full_pipeline() {
    let dir = tempfile::tempdir().unwrap();
    let name = raw_name(b"vie\xFFwer-\xC3\xA9.txt");
    let p = dir.path().join(name);
    std::fs::write(&p, b"hello\nworld\n").unwrap();
    let doc = Document::open(&p).unwrap();
    let idx = index_of(&doc);
    assert_eq!(idx.line_count(), 2);
    let n = prepared(
        Needle::Text("world".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &CancelToken::new())
        .unwrap()
        .unwrap();
    assert_eq!(m.offset, 6);
    let prev = TextPreviewProvider::default()
        .preview(&p, &CancelToken::new())
        .unwrap();
    match prev {
        Preview::Text { lines, .. } => assert_eq!(lines, vec!["hello", "world"]),
        other => panic!("unexpected preview: {other:?}"),
    }
}

#[test]
fn sparse_10gb_first_screen_and_cancellable_search() {
    const SPARSE_LEN: u64 = 10 * 1024 * 1024 * 1024; // 10 GiB
    const MARKER: &[u8] = b"MARKER-NEAR-THE-END";
    let spec = TreeSpec::new().file(
        "sparse.bin",
        FileSpec::sparse_ranges(SPARSE_LEN, &[(SPARSE_LEN - 4096, MARKER)]),
    );
    // A filesystem without sparse support fails at set_len: skip politely.
    let temp = match spec.build_temp() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sparse files unsupported here ({e}); skipping");
            return;
        }
    };
    let p = temp.path().join("sparse.bin");

    let t0 = Instant::now();
    let doc = Document::open(&p).unwrap();
    let open_ms = t0.elapsed().as_millis();
    assert_eq!(doc.source_kind(), SourceKind::File);
    assert_eq!(doc.len(), SPARSE_LEN);
    assert!(open_ms < 500, "open took {open_ms} ms (spec: instant)");

    let t1 = Instant::now();
    let screen = doc.read_range(0, 64 * 1024).unwrap();
    let read_ms = t1.elapsed().as_millis();
    eprintln!("first screen of 10 GiB sparse file in {read_ms} ms (spec: < 100 ms)");
    assert_eq!(screen.len(), 64 * 1024);
    assert_eq!(screen, vec![0u8; 64 * 1024]);
    assert!(
        read_ms < 500,
        "first screen took {read_ms} ms (spec: < 100 ms)"
    );

    // Search near the end finds the marker (backward scan starts at the end).
    let n = prepared(
        Needle::Text("marker-near".into()),
        Encoding::Utf8,
        &SearchOptions::default(),
    );
    let m = find_prev(
        &doc,
        &n,
        doc.len(),
        &SearchOptions::default(),
        &CancelToken::new(),
    )
    .unwrap()
    .unwrap();
    assert_eq!(m.offset, SPARSE_LEN - 4096);

    // A pre-cancelled search aborts between chunk reads instead of scanning 10 GiB.
    let cancel = CancelToken::new();
    cancel.cancel();
    let err = find_next(&doc, &n, 0, &SearchOptions::default(), &cancel).unwrap_err();
    assert!(matches!(err, ofm_viewer::SearchError::Cancelled));
}

#[test]
fn unreadable_file_returns_open_error() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("secret.txt");
    std::fs::write(&p, b"classified").unwrap();
    let mut perms = std::fs::metadata(&p).unwrap().permissions();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        perms.set_mode(0o000);
    }
    std::fs::set_permissions(&p, perms).unwrap();

    match Document::open(&p) {
        Err(ofm_viewer::DocumentError::Open(_)) => {} // expected
        Ok(_) => {
            // Root (or CAP_DAC_OVERRIDE) can read anything: nothing to test here.
            eprintln!("running with elevated privileges; skipping chmod-000 case");
        }
        Err(other) => panic!("unexpected error kind: {other}"),
    }
}

// ---------------------------------------------------------------------------
// Concurrency: concurrent truncate/extend vs reads and search (SIGBUS regression)
// ---------------------------------------------------------------------------

const STRESS_SIZE: u64 = 64 * 1024 * 1024; // 64 MiB
const STRESS_SECONDS: u64 = 3;

/// Spawn a thread that loops `set_len(0)` / `set_len(STRESS_SIZE)` until `stop` is set.
/// Returns the join handle and the shared stop flag.
fn truncate_loop(
    path: std::path::PathBuf,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            f.set_len(0).unwrap();
            f.set_len(STRESS_SIZE).unwrap();
        }
    })
}

#[test]
fn concurrent_truncate_and_read_at_never_faults() {
    // One thread loops read_at over 1 MiB windows, another loops set_len(0)/set_len(size).
    // With the old mmap fast path this process dies with SIGBUS within seconds.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("race.bin");
    {
        let f = std::fs::File::create(&p).unwrap();
        f.set_len(STRESS_SIZE).unwrap();
    }
    let doc = Document::open(&p).unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hammer = truncate_loop(p.clone(), stop.clone());

    let deadline = Instant::now() + std::time::Duration::from_secs(STRESS_SECONDS);
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total_read: u64 = 0;
    while Instant::now() < deadline {
        for off in (0..STRESS_SIZE).step_by(buf.len()) {
            // Must never crash; short reads (possibly 0 past the truncated end) are fine.
            total_read += doc.read_at(off, &mut buf).unwrap() as u64;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    hammer.join().unwrap();
    eprintln!("concurrent truncate/read_at: {total_read} MiB read in {STRESS_SECONDS}s");
}

#[test]
fn concurrent_truncate_and_search_never_faults() {
    // Same race, but through the search path: scanning reads chunk windows via read_at and
    // must survive the file shrinking underneath (short/empty reads, no SIGBUS, no error).
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("race-search.bin");
    {
        let mut f = std::fs::File::create(&p).unwrap();
        // "needle" repeated: something to find when the file is extended.
        let line = b"needle line\n";
        for _ in 0..(STRESS_SIZE as usize / line.len()) {
            f.write_all(line).unwrap();
        }
    }
    let doc = Document::open(&p).unwrap();
    let needle = prepared(
        Needle::Text("needle".into()),
        Encoding::Utf8,
        &SearchOptions::strict(),
    );
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let hammer = truncate_loop(p.clone(), stop.clone());

    let deadline = Instant::now() + std::time::Duration::from_secs(STRESS_SECONDS);
    let cancel = CancelToken::new();
    let mut scans = 0u64;
    while Instant::now() < deadline {
        // Any outcome is fine (hit, miss, cancel); only a crash or a hard error is a bug.
        let _ = find_next(&doc, &needle, 0, &SearchOptions::strict(), &cancel);
        scans += 1;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    hammer.join().unwrap();
    eprintln!("concurrent truncate/search: {scans} scans in {STRESS_SECONDS}s");
}

// ---------------------------------------------------------------------------
// Previews: line-break rules agree with the viewer (lone CR)
// ---------------------------------------------------------------------------

#[test]
fn preview_counts_lone_cr_lines_like_the_viewer() {
    // "alpha\rbeta\n" is two viewer lines (lone CR breaks) but one str::lines entry.
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("lonecr.txt");
    std::fs::write(&p, b"alpha\rbeta\n").unwrap();
    let prev = TextPreviewProvider::default()
        .preview(&p, &CancelToken::new())
        .unwrap();
    match prev {
        Preview::Text {
            lines, total_lines, ..
        } => {
            assert_eq!(lines, vec!["alpha", "beta"]);
            assert_eq!(total_lines, Some(2));
        }
        other => panic!("unexpected preview: {other:?}"),
    }
}

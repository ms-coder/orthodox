//! Search over a [`Document`]: text (needle encoded in the current encoding), raw hex bytes,
//! or a [`regex::bytes`] pattern; forward and backward, optional wrap.
//!
//! Scanning is chunked ([`SEARCH_CHUNK`]) with an overlap tail so matches spanning chunk
//! boundaries are found. Text/hex needles always report every match; regex matches longer
//! than the overlap ([`MIN_OVERLAP`]) that span a chunk boundary may be missed (documented —
//! regex match length is unbounded).
//!
//! Case-insensitive *text* search folds ASCII only (up to four byte variants: as-is, lower,
//! Capitalized, UPPER); Unicode folding is not byte-compatible with arbitrary encodings, so
//! it is out of scope for v1.0. Case-insensitive *regex* search delegates to the regex engine
//! and is Unicode-aware when the pattern is valid UTF-8.
//!
//! UTF-16 alignment: in UTF-16LE/BE modes every reported match (text, hex, or regex) must
//! start at an even byte offset — a code-unit boundary. A needle whose encoded bytes happen
//! to appear across two adjacent code units (e.g. "a" = `61 00` inside U+6100 U+4E00) is a
//! false positive and is skipped; the scan continues at offset+1. The parity of the BOM
//! length (2 for UTF-16) means absolute file offsets can be checked directly.

use ofm_core::CancelToken;
use regex::bytes::RegexBuilder;
use thiserror::Error;

use crate::document::Document;
use crate::encoding::Encoding;

/// Bytes read per search window.
pub const SEARCH_CHUNK: usize = 1024 * 1024;

/// Overlap between consecutive windows; guarantees boundary-spanning matches for needles up
/// to this length.
pub const MIN_OVERLAP: usize = 64 * 1024;

/// A search hit: absolute byte offset and match length in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Absolute byte offset of the match start.
    pub offset: u64,
    /// Match length in bytes.
    pub len: usize,
}

/// Search behaviour switches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SearchOptions {
    /// Distinguish case (default: case-insensitive = false → fold).
    pub case_sensitive: bool,
    /// Wrap around the file end/beginning.
    pub wrap: bool,
}

impl SearchOptions {
    /// Strict, non-wrapping search.
    pub fn strict() -> Self {
        Self {
            case_sensitive: true,
            wrap: false,
        }
    }
}

/// What to search for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Needle {
    /// Text, encoded into the document's encoding at [`prepare`](Needle::prepare) time.
    Text(String),
    /// Raw bytes (hex mode).
    Hex(Vec<u8>),
    /// A [`regex::bytes`] pattern, matched against raw file bytes. For non-UTF-8 encodings
    /// build the pattern from the encoded needle (e.g. `\xEF\xF0...`).
    Regex(String),
}

/// Errors from preparing or running a search.
#[derive(Debug, Error)]
pub enum SearchError {
    /// The needle was empty.
    #[error("empty needle")]
    EmptyNeedle,
    /// The regex failed to compile.
    #[error("regex error: {0}")]
    Regex(#[from] regex::Error),
    /// Cancelled via [`CancelToken`].
    #[error("search cancelled")]
    Cancelled,
    /// A document read failed.
    #[error("read failed: {0}")]
    Read(#[from] crate::document::DocumentError),
}

/// A needle ready for scanning.
///
/// Carries the code-unit alignment of the document's encoding: in UTF-16 modes every
/// reported match is checked to start on an even byte offset (code-unit boundary) so a
/// needle's encoded bytes cannot match across two adjacent code units (e.g. searching for
/// "a" in a file containing U+6100). Build via [`Needle::prepare`].
#[derive(Debug, Clone)]
pub struct PreparedNeedle {
    kind: PreparedKind,
    /// 1 for byte-oriented encodings, 2 for UTF-16 (matches must start at even offsets).
    unit_align: usize,
}

#[derive(Debug, Clone)]
enum PreparedKind {
    /// Literal byte variants (memmem), searched in order; the minimum offset wins.
    Bytes(Vec<Vec<u8>>),
    /// Compiled byte regex.
    Regex(regex::bytes::Regex),
}

impl PreparedNeedle {
    /// Byte alignment matches must honour: 2 in UTF-16 modes (matches may only start at
    /// even offsets), 1 otherwise.
    pub fn unit_align(&self) -> usize {
        self.unit_align
    }
}

impl Needle {
    /// Encode/compile the needle for `encoding`.
    pub fn prepare(
        self,
        encoding: Encoding,
        opts: &SearchOptions,
    ) -> Result<PreparedNeedle, SearchError> {
        let unit_align = match encoding {
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            _ => 1,
        };
        let kind = match self {
            Needle::Text(s) => {
                if s.is_empty() {
                    return Err(SearchError::EmptyNeedle);
                }
                let raw = encode_text(&s, encoding);
                PreparedKind::Bytes(byte_variants(&raw, opts.case_sensitive))
            }
            Needle::Hex(v) => {
                if v.is_empty() {
                    return Err(SearchError::EmptyNeedle);
                }
                PreparedKind::Bytes(vec![v])
            }
            Needle::Regex(p) => {
                if p.is_empty() {
                    return Err(SearchError::EmptyNeedle);
                }
                let re = RegexBuilder::new(&p)
                    .case_insensitive(!opts.case_sensitive)
                    .build()?;
                PreparedKind::Regex(re)
            }
        };
        Ok(PreparedNeedle { kind, unit_align })
    }
}

/// Encode `text` in `encoding` as raw bytes.
fn encode_text(text: &str, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => text.as_bytes().to_vec(),
        Encoding::Utf16Le => text.encode_utf16().flat_map(|u| u.to_le_bytes()).collect(),
        Encoding::Utf16Be => text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect(),
        _ => encoding_rs::Encoding::for_label(encoding.name().as_bytes()).map_or_else(
            || text.as_bytes().to_vec(),
            |e| {
                let (cow, _, _) = e.encode(text);
                cow.into_owned()
            },
        ),
    }
}

/// ASCII case variants of a literal (an extension of grep's three-way trick). Any
/// single-case haystack word matches one of: as-is, all-lower, Capitalized, UPPER. Mixed-case
/// haystack like `hELLo` only matches when the user types it exactly (the as-is variant).
/// Non-ASCII bytes pass through unchanged.
fn byte_variants(raw: &[u8], case_sensitive: bool) -> Vec<Vec<u8>> {
    if case_sensitive || !raw.iter().any(|b| b.is_ascii_alphabetic()) {
        return vec![raw.to_vec()];
    }
    let lower: Vec<u8> = raw.iter().map(|b| b.to_ascii_lowercase()).collect();
    let upper: Vec<u8> = raw.iter().map(|b| b.to_ascii_uppercase()).collect();
    let mut title = lower.clone();
    if let Some(first) = raw.iter().position(|b| b.is_ascii_alphabetic()) {
        title[first] = raw[first].to_ascii_uppercase();
    }
    let mut out: Vec<Vec<u8>> = Vec::new();
    for v in [raw.to_vec(), lower, title, upper] {
        if !out.contains(&v) {
            out.push(v);
        }
    }
    out
}

fn overlap_for(needle: &PreparedNeedle) -> usize {
    match &needle.kind {
        PreparedKind::Bytes(vs) => vs.iter().map(Vec::len).max().unwrap_or(1).saturating_sub(1),
        PreparedKind::Regex(_) => MIN_OVERLAP,
    }
    .max(1)
}

/// Find the first match at offset >= `from` (wrapping to the top per `opts.wrap`).
pub fn find_next(
    doc: &Document,
    needle: &PreparedNeedle,
    from: u64,
    opts: &SearchOptions,
    cancel: &CancelToken,
) -> Result<Option<Match>, SearchError> {
    let from = from.min(doc.len());
    match scan(doc, needle, from, doc.len(), cancel)? {
        Some(m) => Ok(Some(m)),
        // The wrapped part also covers a match that starts before `from` and runs past it
        // (e.g. repeating "next" on the only hit of the file).
        None if opts.wrap && from > 0 => {
            let hi = (from + overlap_for(needle) as u64).min(doc.len());
            scan(doc, needle, 0, hi, cancel)
        }
        None => Ok(None),
    }
}

/// Find the last match with start offset < `from` (wrapping to the bottom per `opts.wrap`).
pub fn find_prev(
    doc: &Document,
    needle: &PreparedNeedle,
    from: u64,
    opts: &SearchOptions,
    cancel: &CancelToken,
) -> Result<Option<Match>, SearchError> {
    let from = from.min(doc.len());
    match scan_back(doc, needle, 0, from, cancel)? {
        Some(m) => Ok(Some(m)),
        None if opts.wrap && from < doc.len() => scan_back(doc, needle, from, doc.len(), cancel),
        None => Ok(None),
    }
}

/// Scan `[lo, hi)` forward for the first match.
///
/// In UTF-16 modes a candidate match at an odd absolute offset is a false positive (the
/// needle's bytes span two code units); it is skipped and the scan continues at offset+1.
fn scan(
    doc: &Document,
    needle: &PreparedNeedle,
    lo: u64,
    hi: u64,
    cancel: &CancelToken,
) -> Result<Option<Match>, SearchError> {
    let mut pos = lo;
    while pos < hi {
        if cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let read = SEARCH_CHUNK + overlap_for(needle);
        let want = ((hi - pos) as usize).min(read);
        let buf = doc.read_range(pos, want)?;
        if buf.is_empty() {
            return Ok(None);
        }
        // Walk candidates, skipping misaligned (odd-offset) matches in UTF-16 modes.
        let mut base = 0usize;
        while let Some((rel, mlen)) = find_in(&buf[base..], needle, false) {
            let rel = base + rel;
            let abs = pos + rel as u64;
            if needle.unit_align == 2 && !abs.is_multiple_of(2) {
                base = rel + 1;
                continue;
            }
            if rel < SEARCH_CHUNK || buf.len() < want {
                return Ok(Some(Match {
                    offset: abs,
                    len: mlen,
                }));
            }
            // Match fully inside the overlap tail: fall through and advance one chunk; the
            // tail region is re-read by the next window.
            break;
        }
        if buf.len() < want {
            return Ok(None); // EOF
        }
        pos += SEARCH_CHUNK as u64;
    }
    Ok(None)
}

/// Scan `[lo, hi)` backward for the last match with start offset < `hi`.
///
/// Windows are overlapped on both sides: the right overlap gives matches that start before
/// `hi` but extend past it enough room to be recognised in full.
fn scan_back(
    doc: &Document,
    needle: &PreparedNeedle,
    lo: u64,
    hi: u64,
    cancel: &CancelToken,
) -> Result<Option<Match>, SearchError> {
    let overlap = overlap_for(needle) as u64;
    let mut end = hi;
    while end > lo {
        if cancel.is_cancelled() {
            return Err(SearchError::Cancelled);
        }
        let read = SEARCH_CHUNK as u64 + overlap;
        let read_end = end.saturating_add(overlap).min(doc.len());
        let want = (read_end - lo).min(read);
        let start = read_end - want;
        let buf = doc.read_range(start, want as usize)?;
        // Walk matches from the back; skip any starting at/after `hi` (they belong to the
        // forward side of `from`) and any misaligned match in UTF-16 modes.
        let mut limit = buf.len();
        let mut found = None;
        while limit > 0 {
            match find_in(&buf[..limit], needle, true) {
                Some((rel, mlen)) => {
                    let abs = start + rel as u64;
                    if abs < hi && (needle.unit_align != 2 || abs.is_multiple_of(2)) {
                        found = Some((abs, mlen));
                        break;
                    }
                    limit = rel;
                }
                None => break,
            }
        }
        if let Some((abs, mlen)) = found {
            return Ok(Some(Match {
                offset: abs,
                len: mlen,
            }));
        }
        if buf.len() < want as usize {
            return Ok(None); // EOF on the left
        }
        end = start;
    }
    Ok(None)
}

/// First/last match of `needle` in `buf` as `(rel_offset, len)`.
fn find_in(buf: &[u8], needle: &PreparedNeedle, back: bool) -> Option<(usize, usize)> {
    match &needle.kind {
        PreparedKind::Bytes(vs) => {
            let mut best: Option<(usize, usize)> = None;
            for v in vs {
                let found = if back {
                    memchr::memmem::find_iter(buf, v)
                        .last()
                        .map(|o| (o, v.len()))
                } else {
                    memchr::memmem::find(buf, v).map(|o| (o, v.len()))
                };
                best = match (best, found) {
                    (b, None) => b,
                    (None, f) => f,
                    (Some((bo, _)), Some((fo, _))) => {
                        if (!back && fo < bo) || (back && fo > bo) {
                            found
                        } else {
                            best
                        }
                    }
                };
            }
            best
        }
        PreparedKind::Regex(re) => {
            if back {
                re.find_iter(buf)
                    .last()
                    .map(|m| (m.start(), m.end() - m.start()))
            } else {
                re.find(buf).map(|m| (m.start(), m.end() - m.start()))
            }
        }
    }
}

/// Byte offset of line `n` (1-based UI concerns are the caller's; `n` here is 0-based), or
/// `None` if the index does not know that line yet.
pub fn goto_line(index: &crate::line_index::LineIndex, n: usize) -> Option<u64> {
    index.line_start(n)
}

/// Clamp `offset` to a valid document position.
pub fn goto_offset(doc: &Document, offset: u64) -> u64 {
    offset.min(doc.len())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn doc_with(data: &[u8]) -> Document {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.bin");
        std::fs::write(&p, data).unwrap();
        // Leak the tempdir so the file lives for the test's duration.
        std::mem::forget(dir);
        Document::open(&p).unwrap()
    }

    #[test]
    fn finds_text_forward_and_backward() {
        let doc = doc_with(b"one two one three one");
        let n = Needle::Text("one".into())
            .prepare(Encoding::Utf8, &SearchOptions::strict())
            .unwrap();
        let no_cancel = CancelToken::new();
        let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (0, 3));
        let m2 = find_next(&doc, &n, 1, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m2.offset, 8);
        let back = SearchOptions {
            case_sensitive: true,
            wrap: true,
        };
        let mp = find_prev(&doc, &n, 8, &back, &no_cancel).unwrap().unwrap();
        assert_eq!(mp.offset, 0);
        let mp = find_prev(&doc, &n, 100, &back, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(mp.offset, 18);
    }

    #[test]
    fn wrap_around() {
        let doc = doc_with(b"ab cd ab");
        let n = Needle::Text("ab".into())
            .prepare(Encoding::Utf8, &SearchOptions::strict())
            .unwrap();
        let no_cancel = CancelToken::new();
        let opts = SearchOptions {
            case_sensitive: true,
            wrap: true,
        };
        let m = find_next(&doc, &n, 1, &opts, &no_cancel).unwrap().unwrap();
        assert_eq!(m.offset, 6);
        let m = find_next(&doc, &n, 7, &opts, &no_cancel).unwrap().unwrap();
        assert_eq!(m.offset, 0); // wrapped
        let opts_no = SearchOptions::strict();
        assert!(
            find_next(&doc, &n, 7, &opts_no, &no_cancel)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn wrap_finds_a_match_that_straddles_the_start_offset() {
        // Searching from inside the only hit must find that same hit after wrapping.
        let doc = doc_with(b"xx needle yy");
        let n = Needle::Text("needle".into())
            .prepare(Encoding::Utf8, &SearchOptions::strict())
            .unwrap();
        let opts = SearchOptions {
            case_sensitive: true,
            wrap: true,
        };
        let m = find_next(&doc, &n, 4, &opts, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (3, 6));
    }

    #[test]
    fn case_insensitive_ascii() {
        // Single-case words match via the lower/Capitalized/UPPER variants; mixed-case
        // "hELLo" only matches when typed exactly (the as-is variant).
        let doc = doc_with(b"Hello hello HELLO hELLo");
        let n = Needle::Text("hello".into())
            .prepare(Encoding::Utf8, &SearchOptions::default())
            .unwrap();
        let no_cancel = CancelToken::new();
        let mut offs = Vec::new();
        let mut from = 0;
        while let Some(m) =
            find_next(&doc, &n, from, &SearchOptions::default(), &no_cancel).unwrap()
        {
            offs.push(m.offset);
            from = m.offset + 1;
        }
        assert_eq!(offs, vec![0, 6, 12]);
        // Exact-case needle also finds the mixed-case word (searched past the earlier hits).
        let n = Needle::Text("hELLo".into())
            .prepare(Encoding::Utf8, &SearchOptions::default())
            .unwrap();
        let m = find_next(&doc, &n, 13, &SearchOptions::default(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 18);
    }

    #[test]
    fn hex_needle() {
        let doc = doc_with(&[0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xDE, 0xAD]);
        let n = Needle::Hex(vec![0xDE, 0xAD])
            .prepare(Encoding::Utf8, &SearchOptions::strict())
            .unwrap();
        let no_cancel = CancelToken::new();
        let m = find_next(&doc, &n, 1, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 1);
        let m = find_next(&doc, &n, 2, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 6);
    }

    #[test]
    fn regex_needle() {
        let doc = doc_with(b"foo123 bar42 baz");
        let n = Needle::Regex(r"[a-z]+\d+".into())
            .prepare(Encoding::Utf8, &SearchOptions::strict())
            .unwrap();
        let no_cancel = CancelToken::new();
        let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (0, 6));
        // Next search starts after the current match (offset >= from).
        let m = find_next(
            &doc,
            &n,
            m.offset + m.len as u64,
            &SearchOptions::strict(),
            &no_cancel,
        )
        .unwrap()
        .unwrap();
        assert_eq!((m.offset, m.len), (7, 5));
        // Backward from the middle of a match finds it (start 7 < from 9, even though the
        // match extends past `from`).
        let m = find_prev(&doc, &n, 9, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (7, 5));
        // Backward from the very start of a match skips it.
        let m = find_prev(&doc, &n, 7, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (0, 6));
        // Invalid pattern is an error, not a panic.
        assert!(
            Needle::Regex("(unclosed".into())
                .prepare(Encoding::Utf8, &SearchOptions::strict())
                .is_err()
        );
        // Empty needle is an error.
        assert!(
            Needle::Text(String::new())
                .prepare(Encoding::Utf8, &SearchOptions::strict())
                .is_err()
        );
    }

    #[test]
    fn text_needle_in_utf16() {
        // "hi" in UTF-16LE: 68 00 69 00.
        let doc = doc_with(&[0x68, 0x00, 0x69, 0x00, 0x68, 0x00]);
        let n = Needle::Text("hi".into())
            .prepare(Encoding::Utf16Le, &SearchOptions::strict())
            .unwrap();
        let no_cancel = CancelToken::new();
        let m = find_next(&doc, &n, 0, &SearchOptions::strict(), &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 0);
        assert!(
            find_next(&doc, &n, 1, &SearchOptions::strict(), &no_cancel)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn utf16_alignment_rejects_unit_straddling_matches() {
        // U+6100 U+4E00 U+4E00 contains no "a", but the UTF-16LE encoding of "a" (61 00)
        // appears at odd offsets where code units meet. Both endians, with and without BOM,
        // forward and backward must reject those.
        let no_cancel = CancelToken::new();
        let strict = SearchOptions::strict();
        for (enc, le) in [(Encoding::Utf16Le, true), (Encoding::Utf16Be, false)] {
            for bom in [false, true] {
                let mut data = Vec::new();
                if bom {
                    data.extend_from_slice(if le { &[0xFF, 0xFE] } else { &[0xFE, 0xFF] });
                }
                for u in [0x6100u16, 0x4E00, 0x4E00] {
                    let unit = if le { u.to_le_bytes() } else { u.to_be_bytes() };
                    data.extend_from_slice(&unit);
                }
                let doc = doc_with(&data);
                let n = Needle::Text("a".into()).prepare(enc, &strict).unwrap();
                assert!(
                    find_next(&doc, &n, 0, &strict, &no_cancel)
                        .unwrap()
                        .is_none(),
                    "forward false positive enc={enc:?} bom={bom}"
                );
                assert!(
                    find_prev(&doc, &n, doc.len(), &strict, &no_cancel)
                        .unwrap()
                        .is_none(),
                    "backward false positive enc={enc:?} bom={bom}"
                );
                assert_eq!(n.unit_align(), 2);
            }
        }
        // A real "a" after the CJK run is found at its even offset, forward and backward.
        let mut data: Vec<u8> = [0x6100u16, 0x4E00]
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        data.extend_from_slice(&0x0061u16.to_le_bytes()); // "a"
        let doc = doc_with(&data);
        let n = Needle::Text("a".into())
            .prepare(Encoding::Utf16Le, &strict)
            .unwrap();
        let m = find_next(&doc, &n, 0, &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 4);
        let m = find_prev(&doc, &n, doc.len(), &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 4);
        // Starting from inside the match, the next search (no wrap) finds nothing.
        assert!(
            find_next(&doc, &n, 5, &strict, &no_cancel)
                .unwrap()
                .is_none()
        );
        // Byte-oriented encodings never filter.
        let n = Needle::Text("a".into())
            .prepare(Encoding::Utf8, &strict)
            .unwrap();
        assert_eq!(n.unit_align(), 1);
        let doc = doc_with(&[0x61, 0x00, 0x62]);
        let m = find_next(&doc, &n, 0, &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 0); // 00 62 is not "a"; only offset 0 matches
    }

    #[test]
    fn utf16_alignment_applies_to_hex_and_regex_needles() {
        // Hex and regex needles in UTF-16 mode must honour unit alignment as well.
        let no_cancel = CancelToken::new();
        let strict = SearchOptions::strict();
        // U+6100 U+6100 U+0061: bytes 00 61 | 00 61 | 61 00 — the hex needle `61 00`
        // appears misaligned at offset 1 and aligned (a real "a") at offset 4.
        let data = [0x6100u16, 0x6100, 0x0061]
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<u8>>();
        let doc = doc_with(&data);

        let n = Needle::Hex(vec![0x61, 0x00])
            .prepare(Encoding::Utf16Le, &strict)
            .unwrap();
        assert_eq!(n.unit_align(), 2);
        let m = find_next(&doc, &n, 0, &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(
            (m.offset, m.len),
            (4, 2),
            "hex needle must skip the misaligned hit at offset 1"
        );
        let m = find_prev(&doc, &n, doc.len(), &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 4);

        // U+6100 U+6200 U+0062: bytes 00 61 | 00 62 | 62 00 — regex "b" (byte 0x62) matches
        // misaligned at offset 3 (the low byte of U+6200) and aligned at offset 4 ('b').
        let data = [0x6100u16, 0x6200, 0x0062]
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect::<Vec<u8>>();
        let doc = doc_with(&data);
        let n = Needle::Regex(r"b".into())
            .prepare(Encoding::Utf16Le, &strict)
            .unwrap();
        let m = find_next(&doc, &n, 0, &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!((m.offset, m.len), (4, 1), "'b' is the aligned match");
        let m = find_prev(&doc, &n, doc.len(), &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, 4);
    }

    #[test]
    fn utf16_alignment_straddling_chunk_boundary() {
        // A misaligned candidate right at the 1 MiB search-chunk boundary must be rejected
        // and the scan must continue into the next window, finding the aligned hit.
        let no_cancel = CancelToken::new();
        let strict = SearchOptions::strict();
        let mut data = vec![0u8; 2 * SEARCH_CHUNK + 16];
        // Real "a" (61 00) 2 bytes past the boundary.
        data[SEARCH_CHUNK + 2] = 0x61;
        // Misaligned 61 00 straddling the boundary (offset SEARCH_CHUNK-1).
        data[SEARCH_CHUNK - 1] = 0x61;
        let doc = doc_with(&data);
        let n = Needle::Text("a".into())
            .prepare(Encoding::Utf16Le, &strict)
            .unwrap();
        let m = find_next(&doc, &n, 0, &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, SEARCH_CHUNK as u64 + 2);
        let m = find_prev(&doc, &n, doc.len(), &strict, &no_cancel)
            .unwrap()
            .unwrap();
        assert_eq!(m.offset, SEARCH_CHUNK as u64 + 2);
    }

    #[test]
    fn cancellation() {
        let doc = doc_with(&[b'x'; SEARCH_CHUNK * 3]);
        let n = Needle::Text("needle".into())
            .prepare(Encoding::Utf8, &SearchOptions::strict())
            .unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = find_next(&doc, &n, 0, &SearchOptions::strict(), &cancel).unwrap_err();
        assert!(matches!(err, SearchError::Cancelled));
    }

    #[test]
    fn goto_helpers() {
        let doc = doc_with(b"abc");
        assert_eq!(goto_offset(&doc, 99), 3);
        assert_eq!(goto_offset(&doc, 2), 2);
    }
}

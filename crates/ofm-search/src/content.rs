//! Per-file content search (private): line-oriented matching over decoded text.
//!
//! Decoding and detection are the viewer's ([`ofm_viewer::encoding`]): BOM/UTF-8/heuristics
//! via `detect_encoding`, per-line decoding via `decode_range` — this module never touches
//! encodings itself, it only splits raw bytes into lines and maps match positions back to
//! raw byte offsets so F3 can open the viewer at the exact spot.
//!
//! Structure: a [`LineSplitter`] assembles raw lines (and, for endless lines, overlapping
//! segments) from arbitrary chunk reads; each line is decoded and matched with a Unicode
//! `regex` (grep semantics: `^`/`$` anchor to the line, no match can span lines). The first
//! match is reported with its 1-based line number, raw byte offset and a ≤ 200-char snippet;
//! every match is counted (up to [`MAX_COUNT`], then the file is abandoned — the count is
//! capped, not the file).

use std::cell::{Cell, RefCell};
use std::io::{self, Read};
use std::ops::Range;

use ofm_core::CancelToken;
use ofm_viewer::document::Document;
use ofm_viewer::encoding::{Encoding, decode_range, detect_encoding};

use crate::compile::ContentSpec;
use crate::event::{ContentHit, ContentMatch};
use crate::query::SearchEncoding;

/// Bytes read per chunk from the file.
pub(crate) const DEFAULT_CHUNK: usize = 256 * 1024;
/// Raw bytes buffered for one line before it is matched in overlapping segments.
pub(crate) const DEFAULT_MAX_LINE: usize = 1024 * 1024;
/// Overlap between segments of an endless line; matches longer than this that span a
/// segment cut inside one huge line may be missed (same trade-off as the viewer's search).
const SEGMENT_OVERLAP: usize = 64 * 1024;
/// Match count per file is capped here.
pub(crate) const MAX_COUNT: u32 = 10_000;
/// Bytes inspected for the binary (NUL) check.
const HEAD_LEN: usize = 8 * 1024;
/// Chars of context on each side of the match in a snippet (200 "around the match" total).
const SNIPPET_AROUND: usize = 100;

/// Reads a [`Document`] sequentially (the search works on plain `Read` streams so archive
/// entries can use the same code path).
pub(crate) struct DocumentReader<'a> {
    doc: &'a Document,
    pos: u64,
}

impl<'a> DocumentReader<'a> {
    /// Wrap `doc`, reading from the start.
    pub(crate) fn new(doc: &'a Document) -> Self {
        DocumentReader { doc, pos: 0 }
    }
}

impl Read for DocumentReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.doc.read_at(self.pos, buf).map_err(io::Error::other)?;
        self.pos += n as u64;
        Ok(n)
    }
}

/// Assembles raw lines out of arbitrary chunk feeds. Encoding-aware: in UTF-16 modes line
/// breaks are the code units `0x0A`/`0x0D` (CR LF = one break), scanned at code-unit
/// alignment so a `0x0A` byte that is merely half of another code unit is not a break.
/// `\r\n`, `\n` and lone `\r` each terminate a line (viewer semantics).
///
/// A line longer than `max_line` bytes is matched in overlapping segments: the segment is
/// emitted as an unterminated "line" (same line number) and the last `keep` bytes are
/// re-scanned with the next chunk, so matches up to `keep` bytes long that straddle the cut
/// are still found.
pub(crate) struct LineSplitter {
    encoding: Encoding,
    utf16: bool,
    max_line: usize,
    /// Bytes kept as overlap between segments (whole code units in UTF-16 modes).
    keep: usize,
    /// Bytes of the current (unfinished) line.
    pending: Vec<u8>,
    /// Absolute file offset of `pending[0]`.
    pending_start: u64,
    /// How far into `pending` terminators have been resolved (a CR at the very end is left
    /// unresolved until the next chunk decides between CRLF and a lone CR).
    scanned: usize,
}

impl LineSplitter {
    /// New splitter for `encoding`; the first line starts at absolute offset `start`
    /// (i.e. after the BOM, which the caller strips).
    pub(crate) fn new(encoding: Encoding, max_line: usize, start: u64) -> LineSplitter {
        let utf16 = matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be);
        // UTF-16 needs even `max_line` (whole code units) and enough room for the
        // overlap: at least 2 units, so a window always advances.
        let max_line = if utf16 {
            max_line.max(4) & !1
        } else {
            max_line.max(2)
        };
        let mut keep = (max_line / 2).clamp(1, SEGMENT_OVERLAP);
        if utf16 {
            keep = keep.max(2) & !1;
        }
        LineSplitter {
            encoding,
            utf16,
            max_line,
            keep,
            pending: Vec::new(),
            pending_start: start,
            scanned: 0,
        }
    }

    /// Feed the next chunk; `f(start, raw_line, terminated)` is called for every completed
    /// line, and for segments of endless lines with `terminated == false`.
    pub(crate) fn push<F: FnMut(u64, &[u8], bool)>(&mut self, chunk: &[u8], f: &mut F) {
        self.pending.extend_from_slice(chunk);
        // (content_end, next_start) of each completed line, relative to `pending`.
        let mut cuts: Vec<(usize, usize)> = Vec::new();
        let limit = if self.utf16 {
            self.pending.len() & !1
        } else {
            self.pending.len()
        };
        let mut i = self.scanned;
        while i < limit {
            let advanced = if self.utf16 {
                self.scan_unit(i, limit, &mut cuts)
            } else {
                self.scan_byte(i, limit, &mut cuts)
            };
            match advanced {
                Some(next) => i = next,
                // None: a CR at the very end — wait for the next chunk.
                None => break,
            }
        }
        self.scanned = i;
        let consumed = cuts.last().map_or(0, |(_, next)| *next);
        let mut start = self.pending_start;
        let mut prev_next = 0usize;
        for (end, next) in &cuts {
            f(start, &self.pending[prev_next..*end], true);
            start += (*next - prev_next) as u64;
            prev_next = *next;
        }
        if consumed > 0 {
            self.pending.drain(..consumed);
            self.pending_start = start;
            self.scanned = self.scanned.saturating_sub(consumed);
        }
        // Endless line: match in windows of `max_line` bytes that overlap by `keep`
        // bytes, so a match straddling a window edge is found whole in the next window
        // (matches longer than `keep` inside one endless line may be missed). Whole-word
        // checks at a window's left edge see a line start; documented limitation.
        while self.pending.len() > self.max_line {
            let seg = if self.utf16 {
                self.max_line & !1
            } else {
                self.max_line
            };
            let drain = seg - self.keep;
            f(self.pending_start, &self.pending[..seg], false);
            self.pending.drain(..drain);
            self.pending_start += drain as u64;
            self.scanned = 0;
        }
    }

    /// Flush the final unterminated line (call once after the last chunk).
    pub(crate) fn finish<F: FnMut(u64, &[u8], bool)>(&mut self, f: &mut F) {
        if !self.pending.is_empty() {
            f(self.pending_start, &self.pending, false);
            self.pending.clear();
        }
        self.scanned = 0;
    }

    /// Resolve the terminator at/after `i` (byte mode). `Some(next)` = scanning continues
    /// at `next`; `None` = a trailing CR must wait for the next chunk.
    fn scan_byte(&self, i: usize, limit: usize, cuts: &mut Vec<(usize, usize)>) -> Option<usize> {
        let p = i + memchr::memchr2(b'\r', b'\n', &self.pending[i..limit])?;
        if self.pending[p] == b'\n' {
            cuts.push((p, p + 1));
            return Some(p + 1);
        }
        match self.pending.get(p + 1) {
            Some(b'\n') => {
                cuts.push((p, p + 2));
                Some(p + 2)
            }
            Some(_) => {
                cuts.push((p, p + 1));
                Some(p + 1)
            }
            None => None,
        }
    }

    /// Same in UTF-16 mode, at code-unit alignment.
    fn scan_unit(&self, i: usize, limit: usize, cuts: &mut Vec<(usize, usize)>) -> Option<usize> {
        let unit = self.unit_at(i);
        if unit == 0x0A {
            cuts.push((i, i + 2));
            return Some(i + 2);
        }
        if unit != 0x0D {
            return Some(i + 2);
        }
        if i + 2 < limit {
            if self.unit_at(i + 2) == 0x0A {
                cuts.push((i, i + 4));
                Some(i + 4)
            } else {
                cuts.push((i, i + 2));
                Some(i + 2)
            }
        } else {
            None
        }
    }

    fn unit_at(&self, i: usize) -> u16 {
        let b = [self.pending[i], self.pending[i + 1]];
        if self.encoding == Encoding::Utf16Le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        }
    }
}

/// Search one file (or archive entry) read through `src`.
///
/// Returns `Ok(None)` for no match, a binary file skipped, or a cancelled search (a
/// cancelled file simply has no hit). Read errors abort the file as `Err`.
pub(crate) fn search_stream<R: Read + ?Sized>(
    src: &mut R,
    spec: &ContentSpec,
    cancel: &CancelToken,
) -> io::Result<Option<ContentHit>> {
    // The closure below outlives the loop body, so shared state goes through cells.
    let first: RefCell<Option<ContentMatch>> = RefCell::new(None);
    let count = Cell::new(0u32);
    let counted_upto = Cell::new(0u64); // raw end of the last counted match (overlap dedup)
    let line_no = Cell::new(0u64);
    let encoding = Cell::new(Encoding::Utf8);

    let mut on_line = |start: u64, raw: &[u8], terminated: bool| {
        if count.get() >= MAX_COUNT {
            return;
        }
        let text = decode_range(encoding.get(), raw);
        for m in spec.re.find_iter(&text) {
            if spec.whole_words && !is_whole_word(&text, m.start()..m.end()) {
                continue;
            }
            let raw_start = start.saturating_add(raw_offset(encoding.get(), raw, &text, m.start()));
            if raw_start < counted_upto.get() {
                continue; // re-found in the overlap tail of a previous segment
            }
            let raw_end = start.saturating_add(raw_offset(encoding.get(), raw, &text, m.end()));
            counted_upto.set(raw_end);
            if count.get() == 0 {
                let (snippet, snippet_match) = build_snippet(&text, m.start()..m.end());
                first.replace(Some(ContentMatch {
                    line: line_no.get() + 1,
                    byte_offset: raw_start,
                    snippet,
                    snippet_match,
                }));
            }
            count.set(count.get() + 1);
            if count.get() >= MAX_COUNT {
                break;
            }
        }
        if terminated {
            line_no.set(line_no.get() + 1);
        }
    };

    let mut splitter: Option<LineSplitter> = None;
    let mut buf = vec![0u8; spec.chunk.max(64)];
    loop {
        if cancel.is_cancelled() {
            return Ok(None);
        }
        let n = src.read(&mut buf)?;
        if n == 0 {
            if let Some(s) = splitter.as_mut() {
                s.finish(&mut on_line);
            }
            break;
        }
        let data = &buf[..n];
        match splitter.as_mut() {
            None => {
                let head = &data[..n.min(HEAD_LEN)];
                let (enc, bom_len) = match spec.encoding {
                    SearchEncoding::Auto => {
                        let d = detect_encoding(head);
                        (d.encoding, d.bom_len)
                    }
                    SearchEncoding::Fixed(e) => (e, bom_len_of(e, head)),
                };
                encoding.set(enc);
                if !spec.search_binary
                    && !matches!(enc, Encoding::Utf16Le | Encoding::Utf16Be)
                    && head.contains(&0)
                {
                    return Ok(None); // looks binary
                }
                let skip = bom_len.min(n);
                let mut s = LineSplitter::new(enc, spec.max_line, skip as u64);
                s.push(&data[skip..], &mut on_line);
                splitter = Some(s);
            }
            Some(s) => s.push(data, &mut on_line),
        }
        if count.get() >= MAX_COUNT {
            break;
        }
    }
    let count = count.into_inner();
    Ok(if count == 0 {
        None
    } else {
        let first = first.into_inner();
        // `first` is always set with the first counted match.
        first.map(|f| ContentHit { first: f, count })
    })
}

/// Search an opened [`Document`].
pub(crate) fn search_document(
    doc: &Document,
    spec: &ContentSpec,
    cancel: &CancelToken,
) -> io::Result<Option<ContentHit>> {
    let mut reader = DocumentReader::new(doc);
    search_stream(&mut reader, spec, cancel)
}

/// BOM length for a fixed encoding (only UTF-8/UTF-16 have one we honour).
fn bom_len_of(encoding: Encoding, head: &[u8]) -> usize {
    match encoding {
        Encoding::Utf8 if head.starts_with(&[0xEF, 0xBB, 0xBF]) => 3,
        Encoding::Utf16Le if head.starts_with(&[0xFF, 0xFE]) => 2,
        Encoding::Utf16Be if head.starts_with(&[0xFE, 0xFF]) => 2,
        _ => 0,
    }
}

/// Map a byte offset inside the decoded `text` back to a raw byte offset inside `raw`
/// (relative, 0-based). Exact for every supported encoding; for UTF-8 with invalid bytes
/// the walk mirrors the WHATWG decoder's maximal-subpart segmentation (one U+FFFD per
/// invalid subsequence, a trailing partial sequence dropped).
fn raw_offset(encoding: Encoding, raw: &[u8], text: &str, target: usize) -> u64 {
    match encoding {
        Encoding::Utf8 => raw_offset_utf8(raw, target) as u64,
        Encoding::Utf16Le | Encoding::Utf16Be => {
            // 2 raw bytes per UTF-16 code unit; surrogate pairs cover 4 bytes / 1 char.
            let mut units = 0usize;
            for (i, ch) in text.char_indices() {
                if i >= target {
                    break;
                }
                units += ch.len_utf16();
            }
            units as u64 * 2
        }
        // Single-byte encodings: every raw byte decodes to exactly one char.
        _ => text[..target].chars().count() as u64,
    }
}

fn raw_offset_utf8(raw: &[u8], target: usize) -> usize {
    if std::str::from_utf8(raw).is_ok() {
        return target; // identity: no replacement chars were produced
    }
    let mut ri = 0usize; // raw index
    let mut di = 0usize; // decoded byte index
    while di < target {
        match std::str::from_utf8(&raw[ri..]) {
            Ok(_) => return ri + (target - di), // rest is valid: identity from here
            Err(e) => {
                let v = e.valid_up_to();
                if di + v >= target {
                    return ri + (target - di);
                }
                di += v;
                ri += v;
                match e.error_len() {
                    Some(n) => {
                        di += 3; // one U+FFFD (3 bytes) per invalid subsequence
                        ri += n;
                    }
                    // Truncated sequence at the end: dropped by `decode_range`.
                    None => return raw.len(),
                }
            }
        }
    }
    ri
}

/// Trim `text` around `m` to at most [`SNIPPET_AROUND`] chars on each side; returns the
/// snippet and the byte range of the match inside it (for highlighting).
fn build_snippet(text: &str, m: Range<usize>) -> (String, Range<usize>) {
    let start = text[..m.start]
        .char_indices()
        .rev()
        .take(SNIPPET_AROUND)
        .last()
        .map_or(m.start, |(i, _)| i);
    let mut end = m.end;
    for (n, (i, c)) in text[m.end..].char_indices().enumerate() {
        if n >= SNIPPET_AROUND {
            break;
        }
        end = m.end + i + c.len_utf8();
    }
    (text[start..end].to_owned(), m.start - start..m.end - start)
}

/// Grep `-w` semantics: word characters (Unicode alphanumeric + `_`) may not touch the
/// match from either side (line edges count as boundaries).
fn is_whole_word(text: &str, m: Range<usize>) -> bool {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let before = text[..m.start]
        .chars()
        .next_back()
        .is_none_or(|c| !is_word(c));
    let after = text[m.end..].chars().next().is_none_or(|c| !is_word(c));
    before && after
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::query::{ContentMode, ContentQuery};
    use std::io::Cursor;

    fn query(pattern: &str) -> ContentQuery {
        ContentQuery {
            pattern: pattern.into(),
            mode: ContentMode::Text,
            case_sensitive: true,
            whole_words: false,
            encoding: SearchEncoding::Auto,
            search_binary: false,
        }
    }

    fn spec_of(q: &ContentQuery) -> ContentSpec {
        ContentSpec::new(q).unwrap()
    }

    fn collect_lines(
        data: &[u8],
        encoding: Encoding,
        chunk: usize,
        max_line: usize,
    ) -> Vec<(u64, Vec<u8>, bool)> {
        let mut s = LineSplitter::new(encoding, max_line, 0);
        let mut out = Vec::new();
        for c in data.chunks(chunk) {
            s.push(c, &mut |start, bytes, term| {
                out.push((start, bytes.to_vec(), term));
            });
        }
        s.finish(&mut |start, bytes, term| out.push((start, bytes.to_vec(), term)));
        out
    }

    #[test]
    fn splitter_byte_mode() {
        let lines = collect_lines(b"one\ntwo\r\nthree\rfour", Encoding::Utf8, 1000, 1 << 20);
        let texts: Vec<&[u8]> = lines.iter().map(|(_, b, _)| b.as_slice()).collect();
        assert_eq!(texts, vec![&b"one"[..], b"two", b"three", b"four"]);
        assert!(lines.iter().take(3).all(|(_, _, t)| *t));
        assert!(!lines[3].2); // final line, no terminator
        assert_eq!(lines[0].0, 0);
        assert_eq!(lines[1].0, 4); // after \n
        assert_eq!(lines[2].0, 9); // after \r\n
        assert_eq!(lines[3].0, 15); // after lone \r
    }

    #[test]
    fn splitter_crlf_split_across_chunks() {
        for split in 1..b"a\r\nb".len() {
            let lines = collect_lines(b"a\r\nb", Encoding::Utf8, split, 1 << 20);
            let texts: Vec<&[u8]> = lines.iter().map(|(_, b, _)| b.as_slice()).collect();
            assert_eq!(texts, vec![&b"a"[..], b"b"], "split at {split}");
        }
    }

    #[test]
    fn splitter_utf16_crlf_and_alignment() {
        let enc = |s: &str| {
            s.encode_utf16()
                .flat_map(|u| u.to_le_bytes())
                .collect::<Vec<u8>>()
        };
        let lines = collect_lines(&enc("a\r\nb"), Encoding::Utf16Le, 1000, 1 << 20);
        let texts: Vec<String> = lines
            .iter()
            .map(|(_, b, _)| decode_range(Encoding::Utf16Le, b).into_owned())
            .collect();
        assert_eq!(texts, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(lines[1].0, 6); // "a" (1 unit) + CRLF (2 units) = 3 units = 6 bytes

        // Odd chunk feeds keep code-unit parity.
        let lines = collect_lines(&enc("a\rb"), Encoding::Utf16Le, 3, 1 << 20);
        let texts: Vec<String> = lines
            .iter()
            .map(|(_, b, _)| decode_range(Encoding::Utf16Le, b).into_owned())
            .collect();
        assert_eq!(texts, vec!["a".to_string(), "b".to_string()]);

        // U+0A43 contains byte 0x0A as its LOW byte, but is not a line break.
        let tricky: Vec<u8> = [0x0A43u16, 0x4E00, 0x4E00]
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let lines = collect_lines(&tricky, Encoding::Utf16Le, 2, 1 << 20);
        assert_eq!(lines.len(), 1, "0x0A inside a code unit is not a break");
    }

    #[test]
    fn splitter_segments_tile_the_line() {
        // max_line 8 with an endless line: segments must tile the input exactly and the
        // final segment must reach EOF.
        let data = b"0123456789abcdefgh";
        let lines = collect_lines(data, Encoding::Utf8, 4, 8);
        assert!(lines.iter().all(|(_, _, t)| !t));
        for (start, bytes, _) in &lines {
            assert_eq!(
                &data[*start as usize..*start as usize + bytes.len()],
                &bytes[..]
            );
        }
        assert!(
            lines
                .last()
                .is_some_and(|(s, b, _)| (*s as usize) + b.len() == data.len())
        );
    }

    #[test]
    fn finds_matches_and_offsets() {
        let spec = spec_of(&query("needle"));
        let data = b"filler line\nthe needle is here\nnope\nneedle at start\n";
        let hit = search_stream(&mut Cursor::new(data), &spec, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.count, 2);
        assert_eq!(hit.first.line, 2);
        assert_eq!(hit.first.byte_offset, 12 + 4); // line 2 starts at 12, "needle" at +4
        assert_eq!(hit.first.snippet, "the needle is here");
        assert_eq!(
            &hit.first.snippet[hit.first.snippet_match.clone()],
            "needle"
        );
    }

    #[test]
    fn match_straddling_chunk_boundary() {
        let mut spec = spec_of(&query("boundary"));
        spec.chunk = 8;
        let mut data = Vec::new();
        data.extend_from_slice(&[b'x'; 100]);
        data.extend_from_slice(b"|boundary|");
        data.extend_from_slice(&[b'y'; 100]);
        let hit = search_stream(&mut Cursor::new(&data), &spec, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.first.byte_offset, 101);
        assert_eq!(hit.count, 1);
    }

    #[test]
    fn match_in_segment_of_huge_line() {
        let mut spec = spec_of(&query("deep"));
        spec.max_line = 16; // forces segments
        let mut data = Vec::new();
        data.extend_from_slice(&[b' '; 100]);
        data.extend_from_slice(b"deep");
        data.extend_from_slice(&[b' '; 100]);
        let hit = search_stream(&mut Cursor::new(&data), &spec, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.first.byte_offset, 100);
        assert_eq!(hit.first.line, 1);
        assert_eq!(hit.count, 1, "counted once despite the overlap re-scan");
    }

    #[test]
    fn count_cap_and_non_overlap() {
        let mut spec = spec_of(&query("ab"));
        spec.chunk = 64;
        let data: Vec<u8> = std::iter::repeat_n(b"ab".as_slice(), 20_000)
            .flatten()
            .copied()
            .collect();
        let hit = search_stream(&mut Cursor::new(&data), &spec, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.count, MAX_COUNT);
        assert_eq!(hit.first.byte_offset, 0);

        // find_iter is non-overlapping: "aaaa" holds two "aa" matches.
        let spec = spec_of(&query("aa"));
        let hit = search_stream(&mut Cursor::new(b"aaaa"), &spec, &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.count, 2);
    }

    #[test]
    fn binary_files_are_skipped_unless_asked() {
        let data = b"text with\x00nul\nneedle\n";
        let s = spec_of(&query("needle"));
        assert!(
            search_stream(&mut Cursor::new(data), &s, &CancelToken::new())
                .unwrap()
                .is_none()
        );
        let mut with_binary = spec_of(&query("needle"));
        with_binary.search_binary = true;
        assert!(
            search_stream(&mut Cursor::new(data), &with_binary, &CancelToken::new())
                .unwrap()
                .is_some()
        );
        // UTF-16 NULs are structural, not binary.
        let utf16: Vec<u8> = "needle\n"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        assert!(
            search_stream(&mut Cursor::new(&utf16), &s, &CancelToken::new())
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn utf16_file_with_bom() {
        let mut data = vec![0xFF, 0xFE];
        data.extend(
            "alpha\nNEEDLE beta\n"
                .encode_utf16()
                .flat_map(|u| u.to_le_bytes()),
        );
        let q = ContentQuery {
            case_sensitive: false,
            ..query("needle")
        };
        let hit = search_stream(&mut Cursor::new(&data), &spec_of(&q), &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.first.line, 2);
        // Line 2 starts after BOM (2) + "alpha\n" (6 units = 12 bytes) = 14.
        assert_eq!(hit.first.byte_offset, 14);
    }

    #[test]
    fn fixed_single_byte_encoding() {
        let (encoded, _, _) = encoding_rs::WINDOWS_1251.encode("xx привет yy\n");
        let q = ContentQuery {
            encoding: SearchEncoding::Fixed(Encoding::Windows1251),
            ..query("привет")
        };
        let hit = search_stream(
            &mut Cursor::new(&encoded),
            &spec_of(&q),
            &CancelToken::new(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(hit.first.byte_offset, 3); // "xx " = 3 single-byte chars
        assert_eq!(hit.first.snippet, "xx привет yy");
    }

    #[test]
    fn invalid_utf8_offsets() {
        // 0xE9 alone is invalid UTF-8 and decodes to one U+FFFD; "xyz" starts at raw 5
        // (c0 a1 f2, 0xE9=3, ' '=4, 'x'=5) but decoded byte 7.
        let data = b"caf\xE9 xyz";
        let q = ContentQuery {
            encoding: SearchEncoding::Fixed(Encoding::Utf8),
            ..query("xyz")
        };
        let hit = search_stream(&mut Cursor::new(data), &spec_of(&q), &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.first.byte_offset, 5);
    }

    #[test]
    fn whole_words() {
        let q = ContentQuery {
            whole_words: true,
            ..query("cat")
        };
        let data = b"cat catalog bobcat cat.\n";
        let hit = search_stream(&mut Cursor::new(data), &spec_of(&q), &CancelToken::new())
            .unwrap()
            .unwrap();
        assert_eq!(hit.count, 2);
        assert_eq!(hit.first.byte_offset, 0);
    }

    #[test]
    fn snippet_is_trimmed() {
        let long = "a".repeat(500);
        let line = format!("{long} needle {long}\n");
        let hit = search_stream(
            &mut Cursor::new(line.as_bytes()),
            &spec_of(&query("needle")),
            &CancelToken::new(),
        )
        .unwrap()
        .unwrap();
        assert!(hit.first.snippet.chars().count() <= 200 + "needle".len());
        assert!(hit.first.snippet.contains("needle"));
        assert_eq!(
            &hit.first.snippet[hit.first.snippet_match.clone()],
            "needle"
        );
    }

    #[test]
    fn cancellation_returns_none() {
        let cancel = CancelToken::new();
        cancel.cancel();
        assert!(
            search_stream(
                &mut Cursor::new(b"needle\n"),
                &spec_of(&query("needle")),
                &cancel
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn raw_offset_mapping_cases() {
        // UTF-8 identity.
        assert_eq!(
            raw_offset(Encoding::Utf8, "héllo".as_bytes(), "héllo", 2),
            2
        );
        // Single-byte: char index == raw byte index ("привет": 'и' starts at decoded 4).
        let (enc1251, _, _) = encoding_rs::WINDOWS_1251.encode("привет");
        let text = decode_range(Encoding::Windows1251, &enc1251);
        assert_eq!(raw_offset(Encoding::Windows1251, &enc1251, &text, 4), 2);
        // UTF-16: 2 bytes per BMP unit, 4 for a surrogate pair.
        let text = "\u{1F600}ab"; // emoji = 4 UTF-8 bytes / 2 units, then 'a', 'b'
        assert_eq!(raw_offset(Encoding::Utf16Le, &[], text, 4), 4); // after the emoji
        assert_eq!(raw_offset(Encoding::Utf16Le, &[], text, 5), 6); // after 'a'
        // UTF-8 with an invalid byte: 0xE9 -> one U+FFFD (decoded "caf\u{FFFD} xy").
        assert_eq!(raw_offset_utf8(b"caf\xE9 xy", 6), 4); // ' ' after the FFFD
        assert_eq!(raw_offset_utf8(b"caf\xE9 xy", 7), 5); // 'x'
    }

    #[test]
    fn empty_file_has_no_match() {
        assert!(
            search_stream(
                &mut Cursor::new(b""),
                &spec_of(&query("x")),
                &CancelToken::new()
            )
            .unwrap()
            .is_none()
        );
    }
}

/// Property tests: the chunked scan must agree with a direct whole-buffer scan. The
/// reference is computed independently (plain `find_iter` over the whole text), so this
/// catches line-assembly, line counting and offset-mapping bugs.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod proptests {
    use super::*;
    use crate::query::{ContentMode, ContentQuery};
    use proptest::prelude::*;
    use std::io::Cursor;

    fn spec_with(pattern: &str, chunk: usize, max_line: usize) -> ContentSpec {
        let q = ContentQuery {
            pattern: pattern.into(),
            mode: ContentMode::Text,
            case_sensitive: true,
            ..ContentQuery::text(pattern)
        };
        let mut s = ContentSpec::new(&q).unwrap();
        s.chunk = chunk;
        s.max_line = max_line;
        s
    }

    /// (first byte offset, line, count) computed directly from the whole text.
    fn reference(text: &str, needle: &str) -> Option<(u64, u64, u32)> {
        let re = regex::Regex::new(&regex::escape(needle)).unwrap();
        let mut first = None;
        let mut count = 0u32;
        for m in re.find_iter(text) {
            if first.is_none() {
                let line = 1 + u64::try_from(text[..m.start()].matches('\n').count()).unwrap_or(0);
                first = Some((m.start() as u64, line));
            }
            count += 1;
        }
        first.map(|(o, l)| (o, l, count))
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(150))]

        #[test]
        fn chunked_scan_matches_reference(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec!["a".to_owned(), "ab".to_owned(), "xyz".to_owned(), "\n".to_owned(), "q".to_owned(), "bb".to_owned()]), 0..60),
            needle in proptest::sample::select(vec!["ab".to_owned(), "xyz".to_owned(), "q".to_owned()]),
            chunk in 1usize..17,
        ) {
            let data: String = pieces.concat();
            let spec = spec_with(&needle, chunk, DEFAULT_MAX_LINE);
            let got = search_stream(&mut Cursor::new(data.as_bytes()), &spec, &CancelToken::new()).unwrap();
            match reference(&data, &needle) {
                None => prop_assert!(got.is_none(), "no match expected in {data:?}"),
                Some((off, line, count)) => {
                    let hit = got.expect("match expected");
                    prop_assert_eq!(hit.first.byte_offset, off);
                    prop_assert_eq!(hit.first.line, line);
                    prop_assert_eq!(hit.count, count);
                }
            }
        }

        #[test]
        fn segmented_scan_of_endless_lines(
            pieces in proptest::collection::vec(
                proptest::sample::select(vec!["a".to_owned(), "ab".to_owned(), "xyz".to_owned(), " ".to_owned(), "q".to_owned()]), 0..80),
            needle in proptest::sample::select(vec!["ab".to_owned(), "xyz".to_owned()]),
            chunk in 1usize..13,
        ) {
            // No newlines: one endless line, matched in segments (max_line 32, keep 16;
            // needles are shorter than the overlap).
            let data: String = pieces.concat();
            let spec = spec_with(&needle, chunk, 32);
            let got = search_stream(&mut Cursor::new(data.as_bytes()), &spec, &CancelToken::new()).unwrap();
            match reference(&data, &needle) {
                None => prop_assert!(got.is_none(), "no match expected in {data:?}"),
                Some((off, _line, count)) => {
                    let hit = got.expect("match expected");
                    prop_assert_eq!(hit.first.byte_offset, off);
                    prop_assert_eq!(hit.first.line, 1);
                    prop_assert_eq!(hit.count, count, "segments must neither lose nor double-count matches");
                }
            }
        }
    }
}

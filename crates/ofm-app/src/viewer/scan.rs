//! Encoding-aware line-break scanning and a checkpointed line counter (no GTK).
//!
//! Line breaks are `\n`, `\r\n` and lone `\r` (as in `ofm-viewer`), but recognised per code unit
//! so UTF-16 files work too (`ofm_viewer::LineIndex` scans raw bytes and therefore miscounts
//! UTF-16). The counter keeps only one number per 4 MiB chunk, so a multi-gigabyte file costs
//! kilobytes, and answers "line at offset" / "offset of line" by scanning a single chunk.

use std::sync::Mutex;

use ofm_core::CancelToken;
use ofm_viewer::{Document, DocumentError, Encoding};

/// Bytes per counter checkpoint.
pub const COUNT_CHUNK: u64 = 4 * 1024 * 1024;
/// Extra bytes read behind a chunk so a `\r` at its end can see a following `\n`.
const LOOKAHEAD: usize = 8;

/// Class of one code unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    /// `\r`
    Cr,
    /// `\n`
    Lf,
}

/// Result of [`find_break`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Brk {
    /// A terminator: the line content ends at `content_end`, the next line starts at `next`.
    Found {
        /// Index of the first byte of the terminator.
        content_end: usize,
        /// Index just past the terminator.
        next: usize,
    },
    /// The buffer ends in a `\r` that might pair with a `\n` not read yet.
    NeedMore,
    /// No terminator in the buffer.
    None,
}

/// Which counter checkpoints are compatible (UTF-8 and the single-byte encodings share one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// One byte per unit.
    Byte,
    /// UTF-16 little endian.
    U16Le,
    /// UTF-16 big endian.
    U16Be,
}

/// Counter family of `enc`.
pub fn family(enc: Encoding) -> Family {
    match enc {
        Encoding::Utf16Le => Family::U16Le,
        Encoding::Utf16Be => Family::U16Be,
        _ => Family::Byte,
    }
}

/// Class of the unit starting at byte `i` of `buf` (`None` for any other character or an
/// incomplete unit).
pub fn unit_at(enc: Encoding, buf: &[u8], i: usize) -> Option<Unit> {
    let (lo, hi) = match family(enc) {
        Family::Byte => (*buf.get(i)?, 0u8),
        Family::U16Le => (*buf.get(i)?, *buf.get(i + 1)?),
        Family::U16Be => (*buf.get(i + 1)?, *buf.get(i)?),
    };
    if hi != 0 {
        return None;
    }
    match lo {
        b'\n' => Some(Unit::Lf),
        b'\r' => Some(Unit::Cr),
        _ => None,
    }
}

/// Start index of the first terminator unit at or after `from` (aligned to the unit size).
fn next_candidate(enc: Encoding, buf: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i < buf.len() {
        let q = i + memchr::memchr2(b'\r', b'\n', &buf[i..])?;
        match family(enc) {
            Family::Byte => return Some(q),
            Family::U16Le => {
                if q.is_multiple_of(2) && unit_at(enc, buf, q).is_some() {
                    return Some(q);
                }
            }
            Family::U16Be => {
                if q % 2 == 1 && unit_at(enc, buf, q - 1).is_some() {
                    return Some(q - 1);
                }
            }
        }
        i = q + 1;
    }
    None
}

/// First terminator in `buf` at or after `from` (`from` must be unit-aligned). `at_eof` says the
/// buffer ends at the end of the file, so a trailing `\r` is a lone terminator.
pub fn find_break(enc: Encoding, buf: &[u8], from: usize, at_eof: bool) -> Brk {
    let u = enc.bytes_per_unit();
    let Some(p) = next_candidate(enc, buf, from) else {
        return Brk::None;
    };
    match unit_at(enc, buf, p) {
        Some(Unit::Lf) => Brk::Found {
            content_end: p,
            next: p + u,
        },
        Some(Unit::Cr) => {
            if p + 2 * u <= buf.len() {
                let next = if unit_at(enc, buf, p + u) == Some(Unit::Lf) {
                    p + 2 * u
                } else {
                    p + u
                };
                Brk::Found {
                    content_end: p,
                    next,
                }
            } else if at_eof {
                Brk::Found {
                    content_end: p,
                    next: p + u,
                }
            } else {
                Brk::NeedMore
            }
        }
        None => Brk::None,
    }
}

/// Start index of the last terminator unit lying entirely inside `buf[..end]`.
pub fn rfind_break(enc: Encoding, buf: &[u8], end: usize) -> Option<usize> {
    let mut end = end.min(buf.len());
    while end > 0 {
        let q = memchr::memrchr2(b'\r', b'\n', &buf[..end])?;
        match family(enc) {
            Family::Byte => return Some(q),
            Family::U16Le => {
                if q.is_multiple_of(2) && q + 1 < end && unit_at(enc, buf, q).is_some() {
                    return Some(q);
                }
            }
            Family::U16Be => {
                if q % 2 == 1 && unit_at(enc, buf, q - 1).is_some() {
                    return Some(q - 1);
                }
            }
        }
        end = q;
    }
    None
}

/// Terminators starting before `limit` in `buf` (which holds the chunk plus lookahead).
/// `skip_lf`: the unit before the chunk was a `\r`, so a leading `\n` is part of its terminator.
/// Returns the count and the index just past the last counted terminator.
fn count_in(
    enc: Encoding,
    buf: &[u8],
    limit: usize,
    skip_lf: bool,
    at_eof: bool,
) -> (u64, Option<usize>) {
    let u = enc.bytes_per_unit();
    let mut i = 0;
    if skip_lf && unit_at(enc, buf, 0) == Some(Unit::Lf) {
        i = u;
    }
    let (mut n, mut last) = (0u64, None);
    while let Brk::Found { content_end, next } = find_break(enc, buf, i, at_eof) {
        if content_end >= limit {
            break;
        }
        n += 1;
        last = Some(next);
        i = next;
    }
    (n, last)
}

/// Checkpointed line counts of one file for one encoding family.
#[derive(Debug)]
pub struct LineCounts {
    /// Unit layout the numbers are valid for.
    pub family: Family,
    /// `before[k]`: terminators starting before byte `k * COUNT_CHUNK`; one entry per chunk whose
    /// scan has begun.
    pub before: Vec<u64>,
    /// Terminators in the fully scanned chunks.
    pub terminators: u64,
    /// File length when scanning started.
    pub len: u64,
    /// Offset just past the last terminator (`len` = the file ends with a terminator).
    pub last_term_end: u64,
    /// The whole file was scanned.
    pub complete: bool,
}

impl LineCounts {
    /// Empty counts for a file of `len` bytes.
    pub fn new(family: Family, len: u64) -> Self {
        LineCounts {
            family,
            before: Vec::new(),
            terminators: 0,
            len,
            last_term_end: 0,
            complete: false,
        }
    }

    /// Exact number of lines, once the scan is complete.
    pub fn total_lines(&self) -> Option<u64> {
        self.complete
            .then(|| self.terminators + u64::from(self.len > 0 && self.last_term_end != self.len))
    }
}

/// Peek whether the unit before `off` is a `\r`.
fn prev_is_cr(doc: &Document, enc: Encoding, off: u64) -> Result<bool, DocumentError> {
    let u = enc.bytes_per_unit();
    if off < u as u64 {
        return Ok(false);
    }
    let b = doc.read_range(off - u as u64, u)?;
    Ok(unit_at(enc, &b, 0) == Some(Unit::Cr))
}

/// Scan the whole document into `shared` (worker thread). Reports bytes scanned to `progress`
/// after every chunk and checks `cancel` between chunks; a cancelled scan leaves
/// `complete == false`.
pub fn build_counts(
    doc: &Document,
    enc: Encoding,
    shared: &Mutex<LineCounts>,
    cancel: &CancelToken,
    mut progress: impl FnMut(u64),
) -> Result<(), DocumentError> {
    let len = doc.len();
    let mut off = 0u64;
    while off < len {
        if cancel.is_cancelled() {
            return Ok(());
        }
        if let Ok(mut c) = shared.lock() {
            let t = c.terminators;
            c.before.push(t);
        }
        let take = COUNT_CHUNK.min(len - off) as usize;
        let buf = doc.read_range(off, take + LOOKAHEAD)?;
        let at_eof = off + buf.len() as u64 >= len;
        let skip = prev_is_cr(doc, enc, off)?;
        let (n, last) = count_in(enc, &buf, take, skip, at_eof);
        if let Ok(mut c) = shared.lock() {
            c.terminators += n;
            if let Some(l) = last {
                c.last_term_end = off + l as u64;
            }
        }
        off += take as u64;
        progress(off);
    }
    if let Ok(mut c) = shared.lock() {
        c.complete = true;
    }
    Ok(())
}

/// 0-based line number of `offset`, if the chunk holding it has been scanned.
pub fn line_number_at(
    doc: &Document,
    enc: Encoding,
    shared: &Mutex<LineCounts>,
    offset: u64,
) -> Result<Option<u64>, DocumentError> {
    let k = (offset / COUNT_CHUNK) as usize;
    let base = {
        let Ok(c) = shared.lock() else {
            return Ok(None);
        };
        if c.family != family(enc) {
            return Ok(None);
        }
        match c.before.get(k) {
            Some(b) => *b,
            None => return Ok(None),
        }
    };
    let start = k as u64 * COUNT_CHUNK;
    let take = (offset - start) as usize;
    let buf = doc.read_range(start, take + LOOKAHEAD)?;
    let at_eof = start + buf.len() as u64 >= doc.len();
    let skip = prev_is_cr(doc, enc, start)?;
    let (n, _) = count_in(enc, &buf, take, skip, at_eof);
    Ok(Some(base + n))
}

/// Offset where 0-based line `n` starts (the end of the `n`-th terminator), if known.
pub fn offset_of_line(
    doc: &Document,
    enc: Encoding,
    shared: &Mutex<LineCounts>,
    n: u64,
) -> Result<Option<u64>, DocumentError> {
    if n == 0 {
        return Ok(Some(0));
    }
    let (k, base) = {
        let Ok(c) = shared.lock() else {
            return Ok(None);
        };
        if c.family != family(enc) {
            return Ok(None);
        }
        let first_ge = c.before.partition_point(|&b| b < n);
        if first_ge == 0 {
            return Ok(None);
        }
        (first_ge - 1, c.before[first_ge - 1])
    };
    let start = k as u64 * COUNT_CHUNK;
    let take = COUNT_CHUNK.min(doc.len().saturating_sub(start)) as usize;
    let buf = doc.read_range(start, take + LOOKAHEAD)?;
    let at_eof = start + buf.len() as u64 >= doc.len();
    let skip = prev_is_cr(doc, enc, start)?;
    let u = enc.bytes_per_unit();
    let mut i = if skip && unit_at(enc, &buf, 0) == Some(Unit::Lf) {
        u
    } else {
        0
    };
    let mut seen = base;
    while let Brk::Found { content_end, next } = find_break(enc, &buf, i, at_eof) {
        if content_end >= take {
            break;
        }
        seen += 1;
        if seen == n {
            return Ok(Some(start + next as u64));
        }
        i = next;
    }
    Ok(None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::viewer::testutil::{doc_with, utf16};

    #[test]
    fn find_break_variants() {
        let e = Encoding::Utf8;
        let found = |content_end, next| Brk::Found { content_end, next };
        assert_eq!(find_break(e, b"ab\ncd", 0, true), found(2, 3));
        assert_eq!(find_break(e, b"ab\r\ncd", 0, true), found(2, 4));
        assert_eq!(find_break(e, b"ab\rcd", 0, true), found(2, 3));
        assert_eq!(find_break(e, b"ab\r", 0, false), Brk::NeedMore);
        assert_eq!(find_break(e, b"ab\r", 0, true), found(2, 3));
        assert_eq!(find_break(e, b"abc", 0, true), Brk::None);
    }

    #[test]
    fn find_break_utf16_ignores_misaligned_newline_bytes() {
        let found = |content_end, next| Brk::Found { content_end, next };
        // U+0A0A is 0A 0A in LE: no terminator although the bytes contain 0x0A.
        let le = utf16("a\u{0A0A}b\nc", false);
        assert_eq!(find_break(Encoding::Utf16Le, &le, 0, true), found(6, 8));
        let be = utf16("a\r\nb", true);
        assert_eq!(find_break(Encoding::Utf16Be, &be, 0, true), found(2, 6));
        // BE: 0x0A in the low byte of U+010A must not match.
        let be2 = utf16("\u{010A}x\n", true);
        assert_eq!(find_break(Encoding::Utf16Be, &be2, 0, true), found(4, 6));
    }

    #[test]
    fn rfind_break_works_in_every_family() {
        assert_eq!(rfind_break(Encoding::Utf8, b"ab\ncd\r", 6), Some(5));
        assert_eq!(rfind_break(Encoding::Utf8, b"abcd", 4), None);
        let le = utf16("ab\ncd", false);
        assert_eq!(rfind_break(Encoding::Utf16Le, &le, le.len()), Some(4));
        let be = utf16("ab\ncd", true);
        assert_eq!(rfind_break(Encoding::Utf16Be, &be, be.len()), Some(4));
        let le2 = utf16("\u{0A0A}", false);
        assert_eq!(rfind_break(Encoding::Utf16Le, &le2, le2.len()), None);
    }

    fn count_all(
        data: &[u8],
        enc: Encoding,
    ) -> (Mutex<LineCounts>, Document, ofm_testkit::temp::TempDir) {
        let (dir, document) = doc_with(data);
        let shared = Mutex::new(LineCounts::new(family(enc), document.len()));
        build_counts(&document, enc, &shared, &CancelToken::new(), |_| {}).unwrap();
        (shared, document, dir)
    }

    #[test]
    fn total_lines_matches_conventions() {
        for (data, lines) in [
            (&b""[..], 0),
            (b"a", 1),
            (b"a\n", 1),
            (b"a\nb", 2),
            (b"a\r\nb\r\n", 2),
            (b"a\rb\rc", 3),
            (b"\n\n\n", 3),
            (b"a\r\n\r\nb", 3),
        ] {
            let (c, _, _k) = count_all(data, Encoding::Utf8);
            assert_eq!(
                c.lock().unwrap().total_lines(),
                Some(lines),
                "{:?}",
                String::from_utf8_lossy(data)
            );
        }
        let (c, _, _k) = count_all(&utf16("a\r\nb\r\nc", false), Encoding::Utf16Le);
        assert_eq!(c.lock().unwrap().total_lines(), Some(3));
        let (c, _, _k) = count_all(&utf16("a\nb\n", true), Encoding::Utf16Be);
        assert_eq!(c.lock().unwrap().total_lines(), Some(2));
    }

    #[test]
    fn line_at_offset_and_offset_of_line_roundtrip() {
        let text = "one\ntwo\r\nthree\rfour\n\nsix";
        let (c, d, _k) = count_all(text.as_bytes(), Encoding::Utf8);
        let starts = [0u64, 4, 9, 15, 20, 21];
        for (n, s) in starts.iter().enumerate() {
            assert_eq!(
                offset_of_line(&d, Encoding::Utf8, &c, n as u64).unwrap(),
                Some(*s),
                "line {n}"
            );
            assert_eq!(
                line_number_at(&d, Encoding::Utf8, &c, *s).unwrap(),
                Some(n as u64)
            );
        }
        assert_eq!(
            line_number_at(&d, Encoding::Utf8, &c, 6).unwrap(),
            Some(1),
            "middle of line two"
        );
    }

    #[test]
    fn utf16_lines_roundtrip() {
        let bytes = utf16("ab\r\ncd\nef", false);
        let (c, d, _k) = count_all(&bytes, Encoding::Utf16Le);
        assert_eq!(
            offset_of_line(&d, Encoding::Utf16Le, &c, 1).unwrap(),
            Some(8)
        );
        assert_eq!(
            offset_of_line(&d, Encoding::Utf16Le, &c, 2).unwrap(),
            Some(14)
        );
        assert_eq!(
            line_number_at(&d, Encoding::Utf16Le, &c, 14).unwrap(),
            Some(2)
        );
        // Counts of another family are not used.
        assert_eq!(line_number_at(&d, Encoding::Utf8, &c, 14).unwrap(), None);
    }

    #[test]
    fn counting_across_chunk_boundaries_with_crlf() {
        // A CRLF straddling the 4 MiB boundary counts once and offsets stay exact.
        let mut data = vec![b'x'; COUNT_CHUNK as usize - 1];
        data.extend_from_slice(b"\r\nabc\r\ndef");
        let (c, d, _k) = count_all(&data, Encoding::Utf8);
        assert_eq!(c.lock().unwrap().total_lines(), Some(3));
        assert_eq!(
            offset_of_line(&d, Encoding::Utf8, &c, 1).unwrap(),
            Some(COUNT_CHUNK + 1)
        );
        assert_eq!(
            line_number_at(&d, Encoding::Utf8, &c, COUNT_CHUNK + 2).unwrap(),
            Some(1)
        );
        assert_eq!(
            offset_of_line(&d, Encoding::Utf8, &c, 2).unwrap(),
            Some(COUNT_CHUNK + 6)
        );
    }

    #[test]
    fn cancelled_scan_is_incomplete() {
        let (_k, document) = doc_with(b"a\nb\n");
        let shared = Mutex::new(LineCounts::new(Family::Byte, document.len()));
        let cancel = CancelToken::new();
        cancel.cancel();
        build_counts(&document, Encoding::Utf8, &shared, &cancel, |_| {}).unwrap();
        assert!(!shared.lock().unwrap().complete);
    }
}

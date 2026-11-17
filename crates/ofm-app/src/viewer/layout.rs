//! Text and hex page layout for the viewer (no GTK; runs on the page worker thread).
//!
//! A *row* is what the widget draws on one line: in no-wrap mode a whole file line (cut to the
//! visible columns), in wrap mode one wrapped segment. Every row knows the file offset where it
//! starts and where the next row starts, so scrolling is pure offset arithmetic:
//!
//! - the position of a page is the offset of its first row (`top`), always a row start;
//! - "next row" is `row.next`; "previous row" re-wraps the line before `top`
//!   ([`prev_row_start`]), found by a bounded backward scan ([`line_start`]);
//! - pages are filled from the end when `top` is too close to EOF, so End / PgDn never show a
//!   half-empty screen.
//!
//! Lines are decoded one at a time (at most [`LINE_CAP`] bytes each) per encoding, with a
//! file-offset table per character so search hits can be highlighted and wrapped rows can start
//! at exact offsets. Invalid bytes show as U+FFFD, control characters as a middle dot, tabs are
//! expanded to the tab stops.

use std::ops::Range;
use std::sync::OnceLock;

use ofm_viewer::{
    Document, DocumentError, Encoding, HexRow, align_down_to_boundary, decode_range, hex_rows_from,
    slice_columns, wrap_line,
};
use unicode_width::UnicodeWidthChar;

use super::scan::{Brk, Unit, find_break, rfind_break, unit_at};

/// Longest line (bytes) laid out in one piece; longer lines are split here.
pub const LINE_CAP: usize = 256 * 1024;
/// Tab stop interval in columns.
pub const TAB: usize = 8;
/// Smallest wrap width: keeps a tab expansion (<= 8 columns) from filling a whole row.
pub const MIN_COLS: usize = 16;
/// Bytes per hex row.
pub const HEX_ROW: u64 = 16;

/// Layout parameters that do not change while scrolling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Params {
    /// Text encoding.
    pub enc: Encoding,
    /// Wrap long lines at the viewport width.
    pub wrap: bool,
    /// Viewport width in columns.
    pub cols: usize,
    /// First visible column (no-wrap mode).
    pub hscroll: usize,
}

/// One displayed row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// File offset of the first character of the row.
    pub start: u64,
    /// File offset where the following row starts (after the line terminator for the last row
    /// of a line).
    pub next: u64,
    /// Displayed text (tabs expanded, no terminator, cut to the viewport in no-wrap mode).
    pub text: String,
    /// Byte range of `text` covered by the search match.
    pub hl: Option<Range<usize>>,
}

/// A page of text rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextPage {
    /// Offset of the first row (equals `rows[0].start`).
    pub top: u64,
    /// The rows (fewer than asked only for tiny files).
    pub rows: Vec<Row>,
    /// The last row reaches the end of the file.
    pub eof: bool,
    /// Horizontal scroll actually used (the worker may reveal a match).
    pub hscroll: usize,
}

/// A page of hex rows.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HexPage {
    /// Offset of the first row (a multiple of 16).
    pub top: u64,
    /// The rows.
    pub rows: Vec<HexRow>,
    /// The last row reaches the end of the file.
    pub eof: bool,
}

// ---- decoding ----------------------------------------------------------------------------------

fn table(enc: Encoding) -> &'static [char; 256] {
    static T: [OnceLock<[char; 256]>; 4] = [
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
        OnceLock::new(),
    ];
    let idx = match enc {
        Encoding::Windows1251 => 0,
        Encoding::Ibm866 => 1,
        Encoding::Koi8R => 2,
        _ => 3,
    };
    let enc = match idx {
        0 => Encoding::Windows1251,
        1 => Encoding::Ibm866,
        2 => Encoding::Koi8R,
        _ => Encoding::Iso8859_1,
    };
    T[idx].get_or_init(|| {
        let mut t = ['\u{FFFD}'; 256];
        for (b, slot) in t.iter_mut().enumerate() {
            let byte = [b as u8];
            if let Some(c) = decode_range(enc, &byte).chars().next() {
                *slot = c;
            }
        }
        t
    })
}

/// Call `f(char, byte_len)` for every character of `bytes` in `enc` (never panics; invalid
/// input yields U+FFFD for the bytes it consumed).
fn for_each_char(enc: Encoding, bytes: &[u8], mut f: impl FnMut(char, usize)) {
    match enc {
        Encoding::Utf8 => {
            for chunk in bytes.utf8_chunks() {
                for c in chunk.valid().chars() {
                    f(c, c.len_utf8());
                }
                let bad = chunk.invalid();
                if !bad.is_empty() {
                    f('\u{FFFD}', bad.len());
                }
            }
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let be = enc == Encoding::Utf16Be;
            let unit = |i: usize| -> u16 {
                let pair = [bytes[i], bytes[i + 1]];
                if be {
                    u16::from_be_bytes(pair)
                } else {
                    u16::from_le_bytes(pair)
                }
            };
            let n = bytes.len() / 2 * 2;
            let mut i = 0;
            while i < n {
                let u = unit(i);
                if (0xD800..0xDC00).contains(&u) && i + 4 <= n {
                    let v = unit(i + 2);
                    if (0xDC00..0xE000).contains(&v) {
                        let cp =
                            0x10000 + ((u32::from(u) - 0xD800) << 10) + (u32::from(v) - 0xDC00);
                        f(char::from_u32(cp).unwrap_or('\u{FFFD}'), 4);
                        i += 4;
                        continue;
                    }
                }
                f(char::from_u32(u32::from(u)).unwrap_or('\u{FFFD}'), 2);
                i += 2;
            }
            if bytes.len() > n {
                f('\u{FFFD}', 1);
            }
        }
        _ => {
            let t = table(enc);
            for &b in bytes {
                f(t[usize::from(b)], 1);
            }
        }
    }
}

/// A decoded line: display text plus the file offset of every character.
struct Decoded {
    text: String,
    /// `(byte index in text, file offset)` per displayed character, ascending.
    offs: Vec<(usize, u64)>,
}

impl Decoded {
    /// File offset of the character starting at text byte `idx`.
    fn offset_at(&self, idx: usize, fallback: u64) -> u64 {
        match self.offs.binary_search_by_key(&idx, |e| e.0) {
            Ok(i) => self.offs[i].1,
            Err(_) => fallback,
        }
    }

    /// Byte range of `range` (text bytes) covered by the file range `hl`.
    fn highlight(
        &self,
        range: &Range<usize>,
        hl: &Range<u64>,
        end_off: u64,
    ) -> Option<Range<usize>> {
        let lo = self.offs.partition_point(|e| e.0 < range.start);
        let hi = self.offs.partition_point(|e| e.0 < range.end);
        let (mut a, mut b): (Option<usize>, usize) = (None, 0);
        for i in lo..hi {
            let (idx, off) = self.offs[i];
            let next_off = self.offs.get(i + 1).map_or(end_off, |e| e.1);
            let next_idx = self.offs.get(i + 1).map_or(self.text.len(), |e| e.0);
            if next_off > off && off < hl.end && next_off > hl.start {
                a.get_or_insert(idx);
                b = next_idx.min(range.end);
            }
        }
        a.map(|a| a - range.start..b - range.start)
    }
}

fn bom_len(enc: Encoding, content: &[u8]) -> usize {
    match enc {
        Encoding::Utf8 if content.starts_with(&[0xEF, 0xBB, 0xBF]) => 3,
        Encoding::Utf16Le if content.starts_with(&[0xFF, 0xFE]) => 2,
        Encoding::Utf16Be if content.starts_with(&[0xFE, 0xFF]) => 2,
        _ => 0,
    }
}

fn decode_line(enc: Encoding, content: &[u8], base: u64) -> Decoded {
    let mut text = String::with_capacity(content.len());
    let mut offs = Vec::with_capacity(content.len());
    let mut pos = base;
    let mut col = 0usize;
    for_each_char(enc, content, |c, len| {
        let off = pos;
        pos += len as u64;
        if c == '\t' {
            let n = TAB - col % TAB;
            for k in 0..n {
                // The extra blanks of a tab start at the next character's offset so row starts
                // stay strictly increasing.
                offs.push((text.len(), if k == 0 { off } else { off + len as u64 }));
                text.push(' ');
            }
            col += n;
        } else {
            let c = if c.is_control() { '\u{B7}' } else { c };
            offs.push((text.len(), off));
            text.push(c);
            col += c.width().unwrap_or(0);
        }
    });
    Decoded { text, offs }
}

// ---- reading lines -----------------------------------------------------------------------------

struct RawLine {
    content: Vec<u8>,
    next: u64,
}

/// Read the line starting at `start`: its bytes without the terminator and the offset of the
/// next line. Lines longer than [`LINE_CAP`] are split.
fn read_line(doc: &Document, enc: Encoding, start: u64) -> Result<RawLine, DocumentError> {
    let len = doc.len();
    let u = enc.bytes_per_unit();
    let mut want = 4096usize;
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let more = doc.read_range(start + buf.len() as u64, want)?;
        let got = more.len();
        buf.extend_from_slice(&more);
        let at_eof = got < want || start + buf.len() as u64 >= len;
        if let Brk::Found { content_end, next } = find_break(enc, &buf, 0, at_eof) {
            buf.truncate(content_end);
            return Ok(RawLine {
                content: buf,
                next: start + next as u64,
            });
        }
        if at_eof {
            let n = buf.len();
            buf.truncate(n - n % u);
            return Ok(RawLine {
                content: buf,
                next: start + n as u64,
            });
        }
        if buf.len() >= LINE_CAP {
            let cut = align_down_to_boundary(enc, &buf, LINE_CAP - 4);
            buf.truncate(cut);
            return Ok(RawLine {
                content: buf,
                next: start + cut as u64,
            });
        }
        want = (want * 2).min(LINE_CAP);
    }
}

/// Start of the line containing `off` (the offset after the last terminator before `off`).
pub fn line_start(doc: &Document, enc: Encoding, off: u64) -> Result<u64, DocumentError> {
    let u = enc.bytes_per_unit() as u64;
    let off = (off - off % u).min(doc.len());
    if off == 0 {
        return Ok(0);
    }
    let limit = (off.saturating_sub(LINE_CAP as u64) / u) * u;
    let mut hi = off;
    while hi > limit {
        let lo = (hi.saturating_sub(64 * 1024).max(limit) / u) * u;
        let buf = doc.read_range(lo, (hi - lo) as usize)?;
        if let Some(q) = rfind_break(enc, &buf, buf.len()) {
            return Ok(lo + q as u64 + u);
        }
        hi = lo;
    }
    Ok(limit)
}

// ---- layout of one line ------------------------------------------------------------------------

struct LineLayout {
    rows: Vec<Row>,
    next_line: u64,
    /// Display column of the reveal offset inside this line.
    reveal_col: Option<usize>,
}

fn layout_line(
    doc: &Document,
    p: &Params,
    start: u64,
    hl: Option<&Range<u64>>,
    reveal: Option<u64>,
) -> Result<LineLayout, DocumentError> {
    let u = p.enc.bytes_per_unit() as u64;
    let raw = read_line(doc, p.enc, start)?;
    let next_line = raw.next.max(start + u);
    let skip = if start == 0 {
        bom_len(p.enc, &raw.content)
    } else {
        0
    };
    let dec = decode_line(p.enc, &raw.content[skip..], start + skip as u64);
    let cols = p.cols.max(MIN_COLS);
    let mut rows = Vec::new();
    if p.wrap {
        let segs = wrap_line(&dec.text, cols, TAB);
        let starts: Vec<u64> = segs
            .iter()
            .enumerate()
            .map(|(i, s)| {
                if i == 0 {
                    start
                } else {
                    dec.offset_at(s.start_byte, next_line)
                }
            })
            .collect();
        for (i, seg) in segs.iter().enumerate() {
            let range = seg.start_byte..seg.end_byte;
            let row_next = starts.get(i + 1).copied().unwrap_or(next_line);
            rows.push(Row {
                start: starts[i],
                next: row_next.max(starts[i] + 1),
                text: dec.text[range.clone()].to_owned(),
                hl: hl.and_then(|h| dec.highlight(&range, h, next_line)),
            });
        }
    } else {
        let range = match slice_columns(&dec.text, p.hscroll, cols, TAB) {
            Some(s) => s.start_byte..s.end_byte,
            None => dec.text.len()..dec.text.len(),
        };
        rows.push(Row {
            start,
            next: next_line,
            text: dec.text[range.clone()].to_owned(),
            hl: hl.and_then(|h| dec.highlight(&range, h, next_line)),
        });
    }
    let reveal_col = reveal.filter(|r| (start..next_line).contains(r)).map(|r| {
        let i = dec.offs.partition_point(|e| e.1 <= r).saturating_sub(1);
        let idx = dec.offs.get(i).map_or(0, |e| e.0);
        dec.text[..idx]
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum()
    });
    Ok(LineLayout {
        rows,
        next_line,
        reveal_col,
    })
}

/// Start offset of the row just before the row starting at `top` (`top` itself at the start of
/// the file).
pub fn prev_row_start(doc: &Document, p: &Params, top: u64) -> Result<u64, DocumentError> {
    let enc = p.enc;
    let u = enc.bytes_per_unit() as u64;
    let top = (top - top % u).min(doc.len());
    if top == 0 {
        return Ok(0);
    }
    let ls = line_start(doc, enc, top)?;
    let line = if ls < top {
        ls
    } else {
        // `top` starts a line: the previous line ends before its terminator.
        let back = (2 * u).min(top);
        let b = doc.read_range(top - back, back as usize)?;
        let crlf = back == 2 * u
            && unit_at(enc, &b, 0) == Some(Unit::Cr)
            && unit_at(enc, &b, u as usize) == Some(Unit::Lf);
        let end = top - if crlf { 2 * u } else { u };
        line_start(doc, enc, end)?
    };
    if !p.wrap {
        return Ok(line);
    }
    let ll = layout_line(doc, p, line, None, None)?;
    Ok(ll
        .rows
        .iter()
        .map(|r| r.start)
        .rfind(|&s| s < top)
        .unwrap_or(line))
}

/// Rows starting at the row that contains `top`.
fn build_rows(
    doc: &Document,
    p: &Params,
    top: u64,
    nrows: usize,
    hl: Option<&Range<u64>>,
    reveal: Option<u64>,
) -> Result<(Vec<Row>, Option<usize>), DocumentError> {
    let len = doc.len();
    let mut rows: Vec<Row> = Vec::with_capacity(nrows);
    let mut reveal_col = None;
    if len == 0 || nrows == 0 {
        return Ok((rows, None));
    }
    let mut cur = line_start(doc, p.enc, top.min(len))?;
    while rows.len() < nrows && cur < len {
        let ll = layout_line(doc, p, cur, hl, reveal)?;
        if ll.reveal_col.is_some() {
            reveal_col = ll.reveal_col;
        }
        for r in ll.rows {
            if r.next > top && rows.len() < nrows {
                rows.push(r);
            }
        }
        if ll.next_line <= cur {
            break;
        }
        cur = ll.next_line;
    }
    Ok((rows, reveal_col))
}

/// What to lay out.
#[derive(Debug, Clone, Default)]
pub struct TextReq {
    /// Offset the first row should contain.
    pub top: u64,
    /// Rows that fit the viewport.
    pub nrows: usize,
    /// Show this many rows above `top` (context above a search hit).
    pub back_rows: usize,
    /// Search match to highlight.
    pub hl: Option<Range<u64>>,
    /// Scroll horizontally so this offset is visible (no-wrap mode).
    pub reveal: Option<u64>,
}

/// Lay out a page of text rows.
pub fn text_page(doc: &Document, p: &Params, req: &TextReq) -> Result<TextPage, DocumentError> {
    let len = doc.len();
    let u = p.enc.bytes_per_unit() as u64;
    let mut p = p.clone();
    let mut top = (req.top - req.top % u).min(len);
    if req.back_rows > 0
        && let Some(first) = build_rows(doc, &p, top, 1, None, None)?.0.first()
    {
        top = first.start; // count the context rows from the row that contains `top`
    }
    for _ in 0..req.back_rows {
        let pr = prev_row_start(doc, &p, top)?;
        if pr >= top {
            break;
        }
        top = pr;
    }
    let build =
        |p: &Params, top: u64| build_rows(doc, p, top, req.nrows, req.hl.as_ref(), req.reveal);
    let (mut rows, mut rcol) = build(&p, top)?;
    // Short page (near EOF): move the top up so the screen is full.
    if rows.len() < req.nrows && len > 0 {
        let first = rows.first().map_or(len, |r| r.start);
        let mut t = first;
        for _ in 0..req.nrows - rows.len() {
            let pr = prev_row_start(doc, &p, t)?;
            if pr >= t {
                break;
            }
            t = pr;
        }
        if t < first {
            (rows, rcol) = build(&p, t)?;
        }
    }
    if !p.wrap
        && let Some(col) = rcol
    {
        let cols = p.cols.max(MIN_COLS);
        if col < p.hscroll || col >= p.hscroll + cols {
            let want = col.saturating_sub(cols / 4);
            if want != p.hscroll {
                p.hscroll = want;
                let first = rows.first().map_or(top, |r| r.start);
                (rows, _) = build(&p, first)?;
            }
        }
    }
    let eof = rows.last().is_none_or(|r| r.next >= len);
    Ok(TextPage {
        top: rows.first().map_or(top, |r| r.start),
        rows,
        eof,
        hscroll: p.hscroll,
    })
}

/// Rows `n` below the row containing `top` (clamped by the caller's page fill).
pub fn step_forward(doc: &Document, p: &Params, top: u64, n: usize) -> Result<u64, DocumentError> {
    if n == 0 {
        return Ok(top);
    }
    let (rows, _) = build_rows(doc, p, top, n + 1, None, None)?;
    Ok(match rows.get(n) {
        Some(r) => r.start,
        None => rows.last().map_or(top, |r| r.next),
    })
}

/// `n` rows above `top`.
pub fn step_back(doc: &Document, p: &Params, top: u64, n: usize) -> Result<u64, DocumentError> {
    let mut t = top;
    for _ in 0..n {
        let pr = prev_row_start(doc, p, t)?;
        if pr >= t {
            break;
        }
        t = pr;
    }
    Ok(t)
}

/// Lay out a page of hex rows starting at the row containing `top` (clamped so the page is full).
pub fn hex_page(doc: &Document, top: u64, nrows: usize) -> Result<HexPage, DocumentError> {
    let total = doc.len().div_ceil(HEX_ROW);
    let row = (top / HEX_ROW).min(total.saturating_sub(nrows as u64));
    let rows = hex_rows_from(doc, row, nrows).map_err(|e| match e {
        ofm_viewer::hex::HexError::Read(d) => d,
    })?;
    Ok(HexPage {
        top: row * HEX_ROW,
        eof: row + rows.len() as u64 >= total,
        rows,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::viewer::testutil::{doc_with, utf16};

    fn params(wrap: bool, cols: usize) -> Params {
        Params {
            enc: Encoding::Utf8,
            wrap,
            cols,
            hscroll: 0,
        }
    }

    fn req(top: u64, nrows: usize) -> TextReq {
        TextReq {
            top,
            nrows,
            ..TextReq::default()
        }
    }

    fn texts(page: &TextPage) -> Vec<&str> {
        page.rows.iter().map(|r| r.text.as_str()).collect()
    }

    #[test]
    fn basic_page_without_phantom_line() {
        let (_k, d) = doc_with(b"one\ntwo\nthree\n");
        let page = text_page(&d, &params(false, 40), &req(0, 10)).unwrap();
        assert_eq!(texts(&page), ["one", "two", "three"]);
        assert!(page.eof);
        assert_eq!(page.rows[1].start, 4);
        assert_eq!(page.rows[2].next, 14);
    }

    #[test]
    fn mixed_line_endings() {
        let (_k, d) = doc_with(b"a\r\nb\rc\n\nd");
        let page = text_page(&d, &params(false, 40), &req(0, 10)).unwrap();
        assert_eq!(texts(&page), ["a", "b", "c", "", "d"]);
    }

    #[test]
    fn short_page_fills_from_the_end() {
        let text: String = (1..=30).map(|i| format!("line{i}\n")).collect();
        let (_k, d) = doc_with(text.as_bytes());
        let p = params(false, 40);
        let page = text_page(&d, &p, &req(u64::MAX, 10)).unwrap();
        assert_eq!(page.rows.len(), 10);
        assert_eq!(page.rows[0].text, "line21");
        assert_eq!(page.rows[9].text, "line30");
        assert!(page.eof);
        // A top in the middle of a row snaps to the row start.
        let page = text_page(&d, &p, &req(8, 3)).unwrap();
        assert_eq!(page.top, 6);
        assert_eq!(texts(&page), ["line2", "line3", "line4"]);
    }

    #[test]
    fn wrapping_splits_rows_with_exact_offsets() {
        let (_k, d) = doc_with(b"abcdefghijklmnopqrstuvwxyz0123456789\nshort\n");
        let p = params(true, 16);
        let page = text_page(&d, &p, &req(0, 10)).unwrap();
        assert_eq!(
            texts(&page),
            ["abcdefghijklmnop", "qrstuvwxyz012345", "6789", "short"]
        );
        assert_eq!(
            page.rows.iter().map(|r| r.start).collect::<Vec<_>>(),
            [0, 16, 32, 37]
        );
        // Starting inside a wrapped line shows the row that contains the offset.
        let page = text_page(&d, &p, &req(20, 2)).unwrap();
        assert_eq!(page.top, 16);
        assert_eq!(texts(&page), ["qrstuvwxyz012345", "6789"]);
    }

    #[test]
    fn prev_row_walks_back_through_wrapped_and_plain_lines() {
        let (_k, d) = doc_with(b"abcdefghijklmnopqrstuvwxyz0123456789\nshort\n\nlast\n");
        let p = params(true, 16);
        let mut t = u64::MAX;
        let page = text_page(&d, &p, &req(t, 100)).unwrap();
        let starts: Vec<u64> = page.rows.iter().map(|r| r.start).collect();
        t = page.rows.last().unwrap().next;
        let mut back = Vec::new();
        for _ in 0..starts.len() {
            t = prev_row_start(&d, &p, t).unwrap();
            back.push(t);
        }
        back.reverse();
        assert_eq!(back, starts);
        assert_eq!(prev_row_start(&d, &p, 0).unwrap(), 0);
    }

    #[test]
    fn step_forward_and_back_are_inverse() {
        let text: String = (0..50).map(|i| format!("row {i}\n")).collect();
        let (_k, d) = doc_with(text.as_bytes());
        let p = params(false, 40);
        let f = step_forward(&d, &p, 0, 7).unwrap();
        assert_eq!(f, text.find("row 7").unwrap() as u64);
        assert_eq!(step_back(&d, &p, f, 7).unwrap(), 0);
        assert_eq!(step_back(&d, &p, 0, 3).unwrap(), 0);
    }

    #[test]
    fn tabs_are_expanded_and_control_chars_replaced() {
        let (_k, d) = doc_with(b"a\tb\x01c\n");
        let page = text_page(&d, &params(false, 40), &req(0, 3)).unwrap();
        assert_eq!(page.rows[0].text, "a       b\u{b7}c");
    }

    #[test]
    fn invalid_utf8_shows_replacement_chars_and_keeps_offsets() {
        let (_k, d) = doc_with(b"a\xFF\xFEb\nnext\n");
        let page = text_page(&d, &params(false, 40), &req(0, 3)).unwrap();
        assert_eq!(page.rows[0].text, "a\u{FFFD}\u{FFFD}b");
        assert_eq!(page.rows[1].start, 5);
    }

    #[test]
    fn bom_is_hidden_and_utf16_rows_are_decoded() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend(utf16("hi\r\nthere\n", false));
        let (_k, d) = doc_with(&bytes);
        let p = Params {
            enc: Encoding::Utf16Le,
            ..params(false, 40)
        };
        let page = text_page(&d, &p, &req(0, 5)).unwrap();
        assert_eq!(texts(&page), ["hi", "there"]);
        assert_eq!(page.rows[1].start, 2 + 8);
        let (_k, d) = doc_with(&[0xEF, 0xBB, 0xBF, b'x', b'\n']);
        let page = text_page(&d, &params(false, 40), &req(0, 2)).unwrap();
        assert_eq!(texts(&page), ["x"]);
    }

    #[test]
    fn single_byte_encodings_decode() {
        let (_k, d) = doc_with(&[0xCF, 0xF0, 0xE8, 0xE2, 0xE5, 0xF2, b'\n']);
        let p = Params {
            enc: Encoding::Windows1251,
            ..params(false, 40)
        };
        assert_eq!(
            text_page(&d, &p, &req(0, 2)).unwrap().rows[0].text,
            "Привет"
        );
        let (_k, d) = doc_with(&[0x8F, 0xE0, 0xA8, 0xA2, 0xA5, 0xE2]);
        let p = Params {
            enc: Encoding::Ibm866,
            ..p
        };
        assert_eq!(
            text_page(&d, &p, &req(0, 2)).unwrap().rows[0].text,
            "Привет"
        );
        let (_k, d) = doc_with(&[0xF0, 0xD2, 0xC9, 0xD7, 0xC5, 0xD4]);
        let p = Params {
            enc: Encoding::Koi8R,
            ..p
        };
        assert_eq!(
            text_page(&d, &p, &req(0, 2)).unwrap().rows[0].text,
            "Привет"
        );
    }

    #[test]
    fn highlight_covers_the_match_in_the_row() {
        let (_k, d) = doc_with(b"first\nfind me here\nlast\n");
        let r = TextReq {
            top: 0,
            nrows: 5,
            hl: Some(11..13),
            ..TextReq::default()
        };
        let page = text_page(&d, &params(false, 40), &r).unwrap();
        assert_eq!(page.rows[0].hl, None);
        assert_eq!(page.rows[1].hl, Some(5..7));
        assert_eq!(&page.rows[1].text[5..7], "me");
    }

    #[test]
    fn reveal_scrolls_horizontally_to_a_far_match() {
        let line = format!("{}NEEDLE{}\n", "x".repeat(200), "y".repeat(50));
        let (_k, d) = doc_with(line.as_bytes());
        let r = TextReq {
            top: 0,
            nrows: 3,
            hl: Some(200..206),
            reveal: Some(200),
            ..TextReq::default()
        };
        let page = text_page(&d, &params(false, 80), &r).unwrap();
        assert!(page.hscroll > 100, "hscroll {}", page.hscroll);
        let row = &page.rows[0];
        let hl = row.hl.clone().unwrap();
        assert_eq!(&row.text[hl], "NEEDLE");
    }

    #[test]
    fn hscroll_cuts_the_visible_columns() {
        let (_k, d) = doc_with(b"0123456789abcdefghij\n");
        let p = Params {
            hscroll: 5,
            ..params(false, 16)
        };
        let page = text_page(&d, &p, &req(0, 2)).unwrap();
        assert_eq!(page.rows[0].text, "56789abcdefghij");
    }

    #[test]
    fn very_long_line_is_split_and_every_row_advances() {
        let mut data = vec![b'a'; LINE_CAP * 2 + 100];
        data.push(b'\n');
        data.extend_from_slice(b"end\n");
        let (_k, d) = doc_with(&data);
        let p = params(false, 40);
        let mut t = 0;
        let mut seen = 0;
        loop {
            let page = text_page(&d, &p, &req(t, 1)).unwrap();
            seen += 1;
            assert!(seen < 10, "no progress");
            if page.eof {
                assert_eq!(page.rows[0].text, "end");
                break;
            }
            t = page.rows[0].next;
        }
        assert!(seen >= 3);
    }

    #[test]
    fn empty_file_has_no_rows() {
        let (_k, d) = doc_with(b"");
        let page = text_page(&d, &params(true, 40), &req(0, 5)).unwrap();
        assert!(page.rows.is_empty());
        assert!(page.eof);
        let h = hex_page(&d, 0, 5).unwrap();
        assert!(h.rows.is_empty());
    }

    #[test]
    fn hex_page_is_aligned_and_full() {
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        let (_k, d) = doc_with(&data);
        let h = hex_page(&d, 37, 4).unwrap();
        assert_eq!(h.top, 32);
        assert_eq!(h.rows.len(), 4);
        assert_eq!(h.rows[0].offset, 32);
        assert!(!h.eof);
        let h = hex_page(&d, u64::MAX, 4).unwrap();
        assert_eq!(h.top, 1000u64.div_ceil(16) * 16 - 64);
        assert!(h.eof);
        assert_eq!(h.rows.last().unwrap().bytes.len(), 1000 % 16);
    }

    #[test]
    fn highlight_does_not_include_the_blanks_of_a_preceding_tab() {
        let (_k, d) = doc_with(b"a\tNEEDLE\n");
        let r = TextReq {
            top: 0,
            nrows: 2,
            hl: Some(2..8),
            ..TextReq::default()
        };
        let page = text_page(&d, &params(false, 40), &r).unwrap();
        let row = &page.rows[0];
        assert_eq!(row.text, "a       NEEDLE");
        assert_eq!(&row.text[row.hl.clone().unwrap()], "NEEDLE");
    }

    #[test]
    fn context_rows_are_counted_from_the_row_containing_the_offset() {
        let text: String = (0..30).map(|i| format!("row {i}\n")).collect();
        let (_k, d) = doc_with(text.as_bytes());
        let hit = text.find("row 12").unwrap() as u64 + 2; // inside the row
        let r = TextReq {
            top: hit,
            nrows: 5,
            back_rows: 2,
            ..TextReq::default()
        };
        let page = text_page(&d, &params(false, 40), &r).unwrap();
        assert_eq!(page.rows[0].text, "row 10");
        assert_eq!(page.rows[2].text, "row 12");
    }

    #[test]
    fn line_start_scans_back_and_stops_at_cap() {
        let (_k, d) = doc_with(b"ab\ncd\r\nef");
        assert_eq!(line_start(&d, Encoding::Utf8, 4).unwrap(), 3);
        assert_eq!(line_start(&d, Encoding::Utf8, 3).unwrap(), 3);
        assert_eq!(line_start(&d, Encoding::Utf8, 8).unwrap(), 7);
        assert_eq!(line_start(&d, Encoding::Utf8, 2).unwrap(), 0);
        let (_k, d) = doc_with(&vec![b'x'; LINE_CAP * 3]);
        let s = line_start(&d, Encoding::Utf8, (LINE_CAP * 3) as u64).unwrap();
        assert_eq!(s, (LINE_CAP * 2) as u64);
    }
}

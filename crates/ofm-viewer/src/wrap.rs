//! Display-column wrapping and horizontal-scroll slicing.
//!
//! Columns follow [`unicode_width`]: East-Asian wide/fullwidth = 2, zero-width and combining
//! marks = 0, everything else = 1. Tabs advance to the next multiple of the tab width
//! ([`DEFAULT_TAB_WIDTH`]). A wide character that does not fit at the end of a segment starts
//! the next segment instead of being split; a wide character wider than the whole width still
//! gets its own segment (never an infinite loop, never a lost char).

use unicode_width::UnicodeWidthChar;

/// Default tab stop interval (8, like the terminal).
pub const DEFAULT_TAB_WIDTH: usize = 8;

/// A segment of a line, as byte/char ranges plus display columns.
///
/// `start_byte..end_byte` slices the original line `&str`; the columns are display columns
/// (post-tab-expansion) within the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Segment {
    /// Byte offset of the segment start in the line.
    pub start_byte: usize,
    /// Byte offset of the segment end (exclusive) in the line.
    pub end_byte: usize,
    /// Display column where the segment begins (tab-expanded).
    pub start_col: usize,
    /// Display column just past the segment end.
    pub end_col: usize,
}

/// Expandable display width of one char under the given tab width.
fn char_width(c: char, tab_width: usize, col: usize) -> usize {
    if c == '\t' {
        let tw = tab_width.max(1);
        tw - (col % tw)
    } else {
        c.width().unwrap_or(0)
    }
}

/// Iterate `(byte_idx, char, width, col_before)` of the line.
struct Widths<'a> {
    chars: std::str::CharIndices<'a>,
    col: usize,
    tab_width: usize,
}

impl<'a> Widths<'a> {
    fn new(line: &'a str, tab_width: usize) -> Self {
        Self {
            chars: line.char_indices(),
            col: 0,
            tab_width,
        }
    }
}

impl Iterator for Widths<'_> {
    type Item = (usize, char, usize, usize);
    fn next(&mut self) -> Option<Self::Item> {
        let (i, c) = self.chars.next()?;
        let w = char_width(c, self.tab_width, self.col);
        let at = self.col;
        self.col += w;
        Some((i, c, w, at))
    }
}

/// Wrap one already-decoded line into segments of at most `width` display columns.
///
/// Returns at least one segment (possibly empty for an empty line). `width == 0` yields the
/// whole line as a single segment.
pub fn wrap_line(line: &str, width: usize, tab_width: usize) -> Vec<Segment> {
    if width == 0 {
        return vec![Segment {
            start_byte: 0,
            end_byte: line.len(),
            start_col: 0,
            end_col: total_width(line, tab_width),
        }];
    }
    let mut out = Vec::new();
    let mut seg_start_byte = 0usize;
    let mut seg_start_col = 0usize;
    let mut col = 0usize;
    for (i, _c, w, at) in Widths::new(line, tab_width) {
        if w > 0 && col > seg_start_col && col - seg_start_col + w > width {
            // Break before this char (a wide char never splits; a zero-width char attaches
            // to the current segment instead of triggering a break).
            out.push(Segment {
                start_byte: seg_start_byte,
                end_byte: i,
                start_col: seg_start_col,
                end_col: col,
            });
            seg_start_byte = i;
            seg_start_col = at;
        }
        col = at + w;
    }
    out.push(Segment {
        start_byte: seg_start_byte,
        end_byte: line.len(),
        start_col: seg_start_col,
        end_col: col,
    });
    out
}

/// Total display width of a line.
fn total_width(line: &str, tab_width: usize) -> usize {
    Widths::new(line, tab_width)
        .last()
        .map_or(0, |(_, _, w, at)| at + w)
}

/// Slice a horizontal window `[start_col, start_col + width)` of display columns out of a
/// line (unwrapped mode). Returns `None` if the window lies entirely past the line's end.
///
/// The returned segment is *not* clipped to `width`: a wide char straddling the right edge is
/// included whole (callers that need strict clipping can narrow `end_col` when rendering).
pub fn slice_columns(
    line: &str,
    start_col: usize,
    width: usize,
    tab_width: usize,
) -> Option<Segment> {
    if line.is_empty() && start_col == 0 {
        return Some(Segment {
            start_byte: 0,
            end_byte: 0,
            start_col: 0,
            end_col: 0,
        });
    }
    let mut first: Option<(usize, usize, usize)> = None; // (byte, col, width)
    let mut last_end_byte = 0usize;
    let mut last_end_col = 0usize;
    let end_col = start_col.saturating_add(width.max(1));
    for (i, _c, w, at) in Widths::new(line, tab_width) {
        let right = at + w;
        if right <= start_col {
            continue; // entirely left of the window
        }
        if at >= end_col && first.is_some() {
            break; // entirely right of the window
        }
        if first.is_none() {
            first = Some((i, at, w));
        }
        last_end_byte = i + _c.len_utf8();
        last_end_col = right;
    }
    let (fb, fc, fw) = first?;
    let sb = if fc < start_col && fc + fw > start_col {
        // First char straddles the left edge: start at the next char's boundary.
        let nb = fb + line[fb..].chars().next().map_or(1, char::len_utf8);
        nb.min(line.len())
    } else {
        fb
    };
    let sc = if fc < start_col { fc + fw } else { fc };
    Some(Segment {
        start_byte: sb,
        end_byte: last_end_byte,
        start_col: sc,
        end_col: last_end_col,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn wraps_simple_text() {
        let segs = wrap_line("hello world", 5, DEFAULT_TAB_WIDTH);
        assert_eq!(segs.len(), 3);
        assert_eq!(
            &"hello world"[segs[0].start_byte..segs[0].end_byte],
            "hello"
        );
        assert_eq!(
            &"hello world"[segs[1].start_byte..segs[1].end_byte],
            " worl"
        );
        assert_eq!(&"hello world"[segs[2].start_byte..segs[2].end_byte], "d");
    }

    #[test]
    fn wide_chars_count_double_and_do_not_split() {
        // "ab日本cd": widths 1,1,2,2,1,1
        let segs = wrap_line("ab日本cd", 4, DEFAULT_TAB_WIDTH);
        let text: Vec<&str> = segs
            .iter()
            .map(|s| &"ab日本cd"[s.start_byte..s.end_byte])
            .collect();
        assert_eq!(text, vec!["ab日", "本cd"]);
        // Width smaller than a wide char still emits it (one per segment).
        let segs = wrap_line("日本", 1, DEFAULT_TAB_WIDTH);
        assert_eq!(segs.len(), 2);
    }

    #[test]
    fn zero_width_chars_attach_to_previous_segment() {
        // Combining acute: e + U+0301 = width 1 total.
        let line = "ab\u{0301}c";
        let segs = wrap_line(line, 2, DEFAULT_TAB_WIDTH);
        assert_eq!(&line[segs[0].start_byte..segs[0].end_byte], "ab\u{0301}");
        assert_eq!(&line[segs[1].start_byte..segs[1].end_byte], "c");
    }

    #[test]
    fn tabs_advance_to_stops() {
        let segs = wrap_line("a\tb", 4, 4);
        // 'a' at col 0-1, tab fills 1..4, 'b' at 4..5 -> wrap before 'b'.
        assert_eq!(segs.len(), 2);
        assert_eq!(&"a\tb"[segs[0].start_byte..segs[0].end_byte], "a\t");
        assert_eq!(segs[0].end_col, 4);
        let segs = wrap_line("\tx", 8, 8);
        // The tab alone fills all 8 columns; 'x' lands in the second segment.
        assert_eq!(segs.len(), 2);
        assert_eq!(&"\tx"[segs[1].start_byte..segs[1].end_byte], "x");
    }

    #[test]
    fn empty_and_zero_width() {
        let segs = wrap_line("", 10, 8);
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].start_byte, segs[0].end_byte);
        let segs = wrap_line("abc", 0, 8);
        assert_eq!(segs.len(), 1);
        assert_eq!(&"abc"[segs[0].start_byte..segs[0].end_byte], "abc");
    }

    #[test]
    fn slice_unwrapped_window() {
        let line = "0123456789";
        let seg = slice_columns(line, 2, 4, 8).unwrap();
        assert_eq!(&line[seg.start_byte..seg.end_byte], "2345");
        assert_eq!((seg.start_col, seg.end_col), (2, 6));
        // Wide char straddling the right edge is kept whole.
        let line = "ab日本cd";
        let seg = slice_columns(line, 0, 3, 8).unwrap();
        assert_eq!(&line[seg.start_byte..seg.end_byte], "ab日");
        // Past the end.
        assert!(slice_columns(line, 100, 4, 8).is_none());
        // Empty line at col 0.
        let seg = slice_columns("", 0, 4, 8).unwrap();
        assert_eq!(seg.start_byte, seg.end_byte);
    }

    #[test]
    fn slice_with_tab_window() {
        let line = "a\tb";
        // Window covers cols 1..4 (the tab padding) and 'b' at col 4.
        let seg = slice_columns(line, 1, 4, 4).unwrap();
        assert_eq!(&line[seg.start_byte..seg.end_byte], "\tb");
    }
}

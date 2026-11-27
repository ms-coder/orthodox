//! Background work of the viewer (no GTK): opening, page layout, line counting, search and
//! go-to. Everything that touches the file runs here; results travel to the GTK thread as
//! [`Msg`] values through one `async_channel` per viewer (consumed with `wiring::attach`).
//!
//! The page worker owns the scroll position: the UI sends *relative* moves ("3 rows down",
//! "one page up") or an absolute `set_top`, the worker merges requests queued behind a slow one
//! and answers only the last, so holding a key never builds a backlog.

use crate::i18n::tr;
use std::ops::Range;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ofm_core::CancelToken;
use ofm_vfs::pool::IoPool;
use ofm_viewer::{
    DETECT_LEN, Detection, Document, Encoding, Match, Needle, SearchError, SearchOptions,
    detect_encoding, find_next, find_prev,
};

use super::layout::{
    HEX_ROW, HexPage, Params, TextPage, TextReq, hex_page, step_back, step_forward, text_page,
};
use super::scan::{LineCounts, build_counts, line_number_at, offset_of_line};

/// Messages from the workers to the GTK thread.
#[derive(Debug)]
pub enum Msg {
    /// `Document::open` + encoding detection finished.
    Opened {
        /// Document generation the answer belongs to.
        doc_id: u64,
        /// The document and the detected encoding, or the error text.
        result: Result<(Arc<Document>, Detection), String>,
    },
    /// A laid-out page.
    Page {
        /// Document generation.
        doc_id: u64,
        /// The page or the error text.
        page: Result<Laid, String>,
    },
    /// Line counting progress.
    Index {
        /// Document generation.
        doc_id: u64,
        /// Bytes scanned.
        done: u64,
        /// The scan finished.
        complete: bool,
    },
    /// Offset of the requested line.
    Goto {
        /// Document generation.
        doc_id: u64,
        /// Offset, or `None` when the line is not known (yet).
        offset: Option<u64>,
    },
    /// A search finished.
    Search {
        /// Document generation.
        doc_id: u64,
        /// Search generation.
        search_id: u64,
        /// Match (`None` = not found), or the error text; `Cancelled` is reported as `Ok(None)`
        /// with `cancelled` set.
        result: Result<Option<Match>, String>,
        /// The search was cancelled (the result is meaningless).
        cancelled: bool,
        /// The search wrapped around the end of the file.
        wrapped: bool,
    },
}

/// A laid-out page of either kind plus the line number of its first row.
#[derive(Debug, Clone)]
pub enum Laid {
    /// Text rows; the number is the 1-based line of the first row when counted.
    Text(TextPage, Option<u64>),
    /// Hex rows.
    Hex(HexPage),
}

/// One page request (see the module docs for the merge rules).
#[derive(Clone)]
pub struct PageReq {
    /// Document generation.
    pub doc_id: u64,
    /// The document.
    pub doc: Arc<Document>,
    /// Line counts of the document (for the line number).
    pub counts: Arc<Mutex<LineCounts>>,
    /// Hex instead of text.
    pub hex: bool,
    /// Text layout parameters.
    pub params: Params,
    /// Rows that fit the viewport.
    pub nrows: usize,
    /// Absolute position to jump to.
    pub set_top: Option<u64>,
    /// Jump to the end of the file.
    pub to_end: bool,
    /// Relative move in rows.
    pub rows_delta: i64,
    /// Relative move in pages.
    pub pages_delta: i64,
    /// Rows of context above `set_top`.
    pub back_rows: usize,
    /// Search match to highlight.
    pub hl: Option<Range<u64>>,
    /// Make the start of `hl` visible horizontally.
    pub reveal: bool,
}

/// Combine a queued request with a newer one: new parameters win; relative moves add up unless
/// the newer request jumps to an absolute position.
pub fn merge(old: PageReq, mut new: PageReq) -> PageReq {
    if old.doc_id != new.doc_id || new.set_top.is_some() || new.to_end {
        return new;
    }
    new.set_top = old.set_top;
    new.to_end = old.to_end;
    new.back_rows = old.back_rows;
    new.rows_delta += old.rows_delta;
    new.pages_delta += old.pages_delta;
    new.reveal |= old.reveal;
    new
}

/// Lay out the page `req` asks for, starting from `*cur_top` (updated to the new top).
pub fn compute(req: &PageReq, cur_top: &mut u64) -> Result<Laid, String> {
    let doc = &*req.doc;
    let nrows = req.nrows.max(1);
    let page_rows = nrows.saturating_sub(1).max(1) as i64;
    let delta = req
        .rows_delta
        .saturating_add(req.pages_delta.saturating_mul(page_rows));
    let mut top = if req.to_end {
        u64::MAX
    } else {
        req.set_top.unwrap_or(*cur_top)
    };
    let e = |e: ofm_viewer::DocumentError| e.to_string();
    if req.hex {
        if top != u64::MAX {
            let t = i128::from(top & !(HEX_ROW - 1)) + i128::from(delta) * i128::from(HEX_ROW);
            top = t.clamp(0, i128::from(doc.len())) as u64;
        }
        let page = hex_page(doc, top, nrows).map_err(e)?;
        *cur_top = page.top;
        return Ok(Laid::Hex(page));
    }
    if top != u64::MAX {
        top = top.min(doc.len());
        if delta > 0 {
            top = step_forward(doc, &req.params, top, delta as usize).map_err(e)?;
        } else if delta < 0 {
            top = step_back(doc, &req.params, top, delta.unsigned_abs() as usize).map_err(e)?;
        }
    }
    let t = TextReq {
        top,
        nrows,
        back_rows: req.back_rows,
        hl: req.hl.clone(),
        reveal: req
            .reveal
            .then(|| req.hl.as_ref().map(|h| h.start))
            .flatten(),
    };
    let page = text_page(doc, &req.params, &t).map_err(e)?;
    *cur_top = page.top;
    let line = line_number_at(doc, req.params.enc, &req.counts, page.top)
        .ok()
        .flatten()
        .map(|n| n + 1);
    Ok(Laid::Text(page, line))
}

/// Handle to the page worker thread (it exits when this is dropped).
pub struct PageWorker {
    tx: Sender<PageReq>,
}

impl PageWorker {
    /// Start the worker; answers go to `out`.
    pub fn spawn(out: async_channel::Sender<Msg>) -> PageWorker {
        let (tx, rx) = channel::<PageReq>();
        let spawned = std::thread::Builder::new()
            .name("ofm-viewer-page".into())
            .spawn(move || page_loop(&rx, &out));
        if let Err(e) = spawned {
            tracing::error!(error = %e, "cannot start the viewer page worker");
        }
        PageWorker { tx }
    }

    /// Queue a request.
    pub fn send(&self, req: PageReq) {
        if self.tx.send(req).is_err() {
            tracing::error!("viewer page worker is gone");
        }
    }
}

fn page_loop(rx: &Receiver<PageReq>, out: &async_channel::Sender<Msg>) {
    let (mut cur_top, mut cur_doc) = (0u64, 0u64);
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = merge(req, newer);
        }
        if req.doc_id != cur_doc {
            cur_doc = req.doc_id;
            cur_top = 0;
        }
        let page = catch_unwind(AssertUnwindSafe(|| compute(&req, &mut cur_top)))
            .unwrap_or_else(|_| Err(tr("the viewer worker panicked").to_owned()));
        if out
            .send_blocking(Msg::Page {
                doc_id: req.doc_id,
                page,
            })
            .is_err()
        {
            return;
        }
    }
}

/// Open `path` on the I/O pool and detect its encoding.
pub fn spawn_open(pool: &IoPool, path: PathBuf, doc_id: u64, out: async_channel::Sender<Msg>) {
    pool.spawn(move || {
        let result = Document::open(&path)
            .map_err(|e| e.to_string())
            .and_then(|doc| {
                let head = doc.read_range(0, DETECT_LEN).map_err(|e| e.to_string())?;
                Ok((Arc::new(doc), detect_encoding(&head)))
            });
        let _ = out.send_blocking(Msg::Opened { doc_id, result });
    });
}

/// Count lines of `doc` in the background; progress is reported about every 100 ms.
pub fn spawn_index(
    doc: Arc<Document>,
    enc: Encoding,
    counts: Arc<Mutex<LineCounts>>,
    cancel: CancelToken,
    doc_id: u64,
    out: async_channel::Sender<Msg>,
) {
    let spawned = std::thread::Builder::new()
        .name("ofm-viewer-index".into())
        .spawn(move || {
            let mut last = Instant::now();
            let out2 = out.clone();
            let c2 = cancel.clone();
            let r = catch_unwind(AssertUnwindSafe(|| {
                build_counts(&doc, enc, &counts, &cancel, |done| {
                    if last.elapsed() >= Duration::from_millis(100) {
                        last = Instant::now();
                        let _ = out2.send_blocking(Msg::Index {
                            doc_id,
                            done,
                            complete: false,
                        });
                    }
                })
            }));
            let complete = matches!(r, Ok(Ok(()))) && !c2.is_cancelled();
            let _ = out.send_blocking(Msg::Index {
                doc_id,
                done: doc.len(),
                complete,
            });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "cannot start the viewer line counter");
    }
}

/// Look up the offset of 0-based line `n` on the I/O pool.
pub fn spawn_goto_line(
    pool: &IoPool,
    doc: Arc<Document>,
    enc: Encoding,
    counts: Arc<Mutex<LineCounts>>,
    n: u64,
    doc_id: u64,
    out: async_channel::Sender<Msg>,
) {
    pool.spawn(move || {
        let offset = offset_of_line(&doc, enc, &counts, n).ok().flatten();
        let _ = out.send_blocking(Msg::Goto { doc_id, offset });
    });
}

/// Search direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// Towards the end of the file.
    Forward,
    /// Towards the start.
    Backward,
}

/// What and how to search (kept by the viewer for next/previous).
#[derive(Debug, Clone)]
pub struct SearchSpec {
    /// The needle.
    pub needle: Needle,
    /// Case / wrap options.
    pub opts: SearchOptions,
}

/// Run a search on its own thread; `cancel` stops it.
#[allow(clippy::too_many_arguments)]
pub fn spawn_search(
    doc: Arc<Document>,
    enc: Encoding,
    spec: SearchSpec,
    dir: Dir,
    from: u64,
    cancel: CancelToken,
    doc_id: u64,
    search_id: u64,
    out: async_channel::Sender<Msg>,
) {
    let spawned = std::thread::Builder::new()
        .name("ofm-viewer-search".into())
        .spawn(move || {
            let run = || -> Result<Option<Match>, SearchError> {
                let prepared = spec.needle.prepare(enc, &spec.opts)?;
                match dir {
                    Dir::Forward => find_next(&doc, &prepared, from, &spec.opts, &cancel),
                    Dir::Backward => find_prev(&doc, &prepared, from, &spec.opts, &cancel),
                }
            };
            let r = catch_unwind(AssertUnwindSafe(run)).unwrap_or(Ok(None));
            let (result, cancelled) = match r {
                Err(SearchError::Cancelled) => (Ok(None), true),
                Err(e) => (Err(e.to_string()), false),
                Ok(m) => (Ok(m), false),
            };
            let wrapped = matches!(
                (&result, dir),
                (Ok(Some(m)), Dir::Forward) if m.offset < from
            ) || matches!(
                (&result, dir),
                (Ok(Some(m)), Dir::Backward) if m.offset >= from
            );
            let _ = out.send_blocking(Msg::Search {
                doc_id,
                search_id,
                result,
                cancelled,
                wrapped,
            });
        });
    if let Err(e) = spawned {
        tracing::error!(error = %e, "cannot start the viewer search");
    }
}

/// Parse a hex needle: `48 65 6c`, `48656C`, `0x48 0x65`. `Err` is a message for the dialog.
pub fn parse_hex(text: &str) -> Result<Vec<u8>, String> {
    let mut digits = String::new();
    for tok in text.split(|c: char| c.is_whitespace() || c == ',') {
        let tok = tok
            .strip_prefix("0x")
            .or_else(|| tok.strip_prefix("0X"))
            .unwrap_or(tok);
        digits.push_str(tok);
    }
    if digits.is_empty() {
        return Err(tr("Enter hexadecimal bytes, e.g. 48 65 6C 6C 6F.").to_owned());
    }
    if !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(tr("Only the digits 0-9 and A-F are allowed.").to_owned());
    }
    if !digits.len().is_multiple_of(2) {
        return Err(tr("The number of hexadecimal digits must be even.").to_owned());
    }
    let mut out = Vec::with_capacity(digits.len() / 2);
    for pair in digits.as_bytes().chunks(2) {
        let s = std::str::from_utf8(pair).map_err(|e| e.to_string())?;
        out.push(u8::from_str_radix(s, 16).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

/// Where the go-to dialog wants to go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GotoTarget {
    /// 1-based line.
    Line(u64),
    /// Byte offset.
    Offset(u64),
    /// Percent of the file.
    Percent(f64),
}

/// Parse go-to text: `123` (a line, or an offset when `offset_mode`), `0x1F40` (offset),
/// `50%`. `Err` is a message for the dialog.
pub fn parse_goto(text: &str, offset_mode: bool) -> Result<GotoTarget, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err(
            tr("Enter a line number, an offset (0x... for hex) or a percentage.").to_owned(),
        );
    }
    if let Some(p) = t.strip_suffix('%') {
        let v: f64 = p
            .trim()
            .parse()
            .map_err(|_| tr("Not a valid percentage.").to_owned())?;
        if !(0.0..=100.0).contains(&v) {
            return Err(tr("The percentage must be between 0 and 100.").to_owned());
        }
        return Ok(GotoTarget::Percent(v));
    }
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return u64::from_str_radix(h, 16)
            .map(GotoTarget::Offset)
            .map_err(|_| tr("Not a valid hexadecimal offset.").to_owned());
    }
    let n: u64 = t
        .replace([' ', '_'], "")
        .parse()
        .map_err(|_| tr("Not a valid number.").to_owned())?;
    if offset_mode {
        Ok(GotoTarget::Offset(n))
    } else if n == 0 {
        Err(tr("Line numbers start at 1.").to_owned())
    } else {
        Ok(GotoTarget::Line(n))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::viewer::scan::{Family, LineCounts};
    use crate::viewer::testutil::doc_with;

    fn req(doc: Arc<Document>) -> PageReq {
        PageReq {
            doc_id: 1,
            counts: Arc::new(Mutex::new(LineCounts::new(Family::Byte, doc.len()))),
            doc,
            hex: false,
            params: Params {
                enc: Encoding::Utf8,
                wrap: false,
                cols: 40,
                hscroll: 0,
            },
            nrows: 3,
            set_top: None,
            to_end: false,
            rows_delta: 0,
            pages_delta: 0,
            back_rows: 0,
            hl: None,
            reveal: false,
        }
    }

    fn lines() -> (ofm_testkit::temp::TempDir, Arc<Document>) {
        let text: String = (0..20).map(|i| format!("line {i}\n")).collect();
        let (k, d) = doc_with(text.as_bytes());
        (k, Arc::new(d))
    }

    fn first(l: &Laid) -> String {
        match l {
            Laid::Text(p, _) => p.rows[0].text.clone(),
            Laid::Hex(_) => String::new(),
        }
    }

    #[test]
    fn relative_moves_accumulate_on_the_workers_position() {
        let (_k, doc) = lines();
        let mut top = 0;
        let mut r = req(doc);
        r.rows_delta = 5;
        assert_eq!(first(&compute(&r, &mut top).unwrap()), "line 5");
        r.rows_delta = -2;
        assert_eq!(first(&compute(&r, &mut top).unwrap()), "line 3");
        r.rows_delta = 0;
        r.pages_delta = 1; // nrows 3 -> 2 rows
        assert_eq!(first(&compute(&r, &mut top).unwrap()), "line 5");
        r.pages_delta = 0;
        r.rows_delta = 1000; // clamped by the page fill
        let Laid::Text(p, _) = compute(&r, &mut top).unwrap() else {
            panic!()
        };
        assert_eq!(p.rows[0].text, "line 17");
        assert!(p.eof);
        r.rows_delta = 0;
        r.set_top = Some(0);
        assert_eq!(first(&compute(&r, &mut top).unwrap()), "line 0");
        r.set_top = None;
        r.to_end = true;
        assert_eq!(first(&compute(&r, &mut top).unwrap()), "line 17");
    }

    #[test]
    fn hex_moves_by_rows_of_16() {
        let (_k, d) = doc_with(&vec![7u8; 400]);
        let mut top = 0;
        let mut r = req(Arc::new(d));
        r.hex = true;
        r.rows_delta = 3;
        let Laid::Hex(p) = compute(&r, &mut top).unwrap() else {
            panic!()
        };
        assert_eq!(p.top, 48);
        assert_eq!(top, 48);
    }

    #[test]
    fn merge_adds_relative_moves_and_lets_absolute_win() {
        let (_k, doc) = lines();
        let mut a = req(Arc::clone(&doc));
        a.rows_delta = 2;
        let mut b = req(Arc::clone(&doc));
        b.rows_delta = 3;
        b.pages_delta = 1;
        let m = merge(a.clone(), b.clone());
        assert_eq!((m.rows_delta, m.pages_delta), (5, 1));
        b.set_top = Some(10);
        let m = merge(a.clone(), b);
        assert_eq!((m.rows_delta, m.set_top), (3, Some(10)));
        // A queued absolute jump survives a later relative move.
        let mut j = req(Arc::clone(&doc));
        j.set_top = Some(30);
        j.back_rows = 2;
        let mut mv = req(doc);
        mv.rows_delta = 1;
        let m = merge(j, mv);
        assert_eq!((m.set_top, m.rows_delta, m.back_rows), (Some(30), 1, 2));
    }

    #[test]
    fn line_number_is_reported_once_counted() {
        let (_k, doc) = lines();
        let mut top = 0;
        let mut r = req(Arc::clone(&doc));
        r.set_top = Some(7 * 3);
        let Laid::Text(_, n) = compute(&r, &mut top).unwrap() else {
            panic!()
        };
        assert_eq!(n, None, "nothing counted yet");
        build_counts(&doc, Encoding::Utf8, &r.counts, &CancelToken::new(), |_| {}).unwrap();
        let Laid::Text(p, n) = compute(&r, &mut top).unwrap() else {
            panic!()
        };
        assert_eq!(p.rows[0].text, "line 3");
        assert_eq!(n, Some(4));
    }

    #[test]
    fn parses_hex_needles() {
        assert_eq!(parse_hex("48 65 6c").unwrap(), b"Hel");
        assert_eq!(parse_hex("0x48,0x65").unwrap(), b"He");
        assert_eq!(parse_hex("4865").unwrap(), b"He");
        assert!(parse_hex("").is_err());
        assert!(parse_hex("4").is_err());
        assert!(parse_hex("zz").is_err());
    }

    #[test]
    fn parses_goto_targets() {
        assert_eq!(parse_goto("12", false), Ok(GotoTarget::Line(12)));
        assert_eq!(parse_goto("12", true), Ok(GotoTarget::Offset(12)));
        assert_eq!(parse_goto("0x10", false), Ok(GotoTarget::Offset(16)));
        assert_eq!(parse_goto(" 50% ", false), Ok(GotoTarget::Percent(50.0)));
        assert!(parse_goto("0", false).is_err());
        assert!(parse_goto("abc", false).is_err());
        assert!(parse_goto("101%", false).is_err());
        assert!(parse_goto("", false).is_err());
    }

    #[test]
    fn page_worker_answers_the_latest_request() {
        let (_k, doc) = lines();
        let (tx, rx) = async_channel::unbounded();
        let w = PageWorker::spawn(tx);
        let mut r = req(doc);
        r.rows_delta = 4;
        w.send(r.clone());
        let m = rx.recv_blocking().unwrap();
        let Msg::Page { page, .. } = m else { panic!() };
        assert_eq!(first(&page.unwrap()), "line 4");
        r.rows_delta = 1;
        w.send(r);
        let Msg::Page { page, .. } = rx.recv_blocking().unwrap() else {
            panic!()
        };
        assert_eq!(first(&page.unwrap()), "line 5");
    }

    #[test]
    fn search_finds_forward_backward_and_wraps() {
        let (_k, d) = doc_with(b"alpha beta\nGamma delta\nbeta again\n");
        let doc = Arc::new(d);
        let (tx, rx) = async_channel::unbounded();
        let spec = SearchSpec {
            needle: Needle::Text("beta".into()),
            opts: SearchOptions {
                case_sensitive: false,
                wrap: true,
            },
        };
        let run = |dir, from, id| {
            spawn_search(
                Arc::clone(&doc),
                Encoding::Utf8,
                spec.clone(),
                dir,
                from,
                CancelToken::new(),
                1,
                id,
                tx.clone(),
            );
            let Msg::Search {
                result, wrapped, ..
            } = rx.recv_blocking().unwrap()
            else {
                panic!()
            };
            (result.unwrap(), wrapped)
        };
        let (m, w) = run(Dir::Forward, 0, 1);
        assert_eq!((m.unwrap().offset, w), (6, false));
        let (m, w) = run(Dir::Forward, 7, 2);
        assert_eq!((m.unwrap().offset, w), (23, false));
        let (m, w) = run(Dir::Forward, 24, 3);
        assert_eq!((m.unwrap().offset, w), (6, true));
        let (m, _) = run(Dir::Backward, 23, 4);
        assert_eq!(m.unwrap().offset, 6);
    }
}

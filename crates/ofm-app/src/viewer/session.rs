//! `Session`: the state of one viewer window and its conversation with the workers.
//!
//! All blocking work (open, layout, line counting, search, go-to) runs in [`super::worker`];
//! this type keeps the user-visible state (mode, encoding, position, search), turns commands into
//! page requests and applies the answers to the widgets. Everything here runs on the GTK thread
//! and never touches the file system.

use std::cell::{Cell, RefCell};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::{Arc, Mutex};

use adw::prelude::*;
use ofm_core::CancelToken;
use ofm_viewer::{Document, Encoding, Match, Needle, SearchOptions};

use super::layout::Params;
use super::scan::{LineCounts, family};
use super::view::ViewerView;
use super::worker::{
    Dir, GotoTarget, Laid, Msg, PageReq, PageWorker, SearchSpec, spawn_goto_line, spawn_index,
    spawn_open, spawn_search,
};
use crate::chrome::statusline::{SizeFormat, format_size};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::wiring;

/// Widgets of the window the session updates.
pub struct Ui {
    /// Full path of the file (header).
    pub title: gtk::Label,
    /// Encoding / mode summary (status line, left).
    pub left: gtk::Label,
    /// Position summary (status line, right).
    pub right: gtk::Label,
    /// A standing remark about this document (hidden while empty).
    pub notice: gtk::Label,
    /// Vertical scroll bar.
    pub scroll: gtk::Scrollbar,
}

struct State {
    path: PathBuf,
    siblings: Vec<PathBuf>,
    sibling_idx: usize,
    doc_id: u64,
    doc: Option<Arc<Document>>,
    detected: Option<Encoding>,
    /// `None` = auto-detect.
    choice: Option<Encoding>,
    hex: bool,
    wrap: bool,
    hscroll: usize,
    vp: (usize, usize),
    top: u64,
    eof: bool,
    /// Bytes covered by the current page (scroll bar page size).
    page_bytes: u64,
    line_no: Option<u64>,
    counts: Arc<Mutex<LineCounts>>,
    index_cancel: CancelToken,
    index_done: u64,
    index_complete: bool,
    total_lines: Option<u64>,
    last_search: Option<SearchSpec>,
    hl: Option<Range<u64>>,
    search_id: u64,
    search_cancel: Option<CancelToken>,
    notice: Option<String>,
    failed: bool,
}

/// One open viewer.
pub struct Session {
    ctx: Weak<AppContext>,
    /// The viewer window.
    pub window: adw::ApplicationWindow,
    view: ViewerView,
    ui: Ui,
    worker: PageWorker,
    tx: async_channel::Sender<Msg>,
    st: RefCell<State>,
    /// Guard so programmatic scroll bar updates do not jump.
    scroll_guard: Cell<bool>,
    /// When the user last moved the scroll bar (it is left alone while being dragged).
    user_scroll: Cell<Option<std::time::Instant>>,
    /// One-shot cleanup hook (e.g. the archive viewer's private temp dir), run when the window
    /// closes. `destroy` cannot be used for this: GTK defers it far beyond window teardown.
    on_close: RefCell<Option<Rc<dyn Fn()>>>,
}

/// Initial settings from `[viewer]`.
pub struct Initial {
    /// Wrap long lines.
    pub wrap: bool,
    /// Forced encoding (`None` = auto-detect).
    pub encoding: Option<Encoding>,
}

impl Session {
    /// Create the session of `window`; call [`Session::open`] to load a file.
    pub fn new(
        ctx: &Rc<AppContext>,
        window: adw::ApplicationWindow,
        view: ViewerView,
        ui: Ui,
        initial: &Initial,
    ) -> Rc<Session> {
        let (tx, rx) = async_channel::unbounded();
        let session = Rc::new(Session {
            ctx: Rc::downgrade(ctx),
            window,
            view,
            ui,
            worker: PageWorker::spawn(tx.clone()),
            tx,
            st: RefCell::new(State {
                path: PathBuf::new(),
                siblings: Vec::new(),
                sibling_idx: 0,
                doc_id: 0,
                doc: None,
                detected: None,
                choice: initial.encoding,
                hex: false,
                wrap: initial.wrap,
                hscroll: 0,
                vp: (0, 0),
                top: 0,
                eof: false,
                page_bytes: 0,
                line_no: None,
                counts: Arc::new(Mutex::new(LineCounts::new(family(Encoding::Utf8), 0))),
                index_cancel: CancelToken::new(),
                index_done: 0,
                index_complete: false,
                total_lines: None,
                last_search: None,
                hl: None,
                search_id: 0,
                search_cancel: None,
                notice: None,
                failed: false,
            }),
            scroll_guard: Cell::new(false),
            user_scroll: Cell::new(None),
            on_close: RefCell::new(None),
        });
        let weak = Rc::downgrade(&session);
        wiring::attach(
            rx,
            move |msg| match weak.upgrade() {
                Some(s) => {
                    s.on_msg(msg);
                    std::ops::ControlFlow::Continue(())
                }
                None => std::ops::ControlFlow::Break(()),
            },
            || {},
        );
        let weak = Rc::downgrade(&session);
        session
            .view
            .set_viewport_callback(Rc::new(move |rows, cols| {
                if let Some(s) = weak.upgrade() {
                    s.on_viewport(rows, cols);
                }
            }));
        let weak = Rc::downgrade(&session);
        session
            .ui
            .scroll
            .adjustment()
            .connect_value_changed(move |adj| {
                if let Some(s) = weak.upgrade()
                    && !s.scroll_guard.get()
                {
                    s.user_scroll.set(Some(std::time::Instant::now()));
                    s.jump_to(adj.value() as u64);
                }
            });
        session
    }

    // ---- loading -----------------------------------------------------------------------------

    /// Show `siblings[idx]` (the files the next/previous commands step through).
    pub fn open(self: &Rc<Self>, siblings: Vec<PathBuf>, idx: usize) {
        let Some(path) = siblings.get(idx).cloned() else {
            return;
        };
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        {
            let mut st = self.st.borrow_mut();
            st.index_cancel.cancel();
            if let Some(c) = st.search_cancel.take() {
                c.cancel();
            }
            st.doc_id += 1;
            st.path = path.clone();
            st.siblings = siblings;
            st.sibling_idx = idx;
            st.doc = None;
            st.detected = None;
            st.hex = false;
            st.hscroll = 0;
            st.top = 0;
            st.eof = false;
            st.page_bytes = 0;
            st.line_no = None;
            st.index_done = 0;
            st.index_complete = false;
            st.total_lines = None;
            st.hl = None;
            st.notice = None;
            st.failed = false;
            st.index_cancel = CancelToken::new();
        }
        let name = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |n| n.to_string_lossy())
            .into_owned();
        self.window.set_title(Some(&name));
        self.ui.title.set_text(&path.to_string_lossy());
        self.view.show_message(Some(tr("Opening...")));
        let doc_id = self.st.borrow().doc_id;
        spawn_open(&ctx.services.pool, path, doc_id, self.tx.clone());
        self.update_status();
    }

    /// Stop background work (window closed).
    pub fn shutdown(&self) {
        let st = self.st.borrow();
        st.index_cancel.cancel();
        if let Some(c) = &st.search_cancel {
            c.cancel();
        }
    }

    /// Show `text` in the status line for as long as the window is open.
    pub fn set_notice(&self, text: &str) {
        self.ui.notice.set_text(text);
        self.ui.notice.set_visible(true);
    }

    /// The standing remark, if there is one (tests).
    pub fn notice_text(&self) -> Option<String> {
        self.ui
            .notice
            .property::<bool>("visible")
            .then(|| self.ui.notice.text().to_string())
    }

    /// Register one-shot cleanup to run when the window's close request is granted (the
    /// `close-request` handler runs it before GTK tears the window down; the `destroy`
    /// signal is too late — GTK defers it indefinitely).
    pub fn set_on_close(&self, f: impl Fn() + 'static) {
        *self.on_close.borrow_mut() = Some(Rc::new(f));
    }

    /// Run and clear the [`set_on_close`](Self::set_on_close) hook (window close-request handler).
    pub(crate) fn run_on_close(&self) {
        if let Some(f) = self.on_close.borrow_mut().take() {
            f();
        }
    }

    // ---- state helpers -------------------------------------------------------------------------

    fn encoding_of(st: &State) -> Encoding {
        st.choice.or(st.detected).unwrap_or(Encoding::Utf8)
    }

    /// Encoding in use.
    pub fn encoding(&self) -> Encoding {
        Self::encoding_of(&self.st.borrow())
    }

    /// The forced encoding (`None` = auto).
    pub fn encoding_choice(&self) -> Option<Encoding> {
        self.st.borrow().choice
    }

    /// Whether hex mode is on.
    pub fn is_hex(&self) -> bool {
        self.st.borrow().hex
    }

    /// Whether wrapping is on.
    pub fn is_wrap(&self) -> bool {
        self.st.borrow().wrap
    }

    /// Current top offset.
    pub fn top(&self) -> u64 {
        self.st.borrow().top
    }

    /// The highlighted match.
    pub fn hl(&self) -> Option<Range<u64>> {
        self.st.borrow().hl.clone()
    }

    /// Length of the open document (0 while opening).
    pub fn len(&self) -> u64 {
        self.st.borrow().doc.as_ref().map_or(0, |d| d.len())
    }

    /// True when no document is loaded or it is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Path of the shown file.
    pub fn path(&self) -> PathBuf {
        self.st.borrow().path.clone()
    }

    /// Whether a search is running.
    pub fn searching(&self) -> bool {
        self.st.borrow().search_cancel.is_some()
    }

    /// The last search (for the dialog defaults).
    pub fn last_search(&self) -> Option<SearchSpec> {
        self.st.borrow().last_search.clone()
    }

    fn request(&self, f: impl FnOnce(&mut PageReq)) {
        let st = self.st.borrow();
        let Some(doc) = st.doc.clone() else { return };
        if st.vp.0 == 0 || st.vp.1 == 0 {
            return;
        }
        let mut req = PageReq {
            doc_id: st.doc_id,
            doc,
            counts: Arc::clone(&st.counts),
            hex: st.hex,
            params: Params {
                enc: Self::encoding_of(&st),
                wrap: st.wrap,
                cols: st.vp.1,
                hscroll: if st.wrap { 0 } else { st.hscroll },
            },
            nrows: st.vp.0,
            set_top: None,
            to_end: false,
            rows_delta: 0,
            pages_delta: 0,
            back_rows: 0,
            hl: st.hl.clone(),
            reveal: false,
        };
        drop(st);
        f(&mut req);
        self.worker.send(req);
    }

    fn clear_notice(&self) {
        self.st.borrow_mut().notice = None;
    }

    // ---- commands ------------------------------------------------------------------------------

    /// Scroll by `n` rows (negative = up).
    pub fn move_rows(&self, n: i64) {
        self.clear_notice();
        self.request(|r| r.rows_delta = n);
    }

    /// Scroll by `n` pages (negative = up).
    pub fn move_pages(&self, n: i64) {
        self.clear_notice();
        self.request(|r| r.pages_delta = n);
    }

    /// Go to the start of the file.
    pub fn go_top(&self) {
        self.clear_notice();
        self.request(|r| r.set_top = Some(0));
    }

    /// Go to the end of the file.
    pub fn go_bottom(&self) {
        self.clear_notice();
        self.request(|r| r.to_end = true);
    }

    /// Jump to byte offset `off` (scroll bar, goto).
    pub fn jump_to(&self, off: u64) {
        self.clear_notice();
        self.request(|r| r.set_top = Some(off));
    }

    /// Scroll horizontally by `cols` (negative = left); no-wrap text mode only.
    pub fn scroll_cols(&self, cols: i64) {
        let mut changed = false;
        {
            let mut st = self.st.borrow_mut();
            if !st.wrap && !st.hex {
                let h = (st.hscroll as i64 + cols).max(0) as usize;
                changed = h != st.hscroll;
                st.hscroll = h;
            }
        }
        if changed {
            self.request(|_| {});
        }
    }

    /// Toggle hex mode (F4).
    pub fn toggle_hex(&self) {
        let top = {
            let mut st = self.st.borrow_mut();
            st.hex = !st.hex;
            st.notice = None;
            st.top
        };
        self.request(|r| r.set_top = Some(top));
        self.update_status();
    }

    /// Toggle wrapping (F2).
    pub fn toggle_wrap(&self) {
        let top = {
            let mut st = self.st.borrow_mut();
            st.wrap = !st.wrap;
            st.hscroll = 0;
            st.notice = None;
            st.top
        };
        self.request(|r| r.set_top = Some(top));
        self.update_status();
    }

    /// Force `choice` (`None` = auto-detect) and redraw.
    pub fn set_encoding(self: &Rc<Self>, choice: Option<Encoding>) {
        let (old, top) = {
            let mut st = self.st.borrow_mut();
            let old = Self::encoding_of(&st);
            st.choice = choice;
            st.notice = None;
            st.hl = None;
            (old, st.top)
        };
        if family(old) != family(self.encoding()) {
            self.restart_index();
        }
        self.request(|r| r.set_top = Some(top));
        self.update_status();
    }

    fn restart_index(self: &Rc<Self>) {
        let (doc, enc, counts, cancel, doc_id) = {
            let mut st = self.st.borrow_mut();
            let Some(doc) = st.doc.clone() else { return };
            st.index_cancel.cancel();
            st.index_cancel = CancelToken::new();
            let enc = Self::encoding_of(&st);
            st.counts = Arc::new(Mutex::new(LineCounts::new(family(enc), doc.len())));
            st.index_done = 0;
            st.index_complete = false;
            st.total_lines = None;
            st.line_no = None;
            (
                doc,
                enc,
                Arc::clone(&st.counts),
                st.index_cancel.clone(),
                st.doc_id,
            )
        };
        spawn_index(doc, enc, counts, cancel, doc_id, self.tx.clone());
    }

    /// Go to a line / offset / percentage.
    pub fn goto(self: &Rc<Self>, target: GotoTarget) {
        self.clear_notice();
        let len = self.len();
        match target {
            GotoTarget::Offset(o) => self.jump_to(o.min(len)),
            GotoTarget::Percent(p) => self.jump_to((len as f64 * p / 100.0) as u64),
            GotoTarget::Line(n) => {
                let Some(ctx) = self.ctx.upgrade() else {
                    return;
                };
                let (doc, enc, counts, doc_id) = {
                    let st = self.st.borrow();
                    let Some(doc) = st.doc.clone() else { return };
                    (
                        doc,
                        Self::encoding_of(&st),
                        Arc::clone(&st.counts),
                        st.doc_id,
                    )
                };
                spawn_goto_line(
                    &ctx.services.pool,
                    doc,
                    enc,
                    counts,
                    n - 1,
                    doc_id,
                    self.tx.clone(),
                );
            }
        }
    }

    /// Start a search from the current position and remember it for next/previous.
    pub fn search(self: &Rc<Self>, spec: SearchSpec, dir: Dir) {
        self.st.borrow_mut().last_search = Some(spec.clone());
        self.run_search(spec, dir);
    }

    /// Repeat the last search; `false` when there is none (the caller opens the dialog).
    pub fn search_again(self: &Rc<Self>, dir: Dir) -> bool {
        let Some(spec) = self.last_search() else {
            return false;
        };
        self.run_search(spec, dir);
        true
    }

    fn run_search(self: &Rc<Self>, spec: SearchSpec, dir: Dir) {
        let (doc, enc, from, cancel, doc_id, search_id) = {
            let mut st = self.st.borrow_mut();
            let Some(doc) = st.doc.clone() else { return };
            if let Some(c) = st.search_cancel.take() {
                c.cancel();
            }
            let cancel = CancelToken::new();
            st.search_cancel = Some(cancel.clone());
            st.search_id += 1;
            st.notice = Some(tr("Searching... (Esc cancels)").to_owned());
            let from = match (&st.hl, dir) {
                (Some(h), Dir::Forward) => h.start + 1,
                (Some(h), Dir::Backward) => h.start,
                (None, _) => st.top,
            };
            (
                doc,
                Self::encoding_of(&st),
                from,
                cancel,
                st.doc_id,
                st.search_id,
            )
        };
        spawn_search(
            doc,
            enc,
            spec,
            dir,
            from,
            cancel,
            doc_id,
            search_id,
            self.tx.clone(),
        );
        self.update_status();
    }

    /// Stop a running search; returns whether there was one.
    pub fn cancel_search(&self) -> bool {
        let cancel = self.st.borrow_mut().search_cancel.take();
        match cancel {
            Some(c) => {
                c.cancel();
                self.st.borrow_mut().notice = Some(tr("Search cancelled").to_owned());
                self.update_status();
                true
            }
            None => false,
        }
    }

    /// Show the file `delta` positions away in the panel's file list.
    pub fn step_file(self: &Rc<Self>, delta: i64) {
        let (list, idx) = {
            let st = self.st.borrow();
            (st.siblings.clone(), st.sibling_idx as i64 + delta)
        };
        if idx >= 0 && (idx as usize) < list.len() {
            self.open(list, idx as usize);
        }
    }

    /// The files `viewer.next_file` steps through and the position of the current one.
    pub fn siblings(&self) -> (Vec<PathBuf>, usize) {
        let st = self.st.borrow();
        (st.siblings.clone(), st.sibling_idx)
    }

    // ---- worker answers ------------------------------------------------------------------------

    fn on_viewport(self: &Rc<Self>, rows: usize, cols: usize) {
        self.st.borrow_mut().vp = (rows, cols);
        let top = self.st.borrow().top;
        self.request(|r| r.set_top = Some(top));
    }

    fn on_msg(self: &Rc<Self>, msg: Msg) {
        let current = self.st.borrow().doc_id;
        match msg {
            Msg::Opened { doc_id, result } if doc_id == current => self.on_opened(result),
            Msg::Page { doc_id, page } if doc_id == current => self.on_page(page),
            Msg::Index {
                doc_id,
                done,
                complete,
            } if doc_id == current => {
                let refresh = {
                    let mut st = self.st.borrow_mut();
                    st.index_done = done;
                    st.index_complete = complete;
                    if complete {
                        st.total_lines = st.counts.lock().ok().and_then(|c| c.total_lines());
                    }
                    // The first page may have been laid out before the chunk holding it was
                    // counted: ask again until its line number is known.
                    !st.hex && st.line_no.is_none() && st.doc.is_some()
                };
                if refresh {
                    // No `set_top`: the worker keeps its own position (queued moves survive).
                    self.request(|_| {});
                }
                self.update_status();
            }
            Msg::Goto { doc_id, offset } if doc_id == current => match offset {
                Some(o) => self.jump_to(o),
                None => {
                    self.st.borrow_mut().notice =
                        Some(tr("That line is not counted yet; try again in a moment").to_owned());
                    self.update_status();
                }
            },
            Msg::Search {
                doc_id,
                search_id,
                result,
                cancelled,
                wrapped,
            } if doc_id == current && search_id == self.st.borrow().search_id => {
                self.on_search(result, cancelled, wrapped);
            }
            _ => {}
        }
    }

    fn on_opened(self: &Rc<Self>, result: Result<(Arc<Document>, ofm_viewer::Detection), String>) {
        match result {
            Err(e) => {
                self.st.borrow_mut().failed = true;
                self.view
                    .show_message(Some(&format!("{} {e}", tr("Cannot open the file:"))));
                self.update_status();
            }
            Ok((doc, det)) => {
                let empty = doc.is_empty();
                {
                    let mut st = self.st.borrow_mut();
                    st.detected = Some(det.encoding);
                    st.doc = Some(doc);
                }
                if empty {
                    self.view.show_message(Some(tr("(empty file)")));
                    self.update_status();
                    return;
                }
                self.restart_index();
                self.request(|r| r.set_top = Some(0));
                self.update_status();
            }
        }
    }

    fn on_page(&self, page: Result<Laid, String>) {
        match page {
            Err(e) => {
                self.view
                    .show_message(Some(&format!("{} {e}", tr("Cannot read the file:"))));
            }
            Ok(Laid::Text(p, line)) => {
                if self.st.borrow().hex {
                    return;
                }
                {
                    let mut st = self.st.borrow_mut();
                    st.top = p.top;
                    st.hscroll = p.hscroll;
                    st.eof = p.eof;
                    st.line_no = line;
                    st.page_bytes = p.rows.last().map_or(0, |r| r.next.saturating_sub(p.top));
                }
                self.view.show_text(&p);
            }
            Ok(Laid::Hex(p)) => {
                let (hl, len) = {
                    let mut st = self.st.borrow_mut();
                    if !st.hex {
                        return;
                    }
                    st.top = p.top;
                    st.eof = p.eof;
                    st.line_no = None;
                    st.page_bytes = p.rows.iter().map(|r| r.bytes.len() as u64).sum();
                    (st.hl.clone(), st.doc.as_ref().map_or(0, |d| d.len()))
                };
                self.view.show_hex(&p, hl.as_ref(), len);
            }
        }
        self.update_status();
    }

    fn on_search(
        self: &Rc<Self>,
        result: Result<Option<Match>, String>,
        cancelled: bool,
        wrapped: bool,
    ) {
        self.st.borrow_mut().search_cancel = None;
        if cancelled {
            return;
        }
        match result {
            Err(e) => {
                self.st.borrow_mut().notice = Some(e);
                self.update_status();
            }
            Ok(None) => {
                self.st.borrow_mut().notice = Some(tr("Not found").to_owned());
                self.update_status();
            }
            Ok(Some(m)) => {
                {
                    let mut st = self.st.borrow_mut();
                    st.hl = Some(m.offset..m.offset + m.len.max(1) as u64);
                    st.notice = wrapped.then(|| tr("Search wrapped around").to_owned());
                }
                self.request(|r| {
                    r.set_top = Some(m.offset);
                    r.back_rows = 2;
                    r.reveal = true;
                    r.hl = Some(m.offset..m.offset + m.len.max(1) as u64);
                });
            }
        }
    }

    // ---- status line ---------------------------------------------------------------------------

    fn update_status(&self) {
        let st = self.st.borrow();
        let enc = Self::encoding_of(&st);
        let auto = if st.choice.is_none() {
            format!(" ({})", tr("auto"))
        } else {
            String::new()
        };
        let mode = if st.hex {
            tr("Hex")
        } else if st.wrap {
            tr("Wrap")
        } else {
            tr("Text")
        };
        let left = if st.doc.is_none() {
            if st.failed {
                tr("Error").to_owned()
            } else {
                tr("Opening...").to_owned()
            }
        } else {
            format!("{}{auto}  \u{b7}  {mode}", enc.label())
        };
        self.ui.left.set_text(&left);
        let len = st.doc.as_ref().map_or(0, |d| d.len());
        let right = match &st.notice {
            Some(n) => n.clone(),
            None => status_right(&StatusInfo {
                hex: st.hex,
                top: st.top,
                len,
                eof: st.eof,
                line: st.line_no,
                total_lines: st.total_lines,
                index_done: st.index_done,
                index_complete: st.index_complete,
            }),
        };
        self.ui.right.set_text(&right);
        let (page, top) = (st.page_bytes, st.top);
        drop(st);
        self.sync_scrollbar(len, top, page);
    }

    fn sync_scrollbar(&self, len: u64, top: u64, page: u64) {
        if self
            .user_scroll
            .get()
            .is_some_and(|t| t.elapsed() < std::time::Duration::from_millis(400))
        {
            return;
        }
        let adj = self.ui.scroll.adjustment();
        self.scroll_guard.set(true);
        let upper = len.max(1) as f64;
        adj.configure(
            top as f64,
            0.0,
            upper,
            (page.max(1) as f64 / 4.0).max(1.0),
            page.max(1) as f64,
            page.min(len).max(1) as f64,
        );
        self.scroll_guard.set(false);
    }

    /// The text of the status line's right part (for tests and the quick check in tests).
    pub fn status_text(&self) -> (String, String) {
        (
            self.ui.left.text().to_string(),
            self.ui.right.text().to_string(),
        )
    }

    /// Current file's directory (for dialogs).
    pub fn dir(&self) -> Option<PathBuf> {
        let p = self.st.borrow().path.clone();
        Path::new(&p).parent().map(Path::to_path_buf)
    }

    /// Needle for the search dialog defaults.
    pub fn default_needle(&self) -> Option<Needle> {
        self.st
            .borrow()
            .last_search
            .as_ref()
            .map(|s| s.needle.clone())
    }

    /// Search options of the last search.
    pub fn default_opts(&self) -> SearchOptions {
        self.st.borrow().last_search.as_ref().map_or(
            SearchOptions {
                case_sensitive: false,
                wrap: true,
            },
            |s| s.opts,
        )
    }
}

/// Inputs of [`status_right`].
pub struct StatusInfo {
    /// Hex mode.
    pub hex: bool,
    /// Offset of the first visible row.
    pub top: u64,
    /// File length.
    pub len: u64,
    /// The last row is visible.
    pub eof: bool,
    /// 1-based line of the first row, when counted.
    pub line: Option<u64>,
    /// Total lines, when the scan is complete.
    pub total_lines: Option<u64>,
    /// Bytes scanned so far.
    pub index_done: u64,
    /// The scan finished.
    pub index_complete: bool,
}

/// Right part of the status line: line / offset / percentage.
pub fn status_right(i: &StatusInfo) -> String {
    let pct = if i.len == 0 || i.eof {
        100
    } else {
        ((i.top as u128 * 100) / i.len as u128) as u64
    };
    let mut parts: Vec<String> = Vec::new();
    if !i.hex {
        match (i.line, i.total_lines) {
            (Some(l), Some(t)) => parts.push(format!("{} {l} / {t}", tr("Line"))),
            (Some(l), None) => parts.push(format!("{} {l}", tr("Line"))),
            (None, _) => {}
        }
        if !i.index_complete && i.len > 0 {
            let p = i.index_done * 100 / i.len;
            parts.push(format!("{} {p}%", tr("counting")));
        }
    }
    parts.push(format!(
        "{} {} / {}",
        tr("Offset"),
        i.top,
        format_size(i.len, SizeFormat::Binary)
    ));
    parts.push(format!("{pct}%"));
    parts.join("  \u{b7}  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info() -> StatusInfo {
        StatusInfo {
            hex: false,
            top: 500,
            len: 1000,
            eof: false,
            line: Some(12),
            total_lines: Some(40),
            index_done: 1000,
            index_complete: true,
        }
    }

    #[test]
    fn status_shows_line_offset_and_percent() {
        let s = status_right(&info());
        assert!(s.contains("Line 12 / 40"), "{s}");
        assert!(s.contains("Offset 500 / 1000 B"), "{s}");
        assert!(s.ends_with("50%"), "{s}");
    }

    #[test]
    fn status_marks_counting_and_hex() {
        let s = status_right(&StatusInfo {
            index_complete: false,
            index_done: 250,
            total_lines: None,
            ..info()
        });
        assert!(s.contains("Line 12"), "{s}");
        assert!(s.contains("counting 25%"), "{s}");
        let h = status_right(&StatusInfo {
            hex: true,
            ..info()
        });
        assert!(!h.contains("Line"), "{h}");
    }

    #[test]
    fn at_the_end_it_says_100_percent() {
        let s = status_right(&StatusInfo {
            eof: true,
            top: 900,
            ..info()
        });
        assert!(s.ends_with("100%"), "{s}");
        let e = status_right(&StatusInfo {
            len: 0,
            top: 0,
            eof: true,
            line: None,
            total_lines: Some(0),
            ..info()
        });
        assert!(e.ends_with("100%"), "{e}");
    }
}

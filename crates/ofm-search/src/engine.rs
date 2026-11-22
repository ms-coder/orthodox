//! Thread orchestration (private): one walker thread plus a small bounded pool of content
//! matchers, a supervisor that flushes batches and sends the terminal event, and the
//! shared state between them.
//!
//! ```text
//! walker thread ---Task--> bounded queue ---> min(4, cores) matcher threads
//!     | (name/size/mtime hits direct)             | (content hits, archive hits)
//!     +---------------- both ---------------------+--> SearchShared --> event channel
//! supervisor: flushes partial batches every BATCH_INTERVAL, Progress at <= 10 Hz,
//!             joins everyone, sends exactly one Done/Failed.
//! ```
//!
//! Every send goes through `try_send` + sleep: a full channel throttles workers against a
//! stalled UI, while a cancelled search or a dropped receiver stops them promptly.

use std::any::Any;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError as MpscFull};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use async_channel::{Receiver as EventReceiver, Sender as EventSender, TrySendError};
use ofm_core::{CancelToken, Entry, EntryKind, Location};
use ofm_viewer::document::Document;

use crate::archives::{self, ArchiveCheck};
use crate::compile::{ContentSpec, NameMatcher};
use crate::content::search_document;
use crate::error::SearchError;
use crate::event::{Hit, SearchEvent, SearchProgress, SearchSummary, SearchWarning};
use crate::handle::{BATCH_INTERVAL, BATCH_SIZE, CHANNEL_CAPACITY, PROGRESS_INTERVAL};
use crate::query::{SearchQuery, SizeFilter, TimeFilter};
use crate::walker::{self, RootInfo, WalkOptions, Walked};

/// Depth of the walker-to-matcher task queue (back-pressure on the walker).
const TASK_QUEUE: usize = 64;
/// Sleep between retried channel sends.
const SEND_RETRY: Duration = Duration::from_millis(5);
/// Sleep between matcher polls while waiting for tasks.
const MATCHER_POLL: Duration = Duration::from_millis(50);

/// One file for the matcher pool: filters already passed, `real_path` set.
struct Task {
    path: PathBuf,
    entry: Entry,
}

/// Pending hit batch and its flush deadline.
struct BatchState {
    hits: Vec<Hit>,
    deadline: Option<Instant>,
}

/// State shared by all threads of one search.
pub(crate) struct SearchShared {
    tx: EventSender<SearchEvent>,
    cancel: CancelToken,
    started: Instant,
    batch: Mutex<BatchState>,
    progress_at: Mutex<Instant>,
    dirs: AtomicU64,
    files: AtomicU64,
    /// Hits accepted (pushed into batches).
    hits: AtomicU64,
    current: Mutex<Option<PathBuf>>,
    truncated: AtomicBool,
    failure: Mutex<Option<SearchError>>,
    archive_warned: AtomicBool,
    max_hits: Option<usize>,
}

fn guard<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SearchShared {
    fn new(tx: EventSender<SearchEvent>, cancel: CancelToken, max_hits: Option<usize>) -> Self {
        SearchShared {
            tx,
            cancel,
            started: Instant::now(),
            batch: Mutex::new(BatchState {
                hits: Vec::new(),
                deadline: None,
            }),
            // Seeded "one interval ago" so the very first directory emits a Progress at
            // once (the UI should never wait 100 ms to learn the search is running).
            progress_at: Mutex::new(match Instant::now().checked_sub(PROGRESS_INTERVAL) {
                Some(t) => t,
                None => Instant::now(),
            }),
            dirs: AtomicU64::new(0),
            files: AtomicU64::new(0),
            hits: AtomicU64::new(0),
            current: Mutex::new(None),
            truncated: AtomicBool::new(false),
            failure: Mutex::new(None),
            archive_warned: AtomicBool::new(false),
            max_hits,
        }
    }

    /// Send an event, parking on a full channel. `false` = stop working (cancelled or the
    /// receiver is gone). Terminal events pass `ignore_cancel` so `Done`/`Failed` are still
    /// delivered after a cancel.
    fn send_ev(&self, ev: SearchEvent, ignore_cancel: bool) -> bool {
        let mut ev = ev;
        loop {
            if !ignore_cancel && self.cancel.is_cancelled() {
                return false;
            }
            match self.tx.try_send(ev) {
                Ok(()) => return true,
                Err(TrySendError::Full(back)) => {
                    ev = back;
                    thread::sleep(SEND_RETRY);
                }
                Err(TrySendError::Closed(_)) => {
                    self.cancel.cancel();
                    return false;
                }
            }
        }
    }

    /// Accept a hit; flushes a full batch immediately, arms the 50 ms deadline otherwise.
    /// Enforces `max_hits` (excess hits are dropped, the search is cancelled and marked
    /// truncated).
    fn push_hit(&self, hit: Hit) {
        if let Some(max) = self.max_hits
            && self.hits.load(Ordering::Acquire) >= max as u64
        {
            self.truncated.store(true, Ordering::Release);
            self.cancel.cancel();
            return;
        }
        let flush_now = {
            let mut st = guard(&self.batch);
            if st.hits.is_empty() {
                st.deadline = Some(Instant::now() + BATCH_INTERVAL);
            }
            st.hits.push(hit);
            self.hits.fetch_add(1, Ordering::Relaxed);
            st.hits.len() >= BATCH_SIZE
        };
        if flush_now {
            self.flush();
        }
    }

    /// Send whatever is buffered. Results collected so far stay valid across a cancel, so
    /// the flush ignores the cancel flag (a dropped receiver still stops it).
    fn flush(&self) {
        let batch = {
            let mut st = guard(&self.batch);
            st.deadline = None;
            std::mem::take(&mut st.hits)
        };
        if !batch.is_empty() {
            self.send_ev(SearchEvent::Found(batch), true);
        }
    }

    /// Flush if the partial batch has sat past its deadline.
    fn maybe_flush(&self) {
        let due = {
            let st = guard(&self.batch);
            st.deadline.is_some_and(|d| Instant::now() >= d)
        };
        if due {
            self.flush();
        }
    }

    /// Send `Progress` if the interval elapsed (single-flight via the mutex).
    fn maybe_progress(&self) {
        self.progress_impl(false);
    }

    /// Final progress with the complete counters and no current directory; sent by the
    /// supervisor right before the terminal event so the UI's last update is exact.
    fn progress_final(&self) {
        self.progress_impl(true);
    }

    fn progress_impl(&self, force: bool) {
        let now = Instant::now();
        {
            let mut at = guard(&self.progress_at);
            if !force && now.duration_since(*at) < PROGRESS_INTERVAL {
                return;
            }
            *at = now;
        }
        let current = if force {
            None
        } else {
            guard(&self.current).clone()
        };
        let ev = SearchEvent::Progress(SearchProgress {
            dirs_scanned: self.dirs.load(Ordering::Relaxed),
            files_scanned: self.files.load(Ordering::Relaxed),
            hits: self.hits.load(Ordering::Relaxed),
            current,
        });
        self.send_ev(ev, false);
    }

    /// Report a non-fatal problem with one path.
    fn warn(&self, path: PathBuf, message: String) {
        self.send_ev(SearchEvent::Warning(SearchWarning { path, message }), false);
    }

    /// Record the first failure and stop everyone.
    fn fail(&self, err: SearchError) {
        {
            let mut f = guard(&self.failure);
            if f.is_none() {
                *f = Some(err);
            }
        }
        self.cancel.cancel();
    }

    /// True once the search must stop (user cancel or truncation).
    fn stopping(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

/// Reporting filters (never affect traversal): name mask/regex, size, mtime.
pub(crate) struct EntryFilter {
    name: NameMatcher,
    size: SizeFilter,
    mtime: TimeFilter,
}

impl EntryFilter {
    pub(crate) fn new(name: NameMatcher, size: SizeFilter, mtime: TimeFilter) -> Self {
        EntryFilter { name, size, mtime }
    }

    /// True if an entry with this inner `path` (name = last component), kind, size and
    /// mtime passes the reporting filters. Size applies to files only; a content query
    /// suppresses directory hits at the call sites.
    pub(crate) fn accepts(&self, path: &Path, kind: EntryKind, size: u64, mtime_ns: i128) -> bool {
        let name = path.file_name().unwrap_or(path.as_os_str());
        if !self.name.is_match(name) {
            return false;
        }
        if kind == EntryKind::File {
            if let Some(min) = self.size.min
                && size < min
            {
                return false;
            }
            if let Some(max) = self.size.max
                && size > max
            {
                return false;
            }
        }
        if let Some(after) = self.mtime.after_ns
            && mtime_ns < after
        {
            return false;
        }
        if let Some(before) = self.mtime.before_ns
            && mtime_ns > before
        {
            return false;
        }
        true
    }
}

/// mtime of std metadata as ns since the epoch.
fn mtime_ns(md: &std::fs::Metadata) -> i128 {
    use std::os::unix::fs::MetadataExt;
    i128::from(md.mtime()) * 1_000_000_000 + i128::from(md.mtime_nsec())
}

/// A local hit from a walked entry.
fn tree_hit(w: &Walked) -> Hit {
    let mut e = Entry::new(w.name.clone(), w.kind);
    if w.kind == EntryKind::File {
        e.size = w.md.len();
    }
    e.mtime_ns = mtime_ns(&w.md);
    use std::os::unix::fs::MetadataExt;
    e.mode = w.md.mode();
    e.uid = w.md.uid();
    e.gid = w.md.gid();
    e.real_path = Some(w.path.clone());
    Hit {
        location: Location::Local(w.path.clone()),
        entry: e,
        content: None,
    }
}

fn panic_message(p: Box<dyn Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Everything the worker threads need; shared as an `Arc`.
pub(crate) struct Engine {
    shared: Arc<SearchShared>,
    filter: EntryFilter,
    content: Option<ContentSpec>,
    opts: WalkOptions,
    search_archives: bool,
    match_dirs: bool,
    has_content: bool,
}

impl Engine {
    /// Walker thread body: walks the roots, feeds the matcher pool, emits direct hits.
    fn walker_main(self: Arc<Self>, roots: Vec<RootInfo>, tasks: Option<SyncSender<Task>>) {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.walk_tree(&roots, tasks.as_ref());
        }));
        if let Err(p) = result {
            self.shared
                .fail(SearchError::Panic(format!("walker: {}", panic_message(p))));
        }
        // `tasks` dropped here: matchers drain and exit.
    }

    fn walk_tree(&self, roots: &[RootInfo], tasks: Option<&SyncSender<Task>>) {
        let shared = &self.shared;
        let mut on_dir = |dir: &Path| {
            shared.dirs.fetch_add(1, Ordering::Relaxed);
            *guard(&shared.current) = Some(dir.to_owned());
            shared.maybe_progress();
        };
        let mut on_warn = |path: PathBuf, message: String| {
            shared.warn(path, message);
        };
        let mut on_entry = |w: &Walked| -> bool {
            if shared.stopping() {
                return false;
            }
            if w.kind.is_dir_like() {
                if self.match_dirs && !self.has_content {
                    // Size is meaningless for directories (0; the filter skips it anyway).
                    if self.filter.accepts(&w.path, w.kind, 0, mtime_ns(&w.md)) {
                        shared.push_hit(tree_hit(w));
                    }
                }
            } else {
                shared.files.fetch_add(1, Ordering::Relaxed);
                if self
                    .filter
                    .accepts(&w.path, w.kind, w.md.len(), mtime_ns(&w.md))
                {
                    // Without a content query the file itself is the hit; the pool only
                    // refines (archives inside). With one, only a content match hits.
                    if !self.has_content {
                        shared.push_hit(tree_hit(w));
                    }
                    if self.has_content || self.search_archives {
                        let task = Task {
                            path: w.path.clone(),
                            entry: tree_hit(w).entry,
                        };
                        if let Some(tx) = tasks
                            && !send_task(shared, tx, task)
                        {
                            return false;
                        }
                    }
                }
            }
            !shared.stopping()
        };
        walker::walk(
            roots,
            &self.opts,
            &shared.cancel,
            &mut on_dir,
            &mut on_entry,
            &mut on_warn,
        );
    }

    /// Matcher thread body: drains the task queue.
    fn matcher_main(self: Arc<Self>, tasks: &Mutex<Receiver<Task>>) {
        loop {
            if self.shared.stopping() {
                break;
            }
            let task = {
                let Ok(rx) = tasks.try_lock() else {
                    thread::sleep(MATCHER_POLL);
                    continue;
                };
                rx.recv_timeout(MATCHER_POLL)
            };
            match task {
                Ok(t) => {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        self.process_task(&t);
                    }));
                    if let Err(p) = result {
                        // Invariant 7, matcher scope: one bad file must not lose the search.
                        self.shared.warn(
                            t.path,
                            format!("internal error: matcher panicked: {}", panic_message(p)),
                        );
                    }
                }
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
    }

    /// Content-search a file, or search inside it when it is an archive.
    fn process_task(&self, task: &Task) {
        if self.shared.stopping() || task.entry.kind != EntryKind::File {
            return;
        }
        if self.search_archives {
            match archives::try_open_archive(&task.path, &self.shared.cancel) {
                ArchiveCheck::Inside(mut reader) => {
                    let shared = &self.shared;
                    let mut out = |hit: Hit| shared.push_hit(hit);
                    let mut warn = |msg: String| {
                        shared.warn(task.path.clone(), msg);
                    };
                    let mut sink = archives::ArchiveSink {
                        cancel: &shared.cancel,
                        files_scanned: &shared.files,
                        out: &mut out,
                        warn: &mut warn,
                    };
                    archives::search_inside(
                        &task.path,
                        reader.as_mut(),
                        &self.filter,
                        self.content.as_ref(),
                        &mut sink,
                    );
                    return;
                }
                ArchiveCheck::StubBackend => {
                    // One warning per search, not one per archive.
                    if !self.shared.archive_warned.swap(true, Ordering::Relaxed) {
                        self.shared.warn(
                            task.path.clone(),
                            "archive search not available yet".to_owned(),
                        );
                    }
                    return; // it *is* an archive: never raw-search its bytes
                }
                ArchiveCheck::Unsearchable(reason) => {
                    self.shared
                        .warn(task.path.clone(), format!("archive skipped: {reason}"));
                    return;
                }
                ArchiveCheck::PlainFile => {} // fall through to plain content search
            }
        }
        let Some(spec) = self.content.as_ref() else {
            return; // name-only search: plain files were already reported
        };
        match Document::open(&task.path) {
            Ok(doc) => match search_document(&doc, spec, &self.shared.cancel) {
                Ok(Some(ch)) => {
                    let mut hit = Hit {
                        location: Location::Local(task.path.clone()),
                        entry: task.entry.clone(),
                        content: None,
                    };
                    hit.entry.real_path = Some(task.path.clone());
                    hit.content = Some(ch);
                    self.shared.push_hit(hit);
                }
                Ok(None) => {}
                Err(e) => {
                    self.shared
                        .warn(task.path.clone(), format!("cannot read file: {e}"));
                }
            },
            Err(e) => {
                self.shared
                    .warn(task.path.clone(), format!("cannot open file: {e}"));
            }
        }
    }
}

/// Queue a task without blocking forever: parks briefly while full, gives up once
/// cancelled or once the matchers are gone.
fn send_task(shared: &SearchShared, tx: &SyncSender<Task>, task: Task) -> bool {
    let mut task = task;
    loop {
        if shared.stopping() {
            return false;
        }
        match tx.try_send(task) {
            Ok(()) => return true,
            Err(MpscFull::Full(back)) => {
                task = back;
                thread::sleep(SEND_RETRY);
            }
            Err(MpscFull::Disconnected(_)) => return false,
        }
    }
}

/// Supervisor: keeps batches/progress flowing while workers run, then joins everyone and
/// sends exactly one terminal event.
fn supervisor_main(shared: Arc<SearchShared>, handles: Vec<JoinHandle<()>>) {
    loop {
        if handles.iter().all(|h| h.is_finished()) {
            break;
        }
        shared.maybe_flush();
        shared.maybe_progress();
        thread::sleep(Duration::from_millis(10));
    }
    for h in handles {
        if let Err(p) = h.join() {
            shared.fail(SearchError::Panic(panic_message(p)));
        }
    }
    shared.progress_final();
    shared.flush();
    let failure = guard(&shared.failure).take();
    match failure {
        Some(err) => {
            shared.send_ev(SearchEvent::Failed(err), true);
        }
        None => {
            let truncated = shared.truncated.load(Ordering::Acquire);
            let cancelled = shared.cancel.is_cancelled() && !truncated;
            let summary = SearchSummary {
                hits: shared.hits.load(Ordering::Acquire),
                files_scanned: shared.files.load(Ordering::Acquire),
                dirs_scanned: shared.dirs.load(Ordering::Acquire),
                elapsed: shared.started.elapsed(),
                cancelled,
                truncated,
            };
            shared.send_ev(SearchEvent::Done(summary), true);
        }
    }
}

/// Stat the roots (called on the caller's thread before spawning; one metadata call per
/// root — the documented `SearchError::Root` precheck).
pub(crate) fn stat_roots(roots: &[PathBuf]) -> Result<Vec<RootInfo>, SearchError> {
    roots
        .iter()
        .map(|p| {
            let root_err = |e: std::io::Error| SearchError::Root {
                path: p.clone(),
                source: e,
            };
            let lmd = std::fs::symlink_metadata(p).map_err(root_err)?;
            let is_symlink = lmd.is_symlink();
            let md = if is_symlink {
                std::fs::metadata(p).map_err(root_err)?
            } else {
                lmd
            };
            if md.is_dir() {
                // "cannot be read at all": fail fast instead of warning mid-search.
                drop(std::fs::read_dir(p).map_err(root_err)?);
            }
            Ok(RootInfo {
                path: p.clone(),
                md,
                is_symlink,
            })
        })
        .collect()
}

/// Pool size: explicit, or automatic (min(4, available cores)).
fn pool_size(threads: usize) -> usize {
    if threads == 0 {
        thread::available_parallelism()
            .map_or(4, |n| n.get())
            .clamp(1, 4)
    } else {
        threads
    }
}

/// Start the workers and return the event receiver. Compiles patterns again (validate()
/// already proved them good); a surprise failure becomes a `Failed` event.
pub(crate) fn spawn(
    query: SearchQuery,
    roots: Vec<RootInfo>,
    cancel: CancelToken,
) -> EventReceiver<SearchEvent> {
    let (tx, rx) = async_channel::bounded(CHANNEL_CAPACITY);
    let shared = Arc::new(SearchShared::new(tx, cancel.clone(), query.max_hits));
    let filter = match NameMatcher::new(&query.name) {
        Ok(n) => EntryFilter::new(n, query.size, query.mtime),
        Err(err) => {
            shared.send_ev(SearchEvent::Failed(err), true);
            return rx;
        }
    };
    let content = match &query.content {
        Some(c) => match ContentSpec::new(c) {
            Ok(spec) => Some(spec),
            Err(err) => {
                shared.send_ev(SearchEvent::Failed(err), true);
                return rx;
            }
        },
        None => None,
    };
    let has_content = content.is_some();
    let engine = Arc::new(Engine {
        shared: shared.clone(),
        filter,
        content,
        opts: WalkOptions {
            follow_symlinks: query.follow_symlinks,
            same_filesystem: query.same_filesystem,
            include_hidden: query.include_hidden,
            max_depth: query.max_depth,
        },
        search_archives: query.search_archives,
        match_dirs: query.match_dirs,
        has_content,
    });

    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    let needs_pool = has_content || engine.search_archives;
    let task_rx = if needs_pool {
        let (task_tx, task_rx) = std::sync::mpsc::sync_channel::<Task>(TASK_QUEUE);
        let walker_engine = engine.clone();
        match thread::Builder::new()
            .name("ofm-search-walker".into())
            .spawn(move || walker_engine.walker_main(roots, Some(task_tx)))
        {
            Ok(h) => handles.push(h),
            Err(e) => {
                engine
                    .shared
                    .fail(SearchError::Panic(format!("cannot spawn walker: {e}")));
            }
        }
        Some(Arc::new(Mutex::new(task_rx)))
    } else {
        let walker_engine = engine.clone();
        match thread::Builder::new()
            .name("ofm-search-walker".into())
            .spawn(move || walker_engine.walker_main(roots, None))
        {
            Ok(h) => handles.push(h),
            Err(e) => {
                engine
                    .shared
                    .fail(SearchError::Panic(format!("cannot spawn walker: {e}")));
            }
        }
        None
    };
    if let Some(task_rx) = task_rx {
        for _ in 0..pool_size(query.threads) {
            let matcher_engine = engine.clone();
            let rx = task_rx.clone();
            match thread::Builder::new()
                .name("ofm-search-matcher".into())
                .spawn(move || matcher_engine.matcher_main(&rx))
            {
                Ok(h) => handles.push(h),
                Err(e) => {
                    engine
                        .shared
                        .fail(SearchError::Panic(format!("cannot spawn matcher: {e}")));
                }
            }
        }
    }

    let supervisor_shared = shared.clone();
    match thread::Builder::new()
        .name("ofm-search-supervisor".into())
        .spawn(move || supervisor_main(supervisor_shared, handles))
    {
        Ok(_h) => {}
        Err(e) => {
            // No supervisor: deliver the failure terminal event ourselves.
            shared.fail(SearchError::Panic(format!("cannot spawn supervisor: {e}")));
            shared.flush();
            if let Some(err) = guard(&shared.failure).take() {
                shared.send_ev(SearchEvent::Failed(err), true);
            }
        }
    }
    rx
}

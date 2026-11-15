//! [`JobCtx`]: everything an operation needs while it runs, in one place (frozen by T3.0).
//!
//! The engine (T3.1) builds one `JobCtx` per job with an engine-backed [`JobSink`]; unit tests of
//! an operation (T3.1 copy, T3.2 ops, T3.4 torture) build one with a [`ScriptedSink`] and call the
//! operation directly, or go through `runner::run_sync`. So no operation ever depends on the
//! engine, and the engine never depends on an operation's internals.
//!
//! What it gives an operation:
//! - the `Fs` ([`JobCtx::fs`]) - the only way to touch the file system;
//! - [`JobCtx::checkpoint`]: pause point + cancellation (`Err(JobError::Cancelled)`); call it
//!   between items and every 1 MiB of data;
//! - progress counters with built-in 10 Hz throttling ([`JobCtx::add_bytes`] etc.);
//! - the question protocol with "apply to all" memory ([`JobCtx::resolve_conflict`],
//!   [`JobCtx::resolve_error`], [`JobCtx::attempt`], [`JobCtx::confirm_permanent_delete`]);
//! - the job report ([`JobCtx::warn`], [`JobCtx::note_created`], ...).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::Duration;

use ofm_core::{CancelToken, Responder, Wait};

use crate::error::JobError;
use crate::event::{
    Answer, ConflictAnswer, ConflictKind, ErrorAction, ItemInfo, JobEvent, Operation, Progress,
    Question, Report, Warning,
};
use crate::fs::Fs;
use crate::progress::{Clock, ProgressTracker, SystemClock};
use crate::spec::JobId;

/// How long a blocked worker sleeps between cancellation checks.
const POLL: Duration = Duration::from_millis(100);

/// Where a job's progress and questions go. Implemented by the engine (events + `Responder`) and
/// by [`ScriptedSink`] (tests).
pub trait JobSink: Send {
    /// Deliver a progress snapshot. MUST NOT block (the engine uses an unbounded channel).
    fn progress(&mut self, progress: &Progress);
    /// Ask a question and wait for the answer. Returns `None` when nobody can answer (responder
    /// dropped, job cancelled while waiting): the job then aborts.
    fn ask(&mut self, question: Question) -> Option<Answer>;
}

/// Pause/resume switch shared between the engine and a running job.
#[derive(Debug, Clone, Default)]
pub struct PauseGate {
    inner: Arc<(Mutex<bool>, Condvar)>,
}

impl PauseGate {
    /// A gate that is open (not paused).
    pub fn new() -> Self {
        Self::default()
    }

    /// Close the gate: the job blocks at its next [`JobCtx::checkpoint`].
    pub fn pause(&self) {
        *self.inner.0.lock().unwrap_or_else(PoisonError::into_inner) = true;
    }

    /// Open the gate.
    pub fn resume(&self) {
        *self.inner.0.lock().unwrap_or_else(PoisonError::into_inner) = false;
        self.inner.1.notify_all();
    }

    /// Whether the gate is closed.
    pub fn is_paused(&self) -> bool {
        *self.inner.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Block while paused; returns early when `cancel` fires (polls every 100 ms).
    pub fn wait_while_paused(&self, cancel: &CancelToken) {
        let (lock, cvar) = &*self.inner;
        let mut paused = lock.lock().unwrap_or_else(PoisonError::into_inner);
        while *paused && !cancel.is_cancelled() {
            paused = cvar
                .wait_timeout(paused, POLL)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }
}

/// Resolution of a conflict after "apply to all" memory, conditional answers and the safety
/// checks were applied. Skips are already counted in the report.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution {
    /// Replace the destination (atomic `rename_replace` of the finished temp file).
    Overwrite,
    /// Do nothing for this item.
    Skip,
    /// Write the new item under a fresh unique name (`name (1).ext`).
    RenameNew,
    /// Rename the existing item out of the way (fresh unique name), then publish under the
    /// original name with `rename_noreplace`.
    RenameExisting,
}

/// Outcome of [`JobCtx::resolve_error`] (abort is `Err(JobError::Cancelled)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorResolution {
    /// Try the operation again.
    Retry,
    /// Skip the item (already counted in the report).
    Skip,
}

/// Per-job state handed to every operation. Not `Sync`: one worker thread owns it.
pub struct JobCtx {
    fs: Arc<dyn Fs>,
    cancel: CancelToken,
    pause: PauseGate,
    sink: Box<dyn JobSink>,
    tracker: ProgressTracker,
    conflict_memory: Option<ConflictAnswer>,
    error_memory: Option<ErrorAction>,
    permanent_memory: Option<bool>,
    warnings: Vec<Warning>,
    skipped: u64,
    created: Vec<PathBuf>,
}

impl JobCtx {
    /// Full constructor (the engine uses this).
    pub fn new(
        fs: Arc<dyn Fs>,
        cancel: CancelToken,
        pause: PauseGate,
        sink: Box<dyn JobSink>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        JobCtx {
            fs,
            cancel,
            pause,
            sink,
            tracker: ProgressTracker::new(clock),
            conflict_memory: None,
            error_memory: None,
            permanent_memory: None,
            warnings: Vec::new(),
            skipped: 0,
            created: Vec::new(),
        }
    }

    /// Context with a fresh cancel token, an open pause gate and the system clock (tests).
    pub fn simple(fs: Arc<dyn Fs>, sink: impl JobSink + 'static) -> Self {
        Self::new(
            fs,
            CancelToken::new(),
            PauseGate::new(),
            Box::new(sink),
            Arc::new(SystemClock::new()),
        )
    }

    // ---- environment ----------------------------------------------------------------------
    /// The file system (cheap `Arc` clone, so it can be held while mutating `self`).
    pub fn fs(&self) -> Arc<dyn Fs> {
        Arc::clone(&self.fs)
    }

    /// A clone of the job's cancel token (hand to helpers that loop without the ctx).
    pub fn cancel_token(&self) -> CancelToken {
        self.cancel.clone()
    }

    /// Whether cancellation was requested.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Pause point + cancellation check. Blocks while the job is paused; returns
    /// `Err(JobError::Cancelled)` when cancelled (also when cancelled while paused).
    pub fn checkpoint(&mut self) -> Result<(), JobError> {
        if self.cancel.is_cancelled() {
            return Err(JobError::Cancelled);
        }
        if self.pause.is_paused() {
            self.pause.wait_while_paused(&self.cancel);
            self.tracker.reset_speed();
            if self.cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }
        }
        Ok(())
    }

    /// A process-unique, unpredictable-enough suffix for temp names (`.name.ofm-part-<suffix>`).
    pub fn unique_suffix(&self) -> String {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let mut x = (u64::from(std::process::id()) << 32) ^ u64::from(nanos) ^ n.rotate_left(40);
        // splitmix64 finalizer
        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        x ^= x >> 31;
        format!("{x:012x}")
    }

    // ---- progress -------------------------------------------------------------------------
    /// Plan phase on/off.
    pub fn set_planning(&mut self, planning: bool) {
        self.tracker.set_planning(planning);
        self.pump();
    }

    /// Plan phase: the totals grew.
    pub fn add_total(&mut self, files: u64, bytes: u64) {
        self.tracker.add_total(files, bytes);
        self.pump();
    }

    /// Final totals; ends the planning phase.
    pub fn set_totals(&mut self, files: u64, bytes: u64) {
        self.tracker.set_totals(files, bytes);
        self.pump();
    }

    /// `n` bytes processed.
    pub fn add_bytes(&mut self, n: u64) {
        self.tracker.add_bytes(n);
        self.pump();
    }

    /// One file finished.
    pub fn file_done(&mut self) {
        self.tracker.file_done();
        self.pump();
    }

    /// The item now being processed.
    pub fn set_current(&mut self, path: &Path) {
        self.tracker.set_current(Some(path.to_path_buf()));
        self.pump();
    }

    /// Emit the exact final snapshot (the runner calls this before the job finishes).
    pub fn finish_progress(&mut self) {
        let p = self.tracker.force().clone();
        self.sink.progress(&p);
    }

    fn pump(&mut self) {
        if let Some(p) = self.tracker.take_due() {
            self.sink.progress(p);
        }
    }

    // ---- report ---------------------------------------------------------------------------
    /// Record a non-fatal warning.
    pub fn warn(&mut self, path: impl Into<PathBuf>, message: impl Into<String>) {
        let w = Warning {
            path: path.into(),
            message: message.into(),
        };
        tracing::warn!(path = %w.path.display(), message = %w.message, "job warning");
        self.warnings.push(w);
    }

    /// Record the final path of a top-level result (copy/move destination, new directory).
    pub fn note_created(&mut self, path: impl Into<PathBuf>) {
        self.created.push(path.into());
    }

    /// Count a skipped item.
    pub fn note_skipped(&mut self) {
        self.skipped += 1;
    }

    /// Take the report (counters come from the progress tracker).
    pub fn take_report(&mut self) -> Report {
        let p = self.tracker.snapshot();
        Report {
            warnings: std::mem::take(&mut self.warnings),
            skipped: self.skipped,
            files_done: p.files_done,
            bytes_done: p.bytes_done,
            created: std::mem::take(&mut self.created),
        }
    }

    // ---- questions ------------------------------------------------------------------------
    /// Pre-decide conflicts (`CopyOpts::on_conflict`): behaves as if the user had answered once
    /// with "apply to all".
    pub fn set_conflict_policy(&mut self, policy: Option<ConflictAnswer>) {
        self.conflict_memory = policy;
    }

    /// Decide what to do with an existing destination. Order: remembered "apply to all" (only if
    /// it is allowed for this `kind`) -> ask. Conditional answers are evaluated here
    /// (`OverwriteIfNewer`: source mtime strictly greater; `OverwriteIfSizeDiffers`). An answer
    /// that `kind` forbids is treated as `Skip` with a warning. `Abort` is `Err(Cancelled)`.
    pub fn resolve_conflict(
        &mut self,
        source: ItemInfo,
        existing: ItemInfo,
        kind: ConflictKind,
    ) -> Result<Resolution, JobError> {
        self.checkpoint()?;
        let action = match self.conflict_memory.filter(|a| kind.allows(*a)) {
            Some(a) => a,
            None => {
                let q = Question::Conflict {
                    source: source.clone(),
                    existing: existing.clone(),
                    kind,
                };
                match self.sink.ask(q) {
                    Some(Answer::Conflict {
                        action,
                        apply_to_all,
                    }) => {
                        if !kind.allows(action) {
                            self.warn(
                                &existing.path,
                                format!("answer {action:?} not allowed here; skipped"),
                            );
                            self.skipped += 1;
                            return Ok(Resolution::Skip);
                        }
                        if apply_to_all {
                            self.conflict_memory = Some(action);
                        }
                        action
                    }
                    _ => return Err(JobError::Cancelled),
                }
            }
        };
        let resolution = match action {
            ConflictAnswer::Overwrite => Resolution::Overwrite,
            ConflictAnswer::Skip => Resolution::Skip,
            ConflictAnswer::OverwriteIfNewer if source.mtime_ns > existing.mtime_ns => {
                Resolution::Overwrite
            }
            ConflictAnswer::OverwriteIfSizeDiffers if source.size != existing.size => {
                Resolution::Overwrite
            }
            ConflictAnswer::OverwriteIfNewer | ConflictAnswer::OverwriteIfSizeDiffers => {
                Resolution::Skip
            }
            ConflictAnswer::RenameNew => Resolution::RenameNew,
            ConflictAnswer::RenameExisting => Resolution::RenameExisting,
            ConflictAnswer::Abort => return Err(JobError::Cancelled),
        };
        if resolution == Resolution::Skip {
            self.skipped += 1;
        }
        Ok(resolution)
    }

    /// An operation failed on `path`: ask retry/skip/abort (or use the remembered decision).
    /// `Abort` and "nobody answered" are `Err(JobError::Cancelled)`. A remembered "retry" is never
    /// reused (it would loop forever), so retry always asks again.
    pub fn resolve_error(
        &mut self,
        path: &Path,
        operation: Operation,
        err: &io::Error,
    ) -> Result<ErrorResolution, JobError> {
        self.checkpoint()?;
        tracing::warn!(path = %path.display(), %operation, error = %err, "operation failed");
        let action = match self.error_memory {
            Some(a) => a,
            None => {
                let q = Question::Error {
                    path: path.to_path_buf(),
                    operation,
                    message: err.to_string(),
                    os_error: err.raw_os_error(),
                };
                match self.sink.ask(q) {
                    Some(Answer::Error {
                        action,
                        apply_to_all,
                    }) => {
                        if apply_to_all && action != ErrorAction::Retry {
                            self.error_memory = Some(action);
                        }
                        action
                    }
                    _ => return Err(JobError::Cancelled),
                }
            }
        };
        match action {
            ErrorAction::Retry => Ok(ErrorResolution::Retry),
            ErrorAction::Skip => {
                self.skipped += 1;
                Ok(ErrorResolution::Skip)
            }
            ErrorAction::Abort => Err(JobError::Cancelled),
        }
    }

    /// Run `f` against the `Fs`; on an I/O error ask retry/skip/abort. `Ok(None)` = skipped.
    /// A pause/cancel checkpoint runs before every attempt. This is the workhorse for per-item
    /// operations: `ctx.attempt(path, Operation::Delete, |fs| fs.unlink(path))?`.
    pub fn attempt<T>(
        &mut self,
        path: &Path,
        operation: Operation,
        mut f: impl FnMut(&dyn Fs) -> io::Result<T>,
    ) -> Result<Option<T>, JobError> {
        loop {
            self.checkpoint()?;
            let fs = Arc::clone(&self.fs);
            match f(&*fs) {
                Ok(v) => return Ok(Some(v)),
                Err(e) => match self.resolve_error(path, operation, &e)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => return Ok(None),
                },
            }
        }
    }

    /// Trash is unavailable for `path`: `true` = delete permanently, `false` = skip it (counted).
    /// "Nobody answered" is `Err(Cancelled)`.
    pub fn confirm_permanent_delete(&mut self, path: &Path) -> Result<bool, JobError> {
        self.checkpoint()?;
        let permanent = match self.permanent_memory {
            Some(p) => p,
            None => match self.sink.ask(Question::TrashUnavailable {
                path: path.to_path_buf(),
            }) {
                Some(Answer::Confirm {
                    permanent,
                    apply_to_all,
                }) => {
                    if apply_to_all {
                        self.permanent_memory = Some(permanent);
                    }
                    permanent
                }
                _ => return Err(JobError::Cancelled),
            },
        };
        if !permanent {
            self.skipped += 1;
        }
        Ok(permanent)
    }
}

/// Engine-side helper for [`JobSink::ask`]: send `JobEvent::Ask` over `tx` and wait for the
/// `Responder`, polling `cancel` every 100 ms so a cancelled job never hangs on an unanswered
/// dialog. `None` = dropped responder or cancelled.
pub fn ask_over_events(
    tx: &async_channel::Sender<JobEvent>,
    id: JobId,
    cancel: &CancelToken,
    question: Question,
) -> Option<Answer> {
    let (reply, pending) = Responder::channel();
    // Unbounded channel: never blocks. Err = UI gone.
    tx.try_send(JobEvent::Ask {
        id,
        question,
        reply,
    })
    .ok()?;
    loop {
        match pending.wait_timeout(POLL) {
            Wait::Answered(a) => return Some(a),
            Wait::Dropped => return None,
            Wait::TimedOut if cancel.is_cancelled() => return None,
            Wait::TimedOut => {}
        }
    }
}

// ---- test support (public: T3.1/T3.2/T3.4 use it) ---------------------------------------------

/// What a [`ScriptedSink`] saw.
#[derive(Debug, Default)]
pub struct Recording {
    /// Every question asked, in order.
    pub questions: Vec<Question>,
    /// Every progress snapshot received.
    pub progress: Vec<Progress>,
}

type AnswerFn = Box<dyn FnMut(&Question) -> Option<Answer> + Send>;

/// A [`JobSink`] for tests: answers questions with a closure and records everything.
pub struct ScriptedSink {
    answer: AnswerFn,
    rec: Arc<Mutex<Recording>>,
}

impl ScriptedSink {
    /// Sink answering with `f`; `None` = "nobody answers" (the job aborts). Returns the shared
    /// recording to inspect after the run.
    pub fn new(
        f: impl FnMut(&Question) -> Option<Answer> + Send + 'static,
    ) -> (Self, Arc<Mutex<Recording>>) {
        let rec = Arc::new(Mutex::new(Recording::default()));
        (
            ScriptedSink {
                answer: Box::new(f),
                rec: Arc::clone(&rec),
            },
            rec,
        )
    }

    /// Fixed policy: conflicts -> `conflict`, errors -> `error`, trash unavailable -> `permanent`.
    /// `apply_to_all` is set on every answer.
    pub fn policy(
        conflict: ConflictAnswer,
        error: ErrorAction,
        permanent: bool,
    ) -> (Self, Arc<Mutex<Recording>>) {
        Self::new(move |q| {
            Some(match q {
                Question::Conflict { .. } => Answer::Conflict {
                    action: conflict,
                    apply_to_all: true,
                },
                Question::Error { .. } => Answer::Error {
                    action: error,
                    apply_to_all: true,
                },
                Question::TrashUnavailable { .. } => Answer::Confirm {
                    permanent,
                    apply_to_all: true,
                },
            })
        })
    }

    /// Nobody ever answers: any question aborts the job. Use for "no question expected" tests and
    /// assert the recording has no questions.
    pub fn refusing() -> (Self, Arc<Mutex<Recording>>) {
        Self::new(|_| None)
    }
}

impl JobSink for ScriptedSink {
    fn progress(&mut self, progress: &Progress) {
        self.rec
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .progress
            .push(progress.clone());
    }

    fn ask(&mut self, question: Question) -> Option<Answer> {
        let answer = (self.answer)(&question);
        self.rec
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .questions
            .push(question);
        answer
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::fs::{FileType, RealFs};

    fn item(name: &str, size: u64, mtime_ns: i128) -> ItemInfo {
        ItemInfo {
            path: name.into(),
            kind: FileType::File,
            size,
            mtime_ns,
        }
    }

    fn ctx_with(
        f: impl FnMut(&Question) -> Option<Answer> + Send + 'static,
    ) -> (JobCtx, Arc<Mutex<Recording>>) {
        let (sink, rec) = ScriptedSink::new(f);
        (JobCtx::simple(Arc::new(RealFs), sink), rec)
    }

    fn conflict(action: ConflictAnswer, all: bool) -> Option<Answer> {
        Some(Answer::Conflict {
            action,
            apply_to_all: all,
        })
    }

    #[test]
    fn apply_to_all_asks_once() {
        let (mut ctx, rec) = ctx_with(|_| conflict(ConflictAnswer::Overwrite, true));
        for _ in 0..3 {
            let r = ctx
                .resolve_conflict(item("a", 1, 1), item("b", 2, 2), ConflictKind::Replaceable)
                .unwrap();
            assert_eq!(r, Resolution::Overwrite);
        }
        assert_eq!(rec.lock().unwrap().questions.len(), 1);
    }

    #[test]
    fn remembered_overwrite_is_not_used_for_type_mismatch() {
        let (mut ctx, rec) = ctx_with(|q| match q {
            Question::Conflict {
                kind: ConflictKind::TypeMismatch,
                ..
            } => conflict(ConflictAnswer::Skip, false),
            _ => conflict(ConflictAnswer::Overwrite, true),
        });
        let a = ctx
            .resolve_conflict(item("a", 1, 1), item("b", 1, 1), ConflictKind::Replaceable)
            .unwrap();
        let b = ctx
            .resolve_conflict(item("a", 1, 1), item("b", 1, 1), ConflictKind::TypeMismatch)
            .unwrap();
        assert_eq!((a, b), (Resolution::Overwrite, Resolution::Skip));
        assert_eq!(rec.lock().unwrap().questions.len(), 2);
        assert_eq!(ctx.take_report().skipped, 1);
    }

    #[test]
    fn conditional_answers_and_forbidden_answers() {
        let (mut ctx, _) = ctx_with(|_| conflict(ConflictAnswer::OverwriteIfNewer, true));
        let newer = ctx
            .resolve_conflict(item("a", 1, 10), item("a", 1, 5), ConflictKind::Replaceable)
            .unwrap();
        let older = ctx
            .resolve_conflict(item("a", 1, 5), item("a", 1, 10), ConflictKind::Replaceable)
            .unwrap();
        let same = ctx
            .resolve_conflict(item("a", 1, 5), item("a", 1, 5), ConflictKind::Replaceable)
            .unwrap();
        assert_eq!(
            (newer, older, same),
            (Resolution::Overwrite, Resolution::Skip, Resolution::Skip)
        );

        let (mut ctx, _) = ctx_with(|_| conflict(ConflictAnswer::Overwrite, false));
        let r = ctx
            .resolve_conflict(item("a", 1, 1), item("a", 1, 1), ConflictKind::SameFile)
            .unwrap();
        assert_eq!(r, Resolution::Skip, "forbidden answer degrades to skip");
        assert_eq!(ctx.take_report().warnings.len(), 1);

        let (mut ctx, _) = ctx_with(|_| conflict(ConflictAnswer::OverwriteIfSizeDiffers, false));
        let r = ctx
            .resolve_conflict(item("a", 1, 1), item("a", 2, 1), ConflictKind::Replaceable)
            .unwrap();
        assert_eq!(r, Resolution::Overwrite);
    }

    #[test]
    fn abort_and_no_answer_cancel() {
        let (mut ctx, _) = ctx_with(|_| conflict(ConflictAnswer::Abort, false));
        assert!(matches!(
            ctx.resolve_conflict(item("a", 1, 1), item("a", 1, 1), ConflictKind::Replaceable),
            Err(JobError::Cancelled)
        ));
        let (sink, _) = ScriptedSink::refusing();
        let mut ctx = JobCtx::simple(Arc::new(RealFs), sink);
        assert!(matches!(
            ctx.confirm_permanent_delete(Path::new("/x")),
            Err(JobError::Cancelled)
        ));
    }

    #[test]
    fn attempt_retries_then_skips_and_remembers() {
        let mut answers = vec![ErrorAction::Retry, ErrorAction::Skip].into_iter();
        let (mut ctx, rec) = ctx_with(move |_| {
            Some(Answer::Error {
                action: answers.next().unwrap_or(ErrorAction::Abort),
                apply_to_all: true,
            })
        });
        let mut calls = 0;
        let r = ctx
            .attempt(Path::new("/p"), Operation::Delete, |_| {
                calls += 1;
                Err::<(), _>(io::Error::from_raw_os_error(13))
            })
            .unwrap();
        assert_eq!((r, calls), (None, 2));
        // "skip all" is remembered: the next failure does not ask again.
        let r = ctx
            .attempt(Path::new("/q"), Operation::Delete, |_| {
                Err::<(), _>(io::Error::from_raw_os_error(5))
            })
            .unwrap();
        assert_eq!(r, None);
        let rec = rec.lock().unwrap();
        assert_eq!(rec.questions.len(), 2);
        assert!(matches!(
            &rec.questions[0],
            Question::Error {
                os_error: Some(13),
                operation: Operation::Delete,
                ..
            }
        ));
        drop(rec);
        assert_eq!(ctx.take_report().skipped, 2);
    }

    #[test]
    fn checkpoint_observes_cancel_and_pause() {
        let (sink, _) = ScriptedSink::refusing();
        let cancel = CancelToken::new();
        let pause = PauseGate::new();
        let mut ctx = JobCtx::new(
            Arc::new(RealFs),
            cancel.clone(),
            pause.clone(),
            Box::new(sink),
            Arc::new(SystemClock::new()),
        );
        assert!(ctx.checkpoint().is_ok());
        pause.pause();
        let resumer = {
            let pause = pause.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(150));
                pause.resume();
            })
        };
        let t = std::time::Instant::now();
        assert!(ctx.checkpoint().is_ok(), "resumes after the pause");
        assert!(t.elapsed() >= Duration::from_millis(100));
        resumer.join().unwrap();
        // Cancelling while paused wakes the worker.
        pause.pause();
        let c2 = cancel.clone();
        let canceller = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c2.cancel();
        });
        assert!(matches!(ctx.checkpoint(), Err(JobError::Cancelled)));
        canceller.join().unwrap();
    }

    #[test]
    fn progress_is_throttled_through_the_sink() {
        let (sink, rec) = ScriptedSink::refusing();
        let mut ctx = JobCtx::simple(Arc::new(RealFs), sink);
        for _ in 0..10_000 {
            ctx.add_bytes(1);
        }
        let n = rec.lock().unwrap().progress.len();
        assert!(
            (1..=3).contains(&n),
            "burst of updates -> few snapshots, got {n}"
        );
        ctx.finish_progress();
        let rec = rec.lock().unwrap();
        assert_eq!(rec.progress.last().map(|p| p.bytes_done), Some(10_000));
    }

    #[test]
    fn unique_suffixes_differ() {
        let (sink, _) = ScriptedSink::refusing();
        let ctx = JobCtx::simple(Arc::new(RealFs), sink);
        assert_ne!(ctx.unique_suffix(), ctx.unique_suffix());
    }

    #[test]
    fn ask_over_events_answers_and_cancels() {
        let (tx, rx) = async_channel::unbounded();
        let cancel = CancelToken::new();
        let q = Question::TrashUnavailable { path: "/x".into() };
        let t = std::thread::spawn({
            let cancel = cancel.clone();
            let q = q.clone();
            move || ask_over_events(&tx, JobId(1), &cancel, q)
        });
        let ev = rx.recv_blocking().unwrap();
        let JobEvent::Ask { reply, .. } = ev else {
            panic!("expected Ask")
        };
        assert!(reply.answer(Answer::Confirm {
            permanent: true,
            apply_to_all: false
        }));
        assert!(matches!(
            t.join().unwrap(),
            Some(Answer::Confirm {
                permanent: true,
                ..
            })
        ));

        // Unanswered + cancelled: the worker comes back with None.
        let (tx, rx) = async_channel::unbounded();
        let t = std::thread::spawn({
            let cancel = cancel.clone();
            move || ask_over_events(&tx, JobId(2), &cancel, q)
        });
        let _held = rx.recv_blocking().unwrap();
        cancel.cancel();
        assert!(t.join().unwrap().is_none());
    }
}

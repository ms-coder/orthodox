//! The job engine (T3.1; public API frozen by T3.0, see ADR-0006).
//!
//! Scheduling: one FIFO **queue lane** thread runs `RunMode::Queued` jobs one after another; every
//! `RunMode::Parallel` job gets its own thread immediately. Each job runs `runner::run_job` with a
//! `JobCtx` whose sink turns progress into `JobEvent::Progress` (unbounded channel, never blocks)
//! and questions into `JobEvent::Ask` via `ctx::ask_over_events`.
//!
//! Guarantees:
//! - events per job arrive in order `Queued, Started, (Progress|Ask|StateChanged)*, Finished`;
//!   `Queued` is sent before `submit` returns; exactly one `Finished` per job. Every lifecycle
//!   event is sent while holding the state lock, and a job's own progress/questions are only sent
//!   by its worker between `Started` and `Finished`, so the order holds across threads;
//! - `cancel` on a queued job removes it (`Finished{Cancelled}` without `Started`); on a running
//!   job it fires the `CancelToken`, which also wakes a paused worker (`PauseGate` polls it) and
//!   a worker waiting for an answer (`ask_over_events` polls it);
//! - `pause`/`resume` close/open the job's `PauseGate` and emit `StateChanged`;
//! - a panic in a worker never kills the lane (`run_job` catches it; a panic outside it is caught
//!   again here and reported as `Finished{Failed}`);
//! - dropping the engine cancels everything and joins the workers (bounded wait);
//! - workers run with a large stack and never block on the UI except through `Ask`.

use std::collections::{BTreeMap, VecDeque};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ofm_core::CancelToken;

use crate::ctx::{JobCtx, JobSink, PauseGate, ask_over_events};
use crate::error::JobError;
use crate::event::{Answer, JobEvent, JobState, Outcome, Progress, Question, Report};
use crate::fs::{Fs, RealFs};
use crate::progress::{Clock, SystemClock};
use crate::runner::run_job;
use crate::spec::{JobId, JobSpec, RunMode};

/// Stack size of worker threads (deep trees, generous margin).
const WORKER_STACK: usize = 16 << 20;

/// How long `Drop` waits for workers to observe the cancellation.
const JOIN_TIMEOUT: Duration = Duration::from_secs(10);

/// Snapshot of one job for the jobs view / quit prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    /// Id.
    pub id: JobId,
    /// Title (`JobSpec::title`).
    pub title: String,
    /// Scheduling mode.
    pub mode: RunMode,
    /// Current state.
    pub state: JobState,
}

struct Entry {
    title: String,
    mode: RunMode,
    state: JobState,
    cancel: CancelToken,
    pause: PauseGate,
    /// Taken by the worker when the job starts.
    spec: Option<JobSpec>,
}

#[derive(Default)]
struct State {
    jobs: BTreeMap<JobId, Entry>,
    queue: VecDeque<JobId>,
    shutdown: bool,
}

struct Shared {
    state: Mutex<State>,
    wake: Condvar,
    tx: async_channel::Sender<JobEvent>,
    fs: Arc<dyn Fs>,
    clock: Arc<dyn Clock>,
    next_id: AtomicU64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Send an event (unbounded: never blocks; an error only means the UI side is gone).
    fn send(&self, ev: JobEvent) {
        if self.tx.try_send(ev).is_err() {
            tracing::debug!("job event dropped: no receiver");
        }
    }

    /// Mark `id` finished and send its one `Finished` (no-op if already finished). Called with
    /// the state lock held.
    fn finish_locked(&self, st: &mut State, id: JobId, outcome: Outcome) {
        if let Some(e) = st.jobs.get_mut(&id)
            && e.state != JobState::Finished
        {
            e.state = JobState::Finished;
            e.spec = None;
            self.send(JobEvent::Finished { id, outcome });
        }
    }

    fn finish(&self, id: JobId, outcome: Outcome) {
        let mut st = self.lock();
        self.finish_locked(&mut st, id, outcome);
    }
}

/// The engine-backed [`JobSink`].
struct EngineSink {
    id: JobId,
    tx: async_channel::Sender<JobEvent>,
    cancel: CancelToken,
}

impl JobSink for EngineSink {
    fn progress(&mut self, progress: &Progress) {
        // Unbounded channel: never blocks the worker.
        let _ = self.tx.try_send(JobEvent::Progress {
            id: self.id,
            progress: progress.clone(),
        });
    }

    fn ask(&mut self, question: Question) -> Option<Answer> {
        ask_over_events(&self.tx, self.id, &self.cancel, question)
    }
}

fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_owned()
    }
}

/// Run job `id` on the current thread (lane or parallel worker).
fn run_one(shared: &Shared, id: JobId) {
    let (spec, cancel, pause) = {
        let mut st = shared.lock();
        let Some(e) = st.jobs.get_mut(&id) else {
            return;
        };
        if e.state == JobState::Finished {
            return;
        }
        if e.cancel.is_cancelled() {
            shared.finish_locked(&mut st, id, Outcome::Cancelled(Report::default()));
            return;
        }
        let Some(spec) = e.spec.take() else {
            shared.finish_locked(
                &mut st,
                id,
                Outcome::Failed {
                    error: JobError::Panic("job has no spec".into()).to_string(),
                    path: None,
                    report: Report::default(),
                },
            );
            return;
        };
        e.state = JobState::Running;
        let (cancel, pause) = (e.cancel.clone(), e.pause.clone());
        shared.send(JobEvent::Started { id });
        (spec, cancel, pause)
    };
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        let sink = EngineSink {
            id,
            tx: shared.tx.clone(),
            cancel: cancel.clone(),
        };
        let mut ctx = JobCtx::new(
            Arc::clone(&shared.fs),
            cancel,
            pause,
            Box::new(sink),
            Arc::clone(&shared.clock),
        );
        run_job(&spec, &mut ctx)
    }))
    .unwrap_or_else(|payload| {
        let msg = panic_text(&*payload);
        tracing::error!(panic = %msg, job = id.0, "job worker panicked");
        Outcome::Failed {
            error: JobError::Panic(msg).to_string(),
            path: None,
            report: Report::default(),
        }
    });
    shared.finish(id, outcome);
}

/// The queue lane: FIFO, one job at a time, until shutdown.
fn lane_loop(shared: &Shared) {
    loop {
        let next = {
            let mut st = shared.lock();
            loop {
                if let Some(id) = st.queue.pop_front() {
                    break Some(id);
                }
                if st.shutdown {
                    break None;
                }
                st = shared.wake.wait(st).unwrap_or_else(PoisonError::into_inner);
            }
        };
        let Some(id) = next else { return };
        // A panic here (outside run_job's own catch) must not end the lane.
        if catch_unwind(AssertUnwindSafe(|| run_one(shared, id))).is_err() {
            shared.finish(
                id,
                Outcome::Failed {
                    error: JobError::Panic("job worker panicked".into()).to_string(),
                    path: None,
                    report: Report::default(),
                },
            );
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .stack_size(WORKER_STACK)
        .spawn(f)
}

/// Handle to the engine; owns the worker threads.
pub struct JobEngine {
    events: async_channel::Receiver<JobEvent>,
    shared: Arc<Shared>,
    lane: Mutex<Option<JoinHandle<()>>>,
    workers: Mutex<Vec<JoinHandle<()>>>,
}

impl std::fmt::Debug for JobEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobEngine")
            .field("jobs", &self.shared.lock().jobs.len())
            .finish_non_exhaustive()
    }
}

impl JobEngine {
    /// Start an engine using the real file system (`RealFs`) and the system clock.
    pub fn new() -> Self {
        Self::with_fs(Arc::new(RealFs))
    }

    /// Start an engine on a custom `Fs` (tests: `FaultyFs`).
    pub fn with_fs(fs: Arc<dyn Fs>) -> Self {
        let (tx, events) = async_channel::unbounded();
        JobEngine {
            events,
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                wake: Condvar::new(),
                tx,
                fs,
                clock: Arc::new(SystemClock::new()),
                next_id: AtomicU64::new(1),
            }),
            lane: Mutex::new(None),
            workers: Mutex::new(Vec::new()),
        }
    }

    /// Submit a job; returns immediately with its id (ids are unique, increasing).
    pub fn submit(&self, spec: JobSpec, mode: RunMode) -> JobId {
        let id = JobId(self.shared.next_id.fetch_add(1, Ordering::Relaxed));
        let title = spec.title();
        {
            let mut st = self.shared.lock();
            st.jobs.insert(
                id,
                Entry {
                    title: title.clone(),
                    mode,
                    state: JobState::Queued,
                    cancel: CancelToken::new(),
                    pause: PauseGate::new(),
                    spec: Some(spec),
                },
            );
            self.shared.send(JobEvent::Queued { id, title, mode });
            if mode == RunMode::Queued {
                st.queue.push_back(id);
            }
        }
        let started = match mode {
            RunMode::Queued => self.ensure_lane(),
            RunMode::Parallel => self.spawn_parallel(id),
        };
        if let Err(e) = started {
            tracing::error!(error = %e, "cannot start job thread");
            let mut st = self.shared.lock();
            st.queue.retain(|q| *q != id);
            self.shared.finish_locked(
                &mut st,
                id,
                Outcome::Failed {
                    error: format!("cannot start a worker thread: {e}"),
                    path: None,
                    report: Report::default(),
                },
            );
        }
        id
    }

    /// Make sure the lane thread runs and wake it.
    fn ensure_lane(&self) -> std::io::Result<()> {
        let mut lane = self.lane.lock().unwrap_or_else(PoisonError::into_inner);
        if lane.as_ref().is_none_or(|h| h.is_finished()) {
            if let Some(old) = lane.take() {
                let _ = old.join();
            }
            let shared = Arc::clone(&self.shared);
            *lane = Some(spawn("ofm-job-lane", move || lane_loop(&shared))?);
        }
        drop(lane);
        self.shared.wake.notify_all();
        Ok(())
    }

    fn spawn_parallel(&self, id: JobId) -> std::io::Result<()> {
        let shared = Arc::clone(&self.shared);
        let handle = spawn(&format!("ofm-job-{}", id.0), move || run_one(&shared, id))?;
        let mut workers = self.workers.lock().unwrap_or_else(PoisonError::into_inner);
        workers.retain(|h| !h.is_finished());
        workers.push(handle);
        Ok(())
    }

    /// Pause a job at its next checkpoint (no effect on queued/finished jobs).
    pub fn pause(&self, id: JobId) {
        let mut st = self.shared.lock();
        if let Some(e) = st.jobs.get_mut(&id)
            && e.state == JobState::Running
        {
            e.pause.pause();
            e.state = JobState::Paused;
            self.shared.send(JobEvent::StateChanged {
                id,
                state: JobState::Paused,
            });
        }
    }

    /// Resume a paused job.
    pub fn resume(&self, id: JobId) {
        let mut st = self.shared.lock();
        if let Some(e) = st.jobs.get_mut(&id)
            && e.state == JobState::Paused
        {
            e.pause.resume();
            e.state = JobState::Running;
            self.shared.send(JobEvent::StateChanged {
                id,
                state: JobState::Running,
            });
        }
    }

    /// Cancel a job (queued: removed; running: stops, temp files removed).
    pub fn cancel(&self, id: JobId) {
        let mut st = self.shared.lock();
        Self::cancel_locked(&self.shared, &mut st, id);
    }

    fn cancel_locked(shared: &Shared, st: &mut State, id: JobId) {
        let Some(e) = st.jobs.get(&id) else { return };
        e.cancel.cancel();
        if e.state == JobState::Queued && e.mode == RunMode::Queued {
            st.queue.retain(|q| *q != id);
            shared.finish_locked(st, id, Outcome::Cancelled(Report::default()));
        }
        // Running/paused: the worker observes the token at its next checkpoint (a paused or
        // asking worker polls it every 100 ms) and finishes the job itself. A parallel job whose
        // thread has not started yet finishes as cancelled without `Started`.
    }

    /// Cancel every unfinished job (quit with running jobs).
    pub fn cancel_all(&self) {
        let mut st = self.shared.lock();
        let ids: Vec<JobId> = st
            .jobs
            .iter()
            .filter(|(_, e)| e.state != JobState::Finished)
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            Self::cancel_locked(&self.shared, &mut st, id);
        }
    }

    /// Event stream for the UI. There must be exactly ONE consumer (the receiver is a work
    /// queue): `JobsController` owns it.
    pub fn events(&self) -> async_channel::Receiver<JobEvent> {
        self.events.clone()
    }

    /// Ids of jobs not yet finished (queued, running or paused): the quit confirmation.
    pub fn running(&self) -> Vec<JobId> {
        self.shared
            .lock()
            .jobs
            .iter()
            .filter(|(_, e)| e.state != JobState::Finished)
            .map(|(id, _)| *id)
            .collect()
    }

    /// Snapshot of all jobs known to the engine (finished ones until `forget`).
    pub fn jobs(&self) -> Vec<JobInfo> {
        self.shared
            .lock()
            .jobs
            .iter()
            .map(|(id, e)| JobInfo {
                id: *id,
                title: e.title.clone(),
                mode: e.mode,
                state: e.state,
            })
            .collect()
    }

    /// Drop the bookkeeping of a finished job (jobs view "clear finished").
    pub fn forget(&self, id: JobId) {
        let mut st = self.shared.lock();
        if st
            .jobs
            .get(&id)
            .is_some_and(|e| e.state == JobState::Finished)
        {
            st.jobs.remove(&id);
        }
    }
}

impl Default for JobEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for JobEngine {
    fn drop(&mut self) {
        self.shared.lock().shutdown = true;
        self.cancel_all();
        self.shared.wake.notify_all();
        let mut handles: Vec<JoinHandle<()>> = Vec::new();
        if let Some(h) = self
            .lane
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            handles.push(h);
        }
        handles.append(&mut self.workers.lock().unwrap_or_else(PoisonError::into_inner));
        let deadline = Instant::now() + JOIN_TIMEOUT;
        for h in handles {
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            if h.is_finished() {
                let _ = h.join();
            } else {
                tracing::warn!("job worker did not stop in time; detached");
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::event::{ConflictAnswer, ErrorAction};
    use crate::spec::CopyOpts;
    use ofm_core::Location;
    use ofm_testkit::tree::{FileSpec, TreeSpec};

    fn recv(rx: &async_channel::Receiver<JobEvent>) -> JobEvent {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            match rx.try_recv() {
                Ok(ev) => return ev,
                Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
                Err(e) => panic!("no event: {e}"),
            }
        }
    }

    /// Collect events of all jobs until every id in `ids` has finished, answering questions with
    /// `answer`.
    fn drain(
        rx: &async_channel::Receiver<JobEvent>,
        ids: &[JobId],
        mut answer: impl FnMut(JobId, &Question) -> Option<Answer>,
    ) -> Vec<(JobId, String)> {
        let mut log = Vec::new();
        let mut open: Vec<JobId> = ids.to_vec();
        while !open.is_empty() {
            let ev = recv(rx);
            let (id, tag) = match ev {
                JobEvent::Queued { id, .. } => (id, "queued".to_owned()),
                JobEvent::Started { id } => (id, "started".to_owned()),
                JobEvent::StateChanged { id, state } => (id, format!("{state:?}")),
                JobEvent::Progress { id, .. } => (id, "progress".to_owned()),
                JobEvent::Ask {
                    id,
                    question,
                    reply,
                } => {
                    if let Some(a) = answer(id, &question) {
                        reply.answer(a);
                    }
                    (id, "ask".to_owned())
                }
                JobEvent::Finished { id, outcome } => {
                    open.retain(|o| *o != id);
                    let tag = match outcome {
                        Outcome::Ok(_) => "ok",
                        Outcome::Cancelled(_) => "cancelled",
                        Outcome::Failed { .. } => "failed",
                        Outcome::Size(_) => "size",
                    };
                    (id, format!("finished:{tag}"))
                }
            };
            log.push((id, tag));
        }
        log
    }

    fn tags(log: &[(JobId, String)], id: JobId) -> Vec<String> {
        let mut v: Vec<String> = log
            .iter()
            .filter(|(i, _)| *i == id)
            .map(|(_, t)| t.clone())
            .collect();
        v.dedup_by(|a, b| a == "progress" && b == "progress");
        v
    }

    fn copy(src: &std::path::Path, dst: &std::path::Path) -> JobSpec {
        JobSpec::Copy {
            sources: vec![Location::Local(src.to_path_buf())],
            dest_dir: dst.to_path_buf(),
            opts: CopyOpts::default(),
        }
    }

    #[test]
    fn event_order_and_single_finished() {
        let src = TreeSpec::new()
            .file("a", FileSpec::size(3000))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new().build_temp().unwrap();
        let engine = JobEngine::new();
        let rx = engine.events();
        let id = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Queued);
        let log = drain(&rx, &[id], |_, _| None);
        let t = tags(&log, id);
        assert_eq!(t.first().map(String::as_str), Some("queued"));
        assert_eq!(t.get(1).map(String::as_str), Some("started"));
        assert_eq!(t.last().map(String::as_str), Some("finished:ok"));
        assert_eq!(t.iter().filter(|x| x.starts_with("finished")).count(), 1);
        assert!(t.contains(&"progress".to_owned()));
        assert!(dst.path().join("a").is_file());
        assert_eq!(engine.running(), vec![]);
        assert_eq!(engine.jobs().len(), 1);
        engine.forget(id);
        assert!(engine.jobs().is_empty());
    }

    #[test]
    fn queued_jobs_run_in_order_parallel_jobs_run_at_once() {
        // Job 1 blocks on a conflict question; job 2 (queued) must wait; job 3 (parallel) runs.
        let src = TreeSpec::new()
            .file("a", FileSpec::content(b"new"))
            .file("b", FileSpec::content(b"b"))
            .file("c", FileSpec::content(b"c"))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new()
            .file("a", FileSpec::content(b"old"))
            .build_temp()
            .unwrap();
        let engine = JobEngine::new();
        let rx = engine.events();
        let j1 = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Queued);
        let j2 = engine.submit(copy(&src.path().join("b"), dst.path()), RunMode::Queued);
        let j3 = engine.submit(copy(&src.path().join("c"), dst.path()), RunMode::Parallel);
        let mut held = None;
        let mut log = Vec::new();
        // Until job 3 finished, job 2 must not have started; hold job 1's question.
        loop {
            match recv(&rx) {
                JobEvent::Ask { id, reply, .. } => {
                    assert_eq!(id, j1);
                    held = Some(reply);
                }
                JobEvent::Started { id } => log.push((id, "started")),
                JobEvent::Finished { id, .. } => {
                    log.push((id, "finished"));
                    if id == j3 && held.is_some() {
                        break;
                    }
                }
                _ => {}
            }
            if held.is_some() && log.contains(&(j3, "finished")) {
                break;
            }
        }
        assert!(!log.contains(&(j2, "started")), "{log:?}");
        assert_eq!(engine.running(), vec![j1, j2]);
        held.unwrap().answer(Answer::Conflict {
            action: ConflictAnswer::Overwrite,
            apply_to_all: false,
        });
        let rest = drain(&rx, &[j1, j2], |_, _| None);
        let order: Vec<_> = rest
            .iter()
            .filter(|(_, t)| t == "started" || t.starts_with("finished"))
            .cloned()
            .collect();
        assert_eq!(
            order,
            vec![
                (j1, "finished:ok".to_owned()),
                (j2, "started".to_owned()),
                (j2, "finished:ok".to_owned())
            ]
        );
        assert_eq!(std::fs::read(dst.path().join("a")).unwrap(), b"new");
    }

    #[test]
    fn cancel_wakes_paused_and_asking_workers() {
        let src = TreeSpec::new()
            .file("a", FileSpec::content(b"new"))
            .file("big", FileSpec::size(8 << 20))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new()
            .file("a", FileSpec::content(b"old"))
            .build_temp()
            .unwrap();
        let engine = JobEngine::new();
        let rx = engine.events();
        // Asking worker: cancel while the question is unanswered.
        let j1 = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Parallel);
        let _held = loop {
            if let JobEvent::Ask { reply, .. } = recv(&rx) {
                break reply;
            }
        };
        engine.cancel(j1);
        let log = drain(&rx, &[j1], |_, _| None);
        assert_eq!(tags(&log, j1).last().unwrap(), "finished:cancelled");
        assert_eq!(std::fs::read(dst.path().join("a")).unwrap(), b"old");

        // Queued job cancelled before it starts: no Started.
        let blocker = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Queued);
        let queued = engine.submit(copy(&src.path().join("big"), dst.path()), RunMode::Queued);
        let _held = loop {
            if let JobEvent::Ask { reply, .. } = recv(&rx) {
                break reply;
            }
        };
        engine.cancel(queued);
        // Paused worker: pause it, then cancel.
        engine.pause(blocker);
        engine.cancel(blocker);
        let log = drain(&rx, &[blocker, queued], |_, _| None);
        assert_eq!(tags(&log, queued), vec!["finished:cancelled"]);
        let b = tags(&log, blocker);
        assert_eq!(b.first().unwrap(), "Paused");
        assert_eq!(b.last().unwrap(), "finished:cancelled");
        assert!(!dst.path().join("big").exists());
        let names: Vec<_> = std::fs::read_dir(dst.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no temp files left");
    }

    #[test]
    fn pause_and_resume_emit_state_changes() {
        let src = TreeSpec::new()
            .file("a", FileSpec::content(b"new"))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new()
            .file("a", FileSpec::content(b"old"))
            .build_temp()
            .unwrap();
        let engine = JobEngine::new();
        let rx = engine.events();
        let id = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Queued);
        let held = loop {
            if let JobEvent::Ask { reply, .. } = recv(&rx) {
                break reply;
            }
        };
        engine.pause(id);
        assert_eq!(engine.jobs()[0].state, JobState::Paused);
        held.answer(Answer::Conflict {
            action: ConflictAnswer::Skip,
            apply_to_all: false,
        });
        engine.resume(id);
        let log = drain(&rx, &[id], |_, _| None);
        let t = tags(&log, id);
        assert_eq!(t.first().unwrap(), "Paused");
        assert!(t.contains(&"Running".to_owned()));
        assert_eq!(t.last().unwrap(), "finished:ok");
        // No effect on finished jobs.
        engine.pause(id);
        assert_eq!(engine.jobs()[0].state, JobState::Finished);
    }

    struct PanicFs;
    impl Fs for PanicFs {
        fn open_read(&self, _: &std::path::Path) -> std::io::Result<Box<dyn crate::FsFile>> {
            panic!("boom")
        }
        fn open_read_follow(&self, p: &std::path::Path) -> std::io::Result<Box<dyn crate::FsFile>> {
            self.open_read(p)
        }
        fn create_excl(
            &self,
            p: &std::path::Path,
            m: u32,
        ) -> std::io::Result<Box<dyn crate::FsFile>> {
            RealFs.create_excl(p, m)
        }
        fn stat(&self, p: &std::path::Path) -> std::io::Result<crate::FsStat> {
            RealFs.stat(p)
        }
        fn lstat(&self, p: &std::path::Path) -> std::io::Result<crate::FsStat> {
            RealFs.lstat(p)
        }
        fn read_dir(&self, p: &std::path::Path) -> std::io::Result<Vec<std::ffi::OsString>> {
            RealFs.read_dir(p)
        }
        fn mkdir(&self, p: &std::path::Path, m: u32) -> std::io::Result<()> {
            RealFs.mkdir(p, m)
        }
        fn rename_noreplace(
            &self,
            a: &std::path::Path,
            b: &std::path::Path,
        ) -> std::io::Result<()> {
            RealFs.rename_noreplace(a, b)
        }
        fn rename_replace(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
            RealFs.rename_replace(a, b)
        }
        fn unlink(&self, p: &std::path::Path) -> std::io::Result<()> {
            RealFs.unlink(p)
        }
        fn rmdir(&self, p: &std::path::Path) -> std::io::Result<()> {
            RealFs.rmdir(p)
        }
        fn symlink(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
            RealFs.symlink(a, b)
        }
        fn hard_link(&self, a: &std::path::Path, b: &std::path::Path) -> std::io::Result<()> {
            RealFs.hard_link(a, b)
        }
        fn read_link(&self, p: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
            RealFs.read_link(p)
        }
        fn set_mode(&self, p: &std::path::Path, m: u32) -> std::io::Result<()> {
            RealFs.set_mode(p, m)
        }
        fn set_times(&self, p: &std::path::Path, a: i128, m: i128, f: bool) -> std::io::Result<()> {
            RealFs.set_times(p, a, m, f)
        }
        fn set_owner(
            &self,
            p: &std::path::Path,
            u: Option<u32>,
            g: Option<u32>,
            f: bool,
        ) -> std::io::Result<()> {
            RealFs.set_owner(p, u, g, f)
        }
        fn sync_dir(&self, p: &std::path::Path) -> std::io::Result<()> {
            RealFs.sync_dir(p)
        }
        fn list_xattrs(
            &self,
            p: &std::path::Path,
            f: bool,
        ) -> std::io::Result<Vec<std::ffi::OsString>> {
            RealFs.list_xattrs(p, f)
        }
        fn get_xattr(
            &self,
            p: &std::path::Path,
            n: &std::ffi::OsStr,
            f: bool,
        ) -> std::io::Result<Option<Vec<u8>>> {
            RealFs.get_xattr(p, n, f)
        }
        fn set_xattr(
            &self,
            p: &std::path::Path,
            n: &std::ffi::OsStr,
            v: &[u8],
            f: bool,
        ) -> std::io::Result<()> {
            RealFs.set_xattr(p, n, v, f)
        }
        fn pin(&self, p: &std::path::Path, f: bool) -> std::io::Result<crate::FsPin> {
            RealFs.pin(p, f)
        }
        fn trash(&self, p: &std::path::Path) -> Result<(), crate::TrashError> {
            RealFs.trash(p)
        }
        fn is_root(&self) -> bool {
            false
        }
    }

    #[test]
    fn worker_panic_is_a_failed_job_and_the_lane_survives() {
        let src = TreeSpec::new()
            .file("a", FileSpec::size(10))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new().build_temp().unwrap();
        let engine = JobEngine::with_fs(Arc::new(PanicFs));
        let rx = engine.events();
        let j1 = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Queued);
        let j2 = engine.submit(
            JobSpec::Mkdir {
                path: dst.path().join("x"),
            },
            RunMode::Queued,
        );
        let log = drain(&rx, &[j1, j2], |_, q| {
            Some(match q {
                Question::Error { .. } => Answer::Error {
                    action: ErrorAction::Abort,
                    apply_to_all: false,
                },
                _ => return None,
            })
        });
        assert_eq!(tags(&log, j1).last().unwrap(), "finished:failed");
        // The lane is still alive: job 2 ran (its own result depends on T3.2).
        assert!(tags(&log, j2).contains(&"started".to_owned()));
        // The temp file of the panicking copy was not even created; nothing is left behind.
        assert_eq!(
            std::fs::read_dir(dst.path()).unwrap().count(),
            usize::from(dst.path().join("x").exists())
        );
    }

    #[test]
    fn drop_cancels_and_joins() {
        let src = TreeSpec::new()
            .file("a", FileSpec::content(b"new"))
            .build_temp()
            .unwrap();
        let dst = TreeSpec::new()
            .file("a", FileSpec::content(b"old"))
            .build_temp()
            .unwrap();
        let engine = JobEngine::new();
        let rx = engine.events();
        let _ = engine.submit(copy(&src.path().join("a"), dst.path()), RunMode::Parallel);
        let _held = loop {
            if let JobEvent::Ask { reply, .. } = recv(&rx) {
                break reply;
            }
        };
        let t = Instant::now();
        drop(engine);
        assert!(t.elapsed() < Duration::from_secs(5));
        let mut finished = false;
        while let Ok(ev) = rx.try_recv() {
            finished |= matches!(
                ev,
                JobEvent::Finished {
                    outcome: Outcome::Cancelled(_),
                    ..
                }
            );
        }
        assert!(finished);
    }
}

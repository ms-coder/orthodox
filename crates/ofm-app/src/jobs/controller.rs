//! [`JobsController`]: the single UI-side owner of the `JobEngine`, the job table the jobs view
//! renders and the pump that turns engine events into UI reactions. Main thread only.
//!
//! The engine is created lazily by [`JobsController::submit`]. UI tests that must not spawn worker
//! threads feed synthetic events through [`JobsController::handle_event`] (an `Ask` event can be
//! built with `ofm_core::Responder::channel()`); T3.3 tests the dialogs that way before the real
//! engine (T3.1) is merged. [`JobsController::capture`] additionally records submits and control
//! calls instead of sending them to an engine.

use std::cell::{Cell, OnceCell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use ofm_core::{Location, Responder};
use ofm_jobs::{
    Answer, JobEngine, JobEvent, JobId, JobSpec, JobState, Outcome, Progress, Question, RunMode,
};

use super::targets::focus_target;
use super::ui;
use crate::context::AppContext;
use crate::dialogs::conflict;
use crate::i18n::tr;
use crate::panel::PanelController;
use crate::signal::Signal;
use crate::wiring;

/// One row of the jobs view.
#[derive(Debug, Clone)]
pub struct JobEntry {
    /// Engine id.
    pub id: JobId,
    /// Title from `JobSpec::title`.
    pub title: String,
    /// Scheduling mode.
    pub mode: RunMode,
    /// Lifecycle state.
    pub state: JobState,
    /// Last progress snapshot.
    pub progress: Progress,
    /// True while a question dialog for this job is open (the worker is blocked).
    pub asking: bool,
    /// Set when the job finished.
    pub outcome: Option<Outcome>,
    /// A pause or cancel the user asked for that the worker has not acted on yet.
    pub requested: Option<Requested>,
}

/// What the user asked of a running job; shown as "Pausing..." / "Cancelling..." at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requested {
    /// Pause requested, the worker still runs to its next checkpoint.
    Pausing,
    /// Cancel requested, the job has not reported its end yet.
    Cancelling,
}

/// What observers of the job table are told (the jobs view, status line, quit prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobsEvent {
    /// A job was added.
    Added(JobId),
    /// State, progress or question flag of a job changed.
    Updated(JobId),
    /// A job finished (its `outcome` is set).
    Finished(JobId),
    /// Finished jobs were removed from the table.
    Cleared,
}

/// Completion callback of a job (see [`SubmitOpts::on_finished`]).
pub type FinishedFn = Box<dyn FnOnce(&Rc<AppContext>, &Outcome)>;

/// What the UI wants done around a job besides running it.
#[derive(Default)]
pub struct SubmitOpts {
    /// Panel whose selection is cleared when the job finishes successfully.
    pub origin: Option<Weak<PanelController>>,
    /// Called once with the outcome when the job finished (any outcome).
    pub on_finished: Option<FinishedFn>,
}

struct PendingAsk {
    id: JobId,
    question: Question,
    reply: Responder<Answer>,
}

/// A control call recorded instead of being sent to the engine (see [`JobsController::capture`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// `pause(id)`
    Pause(JobId),
    /// `resume(id)`
    Resume(JobId),
    /// `cancel(id)`
    Cancel(JobId),
    /// `cancel_all()`
    CancelAll,
}

/// Test seam: when installed, submits and control calls are recorded instead of reaching the
/// engine (no worker threads). Jobs still appear in the table through synthetic `Queued`
/// events; the test feeds `Started`/`Progress`/`Finished` through `handle_event`.
#[derive(Default)]
pub struct Capture {
    /// Every submitted job, in order.
    pub submitted: RefCell<Vec<(JobId, JobSpec, RunMode)>>,
    /// Every pause/resume/cancel call, in order.
    pub controls: RefCell<Vec<Control>>,
    next: Cell<u64>,
}

/// Owner of the engine and the job table.
pub struct JobsController {
    engine: OnceCell<JobEngine>,
    jobs: RefCell<Vec<JobEntry>>,
    /// Observers; see [`JobsEvent`].
    pub changed: Signal<JobsEvent>,
    pump_started: Cell<bool>,
    meta: RefCell<HashMap<JobId, SubmitOpts>>,
    asks: RefCell<VecDeque<PendingAsk>>,
    ask_open: Cell<bool>,
    ask_dialogs: RefCell<HashMap<JobId, adw::Dialog>>,
    capture: RefCell<Option<Rc<Capture>>>,
    quit_confirmed: Cell<bool>,
    quitting: Cell<bool>,
}

impl JobsController {
    /// Empty controller; no threads are started until the first [`submit`](Self::submit).
    pub fn new() -> Self {
        JobsController {
            engine: OnceCell::new(),
            jobs: RefCell::new(Vec::new()),
            changed: Signal::new(),
            pump_started: Cell::new(false),
            meta: RefCell::new(HashMap::new()),
            asks: RefCell::new(VecDeque::new()),
            ask_open: Cell::new(false),
            ask_dialogs: RefCell::new(HashMap::new()),
            capture: RefCell::new(None),
            quit_confirmed: Cell::new(false),
            quitting: Cell::new(false),
        }
    }

    /// Install the test seam (see [`Capture`]) and return it.
    pub fn capture(&self) -> Rc<Capture> {
        let c = Rc::new(Capture::default());
        *self.capture.borrow_mut() = Some(Rc::clone(&c));
        c
    }

    /// Use a prepared engine (tests: `JobEngine::with_fs(FaultyFs)`); must be called before the
    /// first submit. Returns false if an engine already exists.
    pub fn set_engine(&self, engine: JobEngine) -> bool {
        self.engine.set(engine).is_ok()
    }

    /// The user confirmed quitting with running jobs (the next close request goes through).
    pub fn quit_confirmed(&self) -> bool {
        self.quit_confirmed.get()
    }

    /// Remember that quitting was confirmed.
    pub fn set_quit_confirmed(&self) {
        self.quit_confirmed.set(true);
    }

    /// Quitting was decided and the cancelled jobs are still cleaning up (see `jobs::quit`).
    pub fn quitting(&self) -> bool {
        self.quitting.get()
    }

    /// Mark that the cancelled jobs are being waited for before the window closes.
    pub fn set_quitting(&self, on: bool) {
        self.quitting.set(on);
    }

    /// Submit a job and start pumping engine events on the GTK main loop (first call only).
    /// Returns immediately; the worker does all I/O.
    pub fn submit(self: &Rc<Self>, ctx: &Rc<AppContext>, spec: JobSpec, mode: RunMode) -> JobId {
        self.submit_with(ctx, spec, mode, SubmitOpts::default())
    }

    /// [`submit`](Self::submit) with UI follow-ups (selection clearing, completion callback).
    pub fn submit_with(
        self: &Rc<Self>,
        ctx: &Rc<AppContext>,
        spec: JobSpec,
        mode: RunMode,
        opts: SubmitOpts,
    ) -> JobId {
        let capture = self.capture.borrow().clone();
        if let Some(cap) = capture {
            let id = JobId(cap.next.get() + 1);
            cap.next.set(id.0);
            let title = spec.title();
            cap.submitted.borrow_mut().push((id, spec, mode));
            self.meta.borrow_mut().insert(id, opts);
            self.handle_event(ctx, JobEvent::Queued { id, title, mode });
            return id;
        }
        let engine = self.engine.get_or_init(JobEngine::new);
        self.start_pump(ctx, engine);
        let id = engine.submit(spec, mode);
        // Events are delivered by the pump on a later main-loop iteration, so the metadata is
        // in place before the first event of this job is handled.
        self.meta.borrow_mut().insert(id, opts);
        id
    }

    fn start_pump(self: &Rc<Self>, ctx: &Rc<AppContext>, engine: &JobEngine) {
        if self.pump_started.replace(true) {
            return;
        }
        let (weak_self, weak_ctx) = (Rc::downgrade(self), Rc::downgrade(ctx));
        wiring::attach(
            engine.events(),
            move |ev| match (weak_self.upgrade(), weak_ctx.upgrade()) {
                (Some(this), Some(ctx)) => {
                    this.handle_event(&ctx, ev);
                    ControlFlow::Continue(())
                }
                _ => ControlFlow::Break(()),
            },
            || {},
        );
    }

    /// Pause a job.
    pub fn pause(&self, id: JobId) {
        self.update(id, |j| {
            if j.state == JobState::Running && j.requested.is_none() {
                j.requested = Some(Requested::Pausing);
            }
        });
        if let Some(c) = self.capture.borrow().as_ref() {
            c.controls.borrow_mut().push(Control::Pause(id));
        } else if let Some(e) = self.engine.get() {
            e.pause(id);
        }
    }

    /// Resume a job.
    pub fn resume(&self, id: JobId) {
        // Resumed before the worker reached its pause checkpoint: drop the pending
        // "Pausing…" at once (with it left set, the row could show it forever) and tell the
        // observers so the jobs view updates.
        if self
            .job(id)
            .is_some_and(|j| j.requested == Some(Requested::Pausing))
        {
            self.update(id, |j| j.requested = None);
        }
        if let Some(c) = self.capture.borrow().as_ref() {
            c.controls.borrow_mut().push(Control::Resume(id));
        } else if let Some(e) = self.engine.get() {
            e.resume(id);
        }
    }

    /// Cancel a job.
    pub fn cancel(&self, id: JobId) {
        self.update(id, |j| {
            if j.state != JobState::Finished {
                j.requested = Some(Requested::Cancelling);
            }
        });
        if let Some(c) = self.capture.borrow().as_ref() {
            c.controls.borrow_mut().push(Control::Cancel(id));
        } else if let Some(e) = self.engine.get() {
            e.cancel(id);
        }
    }

    /// Cancel every unfinished job (after the user confirmed quitting).
    pub fn cancel_all(&self) {
        let ids: Vec<JobId> = self
            .jobs
            .borrow()
            .iter()
            .filter(|j| j.state != JobState::Finished)
            .map(|j| j.id)
            .collect();
        for id in ids {
            self.update(id, |j| j.requested = Some(Requested::Cancelling));
        }
        if let Some(c) = self.capture.borrow().as_ref() {
            c.controls.borrow_mut().push(Control::CancelAll);
        } else if let Some(e) = self.engine.get() {
            e.cancel_all();
        }
    }

    /// Pause a running job, resume a paused one (queued/finished jobs: nothing).
    pub fn toggle_pause(&self, id: JobId) {
        match self.job(id).map(|j| j.state) {
            Some(JobState::Running) => self.pause(id),
            Some(JobState::Paused) => self.resume(id),
            _ => {}
        }
    }

    /// Snapshot of the job table, oldest first.
    pub fn jobs(&self) -> Vec<JobEntry> {
        self.jobs.borrow().clone()
    }

    /// The job with `id`, if still in the table.
    pub fn job(&self, id: JobId) -> Option<JobEntry> {
        self.jobs.borrow().iter().find(|j| j.id == id).cloned()
    }

    /// True while any job is queued, running or paused (quit confirmation).
    pub fn has_unfinished(&self) -> bool {
        self.unfinished_count() > 0
    }

    /// Number of jobs that have not finished.
    pub fn unfinished_count(&self) -> usize {
        self.jobs
            .borrow()
            .iter()
            .filter(|j| j.state != JobState::Finished)
            .count()
    }

    /// Drop finished jobs from the table (`jobs.clear_finished`).
    pub fn clear_finished(&self) {
        let ids: Vec<JobId> = {
            let mut jobs = self.jobs.borrow_mut();
            let ids = jobs
                .iter()
                .filter(|j| j.state == JobState::Finished)
                .map(|j| j.id)
                .collect();
            jobs.retain(|j| j.state != JobState::Finished);
            ids
        };
        if self.capture.borrow().is_none()
            && let Some(e) = self.engine.get()
        {
            for id in ids {
                e.forget(id);
            }
        }
        self.changed.emit(&JobsEvent::Cleared);
    }

    /// Apply one engine event on the main thread: update the table, emit [`JobsEvent`]s, open the
    /// conflict/error/trash dialog for `Ask` (`dialogs::conflict::present`), and on `Finished`
    /// refresh panels and show errors. Public so tests can feed synthetic events.
    pub fn handle_event(self: &Rc<Self>, ctx: &Rc<AppContext>, ev: JobEvent) {
        match ev {
            JobEvent::Queued { id, title, mode } => {
                self.jobs.borrow_mut().push(JobEntry {
                    id,
                    title,
                    mode,
                    state: JobState::Queued,
                    progress: Progress::default(),
                    asking: false,
                    outcome: None,
                    requested: None,
                });
                self.changed.emit(&JobsEvent::Added(id));
            }
            JobEvent::Started { id } => self.update(id, |j| j.state = JobState::Running),
            JobEvent::StateChanged { id, state } => self.update(id, |j| {
                j.state = state;
                // The worker acted on the pause; a pending cancel stays until the job ends.
                if j.requested == Some(Requested::Pausing) {
                    j.requested = None;
                }
            }),
            JobEvent::Progress { id, progress } => self.update(id, |j| j.progress = progress),
            JobEvent::Ask {
                id,
                question,
                reply,
            } => {
                self.asks.borrow_mut().push_back(PendingAsk {
                    id,
                    question,
                    reply,
                });
                self.show_next_ask(ctx);
            }
            JobEvent::Finished { id, outcome } => self.finished(ctx, id, outcome),
        }
    }

    fn update(&self, id: JobId, f: impl FnOnce(&mut JobEntry)) {
        let found = {
            let mut jobs = self.jobs.borrow_mut();
            jobs.iter_mut().find(|j| j.id == id).map(f).is_some()
        };
        if found {
            self.changed.emit(&JobsEvent::Updated(id));
        }
    }

    // ---- questions ------------------------------------------------------------------------------
    /// Show the next queued question unless one is already open (one dialog at a time).
    fn show_next_ask(self: &Rc<Self>, ctx: &Rc<AppContext>) {
        while !self.ask_open.get() {
            let Some(PendingAsk {
                id,
                question,
                reply,
            }) = self.asks.borrow_mut().pop_front()
            else {
                return;
            };
            self.ask_open.set(true);
            self.update(id, |j| j.asking = true);
            let (weak_self, weak_ctx) = (Rc::downgrade(self), Rc::downgrade(ctx));
            let on_closed: Box<dyn FnOnce()> = Box::new(move || {
                let (Some(this), Some(ctx)) = (weak_self.upgrade(), weak_ctx.upgrade()) else {
                    return;
                };
                this.ask_dialogs.borrow_mut().remove(&id);
                this.ask_open.set(false);
                this.update(id, |j| j.asking = false);
                this.show_next_ask(&ctx);
            });
            match conflict::present_with(ctx, id, question, reply, on_closed) {
                Some(dialog) => {
                    self.ask_dialogs.borrow_mut().insert(id, dialog);
                }
                None => {
                    // No window: the responder was dropped, which aborts the job.
                    self.ask_open.set(false);
                    self.update(id, |j| j.asking = false);
                }
            }
        }
    }

    // ---- completion -----------------------------------------------------------------------------
    fn finished(self: &Rc<Self>, ctx: &Rc<AppContext>, id: JobId, outcome: Outcome) {
        // A job that ended (cancelled, aborted) takes its open question with it.
        self.asks.borrow_mut().retain(|a| a.id != id);
        let open = self.ask_dialogs.borrow().get(&id).cloned();
        if let Some(d) = open {
            d.close();
        }
        let title = {
            let mut jobs = self.jobs.borrow_mut();
            jobs.iter_mut().find(|j| j.id == id).map(|j| {
                j.state = JobState::Finished;
                j.asking = false;
                j.requested = None;
                j.outcome = Some(outcome.clone());
                j.title.clone()
            })
        };
        let meta = self.meta.borrow_mut().remove(&id);
        self.changed.emit(&JobsEvent::Finished(id));

        let title = title.unwrap_or_default();
        if let Some(report) = outcome.report() {
            refresh_panels(ctx, &report.created);
        }
        if let Some(meta) = meta {
            if outcome.is_success()
                && let Some(c) = meta.origin.as_ref().and_then(Weak::upgrade)
            {
                c.mutate(|m| m.select_all(false));
            }
            if let Some(cb) = meta.on_finished {
                cb(ctx, &outcome);
            }
        }
        report_problems(ctx, &title, &outcome);
    }
}

/// After a job: make every panel show the new state and put its cursor on the first created
/// item inside it. The watcher delivers the changes for local directories; panels without a
/// watch (archives, lists, a watch still starting) are reloaded explicitly.
fn refresh_panels(ctx: &Rc<AppContext>, created: &[PathBuf]) {
    for side in 0..2 {
        let panel = ctx.host(side).active();
        let focus = match panel.location() {
            Location::Local(dir) => created.iter().find_map(|p| focus_target(&dir, p)),
            _ => None,
        };
        match (panel.watching(), focus) {
            (true, Some(name)) => panel.focus_name(name),
            (true, None) => {}
            (false, Some(name)) => panel.reload_and_focus(name),
            (false, None) => panel.reload(),
        }
    }
}

/// Failures and warnings are shown once, when the job ends.
fn report_problems(ctx: &Rc<AppContext>, title: &str, outcome: &Outcome) {
    match outcome {
        Outcome::Failed { error, .. } => {
            ui::message(ctx, tr("Operation failed"), &format!("{title}\n\n{error}"));
        }
        Outcome::Ok(r) | Outcome::Cancelled(r) if !r.warnings.is_empty() => {
            const SHOWN: usize = 10;
            let mut text = format!("{title}\n");
            for w in r.warnings.iter().take(SHOWN) {
                text.push_str(&format!("\n{}: {}", w.path.display(), w.message));
            }
            if r.warnings.len() > SHOWN {
                text.push_str(&format!(
                    "\n... {} {}",
                    r.warnings.len() - SHOWN,
                    tr("more")
                ));
            }
            ui::message(ctx, tr("Completed with warnings"), &text);
        }
        _ => {}
    }
}

impl Default for JobsController {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_controller_has_nothing_unfinished_and_no_engine() {
        let c = JobsController::new();
        assert!(!c.has_unfinished());
        assert!(c.jobs().is_empty());
        // No engine yet: control calls are harmless no-ops.
        c.pause(JobId(1));
        c.cancel_all();
        c.clear_finished();
    }

    /// T8.10: resume before the worker's pause checkpoint must clear the pending "Pausing…"
    /// immediately (it used to stay until some later state change, i.e. forever).
    #[test]
    fn resume_clears_a_pending_pausing_request() {
        let c = JobsController::new();
        let cap = c.capture();
        let id = JobId(7);
        c.jobs.borrow_mut().push(JobEntry {
            id,
            title: "copy".into(),
            mode: RunMode::Queued,
            state: JobState::Running,
            progress: Progress::default(),
            asking: false,
            outcome: None,
            requested: Some(Requested::Pausing),
        });
        c.resume(id);
        assert_eq!(c.job(id).and_then(|j| j.requested), None);
        assert_eq!(*cap.controls.borrow(), vec![Control::Resume(id)]);
        // A cancel request survives resume.
        c.update(id, |j| j.requested = Some(Requested::Cancelling));
        c.resume(id);
        assert_eq!(
            c.job(id).and_then(|j| j.requested),
            Some(Requested::Cancelling)
        );
    }
}

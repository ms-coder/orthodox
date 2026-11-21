//! Content mode of the comparison: the byte-for-byte pass over same-name, same-size pairs.
//!
//! The metadata pass (name + size) has already been applied when this starts. The candidate
//! pairs go to the read-only `CompareContent` job (`ofm-jobs`, T8.1b) as a parallel job (it shows
//! up in the jobs view, Ctrl+J, like any other). Per-pair results stream back through a
//! `ResultSink` into an `async-channel`; the main thread applies them in batches
//! (`apply_content_result`, then both panels' selection is replaced from the updated result), so
//! marks appear while the job runs. `panel.cancel` (Esc) cancels the job; the marks found so far
//! stay.
//!
//! [`Tracker`] is the pure part (result bookkeeping and counts) and is unit-tested.

use std::cell::RefCell;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::rc::Rc;

use ofm_core::Location;
use ofm_core::compare::{CompareResult, ContentPair, apply_content_result};
use ofm_jobs::{CompareContentOpts, JobSpec, Outcome, PairResult, ResultSink, RunMode};

use super::marks::{self, Counts};
use super::options::MarkFilter;
use super::{CompareState, message, summary_text};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::{self, SubmitOpts};
use crate::panel::{LEFT, RIGHT};

/// What the byte comparison said about one pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same bytes.
    Equal,
    /// Different bytes.
    Differs,
    /// Skipped (special file, directory) or unreadable: stays unmarked, reported in the summary.
    Unverified,
}

impl From<&PairResult> for Verdict {
    fn from(r: &PairResult) -> Self {
        match r {
            PairResult::Equal => Verdict::Equal,
            PairResult::Differs => Verdict::Differs,
            PairResult::Skipped(_) | PairResult::Error(_) => Verdict::Unverified,
        }
    }
}

/// Bookkeeping of a running content comparison: the metadata result plus the undecided pairs.
pub struct Tracker {
    result: CompareResult,
    pending: Vec<Option<ContentPair>>,
    left_total: usize,
    filter: MarkFilter,
    unverified: usize,
}

impl Tracker {
    /// Take over `result` (its candidates become the pending list).
    pub fn new(mut result: CompareResult, left_total: usize, filter: MarkFilter) -> Self {
        let pending = std::mem::take(&mut result.content_candidates)
            .into_iter()
            .map(Some)
            .collect();
        Tracker {
            result,
            pending,
            left_total,
            filter,
            unverified: 0,
        }
    }

    /// Number of pairs handed to the job.
    pub fn total(&self) -> usize {
        self.pending.len()
    }

    /// Pairs not decided yet.
    pub fn remaining(&self) -> usize {
        self.pending.iter().flatten().count()
    }

    /// Record the verdict for pair `index`; `true` when it changed the marks. Unknown or
    /// already-decided indexes are ignored.
    pub fn apply(&mut self, index: usize, verdict: Verdict) -> bool {
        let Some(pair) = self.pending.get_mut(index).and_then(Option::take) else {
            return false;
        };
        match verdict {
            Verdict::Equal => false,
            Verdict::Differs => {
                apply_content_result(&mut self.result, pair, false);
                true
            }
            Verdict::Unverified => {
                self.unverified += 1;
                false
            }
        }
    }

    /// Names to select on the left and right now.
    pub fn wanted(&self) -> [HashSet<OsString>; 2] {
        [
            marks::names(&self.result.left_marks, self.filter),
            marks::names(&self.result.right_marks, self.filter),
        ]
    }

    /// Current counts: undecided and unverifiable pairs are not "identical".
    pub fn counts(&self) -> Counts {
        let mut c = marks::counts(&self.result, self.left_total, self.filter);
        let remaining = self.remaining();
        c.identical = c.identical.saturating_sub(remaining + self.unverified);
        c.pending = remaining;
        c
    }

    /// Pairs that could not be compared (special files, unreadable).
    pub fn unverified(&self) -> usize {
        self.unverified
    }
}

/// Message while the job runs.
pub fn progress_text(t: &Tracker) -> String {
    format!(
        "{}; {} {}/{} ({})",
        summary_text(&t.counts()),
        tr("comparing content…"),
        t.total() - t.remaining(),
        t.total(),
        tr("Esc cancels"),
    )
}

/// Final message (`cancelled`: the job stopped early).
pub fn final_text(t: &Tracker, cancelled: bool) -> String {
    let mut text = summary_text(&t.counts());
    if t.unverified() > 0 {
        text = format!("{text}; {} {}", t.unverified(), tr("could not be compared"));
    }
    if cancelled && t.remaining() > 0 {
        text = format!(
            "{text}; {} ({} {})",
            tr("content comparison cancelled"),
            t.remaining(),
            tr("files not compared")
        );
    }
    text
}

/// Why the content pass could not start.
#[derive(Debug, PartialEq, Eq)]
pub enum Unavailable {
    /// A panel is not a local directory (paths are needed to read the files).
    NotLocal,
    /// The jobs engine is not installed.
    NoJobs,
}

/// Messages from the job to the main-thread consumer.
enum Update {
    Pair(usize, Verdict),
    Done { cancelled: bool },
}

/// A running content comparison (state in [`CompareState`]).
pub struct ContentRun {
    job: ofm_jobs::JobId,
}

/// Start the content job for the candidates of `tracker`. The caller shows the first message.
pub fn start(
    ctx: &Rc<AppContext>,
    locations: &[Location; 2],
    tracker: Tracker,
) -> Result<(), Unavailable> {
    let (Location::Local(left_dir), Location::Local(right_dir)) = (&locations[0], &locations[1])
    else {
        return Err(Unavailable::NotLocal);
    };
    let (Some(state), Some(jobs)) = (ctx.ext.get::<CompareState>(), jobs::controller(ctx)) else {
        return Err(Unavailable::NoJobs);
    };
    cancel_running(ctx);
    let pairs: Vec<(PathBuf, PathBuf)> = tracker
        .pending
        .iter()
        .flatten()
        .map(|p| (left_dir.join(&p.left.name), right_dir.join(&p.right.name)))
        .collect();
    let (tx, rx) = async_channel::unbounded::<Update>();
    let sink = {
        let tx = tx.clone();
        ResultSink::new(move |i, r| {
            let _ = tx.try_send(Update::Pair(i, Verdict::from(r)));
        })
    };
    let on_finished = Box::new(move |_: &Rc<AppContext>, outcome: &Outcome| {
        let _ = tx.try_send(Update::Done {
            cancelled: !matches!(outcome, Outcome::Ok(_)),
        });
    });
    let token = state.content_token.get() + 1;
    state.content_token.set(token);
    let job = jobs.submit_with(
        ctx,
        JobSpec::CompareContent {
            pairs,
            opts: CompareContentOpts {
                on_result: Some(sink),
                ..CompareContentOpts::default()
            },
        },
        RunMode::Parallel,
        SubmitOpts {
            origin: None,
            on_finished: Some(on_finished),
        },
    );
    *state.content.borrow_mut() = Some(ContentRun { job });

    let weak = Rc::downgrade(ctx);
    let tracker = Rc::new(RefCell::new(tracker));
    gtk::glib::spawn_future_local(async move {
        while let Ok(first) = rx.recv().await {
            let Some(ctx) = weak.upgrade() else {
                return;
            };
            let Some(state) = ctx.ext.get::<CompareState>() else {
                return;
            };
            if state.content_token.get() != token {
                return; // superseded by a newer comparison
            }
            // Take everything that has arrived, then update the panels once.
            let mut batch = vec![first];
            while let Ok(more) = rx.try_recv() {
                batch.push(more);
            }
            let mut done = None;
            let mut changed = false;
            {
                let mut t = tracker.borrow_mut();
                for u in batch {
                    match u {
                        Update::Pair(i, v) => changed |= t.apply(i, v),
                        Update::Done { cancelled } => done = Some(cancelled),
                    }
                }
            }
            let t = tracker.borrow();
            if changed {
                let wanted = t.wanted();
                ctx.host(LEFT)
                    .active()
                    .mutate(|m| marks::replace_selection(m, &wanted[0]));
                ctx.host(RIGHT)
                    .active()
                    .mutate(|m| marks::replace_selection(m, &wanted[1]));
            }
            match done {
                Some(cancelled) => {
                    state.content.borrow_mut().take();
                    message::show(&ctx, &final_text(&t, cancelled));
                    return;
                }
                None => message::show_sticky(&ctx, &progress_text(&t)),
            }
        }
        // The channel closed without a `Done` (engine gone): release the run.
        if let Some(state) = weak.upgrade().and_then(|c| c.ext.get::<CompareState>())
            && state.content_token.get() == token
        {
            state.content.borrow_mut().take();
        }
    });
    Ok(())
}

/// Cancel a running content comparison (Esc). `true` when there was one; the job's completion
/// then reports what was compared so far.
pub fn cancel_running(ctx: &Rc<AppContext>) -> bool {
    let Some(state) = ctx.ext.get::<CompareState>() else {
        return false;
    };
    let job = state.content.borrow().as_ref().map(|r| r.job);
    match (job, jobs::controller(ctx)) {
        (Some(job), Some(jobs)) => {
            jobs.cancel(job);
            true
        }
        _ => false,
    }
}

/// Text appended to the summary when the content pass cannot run.
pub fn unavailable_note(why: &Unavailable, pending: usize) -> String {
    let reason = match why {
        Unavailable::NotLocal => tr("content compare needs local directories"),
        Unavailable::NoJobs => tr("content compare is not available"),
    };
    format!("{reason} ({pending} {})", tr("files not compared"))
}

#[cfg(test)]
mod tests {
    use ofm_core::compare::{CompareEntry, CompareKind, CompareMode, CompareOptions, compare};

    use super::*;

    fn file(name: &str, size: u64) -> CompareEntry {
        CompareEntry {
            name: name.into(),
            kind: CompareKind::File,
            size,
            mtime_ns: 0,
            link_target: None,
        }
    }

    fn tracker() -> Tracker {
        let l = vec![file("a", 1), file("b", 1), file("c", 1), file("size", 2)];
        let r = vec![file("a", 1), file("b", 1), file("c", 1), file("size", 3)];
        let opts = CompareOptions {
            mode: CompareMode::Content,
            ..CompareOptions::default()
        };
        let res = compare(&l, &r, &opts);
        assert_eq!(res.content_candidates.len(), 3);
        Tracker::new(res, 4, MarkFilter::AllDiffering)
    }

    #[test]
    fn results_mark_incrementally() {
        let mut t = tracker();
        assert_eq!((t.total(), t.remaining()), (3, 3));
        // Before any verdict only the size difference is known.
        let c = t.counts();
        assert_eq!((c.left, c.right, c.identical, c.pending), (1, 1, 0, 3));
        assert!(!t.apply(0, Verdict::Equal));
        assert!(t.apply(1, Verdict::Differs));
        let [l, r] = t.wanted();
        assert!(l.contains(&OsString::from("b")) && r.contains(&OsString::from("b")));
        assert!(!l.contains(&OsString::from("a")));
        // A result for the same pair twice, or for a bogus index, changes nothing.
        assert!(!t.apply(1, Verdict::Differs));
        assert!(!t.apply(99, Verdict::Differs));
        assert!(!t.apply(2, Verdict::Unverified));
        let c = t.counts();
        assert_eq!((c.left, c.right, c.identical, c.pending), (2, 2, 1, 0));
        assert_eq!(t.unverified(), 1);
    }

    #[test]
    fn texts() {
        let mut t = tracker();
        t.apply(0, Verdict::Equal);
        assert_eq!(
            progress_text(&t),
            "1 differing on the left, 1 on the right, 1 identical; comparing content… 1/3 (Esc cancels)"
        );
        t.apply(1, Verdict::Unverified);
        assert_eq!(
            final_text(&t, true),
            "1 differing on the left, 1 on the right, 1 identical; 1 could not be compared; \
             content comparison cancelled (1 files not compared)"
        );
    }

    #[test]
    fn verdicts_from_job_results() {
        use ofm_jobs::SkipReason;
        assert_eq!(Verdict::from(&PairResult::Equal), Verdict::Equal);
        assert_eq!(Verdict::from(&PairResult::Differs), Verdict::Differs);
        assert_eq!(
            Verdict::from(&PairResult::Skipped(SkipReason::SpecialFile)),
            Verdict::Unverified
        );
    }

    #[test]
    fn note_names_the_reason() {
        assert_eq!(
            unavailable_note(&Unavailable::NotLocal, 3),
            "content compare needs local directories (3 files not compared)"
        );
    }
}

//! Compare directories (F27, `panel.compare_dirs`, Shift+F2). Owner: T8.1c.
//!
//! Flow: the command opens the options dialog (`dialogs::compare`); accepting it snapshots the
//! two panels' visible listings (in memory, nothing is read from disk), runs
//! `ofm_core::compare::compare` on the I/O pool and applies the result on the main thread as
//! selection marks in both panels (replacing the previous selection), followed by a one-line
//! message. Marks are plain selection by name, so they survive re-sorts and watcher refreshes
//! exactly like a hand-made selection.
//!
//! - [`options`]: dialog settings and parsing (pure).
//! - [`marks`]: result -> selected names, counts, selection replacement (pure).
//! - [`message`]: the transient message row above the F-key bar.
//! - [`content`]: the content mode (waits for the `CompareContent` job, T8.1b).

pub mod content;
pub mod marks;
pub mod message;
pub mod options;

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::rc::Rc;

use ofm_core::compare::{CompareEntry, CompareMode, CompareResult, compare};
use ofm_core::{Location, cmd};

use crate::context::AppContext;
use crate::dialogs;
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::panel::{LEFT, LoadState, PanelController, RIGHT};
use crate::window::MainWindow;
use crate::wiring;

pub use options::{CompareSettings, MarkFilter};

/// Module state in `ctx.ext`.
#[derive(Default)]
pub struct CompareState {
    /// The settings of the last comparison (the dialog starts from them).
    settings: RefCell<CompareSettings>,
    /// Bumped per run; a result of an older run is dropped.
    generation: Cell<u64>,
    /// Runs whose result was applied or dropped (for tests and `is_running`).
    finished: Cell<u64>,
    /// Text of the last message.
    last_message: RefCell<Option<String>>,
    /// The message row (once the window exists).
    row: message::RowSlot,
    /// The running content job, if any.
    content: RefCell<Option<content::ContentRun>>,
    /// Bumped per content run; its consumer stops when it no longer matches.
    content_token: Cell<u64>,
}

impl CompareState {
    /// Settings of the last comparison (defaults before the first one).
    pub fn settings(&self) -> CompareSettings {
        self.settings.borrow().clone()
    }

    /// Text of the last message shown.
    pub fn last_message(&self) -> Option<String> {
        self.last_message.borrow().clone()
    }

    /// Is a comparison running (metadata pass on a worker, or the content job)?
    pub fn is_running(&self) -> bool {
        self.finished.get() < self.generation.get() || self.content.borrow().is_some()
    }
}

/// Register `panel.compare_dirs`.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::PANEL_COMPARE_DIRS, |ctx| {
        let initial = ctx
            .ext
            .get::<CompareState>()
            .map(|s| s.settings())
            .unwrap_or_default();
        let ctx2 = Rc::clone(ctx);
        dialogs::compare::present(ctx, &initial, move |settings| start(&ctx2, settings));
    });
}

/// Create the state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(CompareState::default()));
}

/// Build the message row.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    message::install_ui(ctx, win);
}

/// Listing of a panel as the engine wants it: the visible rows (so the active filter and hidden
/// files are respected), without the synthetic `..`.
fn snapshot(c: &PanelController) -> Vec<CompareEntry> {
    let m = c.model();
    (0..m.visible_len())
        .filter_map(|i| m.visible(i))
        .filter(|e| e.name != "..")
        .map(CompareEntry::from)
        .collect()
}

/// What the worker sends back.
struct Outcome {
    result: CompareResult,
    left_total: usize,
}

/// Compare the left and right panel with `settings` and select the differences in both.
///
/// The comparison itself runs on the I/O pool; the result is applied on the main thread when it
/// arrives (unless a newer run started or a panel changed directory meanwhile).
pub fn start(ctx: &Rc<AppContext>, settings: CompareSettings) {
    let Some(state) = ctx.ext.get::<CompareState>() else {
        return;
    };
    *state.settings.borrow_mut() = settings.clone();
    let (left, right) = (ctx.host(LEFT).active(), ctx.host(RIGHT).active());
    if left.state() != LoadState::Idle || right.state() != LoadState::Idle {
        message::show(ctx, tr("Wait until both panels are loaded."));
        return;
    }
    let locations: [Location; 2] = [left.location(), right.location()];
    let (l_entries, r_entries) = (snapshot(&left), snapshot(&right));
    let generation = state.generation.get() + 1;
    state.generation.set(generation);
    // A newer comparison supersedes a content job still running.
    if content::cancel_running(ctx) {
        state.content_token.set(state.content_token.get() + 1);
        state.content.borrow_mut().take();
    }

    let opts = settings.to_options();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let left_total = marks::compared_len(&l_entries, &opts);
        let result = compare(&l_entries, &r_entries, &opts);
        let _ = tx.send_blocking(Outcome { result, left_total });
    });
    let (weak, weak_closed) = (Rc::downgrade(ctx), Rc::downgrade(ctx));
    wiring::attach(
        rx,
        move |outcome: Outcome| {
            if let Some(ctx) = weak.upgrade() {
                finish(&ctx, generation, &locations, &settings, outcome);
            }
            ControlFlow::Break(())
        },
        move || {
            // A worker that died without a result must not leave the run "running" forever.
            if let Some(state) = weak_closed
                .upgrade()
                .and_then(|c| c.ext.get::<CompareState>())
                && state.finished.get() < generation
            {
                state.finished.set(generation);
            }
        },
    );
}

/// Apply a finished comparison (main thread).
fn finish(
    ctx: &Rc<AppContext>,
    generation: u64,
    locations: &[Location; 2],
    settings: &CompareSettings,
    outcome: Outcome,
) {
    let Some(state) = ctx.ext.get::<CompareState>() else {
        return;
    };
    state.finished.set(state.finished.get().max(generation));
    let (left, right) = (ctx.host(LEFT).active(), ctx.host(RIGHT).active());
    let stale = state.generation.get() != generation
        || left.location() != locations[0]
        || right.location() != locations[1];
    if stale {
        tracing::debug!("dropping a stale directory comparison");
        return;
    }
    let Outcome { result, left_total } = outcome;
    let tracker = content::Tracker::new(result, left_total, settings.mark);
    let wanted = tracker.wanted();
    left.mutate(|m| marks::replace_selection(m, &wanted[0]));
    right.mutate(|m| marks::replace_selection(m, &wanted[1]));

    if settings.mode != CompareMode::Content || tracker.remaining() == 0 {
        message::show(ctx, &summary_text(&tracker.counts()));
        return;
    }
    // Content mode: the byte comparison of the same-size pairs streams in as a job.
    let (pending, summary) = (tracker.remaining(), summary_text(&tracker.counts()));
    let first = content::progress_text(&tracker);
    match content::start(ctx, locations, tracker) {
        Ok(()) => message::show_sticky(ctx, &first),
        Err(why) => message::show(
            ctx,
            &format!("{summary}; {}", content::unavailable_note(&why, pending)),
        ),
    }
}

/// "3 differing on the left, 2 on the right, 10 identical".
pub fn summary_text(c: &marks::Counts) -> String {
    format!(
        "{} {}, {} {}, {} {}",
        c.left,
        tr("differing on the left"),
        c.right,
        tr("on the right"),
        c.identical,
        tr("identical"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_wording() {
        let c = marks::Counts {
            left: 3,
            right: 2,
            identical: 10,
            pending: 0,
        };
        assert_eq!(
            summary_text(&c),
            "3 differing on the left, 2 on the right, 10 identical"
        );
    }
}

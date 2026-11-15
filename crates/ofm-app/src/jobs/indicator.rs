//! Progress in the status area (F15): one slim row above the F-key bar that appears while jobs
//! are queued, running or paused: title, progress bar, speed/ETA and "+N more". Clicking it opens
//! the jobs view (`app.jobs`). Colours come from the theme through the `ofm-statusbox`,
//! `ofm-status`, `ofm-status-dim` and `ofm-progress` classes.

use std::rc::Rc;

use adw::prelude::*;
use ofm_core::cmd;
use ofm_jobs::{JobState, RunMode};

use super::controller::{JobEntry, JobsController};
use super::fmt;
use crate::context::AppContext;
use crate::i18n::tr;
use crate::window::MainWindow;

/// The row's widgets (kept in `ctx.ext` so tests can read them).
pub struct Indicator {
    /// Reveals the row while there is something to show.
    pub revealer: gtk::Revealer,
    /// Title of the job shown.
    pub title: gtk::Label,
    /// Progress bar.
    pub bar: gtk::ProgressBar,
    /// Percentage, speed, ETA.
    pub text: gtk::Label,
    /// "+N more" when several jobs are active.
    pub more: gtk::Label,
}

/// The job worth showing: the first running one, else a paused one, else the first queued one.
pub fn pick(jobs: &[JobEntry]) -> Option<&JobEntry> {
    let active = |s: JobState| jobs.iter().find(|j| j.state == s);
    active(JobState::Running)
        .or_else(|| active(JobState::Paused))
        .or_else(|| active(JobState::Queued))
}

/// Text next to the bar: `45%  12 MiB/s  ETA 0:32`, or the state for paused/queued jobs.
pub fn status_text(job: &JobEntry) -> String {
    let queued = job.mode == RunMode::Queued && job.state == JobState::Queued;
    match (job.state, job.asking) {
        (_, true) => tr("waiting for your answer").to_owned(),
        (JobState::Queued, _) if queued => tr("queued").to_owned(),
        (JobState::Paused, _) => tr("paused").to_owned(),
        _ => {
            let mut parts = Vec::new();
            if let Some(f) = fmt::fraction(&job.progress) {
                parts.push(format!("{:.0}%", f * 100.0));
            } else if job.progress.planning {
                parts.push(tr("scanning").to_owned());
            }
            if job.progress.bytes_per_sec > 0 {
                parts.push(fmt::speed(job.progress.bytes_per_sec));
            }
            if let Some(eta) = job.progress.eta_secs {
                parts.push(format!("{} {}", tr("ETA"), fmt::duration(eta)));
            }
            parts.join("  ")
        }
    }
}

impl Indicator {
    fn refresh(&self, ctrl: &JobsController) {
        let jobs = ctrl.jobs();
        let Some(job) = pick(&jobs) else {
            self.revealer.set_reveal_child(false);
            return;
        };
        self.title.set_text(&job.title);
        match fmt::fraction(&job.progress) {
            Some(f) => self.bar.set_fraction(f),
            None if job.state == JobState::Running => self.bar.pulse(),
            None => self.bar.set_fraction(0.0),
        }
        self.text.set_text(&status_text(job));
        let others = jobs
            .iter()
            .filter(|j| j.state != JobState::Finished && j.id != job.id)
            .count();
        self.more.set_visible(others > 0);
        self.more.set_text(&format!("+{others}"));
        self.revealer.set_reveal_child(true);
    }
}

/// Build the row into the window's F-key slot (above the F-key bar).
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let Some(ctrl) = super::controller(ctx) else {
        return;
    };
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("ofm-statusbox");
    let title = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::Middle)
        .css_classes(["ofm-status"])
        .build();
    let bar = gtk::ProgressBar::new();
    bar.add_css_class("ofm-progress");
    bar.set_valign(gtk::Align::Center);
    bar.set_size_request(200, -1);
    let text = gtk::Label::builder()
        .css_classes(["ofm-status-dim"])
        .width_chars(26)
        .xalign(0.0)
        .build();
    let more = gtk::Label::builder()
        .css_classes(["ofm-status-dim"])
        .visible(false)
        .build();
    row.append(&title);
    row.append(&bar);
    row.append(&text);
    row.append(&more);
    row.set_tooltip_text(Some(tr("Background jobs (Ctrl+J)")));
    row.update_property(&[gtk::accessible::Property::Label(tr("Job progress"))]);
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
    revealer.set_child(Some(&row));
    revealer.set_reveal_child(false);
    win.fkeybar_slot.prepend(&revealer);

    // Mouse: a click opens the jobs view (same command as the key).
    let click = gtk::GestureClick::new();
    let weak_ctx = Rc::downgrade(ctx);
    click.connect_released(move |_, _, _, _| {
        if let Some(ctx) = weak_ctx.upgrade() {
            ctx.execute(cmd::APP_JOBS);
        }
    });
    row.add_controller(click);

    let ind = Rc::new(Indicator {
        revealer,
        title,
        bar,
        text,
        more,
    });
    {
        let ind = Rc::downgrade(&ind);
        let ctrl2 = Rc::downgrade(&ctrl);
        ctrl.changed.connect(move |_| {
            if let (Some(ind), Some(ctrl)) = (ind.upgrade(), ctrl2.upgrade()) {
                ind.refresh(&ctrl);
            }
        });
    }
    ctx.ext.insert(ind);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use ofm_jobs::{JobId, Progress};

    fn job(id: u64, state: JobState, mode: RunMode) -> JobEntry {
        JobEntry {
            id: JobId(id),
            title: format!("job {id}"),
            mode,
            state,
            progress: Progress::default(),
            asking: false,
            outcome: None,
        }
    }

    #[test]
    fn picks_running_before_paused_before_queued() {
        let jobs = [
            job(1, JobState::Finished, RunMode::Queued),
            job(2, JobState::Queued, RunMode::Queued),
            job(3, JobState::Paused, RunMode::Queued),
            job(4, JobState::Running, RunMode::Parallel),
        ];
        assert_eq!(pick(&jobs).map(|j| j.id), Some(JobId(4)));
        assert_eq!(pick(&jobs[..3]).map(|j| j.id), Some(JobId(3)));
        assert_eq!(pick(&jobs[..2]).map(|j| j.id), Some(JobId(2)));
        assert!(pick(&jobs[..1]).is_none());
    }

    #[test]
    fn status_texts() {
        let mut j = job(1, JobState::Running, RunMode::Queued);
        j.progress = Progress {
            bytes_done: 50,
            bytes_total: 200,
            bytes_per_sec: 2048,
            eta_secs: Some(9),
            ..Progress::default()
        };
        assert_eq!(status_text(&j), "25%  2.0 KiB/s  ETA 0:09");
        j.state = JobState::Paused;
        assert_eq!(status_text(&j), "paused");
        j.state = JobState::Queued;
        assert_eq!(status_text(&j), "queued");
        j.state = JobState::Running;
        j.asking = true;
        assert_eq!(status_text(&j), "waiting for your answer");
    }
}

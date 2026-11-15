//! The jobs view (`app.jobs`, Ctrl+J; F15): every job with state, progress bar, speed, ETA and
//! the file being processed; pause/resume (Space), cancel (Del), clear finished (Ctrl+L). It is a
//! modal dialog so the keys resolve in the `dialog` keymap section; Ctrl+J again closes it.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use adw::prelude::*;
use ofm_jobs::{JobId, JobState};

use super::actions::{self, Action};
use super::controller::{JobEntry, JobsController, JobsEvent};
use super::fmt;
use super::ui;
use crate::context::AppContext;
use crate::i18n::tr;
use crate::navigation::modal;

/// The open jobs view, if any (state in `ctx.ext`).
#[derive(Default)]
pub struct JobsViewState {
    open: RefCell<Option<adw::Dialog>>,
}

impl JobsViewState {
    /// True while the view is shown.
    pub fn is_open(&self) -> bool {
        self.open.borrow().is_some()
    }

    /// The jobs list widget of the open view (tests).
    pub fn dialog(&self) -> Option<adw::Dialog> {
        self.open.borrow().clone()
    }
}

/// Create the state holder.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(JobsViewState::default()));
}

/// Widgets of one job row.
struct Row {
    id: JobId,
    widget: gtk::ListBoxRow,
    title: gtk::Label,
    state: gtk::Label,
    bar: gtk::ProgressBar,
    detail: gtk::Label,
    current: gtk::Label,
}

impl Row {
    fn new(job: &JobEntry) -> Row {
        let title = gtk::Label::builder()
            .xalign(0.0)
            .hexpand(true)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .css_classes(["heading"])
            .build();
        let state = gtk::Label::builder()
            .xalign(1.0)
            .css_classes(["dim-label"])
            .build();
        let head = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        head.append(&title);
        head.append(&state);
        let bar = gtk::ProgressBar::new();
        bar.add_css_class("ofm-progress");
        let detail = gtk::Label::builder().xalign(0.0).wrap(true).build();
        let current = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Start)
            .css_classes(["dim-label", "caption"])
            .build();
        let column = modal::padded_column(3);
        column.append(&head);
        column.append(&bar);
        column.append(&detail);
        column.append(&current);
        let widget = gtk::ListBoxRow::new();
        widget.set_child(Some(&column));
        let row = Row {
            id: job.id,
            widget,
            title,
            state,
            bar,
            detail,
            current,
        };
        row.refresh(job);
        row
    }

    fn refresh(&self, job: &JobEntry) {
        self.title.set_text(&job.title);
        let mode = match (job.mode, job.state) {
            (ofm_jobs::RunMode::Parallel, s) if s != JobState::Finished => " \u{b7} parallel",
            _ => "",
        };
        self.state
            .set_text(&format!("{}{mode}", fmt::state_text(job.state, job.asking)));
        let finished = job.state == JobState::Finished;
        self.bar.set_visible(!finished);
        match fmt::fraction(&job.progress) {
            Some(f) => self.bar.set_fraction(f),
            None if job.state == JobState::Running => self.bar.pulse(),
            None => self.bar.set_fraction(0.0),
        }
        match (&job.outcome, finished) {
            (Some(o), true) => self.detail.set_text(&fmt::outcome_text(o)),
            _ => self.detail.set_text(&fmt::details(&job.progress)),
        }
        let cur = job
            .progress
            .current
            .as_ref()
            .filter(|_| !finished)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.current.set_visible(!cur.is_empty());
        self.current.set_text(&cur);
        self.widget
            .update_property(&[gtk::accessible::Property::Label(&format!(
                "{}, {}",
                job.title,
                fmt::state_text(job.state, job.asking)
            ))]);
    }
}

/// The job the buttons and keys act on. The selected row wins while it is still unfinished;
/// otherwise the most relevant unfinished job: a running one, then a paused one, then the oldest
/// queued one. Without this, a finished job at the top of the list (the oldest row, selected by
/// default) swallowed Cancel / Pause / Del while the running job sat further down.
fn pick_target(selected: Option<JobId>, jobs: &[(JobId, JobState)]) -> Option<JobId> {
    if let Some(sel) = selected
        && jobs
            .iter()
            .any(|(id, st)| *id == sel && *st != JobState::Finished)
    {
        return Some(sel);
    }
    [JobState::Running, JobState::Paused, JobState::Queued]
        .into_iter()
        .find_map(|want| jobs.iter().find(|(_, st)| *st == want).map(|(id, _)| *id))
}

struct View {
    ctrl: Rc<JobsController>,
    list: gtk::ListBox,
    empty: gtk::Label,
    rows: RefCell<Vec<Row>>,
    pause: gtk::Button,
}

impl View {
    fn selected(&self) -> Option<JobId> {
        let row = self.list.selected_row()?;
        self.rows
            .borrow()
            .iter()
            .find(|r| r.widget == row)
            .map(|r| r.id)
    }

    fn states(&self) -> Vec<(JobId, JobState)> {
        self.ctrl.jobs().iter().map(|j| (j.id, j.state)).collect()
    }

    /// The job an action applies to (see [`pick_target`]).
    fn target(&self) -> Option<JobId> {
        pick_target(self.selected(), &self.states())
    }

    /// Keep the selection on a job that can still be controlled: when the selected job finished
    /// (or nothing is selected) move it to the running / paused / queued one, else to the first row.
    fn reselect(&self) {
        let want = self.target();
        let rows = self.rows.borrow();
        let row = want
            .and_then(|id| rows.iter().find(|r| r.id == id))
            .or_else(|| {
                if self.list.selected_row().is_none() {
                    rows.first()
                } else {
                    None
                }
            });
        if let Some(row) = row {
            self.list.select_row(Some(&row.widget));
        }
    }

    fn add(&self, job: &JobEntry) {
        let row = Row::new(job);
        self.list.append(&row.widget);
        self.rows.borrow_mut().push(row);
        self.empty.set_visible(false);
        self.reselect();
    }

    fn sync(&self, id: JobId) {
        let Some(job) = self.ctrl.job(id) else { return };
        if let Some(row) = self.rows.borrow().iter().find(|r| r.id == id) {
            row.refresh(&job);
        }
        self.reselect();
        self.update_buttons();
    }

    fn drop_cleared(&self) {
        let alive: Vec<JobId> = self.ctrl.jobs().iter().map(|j| j.id).collect();
        self.rows.borrow_mut().retain(|r| {
            let keep = alive.contains(&r.id);
            if !keep {
                self.list.remove(&r.widget);
            }
            keep
        });
        self.reselect();
        self.empty.set_visible(self.rows.borrow().is_empty());
        self.update_buttons();
    }

    fn update_buttons(&self) {
        let label = match self
            .target()
            .and_then(|id| self.ctrl.job(id))
            .map(|j| j.state)
        {
            Some(JobState::Paused) => tr("_Resume"),
            _ => tr("_Pause"),
        };
        self.pause.set_label(label);
    }

    fn act(&self, action: Action) -> bool {
        match action {
            Action::PauseResume => {
                if let Some(id) = self.target() {
                    self.ctrl.toggle_pause(id);
                }
            }
            Action::Cancel => {
                if let Some(id) = self.target() {
                    self.ctrl.cancel(id);
                }
            }
            Action::ClearFinished => self.ctrl.clear_finished(),
            Action::Queue | Action::ApplyAll => return false,
        }
        true
    }
}

/// `app.jobs`: open the view, or close it if it is already open.
pub fn toggle(ctx: &Rc<AppContext>) {
    let Some(state) = ctx.ext.get::<JobsViewState>() else {
        return;
    };
    let open = state.open.borrow().clone();
    if let Some(d) = open {
        d.close();
        return;
    }
    let Some(ctrl) = super::controller(ctx) else {
        return;
    };

    let dialog = adw::Dialog::new();
    dialog.set_title(tr("Jobs"));
    dialog.set_content_width(660);
    dialog.set_content_height(440);
    let list = gtk::ListBox::new();
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_widget_name("ofm-jobs-list");
    list.update_property(&[gtk::accessible::Property::Label(tr("Background jobs"))]);
    let empty = ui::caption(tr("No background jobs. Copy, move and delete run here."));
    empty.set_margin_top(24);
    empty.set_halign(gtk::Align::Center);
    let scroller = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .build();
    let pause = ui::button(tr("_Pause"));
    let cancel_job = ui::button(tr("Cancel _job"));
    cancel_job.add_css_class("destructive-action");
    let clear = ui::button(tr("Clear _finished"));
    let close = ui::button(tr("Close"));
    let column = modal::padded_column(8);
    column.append(&empty);
    column.append(&scroller);
    column.append(&ui::button_row(&[&clear, &pause, &cancel_job, &close]));
    dialog.set_child(Some(&modal::with_header(&column)));
    dialog.set_focus(Some(&list));

    let view = Rc::new(View {
        ctrl: Rc::clone(&ctrl),
        list: list.clone(),
        empty,
        rows: RefCell::new(Vec::new()),
        pause: pause.clone(),
    });
    for job in ctrl.jobs() {
        view.add(&job);
    }
    view.empty.set_visible(view.rows.borrow().is_empty());
    view.reselect();
    view.update_buttons();

    // Live updates while the dialog is open.
    let weak_view: Weak<View> = Rc::downgrade(&view);
    let sig = ctrl.changed.connect(move |ev| {
        let Some(view) = weak_view.upgrade() else {
            return;
        };
        match ev {
            JobsEvent::Added(id) => {
                if let Some(job) = view.ctrl.job(*id) {
                    view.add(&job);
                }
                view.update_buttons();
            }
            JobsEvent::Updated(id) | JobsEvent::Finished(id) => view.sync(*id),
            JobsEvent::Cleared => view.drop_cleared(),
        }
    });
    {
        let view = Rc::downgrade(&view);
        list.connect_row_selected(move |_, _| {
            if let Some(v) = view.upgrade() {
                v.update_buttons();
            }
        });
    }
    for (button, action) in [
        (&pause, Action::PauseResume),
        (&cancel_job, Action::Cancel),
        (&clear, Action::ClearFinished),
    ] {
        let view = Rc::downgrade(&view);
        button.connect_clicked(move |_| {
            if let Some(v) = view.upgrade() {
                v.act(action);
            }
        });
    }
    let weak = dialog.downgrade();
    close.connect_clicked(move |_| {
        if let Some(d) = weak.upgrade() {
            d.close();
        }
    });

    let ctx_ok = Rc::clone(ctx);
    let shown = modal::present(ctx, &dialog, move || {
        ui::activate_focused_button(&ctx_ok);
        false // Enter never closes the jobs view; Esc / Ctrl+J do
    });
    if !shown {
        ctrl.changed.disconnect(sig);
        return;
    }
    *state.open.borrow_mut() = Some(dialog.clone());
    {
        let view = Rc::clone(&view);
        actions::bind(ctx, &dialog, move |a| view.act(a));
    }
    let weak_state = Rc::downgrade(&state);
    let ctrl2 = Rc::clone(&ctrl);
    dialog.connect_closed(move |_| {
        ctrl2.changed.disconnect(sig);
        if let Some(s) = weak_state.upgrade() {
            s.open.borrow_mut().take();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(n: u64, st: JobState) -> (JobId, JobState) {
        (JobId(n), st)
    }

    #[test]
    fn finished_selection_falls_back_to_the_running_job() {
        // Igor's G2 run: three finished copies on top, the big one running at the bottom, the
        // oldest row selected by default. Cancel / Del / Pause must hit the running job.
        let jobs = [
            j(1, JobState::Finished),
            j(2, JobState::Finished),
            j(3, JobState::Finished),
            j(4, JobState::Running),
        ];
        assert_eq!(pick_target(Some(JobId(1)), &jobs), Some(JobId(4)));
        assert_eq!(pick_target(None, &jobs), Some(JobId(4)));
    }

    #[test]
    fn an_unfinished_selection_is_kept() {
        let jobs = [j(1, JobState::Running), j(2, JobState::Queued)];
        assert_eq!(pick_target(Some(JobId(2)), &jobs), Some(JobId(2)));
    }

    #[test]
    fn running_beats_paused_beats_queued_and_nothing_to_do_gives_none() {
        let jobs = [
            j(1, JobState::Queued),
            j(2, JobState::Paused),
            j(3, JobState::Running),
        ];
        assert_eq!(pick_target(None, &jobs), Some(JobId(3)));
        let jobs = [j(1, JobState::Queued), j(2, JobState::Paused)];
        assert_eq!(pick_target(None, &jobs), Some(JobId(2)));
        assert_eq!(
            pick_target(Some(JobId(1)), &[j(1, JobState::Finished)]),
            None
        );
        assert_eq!(pick_target(None, &[]), None);
    }
}

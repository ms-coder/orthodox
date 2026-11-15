//! T3.3: the job side of the UI with synthetic `JobEvent`s (no engine, no worker threads): the
//! conflict / error / trash question dialog, the jobs view, the progress row, the quit prompt,
//! panel refresh + cursor after a job, selection clearing and `file.calc_size`. The real engine
//! flows are covered once T3.1 is merged. Needs a display; `OFM_SHOT_DIR=dir` writes screenshots.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::path::PathBuf;
use std::time::Duration;

use fileops_support::*;
use gtk::prelude::*;
use ofm_app::jobs::indicator::Indicator;
use ofm_app::jobs::view::JobsViewState;
use ofm_app::jobs::{self, Control, JobsController};
use ofm_core::Wait;
use ofm_core::{Responder, cmd};
use ofm_jobs::{
    Answer, ConflictAnswer, ConflictKind, ErrorAction, ItemInfo, ItemKind, JobEvent, JobId,
    JobSpec, JobState, Operation, Outcome, Progress, Question, Report, RunMode, SizeTotals,
};
use ofm_testkit::tree::{FileSpec, TreeSpec};

fn queued(fx: &Fixture, id: u64, title: &str, mode: RunMode) {
    let ctrl = jobs::controller(&fx.ctx).unwrap();
    ctrl.handle_event(
        &fx.ctx,
        JobEvent::Queued {
            id: JobId(id),
            title: title.to_owned(),
            mode,
        },
    );
}

fn started(fx: &Fixture, id: u64) {
    jobs::controller(&fx.ctx)
        .unwrap()
        .handle_event(&fx.ctx, JobEvent::Started { id: JobId(id) });
}

fn finished(fx: &Fixture, id: u64, outcome: Outcome) {
    jobs::controller(&fx.ctx).unwrap().handle_event(
        &fx.ctx,
        JobEvent::Finished {
            id: JobId(id),
            outcome,
        },
    );
}

fn item(path: &str, size: u64, mtime_s: i128) -> ItemInfo {
    ItemInfo {
        path: path.into(),
        kind: ItemKind::File,
        size,
        mtime_ns: mtime_s * 1_000_000_000,
    }
}

/// Send a question and return the worker's side of the answer channel.
fn ask(fx: &Fixture, id: u64, q: Question) -> ofm_core::Pending<Answer> {
    let (reply, pending) = Responder::channel();
    jobs::controller(&fx.ctx).unwrap().handle_event(
        &fx.ctx,
        JobEvent::Ask {
            id: JobId(id),
            question: q,
            reply,
        },
    );
    pending
}

fn answered(p: &ofm_core::Pending<Answer>) -> Option<Answer> {
    match p.wait_timeout(Duration::from_millis(50)) {
        Wait::Answered(a) => Some(a),
        _ => None,
    }
}

fn dropped(p: &ofm_core::Pending<Answer>) -> bool {
    matches!(p.wait_timeout(Duration::from_millis(50)), Wait::Dropped)
}

fn conflict(kind: ConflictKind) -> Question {
    Question::Conflict {
        source: item("/src/report.pdf", 2_500_000, 1_700_000_000),
        existing: item("/dst/report.pdf", 1_200_000, 1_650_000_000),
        kind,
    }
}

#[test]
fn jobs_scenario() {
    if !init() {
        return;
    }
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"aaa"))
        .file("b.txt", FileSpec::content(b"bbb"))
        .dir(
            "sub",
            TreeSpec::new().file("i.txt", FileSpec::content(b"i")),
        );
    let right = TreeSpec::new().file("r.txt", FileSpec::content(b"r"));
    let fx = Fixture::new(
        "io.github.mscoder.OfmFileopsJobs",
        left,
        right,
        theme_from_env().as_deref(),
    );
    let ctx = &fx.ctx;
    let ctrl = jobs::controller(ctx).unwrap();
    let cap = ctrl.capture();
    let lp = fx.left.path().to_owned();

    // ---- progress row ---------------------------------------------------------------------------
    let ind = ctx.ext.get::<Indicator>().unwrap();
    assert!(!ind.revealer.reveals_child(), "hidden without jobs");
    queued(&fx, 1, "Copy 3 items to /mnt/backup", RunMode::Queued);
    started(&fx, 1);
    ctrl.handle_event(
        ctx,
        JobEvent::Progress {
            id: JobId(1),
            progress: Progress {
                bytes_done: 45 << 20,
                bytes_total: 120 << 20,
                files_done: 1,
                files_total: 3,
                bytes_per_sec: 12 << 20,
                eta_secs: Some(6),
                current: Some("/home/me/Videos/holiday-2026.mkv".into()),
                ..Progress::default()
            },
        },
    );
    assert!(ind.revealer.reveals_child());
    assert_eq!(ind.title.text(), "Copy 3 items to /mnt/backup");
    assert_eq!(ind.text.text(), "38%  12 MiB/s  ETA 0:06");
    assert!((ind.bar.fraction() - 0.375).abs() < 1e-9);
    fx.shot("progress-row");

    // ---- conflict dialog ------------------------------------------------------------------------
    let p = ask(&fx, 1, conflict(ConflictKind::Replaceable));
    assert_eq!(fx.modal_depth(), 1);
    assert!(
        ctrl.job(JobId(1)).unwrap().asking,
        "job is marked as waiting"
    );
    assert!(fx.button("ofm-conflict-overwrite").is_sensitive());
    fx.shot("conflict-dialog");
    fx.run(cmd::DIALOG_APPLY_ALL); // Ctrl+A
    fx.run(cmd::DIALOG_OK); // Enter: the safe default
    assert_eq!(
        answered(&p),
        Some(Answer::Conflict {
            action: ConflictAnswer::Skip,
            apply_to_all: true
        })
    );
    assert_eq!(fx.modal_depth(), 0);
    assert!(!ctrl.job(JobId(1)).unwrap().asking);

    // A file/folder conflict: no overwrite choices (ConflictKind::allows), keep-both works.
    let p = ask(&fx, 1, conflict(ConflictKind::TypeMismatch));
    for forbidden in [
        "ofm-conflict-overwrite",
        "ofm-conflict-overwriteifnewer",
        "ofm-conflict-overwriteifsizediffers",
    ] {
        assert!(!fx.button(forbidden).is_sensitive(), "{forbidden}");
    }
    assert!(fx.button("ofm-conflict-renamenew").is_sensitive());
    assert!(fx.button("ofm-conflict-skip").is_sensitive());
    fx.shot("conflict-dialog-mismatch");
    fx.button("ofm-conflict-renameexisting").emit_clicked();
    assert_eq!(
        answered(&p),
        Some(Answer::Conflict {
            action: ConflictAnswer::RenameExisting,
            apply_to_all: false
        })
    );
    // Same file (copy into its own folder): Enter picks "rename new".
    let p = ask(&fx, 1, conflict(ConflictKind::SameFile));
    assert!(!fx.button("ofm-conflict-overwrite").is_sensitive());
    fx.run(cmd::DIALOG_OK);
    assert_eq!(
        answered(&p),
        Some(Answer::Conflict {
            action: ConflictAnswer::RenameNew,
            apply_to_all: false
        })
    );
    // Esc = no answer = the job aborts.
    let p = ask(&fx, 1, conflict(ConflictKind::Replaceable));
    fx.run(cmd::DIALOG_CANCEL);
    assert!(dropped(&p), "closing the dialog drops the responder");
    assert_eq!(fx.modal_depth(), 0);

    // ---- error dialog ---------------------------------------------------------------------------
    let err = |os| Question::Error {
        path: "/dst/big.iso".into(),
        operation: Operation::Copy,
        message: "No space left on device (os error 28)".into(),
        os_error: os,
    };
    let p = ask(&fx, 1, err(Some(28)));
    fx.shot("error-dialog");
    fx.run(cmd::DIALOG_OK); // Enter = Retry
    assert_eq!(
        answered(&p),
        Some(Answer::Error {
            action: ErrorAction::Retry,
            apply_to_all: false
        })
    );
    let p = ask(&fx, 1, err(None));
    fx.run(cmd::DIALOG_APPLY_ALL);
    fx.button("ofm-error-skip").emit_clicked();
    assert_eq!(
        answered(&p),
        Some(Answer::Error {
            action: ErrorAction::Skip,
            apply_to_all: true
        })
    );
    // "Apply to all" is never remembered for a retry (it would loop forever).
    let p = ask(&fx, 1, err(None));
    fx.run(cmd::DIALOG_APPLY_ALL);
    fx.button("ofm-error-retry").emit_clicked();
    assert_eq!(
        answered(&p),
        Some(Answer::Error {
            action: ErrorAction::Retry,
            apply_to_all: false
        })
    );

    // ---- trash unavailable ----------------------------------------------------------------------
    let trash = || Question::TrashUnavailable {
        path: "/mnt/nas/old/photos".into(),
    };
    let p = ask(&fx, 1, trash());
    fx.shot("trash-dialog");
    fx.run(cmd::DIALOG_OK); // Enter = skip (never deletes by accident)
    assert_eq!(
        answered(&p),
        Some(Answer::Confirm {
            permanent: false,
            apply_to_all: false
        })
    );
    let p = ask(&fx, 1, trash());
    fx.run(cmd::DIALOG_APPLY_ALL);
    fx.button("ofm-trash-delete").emit_clicked();
    assert_eq!(
        answered(&p),
        Some(Answer::Confirm {
            permanent: true,
            apply_to_all: true
        })
    );

    // ---- several questions: one dialog at a time -------------------------------------------------
    let p1 = ask(&fx, 1, conflict(ConflictKind::Replaceable));
    let p2 = ask(&fx, 2, err(Some(13)));
    assert_eq!(fx.modal_depth(), 1, "the second waits for the first");
    fx.button("ofm-conflict-overwrite").emit_clicked();
    assert!(matches!(
        answered(&p1),
        Some(Answer::Conflict {
            action: ConflictAnswer::Overwrite,
            ..
        })
    ));
    assert_eq!(fx.modal_depth(), 1, "now the second one is shown");
    fx.run(cmd::DIALOG_OK);
    assert!(matches!(answered(&p2), Some(Answer::Error { .. })));
    assert_eq!(fx.modal_depth(), 0);
    // A job that ends while its question is open (cancelled elsewhere) takes the dialog with it.
    queued(&fx, 3, "Move x", RunMode::Parallel);
    let p3 = ask(&fx, 3, conflict(ConflictKind::Replaceable));
    assert_eq!(fx.modal_depth(), 1);
    finished(&fx, 3, Outcome::Cancelled(Report::default()));
    assert_eq!(fx.modal_depth(), 0);
    assert!(dropped(&p3));

    // ---- jobs view -------------------------------------------------------------------------------
    finished(
        &fx,
        1,
        Outcome::Ok(Report {
            files_done: 3,
            bytes_done: 120 << 20,
            ..Report::default()
        }),
    );
    queued(&fx, 10, "Copy 'big.iso' to /mnt/usb", RunMode::Queued);
    started(&fx, 10);
    ctrl.handle_event(
        ctx,
        JobEvent::Progress {
            id: JobId(10),
            progress: Progress {
                bytes_done: 700 << 20,
                bytes_total: 2000 << 20,
                files_done: 0,
                files_total: 1,
                bytes_per_sec: 85 << 20,
                eta_secs: Some(15),
                current: Some("/home/me/Downloads/big.iso".into()),
                ..Progress::default()
            },
        },
    );
    queued(&fx, 11, "Delete 12 items", RunMode::Queued);
    fx.run(cmd::APP_JOBS);
    let view = ctx.ext.get::<JobsViewState>().unwrap();
    assert!(view.is_open() && fx.modal_depth() == 1);
    let list: gtk::ListBox = fx.find("ofm-jobs-list").downcast().unwrap();
    // Jobs 1 (done), 2? (never queued), 3 (cancelled), 10, 11.
    let before = ctrl.jobs().len();
    assert_eq!(list_len(&list), before);
    fx.shot("jobs-view");
    fx.run(cmd::JOBS_CLEAR_FINISHED); // Ctrl+L
    assert_eq!(ctrl.jobs().len(), 2);
    assert_eq!(list_len(&list), 2, "finished rows disappear");
    list.select_row(list.row_at_index(0).as_ref());
    fx.run(cmd::JOBS_PAUSE_RESUME); // Space
    assert_eq!(
        cap.controls.borrow().last(),
        Some(&Control::Pause(JobId(10)))
    );
    ctrl.handle_event(
        ctx,
        JobEvent::StateChanged {
            id: JobId(10),
            state: JobState::Paused,
        },
    );
    fx.shot("jobs-view-paused");
    fx.run(cmd::JOBS_PAUSE_RESUME);
    assert_eq!(
        cap.controls.borrow().last(),
        Some(&Control::Resume(JobId(10)))
    );
    list.select_row(list.row_at_index(1).as_ref());
    fx.run(cmd::JOBS_CANCEL); // Del
    assert_eq!(
        cap.controls.borrow().last(),
        Some(&Control::Cancel(JobId(11)))
    );
    finished(&fx, 11, Outcome::Cancelled(Report::default()));
    fx.run(cmd::APP_JOBS); // Ctrl+J closes it again
    pump_until("jobs view closed", || !view.is_open());
    assert_eq!(fx.modal_depth(), 0);
    ctrl.clear_finished();

    // ---- Space still toggles a check box in an ordinary dialog ------------------------------------
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_COPY);
    let parallel = fx.check("ofm-transfer-parallel");
    pump_for(400); // the dialog must be mapped before it can take the focus
    assert!(parallel.grab_focus());
    fx.run(cmd::JOBS_PAUSE_RESUME);
    assert!(parallel.is_active(), "Space toggled the focused check box");
    fx.run(cmd::DIALOG_CANCEL);

    // ---- after a job: cursor on the created item, selection cleared -------------------------------
    std::fs::write(lp.join("zzz-new.txt"), b"new").unwrap();
    let before = cap.submitted.borrow().len();
    ctx.active_controller().mutate(|m| {
        m.set_cursor_name("a.txt".as_ref());
        let i = m.cursor().unwrap();
        m.toggle_select(i);
    });
    assert_eq!(ctx.active_controller().model().selected_count(), 1);
    fx.run(cmd::FILE_COPY);
    fx.run(cmd::DIALOG_OK);
    let id = cap.submitted.borrow()[before].0;
    started(&fx, id.0);
    finished(
        &fx,
        id.0,
        Outcome::Ok(Report {
            created: vec![lp.join("zzz-new.txt")],
            files_done: 1,
            ..Report::default()
        }),
    );
    assert_eq!(
        ctx.active_controller().model().selected_count(),
        0,
        "selection is cleared after a successful job"
    );
    pump_until("cursor on the new item", || {
        ctx.host(0)
            .active()
            .model()
            .cursor_entry()
            .is_some_and(|e| e.name == "zzz-new.txt")
    });
    // A nested result focuses the top-level folder that contains it.
    std::fs::create_dir_all(lp.join("deep/er")).unwrap();
    queued(&fx, 20, "Create directory 'er'", RunMode::Parallel);
    finished(
        &fx,
        20,
        Outcome::Ok(Report {
            created: vec![lp.join("deep/er")],
            ..Report::default()
        }),
    );
    pump_until("cursor on deep", || {
        ctx.host(0)
            .active()
            .model()
            .cursor_entry()
            .is_some_and(|e| e.name == "deep")
    });
    // A failed or cancelled job keeps the selection.
    ctx.active_controller().mutate(|m| {
        m.set_cursor_name("b.txt".as_ref());
        let i = m.cursor().unwrap();
        m.toggle_select(i);
    });
    fx.run(cmd::FILE_COPY);
    fx.run(cmd::DIALOG_OK);
    let id = cap.submitted.borrow().last().unwrap().0;
    finished(
        &fx,
        id.0,
        Outcome::Failed {
            error: "/dst/b.txt: Permission denied (os error 13)".into(),
            path: Some(PathBuf::from("/dst/b.txt")),
            report: Report::default(),
        },
    );
    assert_eq!(ctx.active_controller().model().selected_count(), 1);
    assert_eq!(fx.modal_depth(), 1, "the failure is reported");
    fx.shot("failure-message");
    fx.run(cmd::DIALOG_OK);
    assert_eq!(fx.modal_depth(), 0);

    // ---- calculate size ----------------------------------------------------------------------------
    ctx.active_controller().mutate(|m| m.select_all(false));
    fx.cursor_to(0, "sub");
    let before = cap.submitted.borrow().len();
    fx.run(cmd::FILE_CALC_SIZE);
    let (id, spec, mode) = cap.submitted.borrow()[before].clone();
    assert_eq!(mode, RunMode::Parallel);
    assert!(matches!(&spec, JobSpec::CalcSize { paths } if paths == &vec![lp.join("sub")]));
    finished(
        &fx,
        id.0,
        Outcome::Size(SizeTotals {
            bytes: 123_456,
            disk_bytes: 126_976,
            files: 4,
            dirs: 1,
        }),
    );
    let size = {
        let c = ctx.host(0).active();
        let m = c.model();
        (0..m.visible_len())
            .filter_map(|i| m.visible(i))
            .find(|e| e.name == "sub")
            .map(|e| e.size)
    };
    assert_eq!(size, Some(123_456), "the panel shows the calculated size");

    // ---- quit with running jobs ---------------------------------------------------------------------
    for j in ctrl.jobs() {
        if j.state != JobState::Finished {
            finished(&fx, j.id.0, Outcome::Cancelled(Report::default()));
        }
    }
    let window = gtk::Window::new();
    assert!(
        jobs::allow_close(ctx, &window),
        "nothing running: quit at once"
    );
    queued(&fx, 30, "Copy 'x' to /y", RunMode::Queued);
    started(&fx, 30);
    assert!(!jobs::allow_close(ctx, &window), "a running job: ask first");
    assert_eq!(fx.modal_depth(), 1);
    fx.shot("quit-dialog");
    fx.run(cmd::DIALOG_OK);
    assert!(cap.controls.borrow().contains(&Control::CancelAll));
    assert!(ctrl.quit_confirmed());
    assert!(
        jobs::allow_close(ctx, &window),
        "confirmed: the next close goes through"
    );
    let _: &JobsController = &ctrl;
}

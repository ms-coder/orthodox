//! The forward flow of a batch rename: selection -> temp file -> editor -> plan -> preview ->
//! job -> (rollback offer). See the module docs of [`super`]. The main thread only builds
//! dialogs and starts workers; reading the edited file, listing the directory and writing logs
//! all happen on the I/O pool.

use std::cell::Cell;
use std::collections::HashSet;
use std::ffi::OsString;

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use ofm_core::batch_rename::{BatchError, Plan, parse_edited, plan};
use ofm_jobs::{
    FileId, Fs, JobSpec, Outcome, RealFs, RenameBatchSpec, RollbackSpec, RunMode, load_log,
    snapshot_identities,
};
use ofm_vfs::pool::IoPool;

use super::{BatchState, Phase, glue, logs, state, workspace};
use crate::context::AppContext;
use crate::desktop::editor::{self, Choice, EditorEnv};
use crate::dialogs::{batchrename_preview, batchrename_prompt};
use crate::i18n::tr;
use crate::jobs::targets::Targets;
use crate::jobs::{self, SubmitOpts, ui};
use crate::panel::PanelController;
use crate::terminal;
use crate::wiring;

/// One batch rename in progress. Cleans up (removes the temp directory, frees the module's
/// "busy" flag) in [`Session::end`], also when dropped.
pub(crate) struct Session {
    ctx: Weak<AppContext>,
    state: Rc<BatchState>,
    pool: IoPool,
    dir: PathBuf,
    panel: Weak<PanelController>,
    /// The names in the editor buffer, in line order.
    editor_names: Vec<OsString>,
    /// Names left out of the buffer.
    unsupported: Vec<OsString>,
    /// Names the panel showed (the planner also checks the fresh listing).
    panel_names: Vec<OsString>,
    ws: workspace::Workspace,
    unique: String,
    ended: Cell<bool>,
}

impl Session {
    fn ctx(&self) -> Option<Rc<AppContext>> {
        self.ctx.upgrade()
    }

    /// Over: remove the temp directory (on the pool) and allow the next batch.
    fn end(&self) {
        if self.ended.replace(true) {
            return;
        }
        self.state.set_active(false);
        self.state.set_phase(Phase::Idle);
        let ws = self.ws.clone();
        self.pool.spawn(move || workspace::remove(&ws));
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.end();
    }
}

/// What the worker found after the editor closed.
#[derive(Debug)]
struct Planned {
    plan: Plan,
    identities: Vec<(OsString, FileId)>,
    /// The directory of the batch whose undo information this one replaces.
    replaces: Option<String>,
}

/// Why the edited text cannot be applied.
#[derive(Debug)]
enum Problem {
    Batch(BatchError),
    Io(String),
    NoChanges,
}

impl Problem {
    fn text(&self) -> String {
        match self {
            Problem::Batch(e) => glue::problem_text(e),
            Problem::Io(e) => format!("{} {e}", tr("The edited file cannot be read:")),
            Problem::NoChanges => tr("No name was changed.").to_owned(),
        }
    }
}

/// Show `text` in a plain message dialog and remember it (tests read it back).
pub(crate) fn notice(ctx: &Rc<AppContext>, text: &str) {
    if let Some(s) = state(ctx) {
        s.set_notice(text);
    }
    ui::message(ctx, tr("Batch rename"), text);
}

/// `file.batch_rename`: start a batch rename of the selection of the active panel.
pub fn start(ctx: &Rc<AppContext>) {
    let Some(st) = state(ctx) else {
        return;
    };
    if st.active() {
        notice(ctx, tr("A batch rename is already in progress."));
        return;
    }
    let panel = ctx.active_controller();
    let Some(dir) = panel.location().local_path().map(Path::to_path_buf) else {
        notice(ctx, tr("Batch rename works in local folders only."));
        return;
    };
    let Some(targets) = Targets::from_active(ctx) else {
        notice(
            ctx,
            tr("Select the files to rename, or put the cursor on one (not on '..')."),
        );
        return;
    };
    let prepared = match glue::prepare(&targets.names()) {
        Ok(p) => p,
        Err(refused) if refused.is_empty() => {
            notice(ctx, tr("There is nothing to rename."));
            return;
        }
        Err(refused) => {
            notice(
                ctx,
                &format!(
                    "{} {}",
                    tr(
                        "These names cannot be edited as text (not valid UTF-8, or they contain a line break), so they are left out:"
                    ),
                    glue::name_list(&refused, 10)
                ),
            );
            return;
        }
    };
    let panel_names: Vec<OsString> = {
        let m = panel.model();
        (0..m.visible_len())
            .filter_map(|i| m.visible(i))
            .map(|e| e.name.clone())
            .collect()
    };
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let unique = glue::unique_id(std::process::id(), nanos, st.next_seq());
    st.set_active(true);
    st.set_phase(Phase::Editing);

    let (text, worker_unique) = (prepared.text.clone(), unique.clone());
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let made = workspace::create(&workspace::base_dir(), &worker_unique, &text);
        let _ = tx.send_blocking(made);
    });
    let (weak_ctx, st_for_handler) = (Rc::downgrade(ctx), Rc::clone(&st));
    let panel_weak = Rc::downgrade(&panel);
    wiring::attach(
        rx,
        move |made| {
            let Some(ctx) = weak_ctx.upgrade() else {
                return ControlFlow::Break(());
            };
            match made {
                Ok(ws) => {
                    let session = Rc::new(Session {
                        ctx: Rc::downgrade(&ctx),
                        state: Rc::clone(&st_for_handler),
                        pool: ctx.services.pool.clone(),
                        dir: dir.clone(),
                        panel: panel_weak.clone(),
                        editor_names: prepared.editor_names.clone(),
                        unsupported: prepared.unsupported.clone(),
                        panel_names: panel_names.clone(),
                        ws,
                        unique: unique.clone(),
                        ended: Cell::new(false),
                    });
                    open_editor(&session);
                }
                Err(e) => {
                    st_for_handler.set_active(false);
                    st_for_handler.set_phase(Phase::Idle);
                    notice(
                        &ctx,
                        &format!("{} {e}", tr("Cannot prepare the editor file:")),
                    );
                }
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

/// Open (or re-open) the editor on the temp file.
fn open_editor(sess: &Rc<Session>) {
    let Some(ctx) = sess.ctx() else {
        return;
    };
    sess.state.set_phase(Phase::Editing);
    let name = std::ffi::OsStr::new(workspace::FILE_NAME);
    let choice = {
        let cfg = ctx.config.borrow();
        editor::choose(&cfg.editor, name, &EditorEnv::from_process())
    };
    if let Choice::Command {
        words,
        terminal: true,
        ..
    } = &choice
        && let Some(handle) = terminal::handle(&ctx)
    {
        let argv = editor::argv(words, &sess.ws.file);
        let (for_error, for_exit) = (Rc::clone(sess), Rc::clone(sess));
        let started = handle.run_program_notify(
            &argv,
            Some(sess.dir.as_path()),
            move |msg| {
                if let Some(ctx) = for_error.ctx() {
                    notice(&ctx, &msg);
                }
                for_error.end();
            },
            move |status| {
                if status == 0 {
                    after_edit(&for_exit);
                } else {
                    if let Some(ctx) = for_exit.ctx() {
                        notice(
                            &ctx,
                            tr("The editor exited with an error, so nothing was renamed."),
                        );
                    }
                    for_exit.end();
                }
            },
        );
        if let Err(msg) = started {
            notice(&ctx, &msg);
            sess.end();
        }
        return;
    }
    // A GUI editor starts detached: nothing tells us when it is done, so the user presses Apply.
    editor::open_in_editor(&ctx, &sess.ws.file);
    sess.state.set_phase(Phase::Waiting);
    let s = Rc::clone(sess);
    batchrename_prompt::waiting(&ctx, move |apply| {
        if apply {
            after_edit(&s);
        } else {
            s.end();
        }
    });
}

/// Read, parse and plan the edited file on the pool.
fn after_edit(sess: &Rc<Session>) {
    sess.state.set_phase(Phase::Planning);
    let Some(ctx) = sess.ctx() else {
        return;
    };
    let (file, dir) = (sess.ws.file.clone(), sess.dir.clone());
    let (names, panel_names) = (sess.editor_names.clone(), sess.panel_names.clone());
    let last = glue::last_log(&ctx.paths.state_dir);
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(plan_blocking(&file, &dir, &names, &panel_names, &last));
    });
    let s = Rc::clone(sess);
    wiring::attach(
        rx,
        move |result| {
            on_planned(&s, result);
            ControlFlow::Break(())
        },
        || {},
    );
}

/// The worker half of [`after_edit`]: read the edited file, parse it, plan it against the
/// fresh directory listing (plus what the panel showed) and record the identities of the files.
fn plan_blocking(
    file: &Path,
    dir: &Path,
    names: &[OsString],
    panel_names: &[OsString],
    last_log: &Path,
) -> Result<Planned, Problem> {
    let text = workspace::read_edited(file).map_err(|e| Problem::Io(e.to_string()))?;
    let ops = parse_edited(names, &text).map_err(Problem::Batch)?;
    if ops.is_empty() {
        return Err(Problem::NoChanges);
    }
    let fs = RealFs;
    let listing = fs
        .read_dir(dir)
        .map_err(|e| Problem::Io(format!("{}: {e}", dir.to_string_lossy())))?;
    let existing: HashSet<OsString> = glue::existing_names(listing, panel_names);
    let plan = plan(&ops, &existing).map_err(Problem::Batch)?;
    if plan.steps.is_empty() {
        return Err(Problem::NoChanges);
    }
    let identities =
        snapshot_identities(&fs, dir, &plan).map_err(|e| Problem::Io(e.to_string()))?;
    Ok(Planned {
        plan,
        identities,
        replaces: logs::previous_batch(last_log),
    })
}

fn on_planned(sess: &Rc<Session>, result: Result<Planned, Problem>) {
    let Some(ctx) = sess.ctx() else {
        return;
    };
    match result {
        Err(problem) => {
            let text = problem.text();
            sess.state.set_phase(Phase::Problem(text.clone()));
            let s = Rc::clone(sess);
            batchrename_prompt::problem(&ctx, &text, move |reopen| {
                if reopen {
                    open_editor(&s);
                } else {
                    s.end();
                }
            });
        }
        Ok(planned) => show_preview(sess, &ctx, planned),
    }
}

fn show_preview(sess: &Rc<Session>, ctx: &Rc<AppContext>, planned: Planned) {
    let lines = glue::preview_lines(&planned.plan.preview);
    let mut notes = Vec::new();
    let temps = glue::temp_steps(&planned.plan);
    if temps > 0 {
        notes.push(format!(
            "{} {temps} {}",
            tr("Swapped or chained names need"),
            tr("temporary rename step(s); they are done and undone automatically.")
        ));
    }
    if !sess.unsupported.is_empty() {
        notes.push(format!(
            "{} {}",
            tr("Not renamed (cannot be edited as text):"),
            glue::name_list(&sess.unsupported, 5)
        ));
    }
    if let Some(dir) = &planned.replaces {
        notes.push(format!(
            "{} {dir}.",
            tr("Applying replaces the undo information of the previous batch rename in")
        ));
    }
    let n = lines.len();
    let heading = format!(
        "{n} {} {}",
        if n == 1 {
            tr("item in")
        } else {
            tr("items in")
        },
        sess.dir.to_string_lossy()
    );
    sess.state.set_phase(Phase::Preview(lines.clone()));
    let s = Rc::clone(sess);
    batchrename_preview::present(
        ctx,
        batchrename_preview::PreviewView {
            title: tr("Batch rename"),
            heading,
            lines,
            notes,
            apply_label: tr("_Apply"),
        },
        move |apply| {
            if apply {
                apply_plan(&s, planned);
            } else {
                s.end();
            }
        },
    );
}

/// Submit the job. The session is over after this: the job owns the rest.
fn apply_plan(sess: &Rc<Session>, planned: Planned) {
    let Some(ctx) = sess.ctx() else {
        return;
    };
    let Some(controller) = jobs::controller(&ctx) else {
        sess.end();
        return;
    };
    let pending = glue::pending_log(&ctx.paths.state_dir, &sess.unique);
    let spec = RenameBatchSpec {
        dir: sess.dir.clone(),
        plan: planned.plan,
        undo_log: pending.clone(),
        identities: planned.identities,
    };
    let opts = SubmitOpts {
        origin: sess.panel.upgrade().map(|p| Rc::downgrade(&p)),
        on_finished: Some(Box::new(move |ctx, outcome| {
            batch_finished(ctx, &pending, outcome);
        })),
    };
    controller.submit_with(&ctx, JobSpec::RenameBatch(spec), RunMode::Parallel, opts);
    sess.end();
    sess.state.set_phase(Phase::Applying);
}

/// The batch job is over: keep its log as the undo log, or offer to roll back a partial batch.
fn batch_finished(ctx: &Rc<AppContext>, pending: &Path, outcome: &Outcome) {
    let last = glue::last_log(&ctx.paths.state_dir);
    let stopped = match outcome {
        Outcome::Ok(_) => {
            keep_log(ctx, pending, &last);
            return;
        }
        Outcome::Cancelled(r) => Some((
            tr("The batch rename was cancelled.").to_owned(),
            r.files_done,
        )),
        Outcome::Failed { error, report, .. } => Some((error.clone(), report.files_done)),
        Outcome::Size(_) => None,
    };
    let Some((why, done)) = stopped else {
        return;
    };
    if done == 0 {
        discard_log(ctx, pending);
        return;
    }
    let (ctx_for_answer, pending_for_answer) = (Rc::clone(ctx), pending.to_path_buf());
    batchrename_prompt::rollback_offer(ctx, &why, done, move |roll_back| {
        if roll_back {
            rollback_from(&ctx_for_answer, &pending_for_answer);
        } else {
            keep_log(&ctx_for_answer, &pending_for_answer, &last);
        }
    });
}

/// `pending` becomes the undo log (pool).
fn keep_log(ctx: &Rc<AppContext>, pending: &Path, last: &Path) {
    let (pending, last) = (pending.to_path_buf(), last.to_path_buf());
    ctx.services.pool.spawn(move || {
        if let Err(e) = logs::promote(&pending, &last) {
            tracing::warn!(error = %e, "batch rename: cannot keep the undo log");
        }
    });
}

fn discard_log(ctx: &Rc<AppContext>, pending: &Path) {
    let pending = pending.to_path_buf();
    ctx.services.pool.spawn(move || logs::discard(&pending));
}

/// Load the log at `log_path` on the pool and submit a rollback job for it.
fn rollback_from(ctx: &Rc<AppContext>, log_path: &Path) {
    let path = log_path.to_path_buf();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(load_log(&path).map(|log| (log, path)));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |loaded| {
            let Some(ctx) = weak.upgrade() else {
                return ControlFlow::Break(());
            };
            match loaded {
                Ok((log, path)) => submit_rollback(&ctx, log, path),
                Err(e) => notice(&ctx, &format!("{} {e}", tr("The undo log cannot be read:"))),
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

/// Submit `JobSpec::RenameRollback` for `log` (read from `log_path`). If the rollback leaves a log
/// behind (it could not undo everything) that log becomes the undo log, so the undo command can
/// retry.
pub(crate) fn submit_rollback(
    ctx: &Rc<AppContext>,
    log: ofm_core::batch_rename::UndoLog,
    log_path: PathBuf,
) {
    let Some(controller) = jobs::controller(ctx) else {
        return;
    };
    let last = glue::last_log(&ctx.paths.state_dir);
    let leftover = log_path.clone();
    let opts = SubmitOpts {
        origin: None,
        on_finished: Some(Box::new(move |ctx, _outcome| {
            if leftover != last {
                keep_log(ctx, &leftover, &last);
            }
        })),
    };
    controller.submit_with(
        ctx,
        JobSpec::RenameRollback(RollbackSpec { log, log_path }),
        RunMode::Parallel,
        opts,
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    fn setup(tag: &str, files: &[&str], edited: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("ofm-flow-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        for f in files {
            std::fs::write(dir.join(f), f).unwrap();
        }
        let file = base.join("names.txt");
        std::fs::write(&file, edited).unwrap();
        (base, dir, file)
    }

    #[test]
    fn plans_a_swap_with_temp_steps_hidden_from_the_preview() {
        let (base, dir, file) = setup("swap", &["a", "b", "c"], "b\na\nc\n");
        let names = [os("a"), os("b"), os("c")];
        let p = plan_blocking(&file, &dir, &names, &names, &base.join("last.toml")).unwrap();
        assert_eq!(p.plan.preview.len(), 2);
        assert!(glue::temp_steps(&p.plan) > 0);
        assert_eq!(
            p.identities.len(),
            p.plan
                .steps
                .iter()
                .filter(|s| s.kind != ofm_core::batch_rename::StepKind::FromTemp)
                .count()
        );
        assert_eq!(p.replaces, None);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn collision_with_an_unselected_file_is_an_error() {
        // `z` exists but is not part of the selection (nor of the panel snapshot).
        let (base, dir, file) = setup("coll", &["a", "b", "z"], "z\nb\n");
        let names = [os("a"), os("b")];
        let e = plan_blocking(&file, &dir, &names, &names, &base.join("last.toml")).unwrap_err();
        assert!(
            matches!(e, Problem::Batch(BatchError::Collision { .. })),
            "{e:?}"
        );
        assert!(e.text().contains("'z'"));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn unchanged_text_is_no_changes_and_a_short_file_is_a_line_count_problem() {
        let (base, dir, file) = setup("nochange", &["a", "b"], "a\nb\n");
        let names = [os("a"), os("b")];
        let last = base.join("last.toml");
        assert!(matches!(
            plan_blocking(&file, &dir, &names, &names, &last),
            Err(Problem::NoChanges)
        ));
        std::fs::write(&file, "a\n").unwrap();
        let e = plan_blocking(&file, &dir, &names, &names, &last).unwrap_err();
        assert!(matches!(e, Problem::Batch(BatchError::LineCount { .. })));
        std::fs::write(&file, "a\nb/c\n").unwrap();
        let e = plan_blocking(&file, &dir, &names, &names, &last).unwrap_err();
        assert!(matches!(
            e,
            Problem::Batch(BatchError::InvalidTarget { .. })
        ));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn duplicate_targets_and_unreadable_files_are_reported() {
        let (base, dir, file) = setup("dup", &["a", "b"], "x\nx\n");
        let names = [os("a"), os("b")];
        let last = base.join("last.toml");
        let e = plan_blocking(&file, &dir, &names, &names, &last).unwrap_err();
        assert!(matches!(e, Problem::Batch(BatchError::DuplicateTarget(_))));
        let e = plan_blocking(&base.join("missing.txt"), &dir, &names, &names, &last).unwrap_err();
        assert!(matches!(e, Problem::Io(_)));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn replacing_a_previous_undo_log_is_announced() {
        let (base, dir, file) = setup("replace", &["a"], "b\n");
        let names = [os("a")];
        let last = base.join("last.toml");
        // A previous batch's log exists.
        let ops = [ofm_core::batch_rename::RenameOp {
            from: os("q"),
            to: os("r"),
        }];
        let existing: HashSet<OsString> = [os("q")].into_iter().collect();
        let prev = ofm_core::batch_rename::UndoLog::new(
            PathBuf::from("/other/dir"),
            &plan(&ops, &existing).unwrap(),
        );
        ofm_core::batch_rename::save_undo_log(&prev, &last).unwrap();
        let p = plan_blocking(&file, &dir, &names, &names, &last).unwrap();
        assert_eq!(p.replaces.as_deref(), Some("/other/dir"));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn io_error_text_is_not_empty() {
        assert!(
            Problem::Io(std::io::Error::other("boom").to_string())
                .text()
                .contains("boom")
        );
    }
}

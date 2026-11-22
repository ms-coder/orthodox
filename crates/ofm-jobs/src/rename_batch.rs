//! Batch rename execution (F20, T8.3b): runs the ordered steps of a `ofm_core::batch_rename::Plan`
//! and its inverse. Data safety first (CLAUDE.md invariant 3, ADR-0004):
//!
//! * every step is `rename_noreplace` (with the shared NFS/FUSE fallback of `copy::noreplace`):
//!   a step whose target exists FAILS and the job stops; nothing is ever overwritten;
//! * before the first step the whole plan is simulated against the directory listing (`from`
//!   exists, `to` is free, in plan order), so a stale plan fails before anything is touched;
//! * before every step the source is `lstat`ed (never followed) and, when its identity
//!   (`dev`, `ino`) was recorded at plan time ([`RenameBatchSpec::identities`]) or by an earlier
//!   step of this job (temp names), it must still be the same inode;
//! * the undo log (`UndoLog`, saved with the planner's atomic 0600 writer) is WRITTEN AHEAD: before
//!   a step runs, the log already lists it (in chunks of [`chunk_for`] steps, so big batches do not
//!   rewrite the whole file per step: one step for up to 100 steps, then `total / 100`). The log
//!   therefore always contains every step that was done, including the temp names that exist, even
//!   after a hard kill; at most one chunk of trailing steps may be listed without having run. On
//!   error or cancel the job rewrites the log to exactly the completed steps. If a log write fails
//!   the job stops before the step it could not record. Temp names carry the stable
//!   `ofm_core::batch_rename::TEMP_PREFIX`;
//! * cancel is honoured between steps only (a step is one atomic syscall);
//! * on error or cancel the job stops where it is. It never rolls back by itself and never asks:
//!   the caller offers "roll back" by submitting [`RollbackSpec`] built from the log
//!   ([`load_log`]); the outcome's `Report::files_done` is the number of completed steps;
//! * [`RollbackSpec`] (used for "undo last batch" and "roll back partial batch") replays the log
//!   backwards with the same no-replace rule. It first finds how much of the log really happened:
//!   trailing steps that did not run (`from` present, `to` free; only the write-ahead tail can be
//!   in that state) are skipped, the rest must pass `verify_undo` against the directory or the job
//!   refuses without touching anything. The log is updated as steps are undone (same chunking), so
//!   a rollback can be interrupted and resumed; when everything is undone the log file is removed.
//!
//! Names are bytes; every step name must be one plain path component (checked for requests and for
//! logs loaded from disk, which could have been edited).

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ofm_core::batch_rename::{
    BatchError, Plan, Step, StepKind, UndoLog, load_undo_log, save_undo_log, undo_plan, verify_undo,
};

use crate::copy::noreplace;
use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::fs::Fs;

/// `EXDEV`.
const EXDEV: i32 = 18;

/// Identity of a file system object (`st_dev`, `st_ino`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileId {
    /// Device id.
    pub dev: u64,
    /// Inode number.
    pub ino: u64,
}

/// Request: execute a batch rename plan.
#[derive(Debug, Clone)]
pub struct RenameBatchSpec {
    /// Absolute directory containing every name of the plan.
    pub dir: PathBuf,
    /// The plan from `ofm_core::batch_rename::plan`; executed strictly in order.
    pub plan: Plan,
    /// Where the undo log is written (replaced by the first write-ahead, rewritten as the batch
    /// runs). The caller chooses it (state directory); parent directories are created.
    pub undo_log: PathBuf,
    /// Optional identities of the sources recorded at plan time, keyed by the original name
    /// (`from` of `Direct`/`ToTemp` steps). A source that is no longer that inode stops the job
    /// before it is renamed. Empty = no plan-time check (steps are still checked for consistency
    /// between `ToTemp` and `FromTemp`). Use [`snapshot_identities`] on a worker thread.
    pub identities: Vec<(OsString, FileId)>,
}

/// Request: undo executed steps (a finished batch, or the executed part of a stopped one).
#[derive(Debug, Clone)]
pub struct RollbackSpec {
    /// The log of what was done, normally `load_log(path)` of the batch's undo log.
    pub log: UndoLog,
    /// The file the log came from. Rewritten as steps are undone; removed when nothing is left to
    /// undo.
    pub log_path: PathBuf,
}

/// Loads the undo log of a batch (finished, failed or cancelled) for [`RollbackSpec`].
pub fn load_log(path: &Path) -> Result<UndoLog, JobError> {
    load_undo_log(path).map_err(|e| log_error(path, e))
}

/// `lstat`s `names` in `dir` and returns their identities, for [`RenameBatchSpec::identities`].
/// Does file I/O: call it on a worker thread. Only names that appear as a source of a `Direct` or
/// `ToTemp` step of `plan` are looked at.
pub fn snapshot_identities(
    fs: &dyn Fs,
    dir: &Path,
    plan: &Plan,
) -> Result<Vec<(OsString, FileId)>, JobError> {
    let mut out = Vec::new();
    for step in plan.steps.iter().filter(|s| s.kind != StepKind::FromTemp) {
        let path = dir.join(&step.from);
        let st = fs.lstat(&path).map_err(|e| JobError::io(&path, e))?;
        out.push((
            step.from.clone(),
            FileId {
                dev: st.dev,
                ino: st.ino,
            },
        ));
    }
    Ok(out)
}

// ---- helpers ------------------------------------------------------------------------------

fn lossy(s: &OsStr) -> String {
    s.to_string_lossy().into_owned()
}

fn log_error(path: &Path, e: BatchError) -> JobError {
    match e {
        BatchError::Io { path, source } => JobError::io(path, source),
        other => JobError::io(path, io::Error::other(other.to_string())),
    }
}

/// A step name must be a single plain component.
fn check_name(name: &OsStr) -> Result<(), JobError> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
        return Err(JobError::Invalid(format!(
            "batch rename: {:?} is not a plain file name",
            lossy(name)
        )));
    }
    Ok(())
}

fn check_request(dir: &Path, steps: &[Step]) -> Result<(), JobError> {
    if !dir.is_absolute() {
        return Err(JobError::Invalid(format!(
            "batch rename directory must be absolute: {}",
            dir.display()
        )));
    }
    for s in steps {
        check_name(&s.from)?;
        check_name(&s.to)?;
    }
    Ok(())
}

/// Error of step `idx` (0-based) of `total`, keeping the error kind and naming both names.
fn step_error(
    idx: usize,
    total: usize,
    from: &OsStr,
    to: &OsStr,
    path: PathBuf,
    e: &io::Error,
) -> JobError {
    let what = if e.raw_os_error() == Some(EXDEV) {
        "cannot rename across file systems (EXDEV)".to_owned()
    } else {
        e.to_string()
    };
    JobError::io(
        path,
        io::Error::new(
            e.kind(),
            format!(
                "step {}/{total} ({:?} -> {:?}): {what}",
                idx + 1,
                lossy(from),
                lossy(to)
            ),
        ),
    )
}

/// One checked rename inside `dir`. Returns the identity of the renamed object.
fn rename_one(
    fs: &dyn Fs,
    dir: &Path,
    idx: usize,
    total: usize,
    from: &OsStr,
    to: &OsStr,
    expect: Option<FileId>,
) -> Result<FileId, JobError> {
    let src = dir.join(from);
    let dst = dir.join(to);
    let st = fs
        .lstat(&src)
        .map_err(|e| step_error(idx, total, from, to, src.clone(), &e))?;
    let id = FileId {
        dev: st.dev,
        ino: st.ino,
    };
    if let Some(want) = expect
        && want != id
    {
        let e = io::Error::other("the file changed since the rename was planned");
        return Err(step_error(idx, total, from, to, src, &e));
    }
    match noreplace::rename_noreplace(fs, &src, &dst, st.is_dir()) {
        Ok(()) => Ok(id),
        Err(e) => {
            // The target is what conflicts for AlreadyExists; the source otherwise.
            let path = if e.kind() == io::ErrorKind::AlreadyExists {
                dst
            } else {
                src
            };
            Err(step_error(idx, total, from, to, path, &e))
        }
    }
}

/// Keeps the on-disk log equal to the list of steps currently done.
struct LogWriter<'a> {
    dir: &'a Path,
    path: &'a Path,
    /// User-visible renames `(from, to)`; an entry is logged once its target name was reached.
    preview: Vec<(OsString, OsString)>,
}

impl LogWriter<'_> {
    fn save(&self, done: &[Step]) -> Result<(), JobError> {
        let reached: HashSet<&OsString> = done
            .iter()
            .filter(|s| s.kind != StepKind::ToTemp)
            .map(|s| &s.to)
            .collect();
        let plan = Plan {
            steps: done.to_vec(),
            preview: self
                .preview
                .iter()
                .filter(|(_, to)| reached.contains(to))
                .cloned()
                .collect(),
        };
        let log = UndoLog::new(self.dir.to_path_buf(), &plan);
        save_undo_log(&log, self.path).map_err(|e| log_error(self.path, e))
    }
}

/// Simulates `steps` (in order) against the directory listing: every `from` must exist and every
/// `to` must be free at its turn.
fn preflight(fs: &dyn Fs, dir: &Path, steps: &[Step]) -> Result<(), JobError> {
    let names = fs.read_dir(dir).map_err(|e| JobError::io(dir, e))?;
    let mut sim: HashSet<OsString> = names.into_iter().collect();
    for (i, s) in steps.iter().enumerate() {
        if !sim.remove(&s.from) {
            let e = io::Error::from(io::ErrorKind::NotFound);
            return Err(step_error(
                i,
                steps.len(),
                &s.from,
                &s.to,
                dir.join(&s.from),
                &e,
            ));
        }
        if !sim.insert(s.to.clone()) {
            let e = io::Error::from(io::ErrorKind::AlreadyExists);
            return Err(step_error(
                i,
                steps.len(),
                &s.from,
                &s.to,
                dir.join(&s.to),
                &e,
            ));
        }
    }
    Ok(())
}

fn sync_dir_best_effort(ctx: &mut JobCtx, dir: &Path) {
    if let Err(e) = ctx.fs().sync_dir(dir) {
        ctx.warn(dir, format!("could not sync the directory: {e}"));
    }
}

// ---- log chunking and tolerance -----------------------------------------------------------

/// Maximum number of trailing log steps that may not have run (write-ahead tail) or may already be
/// undone (rollback lag). Bounds the cost of the staleness check.
const TAIL_TOLERANCE: usize = 1024;

/// How many steps ahead of the executed ones the log is written: 1 up to 100 steps (exact
/// write-ahead of the current step), `total / 100` above, never more than [`TAIL_TOLERANCE`].
pub fn chunk_for(total: usize) -> usize {
    (total / 100).clamp(1, TAIL_TOLERANCE)
}

/// Whether `pending` steps could still be executed, in order, on a directory with `names`
/// (`from` present, `to` free at each turn): i.e. they have not run.
fn pending_applicable(pending: &[Step], names: &HashSet<OsString>) -> bool {
    if pending.is_empty() {
        return true;
    }
    let mut sim = names.clone();
    pending
        .iter()
        .all(|s| sim.remove(&s.from) && sim.insert(s.to.clone()))
}

/// Number of leading log steps that really ran. Steps after that must be a not-run tail (see the
/// module docs); the executed prefix must pass the planner's `verify_undo`.
fn executed_prefix(log: &UndoLog, names: &HashSet<OsString>) -> Result<usize, JobError> {
    let total = log.steps.len();
    let mut first_err: Option<String> = None;
    for keep in (total.saturating_sub(TAIL_TOLERANCE)..=total).rev() {
        if !pending_applicable(&log.steps[keep..], names) {
            continue;
        }
        let prefix = UndoLog {
            dir: log.dir.clone(),
            steps: log.steps[..keep].to_vec(),
            renames: Vec::new(),
        };
        match verify_undo(&prefix, names) {
            Ok(()) => return Ok(keep),
            Err(e) => {
                first_err.get_or_insert_with(|| e.to_string());
            }
        }
    }
    Err(JobError::Invalid(first_err.unwrap_or_else(|| {
        "undo is no longer applicable".to_owned()
    })))
}

// ---- forward ------------------------------------------------------------------------------

/// Execute a batch rename plan (see the module docs for the guarantees).
pub fn run_batch(ctx: &mut JobCtx, spec: &RenameBatchSpec) -> Result<(), JobError> {
    let steps = &spec.plan.steps;
    let dir = spec.dir.as_path();
    check_request(dir, steps)?;
    if steps.is_empty() {
        return Ok(());
    }
    let fs = ctx.fs();
    preflight(&*fs, dir, steps)?;
    let total = steps.len();
    ctx.set_totals(total as u64, 0);

    let writer = LogWriter {
        dir,
        path: &spec.undo_log,
        preview: spec.plan.preview.clone(),
    };
    let chunk = chunk_for(total);
    let mut ids: HashMap<OsString, FileId> = spec.identities.iter().cloned().collect();
    // `done`: steps completed; `logged`: leading steps listed in the log on disk.
    let mut done = 0usize;
    let mut logged = 0usize;
    let result = (|| {
        for (i, step) in steps.iter().enumerate() {
            ctx.checkpoint()?;
            if i >= logged {
                // Write ahead: the log lists the step before it runs (and fails the job, with the
                // step not run, if it cannot be written; the first write also proves the location).
                let upto = (i + chunk).min(total);
                writer.save(&steps[..upto])?;
                logged = upto;
            }
            ctx.set_current(&dir.join(&step.from));
            let expect = ids.remove(&step.from);
            let id = rename_one(&*fs, dir, i, total, &step.from, &step.to, expect)?;
            ids.insert(step.to.clone(), id);
            done = i + 1;
            if step.kind != StepKind::ToTemp {
                ctx.note_created(dir.join(&step.to));
            }
            ctx.file_done();
        }
        Ok(())
    })();
    if result.is_err()
        && logged != done
        && let Err(e) = writer.save(&steps[..done])
    {
        ctx.warn(
            &spec.undo_log,
            format!("could not trim the undo log to the {done} completed steps: {e}"),
        );
    }
    sync_dir_best_effort(ctx, dir);
    result
}

// ---- rollback / undo ----------------------------------------------------------------------

/// Undo the steps of `spec.log` in reverse order (see the module docs).
pub fn run_rollback(ctx: &mut JobCtx, spec: &RollbackSpec) -> Result<(), JobError> {
    let dir = spec.log.dir.as_path();
    check_request(dir, &spec.log.steps)?;
    if spec.log.steps.is_empty() {
        remove_log(ctx, &spec.log_path);
        return Ok(());
    }
    let fs = ctx.fs();
    let names: HashSet<OsString> = fs
        .read_dir(dir)
        .map_err(|e| JobError::io(dir, e))?
        .into_iter()
        .collect();
    let executed = executed_prefix(&spec.log, &names)?;
    let steps = &spec.log.steps[..executed];
    let undo = undo_plan(&UndoLog {
        dir: spec.log.dir.clone(),
        steps: steps.to_vec(),
        renames: Vec::new(),
    });
    let total = undo.steps.len();
    ctx.set_totals(total as u64, 0);
    let writer = LogWriter {
        dir,
        path: &spec.log_path,
        preview: spec
            .log
            .renames
            .iter()
            .map(|r| (r.from.clone(), r.to.clone()))
            .collect(),
    };
    let chunk = chunk_for(spec.log.steps.len());
    // `remaining`: steps still done; `saved`: steps listed in the log on disk (>= remaining).
    let mut remaining = executed;
    let mut saved = spec.log.steps.len();
    let result = (|| {
        for (i, step) in undo.steps.iter().enumerate() {
            ctx.checkpoint()?;
            ctx.set_current(&dir.join(&step.from));
            rename_one(&*fs, dir, i, total, &step.from, &step.to, None)?;
            remaining -= 1;
            // The goal is restoring names: a log that lists too much is tolerated by the next
            // rollback (its tail is recognised as not-done), so a failed save only warns.
            if remaining > 0 && saved - remaining >= chunk {
                match writer.save(&steps[..remaining]) {
                    Ok(()) => saved = remaining,
                    Err(e) => {
                        ctx.warn(
                            &spec.log_path,
                            format!("could not update the undo log: {e}"),
                        );
                    }
                }
            }
            if step.kind != StepKind::FromTemp {
                // `step.to` is an original name again.
                ctx.note_created(dir.join(&step.to));
            }
            ctx.file_done();
        }
        Ok(())
    })();
    if remaining == 0 {
        remove_log(ctx, &spec.log_path);
    } else if saved != remaining
        && let Err(e) = writer.save(&steps[..remaining])
    {
        ctx.warn(
            &spec.log_path,
            format!("could not trim the undo log to the {remaining} steps still done: {e}"),
        );
    }
    sync_dir_best_effort(ctx, dir);
    result
}

fn remove_log(ctx: &mut JobCtx, path: &Path) {
    match ctx.fs().unlink(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => ctx.warn(path, format!("could not remove the finished undo log: {e}")),
    }
}

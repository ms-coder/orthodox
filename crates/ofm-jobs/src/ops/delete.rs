//! Delete (T3.2 owns this file; F14, ADR-0004 "Delete").
//!
//! `to_trash`: per top-level path `ctx.fs().trash(path)`; `TrashError::Unavailable` ->
//! `ctx.confirm_permanent_delete(path)` (true = permanent delete of that path, false = skipped);
//! other trash errors go through `ctx.resolve_error` (retry/skip/abort).
//!
//! Permanent: depth-first, `lstat` then `unlink` for non-directories (symlinks included, NEVER
//! followed), `rmdir` after the children; read-only directories are not chmod-ed behind the user's
//! back (the error goes to `resolve_error`). A directory whose children were skipped is left in
//! place (no pointless `ENOTEMPTY` question). A missing top-level path is a warning, not an error;
//! entries that vanish while deleting are fine (the goal is reached).
//!
//! The walk goes through checked directory handles (`fs::walk`, M9): a directory swapped for a
//! symlink, or moved away, while the job runs is reported and left alone, never followed.
//!
//! Progress: the permanent variant scans first (`planning`), then `set_totals(entries, 0)`; every
//! removed entry (file, link or directory) is one `file_done`.

use std::io;
use std::path::{Path, PathBuf};

use crate::ctx::{ErrorResolution, JobCtx};
use crate::error::JobError;
use crate::event::Operation;
use crate::fs::{CheckedDir, Selection, TrashError};

/// Delete `paths`.
pub fn run(ctx: &mut JobCtx, paths: &[PathBuf], to_trash: bool) -> Result<(), JobError> {
    if paths.is_empty() {
        return Err(JobError::Invalid("nothing to delete".into()));
    }
    // "/" and anything ending in ".." name no entry that could be removed on its own.
    if let Some(p) = paths.iter().find(|p| p.file_name().is_none()) {
        return Err(JobError::Invalid(format!(
            "refusing to delete {}",
            p.display()
        )));
    }
    if to_trash {
        trash_all(ctx, paths)
    } else {
        delete_all(ctx, paths)
    }
}

fn trash_all(ctx: &mut JobCtx, paths: &[PathBuf]) -> Result<(), JobError> {
    ctx.set_totals(paths.len() as u64, 0);
    let mut selection = Selection::default();
    for path in paths {
        ctx.checkpoint()?;
        ctx.set_current(path);
        trash_one(ctx, &mut selection, path)?;
    }
    Ok(())
}

fn trash_one(ctx: &mut JobCtx, selection: &mut Selection, path: &Path) -> Result<(), JobError> {
    loop {
        ctx.checkpoint()?;
        match ctx.fs().trash(path) {
            Ok(()) => {
                ctx.file_done();
                return Ok(());
            }
            Err(TrashError::Unavailable(why)) => {
                tracing::info!(path = %path.display(), %why, "trash unavailable");
                if ctx.confirm_permanent_delete(path)? {
                    let n = count_entries(ctx, path)?;
                    ctx.add_total(n.saturating_sub(1), 0);
                    delete_item(ctx, selection, path)?;
                }
                return Ok(());
            }
            Err(TrashError::Io(e)) if e.kind() == io::ErrorKind::NotFound => {
                ctx.warn(path, "does not exist; skipped");
                ctx.note_skipped();
                return Ok(());
            }
            Err(TrashError::Io(e)) => match ctx.resolve_error(path, Operation::Trash, &e)? {
                ErrorResolution::Retry => {}
                ErrorResolution::Skip => return Ok(()),
            },
        }
    }
}

fn delete_all(ctx: &mut JobCtx, paths: &[PathBuf]) -> Result<(), JobError> {
    ctx.set_planning(true);
    let mut total = 0;
    for path in paths {
        total += count_entries(ctx, path)?;
    }
    ctx.set_totals(total, 0);
    let mut selection = Selection::default();
    for path in paths {
        ctx.checkpoint()?;
        delete_item(ctx, &mut selection, path)?;
    }
    Ok(())
}

/// Number of entries below and including `path` (symlinks are entries, never followed).
/// Unreadable places count as what could be seen; the deletion pass reports the real errors.
fn count_entries(ctx: &mut JobCtx, path: &Path) -> Result<u64, JobError> {
    let fs = ctx.fs();
    let mut n = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        ctx.checkpoint()?;
        let Ok(st) = fs.lstat(&p) else { continue };
        n += 1;
        if st.is_dir()
            && let Ok(names) = fs.read_dir(&p)
        {
            stack.extend(names.into_iter().map(|name| p.join(name)));
        }
    }
    Ok(n)
}

/// Permanently delete the selected item `path`.
fn delete_item(ctx: &mut JobCtx, selection: &mut Selection, path: &Path) -> Result<(), JobError> {
    ctx.checkpoint()?;
    let fs = ctx.fs();
    let opened = ctx.attempt(path, Operation::Delete, |_| {
        found(selection.open(&fs, path))
    })?;
    match opened {
        None => {} // skipped by the user
        Some(None) => {
            ctx.warn(path, "does not exist; skipped");
            ctx.note_skipped();
        }
        Some(Some((dir, item))) => {
            remove(ctx, &dir, &item, true)?;
        }
    }
    Ok(())
}

/// `Ok(None)` for something that does not exist.
fn found<T>(r: io::Result<T>) -> io::Result<Option<T>> {
    match r {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Remove `path`, an entry of `parent`. `Ok(true)` = it is gone, `Ok(false)` = something below
/// it was skipped (or the user skipped it), so it (and its parents) must stay.
fn remove(ctx: &mut JobCtx, parent: &CheckedDir, path: &Path, top: bool) -> Result<bool, JobError> {
    ctx.checkpoint()?;
    ctx.set_current(path);
    let stat = ctx.attempt(path, Operation::Delete, |_| found(parent.lstat(path)))?;
    let st = match stat {
        None => return Ok(false),
        Some(None) => {
            if top {
                ctx.warn(path, "does not exist; skipped");
                ctx.note_skipped();
            }
            return Ok(true);
        }
        Some(Some(st)) => st,
    };

    if st.is_dir() {
        let entered = ctx.attempt(path, Operation::Delete, |_| found(parent.child(path, &st)))?;
        let dir = match entered {
            None => return Ok(false),
            Some(None) => return Ok(true),
            Some(Some(dir)) => dir,
        };
        let Some(names) = ctx.attempt(path, Operation::Delete, |_| dir.list())? else {
            return Ok(false);
        };
        let mut all_gone = true;
        for name in names {
            all_gone &= remove(ctx, &dir, &path.join(name), false)?;
        }
        if !all_gone {
            return Ok(false);
        }
        drop(dir);
        let done = ctx.attempt(path, Operation::Delete, |_| {
            ignore_missing(parent.entry(path, |fs, p| fs.rmdir(p)))
        })?;
        if done.is_some() {
            ctx.file_done();
        }
        Ok(done.is_some())
    } else {
        let done = ctx.attempt(path, Operation::Delete, |_| {
            ignore_missing(parent.entry(path, |fs, p| fs.unlink(p)))
        })?;
        if done.is_some() {
            ctx.file_done();
        }
        Ok(done.is_some())
    }
}

fn ignore_missing(r: io::Result<()>) -> io::Result<()> {
    match r {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

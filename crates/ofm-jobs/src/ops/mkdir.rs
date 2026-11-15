//! Make directory (T3.2 owns this file; F13): `mkdir -p` semantics, mode 0777 (umask applies),
//! an already existing *directory* (or a symlink to one) is success, an existing non-directory is
//! an error. Intermediate directories are created one by one through `ctx.fs().mkdir`, top-down.
//! `ctx.note_created(path)` on success (the UI derives the first created component below the panel
//! directory from the full path).

use std::io;
use std::path::{Path, PathBuf};

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::event::Operation;

/// Create `path` and missing parents.
pub fn run(ctx: &mut JobCtx, path: &Path) -> Result<(), JobError> {
    if !path.is_absolute() {
        return Err(JobError::Invalid(format!(
            "directory path must be absolute: {}",
            path.display()
        )));
    }

    // Walk up to the deepest existing ancestor; `missing` is bottom-up.
    let mut missing: Vec<PathBuf> = Vec::new();
    let mut cur = path.to_path_buf();
    loop {
        let probe = ctx.attempt(&cur, Operation::Mkdir, |fs| match fs.stat(&cur) {
            Ok(st) => Ok(Ok(Some(st))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Ok(None)),
            // A path component is a regular file: not recoverable, no point in asking.
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => Ok(Err(e)),
            Err(e) => Err(e),
        })?;
        let Some(probe) = probe else {
            return Ok(()); // skipped by the user
        };
        let probe = probe.map_err(|e| JobError::io(&cur, e))?;
        match probe {
            Some(st) if st.is_dir() => break,
            Some(_) => {
                let what = if cur == path {
                    "already exists and is not a directory"
                } else {
                    "is not a directory"
                };
                return Err(JobError::io(
                    cur,
                    io::Error::new(io::ErrorKind::AlreadyExists, what),
                ));
            }
            None => {
                missing.push(cur.clone());
                match cur.parent() {
                    Some(p) => cur = p.to_path_buf(),
                    None => break,
                }
            }
        }
    }

    ctx.set_totals(missing.len() as u64, 0);
    for dir in missing.iter().rev() {
        ctx.set_current(dir);
        let made = ctx.attempt(dir, Operation::Mkdir, |fs| match fs.mkdir(dir, 0o777) {
            Ok(()) => Ok(()),
            // Raced with somebody else creating it: fine if it is a directory now.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => match fs.stat(dir) {
                Ok(st) if st.is_dir() => Ok(()),
                _ => Err(e),
            },
            Err(e) => Err(e),
        })?;
        if made.is_none() {
            return Ok(());
        }
        ctx.file_done();
    }
    ctx.note_created(path);
    Ok(())
}

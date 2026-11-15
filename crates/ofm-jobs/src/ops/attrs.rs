//! chmod / chown (T3.2 owns this file; F19). `ModeChange { set, clear }`: new = (old & !clear) | set,
//! computed per object from `lstat` (only the permission bits `0o7777`).
//!
//! "X for directories only": when `recursive`, the execute bits (0o111) are masked out of `set`
//! for a non-directory that has no execute bit yet, so `a+rx` over a tree makes directories
//! traversable without turning plain files into programs (like `chmod -R a+rX`). `clear` always
//! applies as given. Order: a directory is changed before its children (like GNU `chmod -R`) —
//! so `u+rwx` opens a locked directory — **unless** the change would take away the owner's own
//! read or traverse bits (`u-rx` and friends): then it is changed only after its children
//! (post-order), so the job cannot lock itself out of the subtree it still has to walk. A
//! directory the owner cannot list is temporarily given `u+rx` for the walk and restored
//! afterwards (also on error or cancel); if even that fails, the listing goes through
//! `ctx.attempt` as a per-entry error and is never silently skipped.
//!
//! Symlinks are skipped for chmod (Linux cannot chmod a link) and never followed in recursion;
//! chown uses `lchown` semantics. The owner is changed before the mode (chown clears set-id bits).
//! An object that already has the requested owner/mode costs no syscall. Failures: `ctx.attempt`.

use std::io;
use std::path::{Path, PathBuf};

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::event::Operation;
use crate::fs::FsStat;
use crate::spec::{ModeChange, OwnerChange};

const EXEC_BITS: u32 = 0o111;

/// Apply `mode` and/or `owner` to `paths` (recursively into directories if `recursive`).
pub fn run(
    ctx: &mut JobCtx,
    paths: &[PathBuf],
    mode: Option<ModeChange>,
    owner: Option<OwnerChange>,
    recursive: bool,
) -> Result<(), JobError> {
    let owner = owner.filter(|o| o.uid.is_some() || o.gid.is_some());
    if mode.is_none() && owner.is_none() {
        return Err(JobError::Invalid("no attribute change requested".into()));
    }
    if paths.is_empty() {
        return Err(JobError::Invalid("nothing to change".into()));
    }
    let change = Change {
        mode,
        owner,
        recursive,
    };
    ctx.set_totals(paths.len() as u64, 0);
    for path in paths {
        apply(ctx, path, &change, true)?;
    }
    Ok(())
}

struct Change {
    mode: Option<ModeChange>,
    owner: Option<OwnerChange>,
    recursive: bool,
}

/// New permission bits for an object with `st`.
fn new_perm(st: &FsStat, m: ModeChange, recursive: bool) -> u32 {
    let mut set = m.set & 0o7777;
    if recursive && !st.is_dir() && st.perm() & EXEC_BITS == 0 {
        set &= !EXEC_BITS;
    }
    (st.perm() & !m.clear & 0o7777) | set
}

fn apply(ctx: &mut JobCtx, path: &Path, change: &Change, top: bool) -> Result<(), JobError> {
    ctx.checkpoint()?;
    ctx.set_current(path);
    let stat = ctx.attempt(path, Operation::SetAttrs, |fs| match fs.lstat(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    })?;
    let st = match stat {
        None => return Ok(()), // skipped by the user
        Some(None) => {
            if top {
                ctx.warn(path, "does not exist; skipped");
                ctx.note_skipped();
            }
            return Ok(());
        }
        Some(Some(st)) => st,
    };

    let mut st = st;
    if let Some(o) = change.owner
        && (o.uid.is_some_and(|u| u != st.uid) || o.gid.is_some_and(|g| g != st.gid))
    {
        let changed = ctx.attempt(path, Operation::SetAttrs, |fs| {
            fs.set_owner(path, o.uid, o.gid, false)
        })?;
        if changed.is_some() && change.mode.is_some() && !st.is_symlink() {
            // chown may have cleared set-id bits: compute the mode from what is there now.
            if let Some(fresh) = ctx.attempt(path, Operation::SetAttrs, |fs| fs.lstat(path))? {
                st = fresh;
            }
        }
    }
    // The mode the directory has while its children are walked, and its own change if that must
    // wait until after the walk (post-order, see [`locks_itself_out`]).
    let mut walked_perm = st.perm();
    let mut deferred: Option<u32> = None;
    if let Some(m) = change.mode
        && !st.is_symlink()
    {
        let want = new_perm(&st, m, change.recursive);
        if want != st.perm() {
            if locks_itself_out(&st, want, change.recursive) {
                // Post-order: the directory keeps its owner r/x until its children are done.
                deferred = Some(want);
            } else if ctx
                .attempt(path, Operation::SetAttrs, |fs| fs.set_mode(path, want))?
                .is_some()
            {
                walked_perm = want;
            }
        }
    }
    ctx.file_done();

    if st.is_dir() && change.recursive {
        // If the owner cannot list/traverse the directory as it stands now, open it temporarily.
        let opened = walked_perm & OWNER_RX != OWNER_RX;
        if opened {
            let grant = walked_perm | OWNER_RX;
            ctx.attempt(path, Operation::SetAttrs, |fs| fs.set_mode(path, grant))?;
        }
        let mut walked = Ok(());
        if let Some(names) = ctx.attempt(path, Operation::SetAttrs, |fs| fs.read_dir(path))? {
            ctx.add_total(names.len() as u64, 0);
            for name in names {
                if let Err(e) = apply(ctx, &path.join(name), change, false) {
                    walked = Err(e);
                    break;
                }
            }
        }
        if opened {
            restore_mode(ctx, path, walked_perm);
        }
        walked?;
    }
    if let Some(want) = deferred {
        ctx.attempt(path, Operation::SetAttrs, |fs| fs.set_mode(path, want))?;
    }
    Ok(())
}

/// Owner read + traverse: without both, the job itself could not list a directory or reach its
/// children, so a recursive change that clears either bit on a directory is applied post-order.
const OWNER_RX: u32 = 0o500;

/// `true` if applying `want` before walking would take away an owner r/x bit the directory has.
fn locks_itself_out(st: &FsStat, want: u32, recursive: bool) -> bool {
    recursive && st.is_dir() && st.perm() & OWNER_RX & !want != 0
}

/// Put back the mode a temporarily opened directory had before (best effort: a failure becomes a
/// job warning — the walk's result, including a cancel, is reported either way).
fn restore_mode(ctx: &mut JobCtx, path: &Path, perm: u32) {
    if let Err(e) = ctx.fs().set_mode(path, perm) {
        ctx.warn(path, format!("could not restore mode {perm:o}: {e}"));
    }
}

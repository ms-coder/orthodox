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
//! directory of ours that we cannot list/traverse is temporarily given `u+rx` for the walk
//! and restored afterwards — always, also when the listing itself is denied or the job is
//! aborted or cancelled at that point. Another user's directory is never opened preemptively
//! (the owner's bits say nothing about *our* access; a chmod would only fail with EPERM): it
//! is listed as it stands, only a listing denied with EACCES can lead to a temporary open,
//! and a listing failure is asked through `ctx.attempt` as a per-entry error, never silently
//! skipped.
//!
//! Symlinks are skipped for chmod (Linux cannot chmod a link) and never followed in recursion;
//! chown uses `lchown` semantics. The owner is changed before the mode (chown clears set-id bits).
//! Every object is reached through checked directory handles (`fs::walk`, M9), and `chmod` and
//! `chown` act on a handle of the very object that was `lstat`ed: a file or directory swapped for
//! a symlink (or anything else) meanwhile is reported, never followed or changed.
//! An object that already has the requested owner/mode costs no syscall. Failures: `ctx.attempt`.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use crate::ctx::{ErrorResolution, JobCtx};
use crate::error::JobError;
use crate::event::Operation;
use crate::fs::{CheckedDir, FsPin, FsStat, Selection};
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
    if let Some(p) = paths.iter().find(|p| p.file_name().is_none()) {
        return Err(JobError::Invalid(format!(
            "refusing to change {}",
            p.display()
        )));
    }
    let change = Change {
        mode,
        owner,
        recursive,
    };
    ctx.set_totals(paths.len() as u64, 0);
    let mut selection = Selection::default();
    for path in paths {
        apply_selected(ctx, &mut selection, path, &change)?;
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

/// Apply the change to the selected item `path`.
fn apply_selected(
    ctx: &mut JobCtx,
    selection: &mut Selection,
    path: &Path,
    change: &Change,
) -> Result<(), JobError> {
    ctx.checkpoint()?;
    let fs = ctx.fs();
    let opened = ctx.attempt(path, Operation::SetAttrs, |_| {
        found(selection.open(&fs, path))
    })?;
    match opened {
        None => {} // skipped by the user
        Some(None) => {
            ctx.warn(path, "does not exist; skipped");
            ctx.note_skipped();
        }
        Some(Some((dir, item))) => apply(ctx, &dir, &item, change, true)?,
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

/// `chmod` the object that `st` describes, found as `path` in `parent`.
fn chmod(parent: &CheckedDir, path: &Path, st: &FsStat, perm: u32) -> io::Result<()> {
    let object = parent.pin_entry(path, st)?;
    parent.fs().set_mode(object.path(), perm)
}

/// [`chmod`] that keeps the handle for the restore.
fn grant_mode(parent: &CheckedDir, path: &Path, st: &FsStat, perm: u32) -> io::Result<FsPin> {
    let object = parent.pin_entry(path, st)?;
    parent.fs().set_mode(object.path(), perm)?;
    Ok(object)
}

/// `lchown` semantics for the object that `st` describes.
fn chown(parent: &CheckedDir, path: &Path, st: &FsStat, o: OwnerChange) -> io::Result<()> {
    if st.is_symlink() {
        // By name: the owner of a link changes nothing but the link.
        return parent.entry(path, |fs, p| fs.set_owner(p, o.uid, o.gid, false));
    }
    let object = parent.pin_entry(path, st)?;
    // Following the handle's path ends at the pinned object itself.
    parent.fs().set_owner(object.path(), o.uid, o.gid, true)
}

fn apply(
    ctx: &mut JobCtx,
    parent: &CheckedDir,
    path: &Path,
    change: &Change,
    top: bool,
) -> Result<(), JobError> {
    ctx.checkpoint()?;
    ctx.set_current(path);
    let stat = ctx.attempt(path, Operation::SetAttrs, |_| found(parent.lstat(path)))?;
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
        let changed = ctx.attempt(path, Operation::SetAttrs, |_| chown(parent, path, &st, o))?;
        if changed.is_some() && change.mode.is_some() && !st.is_symlink() {
            // chown may have cleared set-id bits: compute the mode from what is there now (of
            // the same object; anything else under the name is refused by the chmod below).
            if let Some(fresh) = ctx.attempt(path, Operation::SetAttrs, |_| parent.lstat(path))?
                && (fresh.dev, fresh.ino) == (st.dev, st.ino)
            {
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
                .attempt(path, Operation::SetAttrs, |_| {
                    chmod(parent, path, &st, want)
                })?
                .is_some()
            {
                walked_perm = want;
            }
        }
    }
    ctx.file_done();

    if st.is_dir() && change.recursive {
        let Some(dir) = ctx.attempt(path, Operation::SetAttrs, |_| parent.child(path, &st))? else {
            return Ok(());
        };
        // If the job's user cannot list/traverse the directory as it stands now, open it
        // temporarily. The owner's r/x bits govern our access only on a directory we own
        // ourselves; another user's directory is listed as it is (the group or other bits may
        // well allow it — a preemptive chmod there would only fail with EPERM) and is opened
        // only after the listing was actually denied (EACCES).
        // `opened` is a handle on the directory itself, so that the restore reaches exactly it.
        let mut opened: Option<FsPin> = None;
        let listed = if opens_before_listing(effective_uid(), &st, walked_perm) {
            let grant = walked_perm | OWNER_RX;
            opened = ctx.attempt(path, Operation::SetAttrs, |_| {
                grant_mode(parent, path, &st, grant)
            })?;
            ctx.attempt(path, Operation::SetAttrs, |_| dir.list())
        } else {
            list_denied_opening(ctx, parent, &dir, path, &st, walked_perm, &mut opened)
        };
        let mut walked = Ok(());
        if let Ok(Some(names)) = &listed {
            ctx.add_total(names.len() as u64, 0);
            for name in names {
                if let Err(e) = apply(ctx, &dir, &path.join(name), change, false) {
                    walked = Err(e);
                    break;
                }
            }
        }
        // Restore the temporarily granted mode BEFORE propagating the listing/walk result, so
        // an aborted, cancelled or denied listing never leaves it behind (T3.7c).
        if let Some(handle) = opened {
            restore_mode(ctx, path, &handle, walked_perm);
        }
        listed?;
        walked?;
    }
    if let Some(want) = deferred {
        ctx.attempt(path, Operation::SetAttrs, |_| {
            chmod(parent, path, &st, want)
        })?;
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

/// The effective uid of the process. The `Fs` seam (frozen by T3.0) has no euid query, so this
/// one place goes to rustix directly — the same escape hatch `fs::real` uses for `is_root`.
fn effective_uid() -> u32 {
    rustix::process::geteuid().as_raw()
}

/// Whether the walk grants itself owner r/x *before* listing: only on a directory the
/// effective user owns, because only then do the owner's bits govern our own access. On
/// another user's directory the listing is tried first (the group or other bits may allow it)
/// and it is opened reactively, if at all (see [`list_denied_opening`]).
fn opens_before_listing(euid: u32, st: &FsStat, walked_perm: u32) -> bool {
    st.uid == euid && walked_perm & OWNER_RX != OWNER_RX
}

/// List `dir` (the entry `path` of `parent`) for the walk when the owner's r/x bits do not govern
/// our access (another user's directory, or already walkable): the listing is tried as it stands.
/// Only a listing denied with EACCES leads to a temporary open — possible only where the effective
/// user may chmod the directory (their own, or as root); every other failure is asked as the
/// listing error itself. `Ok(None)` = skipped by the user; `*opened` holds the directory if a
/// grant was applied that the caller must restore.
fn list_denied_opening(
    ctx: &mut JobCtx,
    parent: &CheckedDir,
    dir: &CheckedDir,
    path: &Path,
    st: &FsStat,
    walked_perm: u32,
    opened: &mut Option<FsPin>,
) -> Result<Option<Vec<OsString>>, JobError> {
    ctx.checkpoint()?;
    let denied = match dir.list() {
        Ok(names) => return Ok(Some(names)),
        Err(e) => e,
    };
    let grant = walked_perm | OWNER_RX;
    if denied.kind() == io::ErrorKind::PermissionDenied
        && grant != walked_perm
        && (st.uid == effective_uid() || ctx.fs().is_root())
    {
        // Denied, and we may chmod the directory: grant ourselves r/x and list again.
        *opened = ctx.attempt(path, Operation::SetAttrs, |_| {
            grant_mode(parent, path, st, grant)
        })?;
        if opened.is_some() {
            return ctx.attempt(path, Operation::SetAttrs, |_| dir.list());
        }
        return Ok(None); // the user skipped the chmod: the listing is skipped with it
    }
    // Nothing to open (not our directory, nothing to grant) or a different error:
    // retry/skip/abort on the listing itself.
    match ctx.resolve_error(path, Operation::SetAttrs, &denied)? {
        ErrorResolution::Retry => ctx.attempt(path, Operation::SetAttrs, |_| dir.list()),
        ErrorResolution::Skip => Ok(None),
    }
}

/// Put back the mode a temporarily opened directory had before (best effort: a failure becomes a
/// job warning — the walk's result, including a cancel, is reported either way).
fn restore_mode(ctx: &mut JobCtx, path: &Path, handle: &FsPin, perm: u32) {
    if let Err(e) = ctx.fs().set_mode(handle.path(), perm) {
        ctx.warn(path, format!("could not restore mode {perm:o}: {e}"));
    }
}

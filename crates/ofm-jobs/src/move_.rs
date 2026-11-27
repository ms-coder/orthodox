//! Move engine (T3.1; ADR-0004 "Move").
//!
//! Same `st_dev` as the destination directory: `rename_noreplace` per top-level item (atomic, no
//! data copied). A conflict goes through `ctx.resolve_conflict`; `Overwrite` uses `rename_replace`
//! and is only possible non-directory over non-directory; a directory onto an existing directory
//! merges (its entries are moved one by one, the emptied source directory is removed). A rename
//! failing with `EXDEV` (bind mounts, Btrfs subvolumes, mount points inside a source tree) falls
//! back to the cross-device path for that item. Where the file system has no `RENAME_NOREPLACE`
//! (NFS, FUSE) the rename goes through `copy::noreplace` (`link` + `unlink`, else a checked
//! rename). Every item put in place is recorded (dev, ino), so a later source landing on the
//! same name (case-insensitive destination) is asked about and never replaces it (T3.6 B1).
//!
//! Different `st_dev`: the copy machinery in `copy::Mode::Move` — every file is fsynced, its
//! directory fsynced, size/mtime and source stability verified, and only then is that one source
//! item deleted (after checking it is still the same inode); source directories are removed
//! post-order when they became empty. Any failure, skip or cancel leaves the remaining sources
//! where they are: nothing is deleted that has not been completely written and synced.
//!
//! Below a selected directory, sources are listed, renamed and removed through checked directory
//! handles (`fs::walk`, M9): a source directory swapped for a symlink is never followed.
//!
//! Symlinks are never followed by a move (they are moved/recreated as links). "Rename in place"
//! (Shift+F6) arrives as `Move` into the same directory with a literal `rename_mask`.

use std::io;
use std::path::{Path, PathBuf};

use ofm_core::Location;

use crate::copy::{Copier, Mode, Target, enter, noreplace, resolve_target};
use crate::ctx::{ErrorResolution, JobCtx};
use crate::error::JobError;
use crate::event::Operation;
use crate::fs::{CheckedDir, FsStat, within};
use crate::plan;
use crate::spec::CopyOpts;

/// `EXDEV`: rename across file systems.
const EXDEV: i32 = 18;

/// `ENOTEMPTY`.
const ENOTEMPTY: i32 = 39;

/// Move `sources` into `dest_dir`.
pub fn run_move(
    ctx: &mut JobCtx,
    sources: &[Location],
    dest_dir: &Path,
    opts: &CopyOpts,
) -> Result<(), JobError> {
    let mask = plan::parse_mask(opts.rename_mask.as_deref())?;
    let sources = plan::local_paths(sources)?;
    let names = plan::target_names(&sources, mask.as_ref())?;
    plan::check_destination(ctx, &sources, dest_dir, false)?;
    plan::ensure_dir(ctx, dest_dir)?;
    let fs = ctx.fs();
    let dest_dev = fs
        .stat(dest_dir)
        .map_err(|e| JobError::io(dest_dir, e))?
        .dev;

    // Plan: same-device items count as one file each (a rename moves no data); cross-device
    // items are scanned because their data will be copied.
    ctx.set_planning(true);
    let mut renames = 0u64;
    let mut cross: Vec<PathBuf> = Vec::new();
    for src in &sources {
        match fs.lstat(src) {
            Ok(st) if st.dev != dest_dev => cross.push(src.clone()),
            _ => renames += 1, // errors surface (with retry/skip) when the item is moved
        }
    }
    ctx.add_total(renames, 0);
    let totals = plan::scan(ctx, &cross, false)?;
    ctx.set_totals(renames + totals.files, totals.bytes);

    let mut mover = Mover {
        copier: Copier::new(&*fs, Mode::Move, false, true),
        dest_dev,
    };
    for (src, name) in sources.iter().zip(&names) {
        ctx.checkpoint()?;
        mover.item(ctx, src, &dest_dir.join(name), None, true)?;
    }
    Ok(())
}

struct Mover {
    copier: Copier,
    dest_dev: u64,
}

impl Mover {
    /// Move one item (`parent`: the checked directory holding `src`, `None` for a selected
    /// item); `top` items are recorded with `ctx.note_created`.
    fn item(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        dest: &Path,
        parent: Option<&CheckedDir>,
        top: bool,
    ) -> Result<(), JobError> {
        ctx.checkpoint()?;
        ctx.set_current(src);
        let Some(st) = ctx.attempt(src, Operation::Move, |fs| {
            within(fs, parent, src, |fs, p| fs.lstat(p))
        })?
        else {
            return Ok(());
        };
        if st.dev != self.dest_dev {
            return self.copier.copy_item(ctx, src, dest, parent, top);
        }
        let fs = ctx.fs();
        loop {
            let target = resolve_target(
                ctx,
                src,
                &st,
                dest,
                Operation::Move,
                self.copier.published(),
            )?;
            let (to, replace) = match target {
                Target::Skip => return Ok(()),
                Target::Merge(d) => return self.merge(ctx, src, &st, &d, parent, top),
                Target::Fresh(d) => (d, false),
                Target::Replace(d) => (d, true),
            };
            ctx.checkpoint()?;
            let r = within(&*fs, parent, src, |fs, from| {
                if replace {
                    fs.rename_replace(from, &to)
                } else {
                    noreplace::rename_noreplace(fs, from, &to, st.is_dir())
                }
            });
            match r {
                Ok(()) => {
                    tracing::debug!(from = %src.display(), to = %to.display(), "renamed");
                    // A rename keeps the inode: later items of this job never overwrite it.
                    self.copier.note_published((st.dev, st.ino));
                    self.copier.note_published_path(&to);
                    if top {
                        ctx.note_created(to);
                    }
                    ctx.file_done();
                    return Ok(());
                }
                // The name was taken meanwhile: resolve the conflict again.
                Err(e) if !replace && e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) if e.raw_os_error() == Some(EXDEV) => {
                    // Counted as one rename in the plan: add what copying it really involves.
                    let t = plan::count(ctx, &[src.to_path_buf()], false, false)?;
                    ctx.add_total(t.files.saturating_sub(1), t.bytes);
                    return self.copier.copy_item(ctx, src, dest, parent, top);
                }
                Err(e) => match ctx.resolve_error(src, Operation::Move, &e)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => return Ok(()),
                },
            }
        }
    }

    /// Same device, directory onto an existing directory: move the entries one by one, then
    /// remove the source directory if it became empty.
    fn merge(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        st: &FsStat,
        dest: &Path,
        parent: Option<&CheckedDir>,
        top: bool,
    ) -> Result<(), JobError> {
        let fs = ctx.fs();
        let entered = ctx.attempt(src, Operation::Move, |_| enter(&fs, parent, src, st))?;
        let Some(entered) = entered else {
            return Ok(());
        };
        let Some(mut names) = ctx.attempt(src, Operation::Move, |_| entered.dir.list())? else {
            return Ok(());
        };
        names.sort_by(|a, b| {
            use std::os::unix::ffi::OsStrExt;
            a.as_bytes().cmp(b.as_bytes())
        });
        if top {
            ctx.note_created(dest.to_path_buf());
        }
        ctx.add_total(names.len() as u64, 0);
        for name in &names {
            let (from, to) = (src.join(name), dest.join(name));
            self.item(ctx, &from, &to, Some(&entered.dir), false)?;
        }
        loop {
            ctx.checkpoint()?;
            match entered.remove_dir(src) {
                Ok(()) => break,
                Err(e)
                    if e.raw_os_error() == Some(ENOTEMPTY)
                        || e.kind() == io::ErrorKind::AlreadyExists =>
                {
                    tracing::debug!(path = %src.display(), "source dir not empty; kept");
                    break;
                }
                Err(e) => match ctx.resolve_error(src, Operation::Move, &e)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => break,
                },
            }
        }
        ctx.file_done();
        Ok(())
    }
}

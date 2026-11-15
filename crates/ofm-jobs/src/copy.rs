//! Copy engine (T3.1): exactly the algorithm of ADR-0004.
//!
//! `run_copy`: parse `opts.rename_mask` (`JobError::Invalid` before any I/O), resolve `sources`
//! (`plan::local_paths`), validate target names, `plan::check_destination`, `plan::scan` (planning
//! progress), `ctx.set_totals`, create a missing destination directory, then copy every top-level
//! item with a [`Copier`].
//!
//! Per regular file (`file.rs`): open the source `O_NOFOLLOW` (unless following symlinks) and
//! `fstat` it; create `.<name>.ofm-part-<suffix>` in the destination directory with
//! `O_CREAT|O_EXCL` mode 0600; data via FICLONE -> `copy_file_range` -> 1 MiB read/write (sparse
//! sources via SEEK_DATA/SEEK_HOLE + `set_len`), `ctx.checkpoint()` before every chunk of at most
//! 1 MiB; metadata on the temp file (`meta.rs`); fsync when `sync`; verify; publish with
//! `rename_noreplace`, or `rename_replace` only after an `Overwrite` resolution; fsync the
//! directory when `sync`. The temp file is unlinked on EVERY error/cancel path (explicitly, and by
//! a drop guard for panics). An existing destination is never opened for writing.
//!
//! Directories are created with mode 0700, filled (iteratively, so deep trees cannot overflow the
//! stack), and get their final mode/owner/times post-order (also on cancel, best effort).
//! Symlinks are recreated through a temp link + rename unless `follow_symlinks`. FIFOs, sockets
//! and devices are skipped with a warning. Hardlinks inside the set become separate files.
//!
//! The same [`Copier`] in [`Mode::Move`] is the cross-device half of `move_`: always fsync, verify
//! size/mtime and source stability before publishing, and only then delete that one source item
//! (after checking it is still the same inode); directories are removed post-order when empty.

mod file;
mod meta;

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use ofm_core::Location;

use crate::ctx::{ErrorResolution, JobCtx, Resolution};
use crate::error::JobError;
use crate::event::{ConflictKind, ItemInfo, Operation};
use crate::fs::{FileType, Fs, FsStat};
use crate::plan;
use crate::spec::CopyOpts;

/// Data chunk size: buffer of the read/write fallback, the `copy_file_range` request size, and
/// therefore the cancellation granularity.
pub(crate) const CHUNK: usize = 1 << 20;

/// `ENOTEMPTY` (a source directory that still holds skipped items is left alone).
const ENOTEMPTY: i32 = 39;

/// Copy `sources` into `dest_dir`.
pub fn run_copy(
    ctx: &mut JobCtx,
    sources: &[Location],
    dest_dir: &Path,
    opts: &CopyOpts,
) -> Result<(), JobError> {
    let mask = plan::parse_mask(opts.rename_mask.as_deref())?;
    let sources = plan::local_paths(sources)?;
    let names = plan::target_names(&sources, mask.as_ref())?;
    plan::check_destination(ctx, &sources, dest_dir, opts.follow_symlinks)?;
    let totals = plan::scan(ctx, &sources, opts.follow_symlinks)?;
    ctx.set_totals(totals.files, totals.bytes);
    plan::ensure_dir(ctx, dest_dir)?;
    let mut copier = Copier::new(&*ctx.fs(), Mode::Copy, opts.follow_symlinks, opts.sync);
    for (src, name) in sources.iter().zip(&names) {
        ctx.checkpoint()?;
        copier.copy_top(ctx, src, &dest_dir.join(name))?;
    }
    Ok(())
}

/// What the [`Copier`] is used for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    /// Plain copy: sources are only read.
    Copy,
    /// Cross-device move: fsync + verify every item, then delete that source item.
    Move,
}

/// Where and how an item will be written, after conflict resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Target {
    /// Nothing there (or a fresh unique name): publish with `rename_noreplace`.
    Fresh(PathBuf),
    /// The user chose overwrite for this item (non-directory over non-directory only): publish
    /// with `rename_replace`.
    Replace(PathBuf),
    /// Directory onto an existing directory: merge into it.
    Merge(PathBuf),
    /// Skipped (already counted in the report).
    Skip,
}

/// Probe `dest` and decide what to do with an existing item there: same dev+ino as the source =
/// `SameFile`; directory onto directory = merge (never a question); directory vs non-directory =
/// `TypeMismatch`; anything else = `Replaceable`. `RenameNew` picks a free `name (N).ext`;
/// `RenameExisting` renames the existing item out of the way (`rename_noreplace`) and probes again.
/// For a `SameFile` conflict `RenameExisting` behaves like `RenameNew` (both keep the original and
/// add a numbered twin) so the source is never renamed under the running job.
pub(crate) fn resolve_target(
    ctx: &mut JobCtx,
    src: &Path,
    src_st: &FsStat,
    dest: &Path,
    op: Operation,
) -> Result<Target, JobError> {
    let fs = ctx.fs();
    loop {
        let existing = match fs.lstat(dest) {
            Ok(st) => st,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Target::Fresh(dest.to_path_buf()));
            }
            Err(e) => match ctx.resolve_error(dest, op, &e)? {
                ErrorResolution::Retry => continue,
                ErrorResolution::Skip => return Ok(Target::Skip),
            },
        };
        let kind = if (existing.dev, existing.ino) == (src_st.dev, src_st.ino) {
            ConflictKind::SameFile
        } else if src_st.is_dir() && existing.is_dir() {
            return Ok(Target::Merge(dest.to_path_buf()));
        } else if src_st.is_dir() != existing.is_dir() {
            ConflictKind::TypeMismatch
        } else {
            ConflictKind::Replaceable
        };
        let resolution = ctx.resolve_conflict(
            ItemInfo::from_stat(src, src_st),
            ItemInfo::from_stat(dest, &existing),
            kind,
        )?;
        match resolution {
            Resolution::Overwrite if kind == ConflictKind::Replaceable => {
                return Ok(Target::Replace(dest.to_path_buf()));
            }
            // `resolve_conflict` never returns this (it enforces `ConflictKind::allows`); be
            // defensive anyway: never replace across types or over the source itself.
            Resolution::Overwrite => {
                ctx.warn(dest, "overwrite not allowed here; skipped");
                ctx.note_skipped();
                return Ok(Target::Skip);
            }
            Resolution::Skip => return Ok(Target::Skip),
            Resolution::RenameNew => return unique_target(ctx, dest, op),
            Resolution::RenameExisting if kind == ConflictKind::SameFile => {
                return unique_target(ctx, dest, op);
            }
            Resolution::RenameExisting => {
                let Target::Fresh(aside) = unique_target(ctx, dest, op)? else {
                    return Ok(Target::Skip);
                };
                if ctx
                    .attempt(dest, Operation::Rename, |fs| {
                        fs.rename_noreplace(dest, &aside)
                    })?
                    .is_none()
                {
                    return Ok(Target::Skip);
                }
                tracing::debug!(from = %dest.display(), to = %aside.display(), "renamed existing");
                // Probe again: normally free now.
            }
        }
    }
}

/// `Fresh(dir/name (N).ext)` with the first free N (lookup errors: retry/skip/abort).
fn unique_target(ctx: &mut JobCtx, dest: &Path, op: Operation) -> Result<Target, JobError> {
    let (Some(dir), Some(name)) = (dest.parent(), dest.file_name()) else {
        return Err(JobError::Invalid(format!(
            "no file name in {}",
            dest.display()
        )));
    };
    Ok(
        match ctx.attempt(dir, op, |fs| plan::unique_name(fs, dir, name))? {
            Some(n) => Target::Fresh(dir.join(n)),
            None => Target::Skip,
        },
    )
}

/// Result of processing one tree node.
enum Node {
    /// A non-directory was written (final path).
    Done(PathBuf),
    /// Nothing was written.
    Skipped,
    /// A directory was created or is being merged into: its children are processed next.
    Dir(Frame),
}

/// A directory being copied.
struct Frame {
    src: PathBuf,
    dest: PathBuf,
    st: FsStat,
    names: Vec<OsString>,
    next: usize,
    /// Created by this job (gets final metadata) rather than merged into.
    created: bool,
}

impl Frame {
    fn id(&self) -> (u64, u64) {
        (self.st.dev, self.st.ino)
    }
}

/// The copy machinery for one job (state: buffer, loop guards, warning dedup).
pub(crate) struct Copier {
    mode: Mode,
    follow: bool,
    sync: bool,
    is_root: bool,
    buf: Vec<u8>,
    /// (dev, ino) of destination directories this job created: never entered as a source (a
    /// followed symlink pointing into the destination would otherwise copy forever).
    created_dirs: HashSet<(u64, u64)>,
    /// (dev, ino) of the source directories currently open on the walk stack (symlink loops).
    active: HashSet<(u64, u64)>,
    /// Metadata warnings already reported (what, errno): later ones only go to the log, so a copy
    /// to a FAT stick does not produce one warning per file.
    warned: HashSet<(&'static str, Option<i32>)>,
}

impl Copier {
    /// Machinery for one job. `sync` is forced on for [`Mode::Move`].
    pub(crate) fn new(fs: &dyn Fs, mode: Mode, follow: bool, sync: bool) -> Self {
        Copier {
            mode,
            follow: follow && mode == Mode::Copy,
            sync: sync || mode == Mode::Move,
            is_root: fs.is_root(),
            buf: Vec::new(),
            created_dirs: HashSet::new(),
            active: HashSet::new(),
            warned: HashSet::new(),
        }
    }

    fn op(&self) -> Operation {
        match self.mode {
            Mode::Copy => Operation::Copy,
            Mode::Move => Operation::Move,
        }
    }

    /// Copy (or cross-device move) one top-level item to `dest`; records the final top-level
    /// path with `ctx.note_created` as soon as it exists (before a directory is filled, so a
    /// cancelled job still reports it).
    pub(crate) fn copy_top(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        dest: &Path,
    ) -> Result<(), JobError> {
        self.copy_item(ctx, src, dest, true)
    }

    /// Like [`copy_top`](Self::copy_top) for an item below the top level (nothing recorded).
    pub(crate) fn copy_nested(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        dest: &Path,
    ) -> Result<(), JobError> {
        self.copy_item(ctx, src, dest, false)
    }

    fn copy_item(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        dest: &Path,
        top: bool,
    ) -> Result<(), JobError> {
        match self.node(ctx, src, dest)? {
            Node::Done(p) if top => ctx.note_created(p),
            Node::Done(_) | Node::Skipped => {}
            Node::Dir(frame) => {
                if top {
                    ctx.note_created(frame.dest.clone());
                }
                self.walk(ctx, frame)?;
            }
        }
        Ok(())
    }

    /// Process one source entry: stat, resolve the destination, write a non-directory or start
    /// a directory.
    fn node(&mut self, ctx: &mut JobCtx, src: &Path, dest: &Path) -> Result<Node, JobError> {
        ctx.set_current(src);
        let follow = self.follow;
        let Some(st) = ctx.attempt(src, self.op(), |fs| {
            plan::stat_maybe_follow(fs, src, follow)
        })?
        else {
            return Ok(Node::Skipped);
        };
        match st.file_type() {
            FileType::Other => {
                ctx.warn(
                    src,
                    "special file (FIFO, socket or device) not copied; skipped",
                );
                return Ok(Node::Skipped);
            }
            FileType::Dir
                if self.created_dirs.contains(&(st.dev, st.ino))
                    || self.active.contains(&(st.dev, st.ino)) =>
            {
                ctx.warn(
                    src,
                    "directory loop (symlink to a parent or to the destination); skipped",
                );
                return Ok(Node::Skipped);
            }
            _ => {}
        }
        loop {
            let (path, replace) = match resolve_target(ctx, src, &st, dest, self.op())? {
                Target::Skip => return Ok(Node::Skipped),
                Target::Merge(d) => {
                    return Ok(match self.list(ctx, src)? {
                        Some(names) => Node::Dir(Frame {
                            src: src.to_path_buf(),
                            dest: d,
                            st,
                            names,
                            next: 0,
                            created: false,
                        }),
                        None => Node::Skipped,
                    });
                }
                Target::Fresh(d) => (d, false),
                Target::Replace(d) => (d, true),
            };
            let result = match st.file_type() {
                FileType::File => self.copy_file(ctx, src, &path, replace)?,
                FileType::Symlink => self.copy_symlink(ctx, src, &st, &path, replace)?,
                FileType::Dir if !replace => match self.start_dir(ctx, src, &st, &path)? {
                    Some(node) => return Ok(node),
                    None => continue, // the name was taken meanwhile: resolve again
                },
                _ => return Ok(Node::Skipped),
            };
            match result {
                file::Written::Done => return Ok(Node::Done(path)),
                file::Written::Skipped => return Ok(Node::Skipped),
                file::Written::Exists => {} // raced with another writer: resolve again
            }
        }
    }

    /// Sorted entry names of a source directory (`None` = skipped by the user).
    fn list(&mut self, ctx: &mut JobCtx, dir: &Path) -> Result<Option<Vec<OsString>>, JobError> {
        let names = ctx.attempt(dir, self.op(), |fs| fs.read_dir(dir))?;
        Ok(names.map(|mut n| {
            n.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
            n
        }))
    }

    /// Create a destination directory (0700 while it is being filled). `Ok(None)` = the name
    /// exists now (race): the caller resolves the conflict again.
    fn start_dir(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        st: &FsStat,
        dest: &Path,
    ) -> Result<Option<Node>, JobError> {
        // List first: an unreadable source directory then creates nothing.
        let Some(names) = self.list(ctx, src)? else {
            return Ok(Some(Node::Skipped));
        };
        let fs = ctx.fs();
        loop {
            ctx.checkpoint()?;
            match fs.mkdir(dest, 0o700) {
                Ok(()) => break,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(None),
                Err(e) => match ctx.resolve_error(dest, self.op(), &e)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => return Ok(Some(Node::Skipped)),
                },
            }
        }
        match fs.lstat(dest) {
            Ok(d) => {
                self.created_dirs.insert((d.dev, d.ino));
            }
            Err(e) => tracing::debug!(path = %dest.display(), error = %e, "lstat new dir"),
        }
        if self.sync
            && let Some(parent) = dest.parent()
            && let Err(e) = fs.sync_dir(parent)
        {
            self.meta_warn(ctx, parent, "directory not synced", &e);
        }
        self.copy_xattrs(ctx, &*fs, src, dest);
        Ok(Some(Node::Dir(Frame {
            src: src.to_path_buf(),
            dest: dest.to_path_buf(),
            st: st.clone(),
            names,
            next: 0,
            created: true,
        })))
    }

    /// Copy the children of `root` depth-first with an explicit stack; post-order metadata and
    /// (move) source directory removal. On error/cancel the directories still on the stack get
    /// their final metadata (best effort) before the error propagates.
    fn walk(&mut self, ctx: &mut JobCtx, root: Frame) -> Result<(), JobError> {
        self.active.insert(root.id());
        let mut stack = vec![root];
        let result = self.walk_stack(ctx, &mut stack);
        if result.is_err() {
            while let Some(f) = stack.pop() {
                self.active.remove(&f.id());
                if f.created {
                    self.dir_meta(ctx, &f);
                }
            }
        }
        result
    }

    fn walk_stack(&mut self, ctx: &mut JobCtx, stack: &mut Vec<Frame>) -> Result<(), JobError> {
        loop {
            let Some(top) = stack.last_mut() else {
                return Ok(());
            };
            if let Some(name) = top.names.get(top.next).cloned() {
                top.next += 1;
                let (s, d) = (top.src.join(&name), top.dest.join(&name));
                ctx.checkpoint()?;
                if let Node::Dir(f) = self.node(ctx, &s, &d)? {
                    self.active.insert(f.id());
                    stack.push(f);
                }
            } else if let Some(f) = stack.pop() {
                self.active.remove(&f.id());
                self.finish_dir(ctx, &f)?;
            }
        }
    }

    /// Post-order step of a directory: final metadata; in move mode remove the (now empty) source.
    fn finish_dir(&mut self, ctx: &mut JobCtx, f: &Frame) -> Result<(), JobError> {
        if f.created {
            self.dir_meta(ctx, f);
        }
        if self.mode != Mode::Move {
            return Ok(());
        }
        let fs = ctx.fs();
        loop {
            ctx.checkpoint()?;
            match fs.rmdir(&f.src) {
                Ok(()) => return Ok(()),
                // Some children were skipped or not copied: they stay where they are.
                Err(e)
                    if e.raw_os_error() == Some(ENOTEMPTY)
                        || e.kind() == io::ErrorKind::AlreadyExists =>
                {
                    tracing::debug!(path = %f.src.display(), "source dir not empty; kept");
                    return Ok(());
                }
                Err(e) => match ctx.resolve_error(&f.src, Operation::Move, &e)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => return Ok(()),
                },
            }
        }
    }
}

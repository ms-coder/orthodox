//! Extract archive entries (M6, T6.2; ADR-0008). Frozen request types: `spec::ExtractOpts` and
//! `JobSpec::Extract`; the outcome is the ordinary `Outcome::Ok(Report)` (`Report::created` = the
//! top-level destination paths, warnings = skipped unsafe/special entries and per-entry errors).
//!
//! Protocol (same as copy, ADR-0004; read ADR-0008 "Extraction"):
//! 1. open the archive with `ofm_vfs::archive::open_archive` (password via `opts.password`),
//!    `index.select(entries)` -> ids; unsafe entries are never selected (one warning each);
//! 2. one sequential `ArchiveReader::visit` pass in archive order. Every entry is written through
//!    `ctx.fs()`: files to `.<name>.ofm-part-<random>` in the destination directory (O_EXCL, 0600),
//!    the stream copied with `ctx.checkpoint()` before every read of at most 1 MiB, size verified,
//!    mode (`& 0o777`, never setuid/setgid/sticky, never uid/gid) and mtime set, then
//!    `renameat2(NOREPLACE)` (or `rename` after an explicit overwrite). A conflict asks through
//!    `ctx.resolve_conflict` exactly like copy ([`crate::copy::resolve_target`]); failure, cancel
//!    and panic unlink the temp file;
//! 3. directories are created 0700 and get their final mode/mtime AFTER the pass, deepest first
//!    (also when the job fails or is cancelled), so a read-only directory entry cannot lock the job
//!    out of its own output;
//! 4. symlinks are recreated only if their target stays inside the destination (see
//!    [`safe::link_target_ok`]); hard links only to a file this job extracted; devices, FIFOs and
//!    sockets are never created.
//!
//! Anchoring (FIX-M6M7): once the destination directory exists, every call below it goes through
//! `fs::anchored::AnchoredFs`: parent directories are pinned handles opened component by component
//! without following symlinks, so replacing a checked parent with a symlink during the job cannot
//! redirect anything. Symlinks in the destination path chosen by the user are followed once.
//!
//! Defence in depth (archives are hostile input): the index flags are not trusted. Every output
//! path is re-validated from its bytes ([`safe::validate_entry_path`]), every parent directory
//! below the destination is checked with `lstat` and created by the job itself (a symlink or file
//! in the way is a `TypeMismatch` conflict, never followed), files are created `O_EXCL|O_NOFOLLOW`
//! under a temp name and published without replacing. Decompression bombs are stopped while
//! STREAMING: bytes actually written are counted against the declared size, the per-entry and
//! overall compression ratio and the total cap of [`ArchiveLimits`](ofm_vfs::archive::ArchiveLimits)
//! (declared sizes lie).
//!
//! Error policy: an unreadable/corrupt entry (CRC mismatch, truncated stream) is a warning, its
//! temp file is removed and the job goes on; a wrong/missing password, a limit, a failing
//! destination (ENOSPC, EIO, EROFS, quota) or a cancel stops the job. The data of an entry is
//! consumed in one pass, so a destination write error cannot be retried: it fails the job.
//!
//! Modules: `safe` (pure path rules), `dirs` (directory chain + final metadata), `write` (files,
//! symlinks, hard links).

mod dirs;
mod safe;
mod write;

use std::collections::{HashMap, HashSet};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_vfs::archive::{
    ArchiveEntry, ArchiveEntryKind, ArchiveError, ArchiveLimits, ArchiveReader, EntryId,
    EntrySafety, normalize_entry_path, open_archive,
};

use crate::copy::{Copier, Mode};
use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::fs::{AnchoredFs, Fs};
use crate::plan;
use crate::spec::ExtractOpts;

use dirs::DirInfo;

/// Entry-level failures in a row after which the job gives up (a destination that rejects
/// everything, e.g. a read-only directory, must not produce thousands of warnings).
const MAX_CONSECUTIVE_FAILURES: u32 = 50;

/// At most this many "unsafe entry skipped" warnings are listed one by one.
const MAX_UNSAFE_WARNINGS: usize = 100;

/// Extract `entries` of `archive` into `dest_dir` (empty `entries` = everything). Opens the
/// archive on this (worker) thread; the password provider of `opts.password` may block.
pub fn run_extract(
    ctx: &mut JobCtx,
    archive: &Path,
    entries: &[PathBuf],
    dest_dir: &Path,
    opts: &ExtractOpts,
) -> Result<(), JobError> {
    if !archive.is_absolute() || !dest_dir.is_absolute() {
        return Err(JobError::Invalid(
            "archive and destination must be absolute paths".into(),
        ));
    }
    let mut reader = open_archive(archive, &opts.password, &opts.limits, &ctx.cancel_token())?;
    extract_from_reader(ctx, &mut *reader, entries, dest_dir, opts)
}

/// [`run_extract`] on an already opened reader. Public so tests can drive the job with an
/// in-memory [`ArchiveReader`]; the app goes through `JobSpec::Extract`.
pub fn extract_from_reader(
    ctx: &mut JobCtx,
    reader: &mut dyn ArchiveReader,
    entries: &[PathBuf],
    dest_dir: &Path,
    opts: &ExtractOpts,
) -> Result<(), JobError> {
    let index = Arc::clone(reader.index());
    let archive = index.archive().to_path_buf();
    let selection = normalize_selection(entries)?;
    let ids = index.select(&selection);
    report_unsafe(ctx, &index, &selection);
    if ids.is_empty() {
        ctx.warn(&archive, "nothing to extract: no matching entries");
        return Ok(());
    }

    // Totals (declared sizes) for progress, the up-front limit and the free-space check.
    let (mut files, mut bytes) = (0u64, 0u64);
    for e in ids.iter().filter_map(|i| index.get(*i)) {
        match e.kind {
            ArchiveEntryKind::Dir => {}
            ArchiveEntryKind::File | ArchiveEntryKind::Hardlink => {
                files += 1;
                bytes = bytes.saturating_add(e.size);
            }
            _ => files += 1,
        }
    }
    if bytes > opts.limits.warn_total_bytes {
        return Err(JobError::Archive(format!(
            "{}: declared unpacked size of {bytes} bytes exceeds the limit of {} bytes; nothing extracted",
            archive.display(),
            opts.limits.warn_total_bytes
        )));
    }
    if index.warnings().suspicious_ratio_entries > 0 {
        ctx.warn(
            &archive,
            format!(
                "{} entries have a suspicious compression ratio; extraction stops if one exceeds the limit",
                index.warnings().suspicious_ratio_entries
            ),
        );
    }
    ctx.set_totals(files, bytes);
    plan::ensure_dir(ctx, dest_dir)?;
    warn_if_low_on_space(ctx, dest_dir, bytes);

    let anchor = AnchoredFs::new(ctx.fs(), dest_dir).map_err(|e| JobError::io(dest_dir, e))?;
    let anchor = Arc::new(anchor);
    let mut ex = Extractor::new(
        ctx,
        Arc::clone(&anchor),
        &archive,
        dest_dir,
        selection,
        &ids,
        opts,
    );
    // The conflict protocol uses the context's file system: anchor it too.
    let unanchored = ctx.swap_fs(anchor);
    let cancel = ctx.cancel_token();
    let mut failure: Option<JobError> = None;
    let visited = reader.visit(&ids, &cancel, &mut |entry, data| {
        ex.entry(ctx, entry, data).map_err(|e| {
            failure = Some(e);
            // Any error aborts the visit; the real cause travels in `failure`.
            ArchiveError::Cancelled
        })
    });
    // Directory metadata after the content, also on failure/cancel (best effort).
    ex.finish_dirs(ctx);
    ctx.swap_fs(unanchored);
    match (failure, visited) {
        (Some(e), _) => Err(e),
        (None, Err(e)) => Err(e.into()),
        (None, Ok(())) => Ok(()),
    }
}

/// Normalise the user's selection; empty = the whole archive.
fn normalize_selection(entries: &[PathBuf]) -> Result<Vec<PathBuf>, JobError> {
    if entries.is_empty() {
        return Ok(vec![PathBuf::new()]);
    }
    entries
        .iter()
        .map(|p| {
            normalize_entry_path(p).map_err(|why| {
                JobError::Invalid(format!("bad archive path {}: {why:?}", p.display()))
            })
        })
        .collect()
}

/// One warning per unsafe entry below the selection (raw path in the message; the warning's path
/// is the archive, so no consumer can mistake a hostile name for a real location).
fn report_unsafe(ctx: &mut JobCtx, index: &ofm_vfs::archive::ArchiveIndex, selection: &[PathBuf]) {
    let mut n = 0usize;
    for e in index.unsafe_entries() {
        if !selection.iter().any(|s| e.path.starts_with(s)) {
            continue;
        }
        n += 1;
        if n <= MAX_UNSAFE_WARNINGS {
            let why = match e.safety {
                EntrySafety::Unsafe(r) => format!("{r:?}"),
                EntrySafety::Safe => String::new(),
            };
            ctx.warn(
                index.archive(),
                format!("unsafe entry '{}' skipped ({why})", e.path.display()),
            );
        }
        ctx.note_skipped();
    }
    if n > MAX_UNSAFE_WARNINGS {
        ctx.warn(
            index.archive(),
            format!("{} more unsafe entries skipped", n - MAX_UNSAFE_WARNINGS),
        );
    }
}

/// Warn (never refuse) when the declared size does not fit the destination file system.
fn warn_if_low_on_space(ctx: &mut JobCtx, dest_dir: &Path, needed: u64) {
    // The `Fs` seam has no statvfs; this is a read-only query of the destination directory the
    // user chose, and only ever produces a warning.
    if let Ok(v) = rustix::fs::statvfs(dest_dir) {
        let avail = v.f_bavail.saturating_mul(v.f_frsize);
        if needed > avail {
            ctx.warn(
                dest_dir,
                format!("not enough free space: {needed} bytes needed, {avail} available"),
            );
        }
    }
}

/// Why one entry did not make it. `Job` stops everything, `Entry` is a warning and the job goes on.
pub(crate) enum Fail {
    /// Cancel, abort, or an error that ends the job.
    Job(JobError),
    /// Problem with this entry only.
    Entry {
        /// Path concerned (a destination path).
        path: PathBuf,
        /// What happened.
        msg: String,
    },
}

impl From<JobError> for Fail {
    fn from(e: JobError) -> Self {
        Fail::Job(e)
    }
}

impl Fail {
    pub(crate) fn entry(path: &Path, msg: impl Into<String>) -> Fail {
        Fail::Entry {
            path: path.to_path_buf(),
            msg: msg.into(),
        }
    }
}

/// Where a file's bytes are expected to end.
pub(crate) struct Extracted {
    /// Final path on disk.
    pub(crate) path: PathBuf,
    /// `(dev, ino)` of the published file.
    pub(crate) id: (u64, u64),
    /// Size in bytes.
    pub(crate) size: u64,
}

/// State of one extraction job.
pub(crate) struct Extractor {
    /// The anchored file system (the same object as `anchor`).
    pub(crate) fs: Arc<dyn Fs>,
    /// Holds parent directory handles for the duration of an item.
    pub(crate) anchor: Arc<AnchoredFs>,
    pub(crate) dest_dir: PathBuf,
    pub(crate) archive: PathBuf,
    /// `(dev, ino)` of the archive file: makes extracting an entry onto the archive itself a
    /// `SameFile` conflict (never overwritable).
    pub(crate) archive_id: (u64, u64),
    pub(crate) archive_size: u64,
    selection: Vec<PathBuf>,
    selected: HashSet<EntryId>,
    pub(crate) limits: ArchiveLimits,
    pub(crate) sync: bool,
    /// Tracks what this job published (see [`crate::copy::resolve_target`]).
    pub(crate) copier: Copier,
    /// Directories of the output tree by relative path.
    pub(crate) dirs: HashMap<PathBuf, DirInfo>,
    /// Relative directories the user skipped (everything below is skipped silently).
    pub(crate) blocked: HashSet<PathBuf>,
    /// Files published by this job, by archive path (hard link targets).
    pub(crate) extracted: HashMap<PathBuf, Extracted>,
    pub(crate) buf: Vec<u8>,
    /// Bytes actually written so far, all entries.
    pub(crate) total_written: u64,
    pub(crate) warned: HashSet<(&'static str, Option<i32>)>,
    failures_in_a_row: u32,
}

impl Extractor {
    fn new(
        ctx: &JobCtx,
        anchor: Arc<AnchoredFs>,
        archive: &Path,
        dest_dir: &Path,
        selection: Vec<PathBuf>,
        ids: &[EntryId],
        opts: &ExtractOpts,
    ) -> Self {
        // The archive is looked up as the user named it, not through the anchor.
        // Unknown identity (cannot stat) = one that matches nothing.
        let (archive_id, archive_size) = match ctx.fs().stat(archive) {
            Ok(st) => ((st.dev, st.ino), st.size),
            Err(_) => ((u64::MAX, u64::MAX), 0),
        };
        let copier = Copier::new(&*anchor, Mode::Copy, false, opts.sync);
        let fs: Arc<dyn Fs> = anchor.clone();
        Extractor {
            fs,
            anchor,
            dest_dir: dest_dir.to_path_buf(),
            archive: archive.to_path_buf(),
            archive_id,
            archive_size,
            selection,
            selected: ids.iter().copied().collect(),
            limits: opts.limits,
            sync: opts.sync,
            copier,
            dirs: HashMap::new(),
            blocked: HashSet::new(),
            extracted: HashMap::new(),
            buf: Vec::new(),
            total_written: 0,
            warned: HashSet::new(),
            failures_in_a_row: 0,
        }
    }

    /// Handle one visited entry; per-entry problems become warnings.
    fn entry(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        data: &mut dyn Read,
    ) -> Result<(), JobError> {
        ctx.checkpoint()?;
        match self.dispatch(ctx, e, data) {
            Ok(()) => Ok(()),
            Err(Fail::Job(j)) => Err(j),
            Err(Fail::Entry { path, msg }) => {
                ctx.warn(path, msg);
                ctx.note_skipped();
                self.failures_in_a_row += 1;
                if self.failures_in_a_row >= MAX_CONSECUTIVE_FAILURES {
                    return Err(JobError::Archive(format!(
                        "{}: too many entries failed in a row; stopped",
                        self.archive.display()
                    )));
                }
                Ok(())
            }
        }
    }

    /// A real success (something was written): resets the failure streak.
    pub(crate) fn succeeded(&mut self) {
        self.failures_in_a_row = 0;
    }

    fn dispatch(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        data: &mut dyn Read,
    ) -> Result<(), Fail> {
        if !self.selected.contains(&e.id) {
            return self.skip_entry(
                ctx,
                format!(
                    "reader produced the unselected entry '{}'; skipped",
                    e.path.display()
                ),
            );
        }
        if let EntrySafety::Unsafe(r) = e.safety {
            return self.skip_entry(
                ctx,
                format!("unsafe entry '{}' skipped ({r:?})", e.path.display()),
            );
        }
        if let Err(why) = safe::validate_entry_path(&e.path) {
            return self.skip_entry(
                ctx,
                format!("entry '{}' skipped: {}", e.path.display(), why.describe()),
            );
        }
        // Each selected item lands directly in the destination (outermost selection wins).
        let rel = self
            .selection
            .iter()
            .filter_map(|s| safe::relative_to_selection(&e.path, s).map(|r| (s, r)))
            .min_by_key(|(s, _)| s.components().count())
            .map(|(_, r)| r);
        let Some(rel) = rel else {
            return self.skip_entry(
                ctx,
                format!(
                    "entry '{}' is outside the selection; skipped",
                    e.path.display()
                ),
            );
        };
        match e.kind {
            ArchiveEntryKind::Dir => self.do_dir(ctx, e, &rel),
            ArchiveEntryKind::File => self.do_file(ctx, e, &rel, data, e.size),
            ArchiveEntryKind::Symlink => self.do_symlink(ctx, e, &rel),
            ArchiveEntryKind::Hardlink => self.do_hardlink(ctx, e, &rel, data),
            ArchiveEntryKind::Other => self.skip_entry(
                ctx,
                format!(
                    "special file '{}' (device, FIFO or socket) is never extracted; skipped",
                    e.path.display()
                ),
            ),
        }
    }

    /// Skip an entry with a warning (counted as skipped; the warning names the archive).
    fn skip_entry(&self, ctx: &mut JobCtx, msg: String) -> Result<(), Fail> {
        ctx.warn(&self.archive, msg);
        ctx.note_skipped();
        Ok(())
    }

    /// Report a metadata problem once per (what, errno); repeats go to the log.
    pub(crate) fn meta_warn(
        &mut self,
        ctx: &mut JobCtx,
        path: &Path,
        what: &'static str,
        e: &io::Error,
    ) {
        if self.warned.insert((what, e.raw_os_error())) {
            ctx.warn(
                path,
                format!("{what}: {e} (further similar warnings are not repeated)"),
            );
        } else {
            tracing::debug!(path = %path.display(), error = %e, what, "metadata not set");
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ctx::ScriptedSink;
    use crate::event::Outcome;
    use crate::fs::RealFs;
    use crate::runner::run_sync;
    use crate::spec::JobSpec;

    #[test]
    fn missing_archive_fails_cleanly_with_a_typed_error() {
        let (sink, _) = ScriptedSink::refusing();
        let spec = JobSpec::Extract {
            archive: "/nonexistent-ofm-dir/a.zip".into(),
            entries: vec![],
            dest_dir: "/nonexistent-ofm-dir/dst".into(),
            opts: ExtractOpts::default(),
        };
        assert_eq!(spec.title(), "Extract 'a.zip' to /nonexistent-ofm-dir/dst");
        let out = run_sync(&spec, Arc::new(RealFs), sink);
        assert!(matches!(out, Outcome::Failed { .. }), "{out:?}");
    }

    #[test]
    fn relative_paths_are_invalid() {
        let (sink, _) = ScriptedSink::refusing();
        let spec = JobSpec::Extract {
            archive: "a.zip".into(),
            entries: vec![],
            dest_dir: "dst".into(),
            opts: ExtractOpts::default(),
        };
        let out = run_sync(&spec, Arc::new(RealFs), sink);
        assert!(
            matches!(&out, Outcome::Failed { error, .. } if error.contains("invalid request")),
            "{out:?}"
        );
    }
}

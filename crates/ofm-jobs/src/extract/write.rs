//! Writing non-directory entries: regular files (temp name -> data -> metadata -> publish),
//! symlinks and hard links. Same algorithm as `copy/file.rs` (ADR-0004), but the source is the
//! archive's one-pass stream, so a failed destination write cannot be retried and a conflict is
//! resolved BEFORE any data is written.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_vfs::archive::{ArchiveEntry, ArchiveError};

use crate::copy::noreplace;
use crate::copy::{CHUNK, Target, resolve_target};
use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::event::Operation;
use crate::fs::{FileType, Fs, FsFile, FsStat};
use crate::plan::NAME_MAX;

use super::safe;
use super::{Extracted, Extractor, Fail};

/// Attempts to find a free temp name before giving up.
const TEMP_TRIES: u32 = 16;

// Linux errno values.
const EIO: i32 = 5;
const ENFILE: i32 = 23;
const EMFILE: i32 = 24;
const ENOSPC: i32 = 28;
const EROFS: i32 = 30;
const EDQUOT: i32 = 122;
const EPERM: i32 = 1;
const EXDEV: i32 = 18;
const EINVAL: i32 = 22;
const EMLINK: i32 = 31;
const ENOSYS: i32 = 38;
const EOPNOTSUPP: i32 = 95;

/// Errors that make every further write pointless: disk full, quota, read-only file system, I/O
/// error, out of descriptors. They stop the job instead of becoming a warning per entry.
fn fatal_errno(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(EIO | ENOSPC | EROFS | EDQUOT | ENFILE | EMFILE)
    ) || e.kind() == io::ErrorKind::StorageFull
}

/// Classify an error on the destination: fatal ones end the job, others only this entry.
pub(super) fn dest_fail(path: &Path, e: io::Error) -> Fail {
    if fatal_errno(&e) {
        Fail::Job(JobError::io(path, e))
    } else {
        Fail::entry(path, e.to_string())
    }
}

/// `link(2)` is not available for this file (FAT, some FUSE) or not across these directories.
fn link_unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(EPERM | EXDEV | EINVAL | EMLINK | ENOSYS | EOPNOTSUPP)
    ) || e.kind() == io::ErrorKind::Unsupported
}

/// `.<name>.ofm-part-<suffix>`, with `name` shortened (bytewise) to stay within `NAME_MAX`.
pub(super) fn temp_name(name: &OsStr, suffix: &str) -> OsString {
    const MID: &[u8] = b".ofm-part-";
    let budget = NAME_MAX.saturating_sub(1 + MID.len() + suffix.len());
    let b = name.as_bytes();
    let b = &b[..b.len().min(budget)];
    let mut out = Vec::with_capacity(NAME_MAX);
    out.push(b'.');
    out.extend_from_slice(b);
    out.extend_from_slice(MID);
    out.extend_from_slice(suffix.as_bytes());
    OsString::from_vec(out)
}

/// Write all of `data` (short writes continue; `Ok(0)` is an error).
fn write_all(f: &mut dyn FsFile, mut data: &[u8]) -> io::Result<()> {
    while !data.is_empty() {
        match f.write(data) {
            Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero)),
            Ok(n) => data = &data[n.min(data.len())..],
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Unlinks the temp file unless disarmed (explicit cleanup reports failures; drop is the panic
/// safety net: a panic in the archive decoder unwinds through here).
pub(super) struct TempGuard {
    fs: Arc<dyn Fs>,
    path: Option<PathBuf>,
}

impl TempGuard {
    fn new(fs: Arc<dyn Fs>, path: PathBuf) -> Self {
        TempGuard {
            fs,
            path: Some(path),
        }
    }

    fn path(&self) -> &Path {
        self.path.as_deref().unwrap_or(Path::new(""))
    }

    /// The temp is now the published item.
    fn disarm(&mut self) {
        self.path = None;
    }

    /// Remove the temp now (tried twice); a failure is a job warning.
    pub(super) fn cleanup(&mut self, ctx: &mut JobCtx) {
        let Some(p) = self.path.take() else { return };
        let mut last = None;
        for _ in 0..2 {
            match self.fs.unlink(&p) {
                Ok(()) => return,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return,
                Err(e) => last = Some(e),
            }
        }
        if let Some(e) = last {
            ctx.warn(&p, format!("could not remove temporary file: {e}"));
        }
    }
}

impl Drop for TempGuard {
    fn drop(&mut self) {
        if let Some(p) = self.path.take()
            && let Err(e) = self.fs.unlink(&p)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(path = %p.display(), error = %e, "could not remove temporary file");
        }
    }
}

/// A resolved destination for one non-directory entry.
struct Prepared {
    parent: PathBuf,
    name: OsString,
    /// Final path (after a conflict rename).
    path: PathBuf,
    /// The user chose overwrite: publish with `rename_replace`.
    replace: bool,
}

impl Extractor {
    /// Fake `lstat` of an archive entry for the conflict protocol. Its `(dev, ino)` is the
    /// archive file's, so an entry that would land on the archive itself is a `SameFile` conflict.
    pub(crate) fn fake_stat(&self, ft: FileType, perm: u32, size: u64, mtime_ns: i128) -> FsStat {
        let type_bits = match ft {
            FileType::File => 0o100_000,
            FileType::Dir => 0o040_000,
            FileType::Symlink => 0o120_000,
            FileType::Other => 0,
        };
        FsStat {
            dev: self.archive_id.0,
            ino: self.archive_id.1,
            mode: type_bits | (perm & 0o7777),
            nlink: 1,
            size,
            blocks: 0,
            uid: 0,
            gid: 0,
            atime_ns: mtime_ns,
            mtime_ns,
        }
    }

    /// Parent directory, destination path and conflict resolution of one non-directory entry.
    /// `None` = skipped (already counted).
    fn prepare(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        rel: &Path,
        ft: FileType,
        size: u64,
    ) -> Result<Option<Prepared>, Fail> {
        let Some(name) = rel.file_name() else {
            return Err(Fail::entry(rel, "entry has no name"));
        };
        let Some(parent) = self.parent_dir(ctx, rel.parent().unwrap_or(Path::new("")))? else {
            ctx.note_skipped();
            return Ok(None);
        };
        let dest = parent.join(name);
        ctx.set_current(&dest);
        let st = self.fake_stat(ft, e.mode, size, e.mtime_ns);
        let src = self.archive.join(&e.path);
        let target = resolve_target(
            ctx,
            &src,
            &st,
            &dest,
            Operation::Copy,
            self.copier.published(),
        )?;
        let (path, replace) = match target {
            Target::Fresh(p) => (p, false),
            Target::Replace(p) => (p, true),
            // Skip is already counted; a merge cannot happen for a non-directory.
            Target::Skip | Target::Merge(_) => return Ok(None),
        };
        Ok(Some(Prepared {
            parent,
            name: name.to_os_string(),
            path,
            replace,
        }))
    }

    /// Create the temp file in `parent` (fresh suffix on a name collision).
    fn create_temp(
        &self,
        ctx: &JobCtx,
        parent: &Path,
        name: &OsStr,
    ) -> Result<(PathBuf, Box<dyn FsFile>), Fail> {
        let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
        for _ in 0..TEMP_TRIES {
            let tmp = parent.join(temp_name(name, &ctx.unique_suffix()));
            match self.fs.create_excl(&tmp, 0o600) {
                Ok(f) => return Ok((tmp, f)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
                Err(e) => return Err(dest_fail(&parent.join(name), e)),
            }
        }
        Err(dest_fail(&parent.join(name), last))
    }

    /// Rename the finished temp into place: no-replace (with the link/checked-rename fallbacks
    /// of `copy::noreplace` for NFS/FUSE), or replace after an explicit overwrite.
    fn publish(
        &mut self,
        ctx: &mut JobCtx,
        guard: &mut TempGuard,
        dest: &Path,
        replace: bool,
        disarm: bool,
    ) -> Result<(), Fail> {
        ctx.checkpoint()?;
        let tmp = guard.path().to_path_buf();
        let r = if replace {
            self.fs.rename_replace(&tmp, dest)
        } else {
            noreplace::rename_noreplace(&*self.fs, &tmp, dest, false)
        };
        match r {
            Ok(()) => {
                if disarm {
                    guard.disarm();
                }
                Ok(())
            }
            Err(e) if !replace && e.kind() == io::ErrorKind::AlreadyExists => Err(Fail::entry(
                dest,
                "the name appeared while extracting; skipped (nothing was overwritten)",
            )),
            Err(e) => Err(dest_fail(dest, e)),
        }
    }

    /// After a successful publish: durability, bookkeeping, progress.
    #[allow(
        clippy::too_many_arguments,
        reason = "bookkeeping of one finished item"
    )]
    fn record(
        &mut self,
        ctx: &mut JobCtx,
        p: &Prepared,
        durable: bool,
        id: (u64, u64),
        size: u64,
        rel: &Path,
        key: Option<&Path>,
    ) {
        if durable && let Err(e) = self.fs.sync_dir(&p.parent) {
            ctx.warn(&p.parent, format!("directory not synced: {e}"));
        }
        self.copier.note_published(id);
        self.copier.note_published_path(&p.path);
        if let Some(k) = key {
            self.extracted.insert(
                k.to_path_buf(),
                Extracted {
                    path: p.path.clone(),
                    id,
                    size,
                },
            );
        }
        if rel.components().count() == 1 {
            ctx.note_created(&p.path);
        }
        ctx.file_done();
        self.succeeded();
    }

    // ---- regular files -------------------------------------------------------------------

    /// Extract the regular file `e` (`expected` bytes of data) to `rel`.
    pub(crate) fn do_file(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        rel: &Path,
        data: &mut dyn Read,
        expected: u64,
    ) -> Result<(), Fail> {
        let Some(p) = self.prepare(ctx, e, rel, FileType::File, expected)? else {
            return Ok(());
        };
        let (tmp, df) = self.create_temp(ctx, &p.parent, &p.name)?;
        let mut guard = TempGuard::new(Arc::clone(&self.fs), tmp);
        // An overwrite always makes the new file durable before it replaces the old one.
        let durable = self.sync || p.replace;
        let filled = self.fill_temp(ctx, e, data, df, guard.path(), &p.path, expected, durable);
        let (id, written) = match filled {
            Ok(v) => v,
            Err(f) => {
                guard.cleanup(ctx);
                return Err(f);
            }
        };
        if let Err(f) = self.publish(ctx, &mut guard, &p.path, p.replace, true) {
            guard.cleanup(ctx);
            return Err(f);
        }
        self.record(ctx, &p, durable, id, written, rel, Some(&e.path));
        Ok(())
    }

    /// Stream `data` into the open temp file, then metadata, fsync, verify, checked close.
    /// Returns the identity of the temp file and the bytes written.
    #[allow(
        clippy::too_many_arguments,
        reason = "one linear algorithm, kept in one place"
    )]
    fn fill_temp(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        data: &mut dyn Read,
        mut df: Box<dyn FsFile>,
        tmp: &Path,
        dest: &Path,
        expected: u64,
        durable: bool,
    ) -> Result<((u64, u64), u64), Fail> {
        if self.buf.len() < CHUNK {
            self.buf.resize(CHUNK, 0);
        }
        let mut written = 0u64;
        loop {
            // Pause point + cancel before every read of at most 1 MiB.
            ctx.checkpoint()?;
            let n = match data.read(&mut self.buf) {
                Ok(0) => break,
                Ok(n) => n.min(self.buf.len()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(read_fail(ctx, dest, err)),
            };
            written += n as u64;
            self.check_stream_limits(e, written, n as u64, expected)?;
            // A destination write error cannot be retried (the stream is consumed once): stop.
            if let Err(err) = write_all(&mut *df, &self.buf[..n]) {
                return Err(Fail::Job(JobError::io(dest, err)));
            }
            ctx.add_bytes(n as u64);
        }
        if written != expected {
            return Err(Fail::entry(
                dest,
                format!("entry is truncated: {written} of {expected} bytes"),
            ));
        }
        // Mode never carries setuid/setgid/sticky; uid/gid are never applied.
        if let Err(err) = df.set_mode(e.mode & 0o777) {
            self.meta_warn(ctx, dest, "permissions not set", &err);
        }
        if e.mtime_ns != 0
            && let Err(err) = df.set_times(e.mtime_ns, e.mtime_ns)
        {
            self.meta_warn(ctx, dest, "timestamps not set", &err);
        }
        if durable && let Err(err) = df.sync() {
            return Err(Fail::Job(JobError::io(dest, err)));
        }
        let st = df.stat().map_err(|err| dest_fail(dest, err))?;
        if st.size != written {
            return Err(Fail::entry(
                dest,
                format!(
                    "size on disk ({}) differs from the data written ({written})",
                    st.size
                ),
            ));
        }
        // Checked close: NFS/FUSE may report a deferred write error only here.
        if let Err(err) = df.close() {
            return Err(Fail::Job(JobError::io(tmp, err)));
        }
        Ok(((st.dev, st.ino), written))
    }

    /// Decompression-bomb guards on the bytes actually written (declared sizes lie).
    fn check_stream_limits(
        &mut self,
        e: &ArchiveEntry,
        written: u64,
        n: u64,
        expected: u64,
    ) -> Result<(), Fail> {
        let archive = &self.archive;
        if written > expected {
            return Err(bomb(
                archive,
                format!(
                    "entry '{}' produces more data than its declared size of {expected} bytes",
                    e.path.display()
                ),
            ));
        }
        self.total_written = self.total_written.saturating_add(n);
        if self.total_written > self.limits.warn_total_bytes {
            return Err(bomb(
                archive,
                format!("more than {} bytes extracted", self.limits.warn_total_bytes),
            ));
        }
        let ratio = |out: u64, packed: u64| out / packed.max(1);
        match e.packed_size {
            Some(packed) => {
                if written >= self.limits.ratio_min_size
                    && ratio(written, packed) > self.limits.max_ratio
                {
                    return Err(bomb(
                        archive,
                        format!(
                            "entry '{}' expands more than {}:1",
                            e.path.display(),
                            self.limits.max_ratio
                        ),
                    ));
                }
            }
            // Solid formats record no per-entry packed size: judge by the whole archive file.
            None => {
                if self.archive_size > 0
                    && self.total_written >= self.limits.ratio_min_size
                    && ratio(self.total_written, self.archive_size) > self.limits.max_ratio
                {
                    return Err(bomb(
                        archive,
                        format!("archive expands more than {}:1", self.limits.max_ratio),
                    ));
                }
            }
        }
        Ok(())
    }

    // ---- symlinks --------------------------------------------------------------------------

    /// Recreate the symlink `e` at `rel` if its target stays inside the destination.
    pub(crate) fn do_symlink(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        rel: &Path,
    ) -> Result<(), Fail> {
        let Some(target) = e.link_target.as_deref() else {
            return self.skip_entry(
                ctx,
                format!("symlink '{}' has no target; skipped", e.path.display()),
            );
        };
        if let Err(why) = safe::link_target_ok(rel, target) {
            return self.skip_entry(
                ctx,
                format!(
                    "symlink '{}' -> '{}' skipped: {}",
                    e.path.display(),
                    target.display(),
                    why.describe()
                ),
            );
        }
        let Some(p) = self.prepare(
            ctx,
            e,
            rel,
            FileType::Symlink,
            target.as_os_str().len() as u64,
        )?
        else {
            return Ok(());
        };
        let mut tmp = None;
        for _ in 0..TEMP_TRIES {
            let t = p.parent.join(temp_name(&p.name, &ctx.unique_suffix()));
            match self.fs.symlink(target, &t) {
                Ok(()) => {
                    tmp = Some(t);
                    break;
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(dest_fail(&p.path, err)),
            }
        }
        let Some(tmp) = tmp else {
            return Err(Fail::entry(&p.path, "no free temporary name"));
        };
        let mut guard = TempGuard::new(Arc::clone(&self.fs), tmp.clone());
        let result = self.finish_link(ctx, &mut guard, &tmp, &p, e, rel, true);
        if result.is_err() {
            guard.cleanup(ctx);
        }
        result
    }

    /// Times, identity, publish and bookkeeping of a temp link (symlink or hard link).
    #[allow(clippy::too_many_arguments, reason = "one linear step")]
    fn finish_link(
        &mut self,
        ctx: &mut JobCtx,
        guard: &mut TempGuard,
        tmp: &Path,
        p: &Prepared,
        e: &ArchiveEntry,
        rel: &Path,
        symlink: bool,
    ) -> Result<(), Fail> {
        if symlink && e.mtime_ns != 0 {
            // Link times are cosmetic and not supported everywhere: log only.
            if let Err(err) = self.fs.set_times(tmp, e.mtime_ns, e.mtime_ns, false) {
                tracing::debug!(path = %p.path.display(), error = %err, "link times not set");
            }
        }
        let made = self.fs.lstat(tmp).map_err(|err| dest_fail(&p.path, err))?;
        // A hard link must not be "disarmed": renaming onto another name of the same inode is a
        // silent no-op that would leave the temp name behind.
        self.publish(ctx, guard, &p.path, p.replace, symlink)?;
        if !symlink {
            guard.cleanup(ctx);
        }
        let durable = self.sync || p.replace;
        let key = (!symlink).then_some(e.path.as_path());
        self.record(ctx, p, durable, (made.dev, made.ino), made.size, rel, key);
        Ok(())
    }

    // ---- hard links ------------------------------------------------------------------------

    /// A hard link entry: linked to the file this job already extracted for its target, else
    /// skipped with a warning. Where the file system has no hard links the target's data (the
    /// stream of this entry) is written as a copy instead.
    pub(crate) fn do_hardlink(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        rel: &Path,
        data: &mut dyn Read,
    ) -> Result<(), Fail> {
        let known = e
            .link_target
            .as_ref()
            .and_then(|t| self.extracted.get(t))
            .map(|x| (x.path.clone(), x.id, x.size));
        let Some((target_path, target_id, size)) = known else {
            return self.skip_entry(
                ctx,
                format!(
                    "hard link '{}' skipped: its target was not extracted by this job",
                    e.path.display()
                ),
            );
        };
        // The target must still be the very file this job published.
        match self.fs.lstat(&target_path) {
            Ok(st) if st.is_file() && (st.dev, st.ino) == target_id => {}
            _ => {
                return self.skip_entry(
                    ctx,
                    format!(
                        "hard link '{}' skipped: its target changed during extraction",
                        e.path.display()
                    ),
                );
            }
        }
        let Some(p) = self.prepare(ctx, e, rel, FileType::File, size)? else {
            return Ok(());
        };
        let mut tmp = None;
        for _ in 0..TEMP_TRIES {
            let t = p.parent.join(temp_name(&p.name, &ctx.unique_suffix()));
            match self.fs.hard_link(&target_path, &t) {
                Ok(()) => {
                    tmp = Some(t);
                    break;
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
                Err(err) if link_unsupported(&err) => {
                    tracing::debug!(error = %err, "no hard links here; copying the data");
                    return self.copy_data_of_hardlink(ctx, e, rel, data, size, p);
                }
                Err(err) => return Err(dest_fail(&p.path, err)),
            }
        }
        let Some(tmp) = tmp else {
            return Err(Fail::entry(&p.path, "no free temporary name"));
        };
        let mut guard = TempGuard::new(Arc::clone(&self.fs), tmp.clone());
        let result = self.finish_link(ctx, &mut guard, &tmp, &p, e, rel, false);
        if result.is_err() {
            guard.cleanup(ctx);
        }
        result
    }

    /// Fallback of [`do_hardlink`](Self::do_hardlink): write the stream as a regular file.
    fn copy_data_of_hardlink(
        &mut self,
        ctx: &mut JobCtx,
        e: &ArchiveEntry,
        rel: &Path,
        data: &mut dyn Read,
        size: u64,
        p: Prepared,
    ) -> Result<(), Fail> {
        let (tmp, df) = self.create_temp(ctx, &p.parent, &p.name)?;
        let mut guard = TempGuard::new(Arc::clone(&self.fs), tmp);
        let durable = self.sync || p.replace;
        let filled = self.fill_temp(ctx, e, data, df, guard.path(), &p.path, size, durable);
        let (id, written) = match filled {
            Ok(v) => v,
            Err(f) => {
                guard.cleanup(ctx);
                return Err(f);
            }
        };
        if let Err(f) = self.publish(ctx, &mut guard, &p.path, p.replace, true) {
            guard.cleanup(ctx);
            return Err(f);
        }
        self.record(ctx, &p, durable, id, written, rel, Some(&e.path));
        Ok(())
    }
}

/// The job-ending error of a decompression-bomb guard.
fn bomb(archive: &Path, what: String) -> Fail {
    Fail::Job(JobError::Archive(format!(
        "{}: {what}; extraction stopped (possible decompression bomb)",
        archive.display()
    )))
}

/// An error reading the archive stream: a cancel, or a reason the whole job cannot continue
/// (password, limits, archive I/O), or just this entry being corrupt.
fn read_fail(ctx: &JobCtx, dest: &Path, err: io::Error) -> Fail {
    if ctx.is_cancelled() {
        return Fail::Job(JobError::Cancelled);
    }
    if let Some(ae) = err
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ArchiveError>())
    {
        match ae {
            ArchiveError::Cancelled => return Fail::Job(JobError::Cancelled),
            ArchiveError::NeedsPassword(_)
            | ArchiveError::WrongPassword(_)
            | ArchiveError::LimitExceeded { .. }
            | ArchiveError::Io { .. } => return Fail::Job(JobError::Archive(ae.to_string())),
            _ => {}
        }
    }
    Fail::entry(dest, format!("could not read the entry's data: {err}"))
}

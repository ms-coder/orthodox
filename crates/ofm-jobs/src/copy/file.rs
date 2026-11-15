//! One regular file or symlink: temp name in the destination directory, data, metadata, verify,
//! publish (ADR-0004 steps 1-7). The temp file is owned by a [`TempGuard`] from the moment it
//! exists until it was renamed into place, so no error, cancel or panic can leave it behind
//! (only a failing `unlink` can, and that is reported as a warning).

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::ctx::{ErrorResolution, JobCtx};
use crate::error::JobError;
use crate::fs::{Fs, FsFile, FsStat};
use crate::plan::NAME_MAX;

use super::{CHUNK, Copier, Mode};

/// Allowed mtime difference between source and copy when verifying a move: FAT stores mtimes
/// with 2 s granularity.
const MTIME_SLACK_NS: i128 = 2_000_000_000;

/// Attempts to find a free temp name before giving up (collisions are practically impossible).
const TEMP_TRIES: u32 = 16;

/// Outcome of writing one non-directory.
pub(super) enum Written {
    /// Published (and, in move mode, the source handled).
    Done,
    /// Skipped by the user's choice.
    Skipped,
    /// `rename_noreplace` found the name taken (race): resolve the conflict again.
    Exists,
}

/// Why one attempt failed.
enum Fail {
    /// Cancelled or aborted: propagate.
    Job(JobError),
    /// The destination name appeared meanwhile.
    Exists,
    /// An I/O error to ask about (retry restarts the whole item from scratch).
    Io { path: PathBuf, err: io::Error },
}

impl From<JobError> for Fail {
    fn from(e: JobError) -> Self {
        Fail::Job(e)
    }
}

fn at(path: &Path) -> impl FnOnce(io::Error) -> Fail + '_ {
    move |err| Fail::Io {
        path: path.to_path_buf(),
        err,
    }
}

fn invalid(path: &Path, msg: &str) -> Fail {
    Fail::Io {
        path: path.to_path_buf(),
        err: io::Error::new(io::ErrorKind::InvalidData, msg.to_owned()),
    }
}

/// What a successful publish leaves for move mode.
struct Published {
    /// Everything verified and synced: the source may be deleted.
    source_deletable: bool,
    /// (dev, ino) of the published item (the job never overwrites it later, T3.6 B1).
    dest_id: (u64, u64),
}

// Linux errno values used by `kernel_tier_unsupported`.
const EPERM: i32 = 1;
const EIO: i32 = 5;
const EBADF: i32 = 9;
const EXDEV: i32 = 18;
const EINVAL: i32 = 22;
const ENOTTY: i32 = 25;
const ETXTBSY: i32 = 26;
const ENOSYS: i32 = 38;
const EOPNOTSUPP: i32 = 95;

/// A kernel-assisted copy (FICLONE, `copy_file_range`) failed in a way that means "use the next
/// tier", like coreutils (T3.6 m6): unsupported here, refused by a seccomp filter or for an
/// immutable/swap file (`EPERM`, `ETXTBSY`), or `EIO` before this tier copied the first byte (some
/// FUSE and CIFS servers). A genuine I/O error then shows up again in the read/write tier, with
/// the usual retry/skip/abort. `RealFs` already maps most of these to "unsupported"; this covers
/// wrappers and injected faults.
fn kernel_tier_unsupported(e: &io::Error, nothing_copied: bool) -> bool {
    match e.raw_os_error() {
        Some(EPERM | EXDEV | EINVAL | ENOTTY | ETXTBSY | ENOSYS | EOPNOTSUPP | EBADF) => true,
        Some(EIO) => nothing_copied,
        _ => e.kind() == io::ErrorKind::Unsupported,
    }
}

/// Unlinks the temp file unless disarmed (explicit cleanup reports failures; drop is the panic
/// safety net).
struct TempGuard {
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

    /// The temp is now the published file: nothing to clean up.
    fn disarm(&mut self) {
        self.path = None;
    }

    /// Remove the temp now (tried twice); a failure is a job warning.
    fn cleanup(&mut self, ctx: &mut JobCtx) {
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

/// `.<name>.ofm-part-<suffix>`, with `name` shortened (bytewise) to stay within `NAME_MAX`.
pub(crate) fn temp_name(name: &OsStr, suffix: &str) -> OsString {
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

/// Directory and name of a destination path.
fn split(dest: &Path) -> Result<(&Path, &OsStr), Fail> {
    match (dest.parent(), dest.file_name()) {
        (Some(d), Some(n)) => Ok((d, n)),
        _ => Err(Fail::Job(JobError::Invalid(format!(
            "no file name in {}",
            dest.display()
        )))),
    }
}

/// `lseek(SEEK_DATA)` not implemented here (`EINVAL`, `EOPNOTSUPP`).
fn seek_unsupported(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(22 | 95)) || e.kind() == io::ErrorKind::Unsupported
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

impl Copier {
    /// Copy the regular file `src` to `dest` (retry/skip/abort on errors).
    pub(super) fn copy_file(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        dest: &Path,
        replace: bool,
    ) -> Result<Written, JobError> {
        loop {
            match self.try_file(ctx, src, dest, replace) {
                Ok(p) => {
                    self.note_published(p.published.dest_id);
                    if self.mode == Mode::Move {
                        self.finish_move(ctx, src, &p)?;
                    }
                    ctx.file_done();
                    return Ok(Written::Done);
                }
                Err(Fail::Job(e)) => return Err(e),
                Err(Fail::Exists) => return Ok(Written::Exists),
                Err(Fail::Io { path, err }) => match ctx.resolve_error(&path, self.op(), &err)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => return Ok(Written::Skipped),
                },
            }
        }
    }

    /// Move mode, after a verified publish: delete the source if it is still the inode that was
    /// copied. A failed deletion the user skips leaves the source in place (nothing is lost).
    fn finish_move(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        p: &PublishedItem,
    ) -> Result<(), JobError> {
        if !p.published.source_deletable {
            ctx.warn(src, "copy could not be verified; source kept");
            return Ok(());
        }
        let id = p.source_id;
        let removed = ctx.attempt(src, crate::event::Operation::Move, |fs| {
            let now = fs.lstat(src)?;
            if (now.dev, now.ino) != id {
                return Err(io::Error::other(
                    "source was replaced during the move; not deleted",
                ));
            }
            match fs.unlink(src) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            }
        })?;
        if removed.is_none() {
            tracing::debug!(path = %src.display(), "source deletion skipped; source kept");
        }
        Ok(())
    }

    /// Create the temp file next to `dest` (fresh suffix on a name collision).
    fn create_temp(
        &self,
        ctx: &JobCtx,
        fs: &dyn Fs,
        dest: &Path,
    ) -> Result<(PathBuf, Box<dyn FsFile>), Fail> {
        let (dir, name) = split(dest)?;
        let mut last = io::Error::from(io::ErrorKind::AlreadyExists);
        for _ in 0..TEMP_TRIES {
            let tmp = dir.join(temp_name(name, &ctx.unique_suffix()));
            match fs.create_excl(&tmp, 0o600) {
                Ok(f) => return Ok((tmp, f)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = e,
                Err(e) => return Err(at(dest)(e)),
            }
        }
        Err(at(dest)(last))
    }

    /// One attempt: everything from opening the source to the published file.
    fn try_file(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        dest: &Path,
        replace: bool,
    ) -> Result<PublishedItem, Fail> {
        let fs = ctx.fs();
        ctx.checkpoint()?;
        let mut sf = if self.follow {
            fs.open_read_follow(src)
        } else {
            fs.open_read(src)
        }
        .map_err(at(src))?;
        let st = sf.stat().map_err(at(src))?;
        if !st.is_file() {
            return Err(invalid(src, "not a regular file any more"));
        }
        let (tmp, df) = self.create_temp(ctx, &*fs, dest)?;
        let mut guard = TempGuard::new(Arc::clone(&fs), tmp);
        let result =
            self.fill_and_publish(ctx, &*fs, src, &mut *sf, &st, df, &mut guard, dest, replace);
        if result.is_err() {
            guard.cleanup(ctx);
        }
        result.map(|published| PublishedItem {
            published,
            source_id: (st.dev, st.ino),
        })
    }

    #[allow(
        clippy::too_many_arguments,
        reason = "one linear algorithm, kept in one place"
    )]
    fn fill_and_publish(
        &mut self,
        ctx: &mut JobCtx,
        fs: &dyn Fs,
        src: &Path,
        sf: &mut dyn FsFile,
        st: &FsStat,
        mut df: Box<dyn FsFile>,
        guard: &mut TempGuard,
        dest: &Path,
        replace: bool,
    ) -> Result<Published, Fail> {
        // An overwrite always makes the new file durable before it replaces the old one (T3.6
        // M5): otherwise a crash right after the rename can leave neither version.
        let durable = self.sync || replace;
        self.copy_data(ctx, sf, &mut *df, st, src, dest)?;
        let times_ok = self.file_meta(ctx, fs, src, guard.path(), &mut *df, st);
        if durable {
            df.sync().map_err(at(dest))?;
        }
        // Verify before publishing: the copy is complete and the source did not change meanwhile.
        let tst = df.stat().map_err(at(dest))?;
        let now = sf.stat().map_err(at(src))?;
        if now.size != st.size || now.mtime_ns != st.mtime_ns {
            if self.mode == Mode::Move {
                return Err(invalid(src, "file changed while being copied"));
            }
            ctx.warn(
                src,
                "file changed while being copied; the copy may be inconsistent",
            );
        } else if tst.size != st.size {
            return Err(invalid(dest, "size of the copy differs from the source"));
        }
        if self.mode == Mode::Move
            && times_ok
            && (tst.mtime_ns - st.mtime_ns).abs() > MTIME_SLACK_NS
        {
            return Err(invalid(
                dest,
                "timestamp of the copy differs from the source",
            ));
        }
        // Checked close (T3.6 M4): NFS/FUSE may report a deferred write error only here. It must
        // be seen before the temp is published (and, in move mode, before the source goes).
        df.close().map_err(at(dest))?;
        self.publish(ctx, fs, guard, dest, replace)?;
        let mut deletable = true;
        if durable
            && let Some(dir) = dest.parent()
            && let Err(e) = fs.sync_dir(dir)
        {
            ctx.warn(dir, format!("directory not synced: {e}"));
            deletable = false;
        }
        if self.mode == Mode::Move {
            // The published name must be the inode just written.
            match fs.lstat(dest) {
                Ok(d) if (d.dev, d.ino) == (tst.dev, tst.ino) && d.size == st.size => {}
                _ => deletable = false,
            }
        }
        Ok(Published {
            source_deletable: deletable,
            dest_id: (tst.dev, tst.ino),
        })
    }

    /// Rename the finished temp into place: `rename_noreplace` (with the link/checked-rename
    /// fallbacks of [`noreplace`](super::noreplace) where the file system lacks it), or
    /// `rename_replace` when the user chose overwrite for this item.
    fn publish(
        &mut self,
        ctx: &mut JobCtx,
        fs: &dyn Fs,
        guard: &mut TempGuard,
        dest: &Path,
        replace: bool,
    ) -> Result<(), Fail> {
        ctx.checkpoint()?;
        let tmp = guard.path().to_path_buf();
        let r = if replace {
            fs.rename_replace(&tmp, dest)
        } else {
            super::noreplace::rename_noreplace(fs, &tmp, dest, false)
        };
        match r {
            Ok(()) => {
                guard.disarm();
                Ok(())
            }
            Err(e) if !replace && e.kind() == io::ErrorKind::AlreadyExists => Err(Fail::Exists),
            Err(e) => Err(at(dest)(e)),
        }
    }

    // ---- data ---------------------------------------------------------------------------------

    /// Data of the file: reflink, else sparse-aware chunked copy.
    fn copy_data(
        &mut self,
        ctx: &mut JobCtx,
        sf: &mut dyn FsFile,
        df: &mut dyn FsFile,
        st: &FsStat,
        src: &Path,
        dest: &Path,
    ) -> Result<(), Fail> {
        self.range_copied = false;
        if st.size == 0 {
            return Ok(());
        }
        ctx.checkpoint()?;
        // Reflink keeps holes and shares extents (Btrfs/XFS); one call, no chunking possible.
        match df.try_clone_from(&*sf) {
            Ok(true) => {
                ctx.add_bytes(st.size);
                return Ok(());
            }
            Ok(false) => {}
            Err(e) if kernel_tier_unsupported(&e, true) => {
                tracing::debug!(path = %dest.display(), error = %e, "reflink failed; next tier");
                // Whatever a failed clone may have left: start the temp from empty.
                df.set_len(0).map_err(at(dest))?;
            }
            Err(e) => return Err(at(dest)(e)),
        }
        if st.is_sparse() {
            return self.copy_sparse(ctx, sf, df, st, src, dest);
        }
        self.copy_stream(ctx, sf, df, None, src, dest).map(|_| ())
    }

    /// Copy only the data extents (SEEK_DATA/SEEK_HOLE); holes stay holes, the trailing hole is
    /// made with `set_len`.
    fn copy_sparse(
        &mut self,
        ctx: &mut JobCtx,
        sf: &mut dyn FsFile,
        df: &mut dyn FsFile,
        st: &FsStat,
        src: &Path,
        dest: &Path,
    ) -> Result<(), Fail> {
        let mut pos = 0;
        while pos < st.size {
            ctx.checkpoint()?;
            let data = match sf.seek_data(pos) {
                Ok(Some(d)) => d,
                Ok(None) => break,
                // No SEEK_DATA on this file system (some FUSE/network ones): nothing has been
                // written yet, so copy densely from the start instead.
                Err(e) if pos == 0 && seek_unsupported(&e) => {
                    tracing::debug!(path = %src.display(), error = %e, "no SEEK_DATA; dense copy");
                    sf.seek(0).map_err(at(src))?;
                    return self.copy_stream(ctx, sf, df, None, src, dest).map(|_| ());
                }
                Err(e) => return Err(at(src)(e)),
            };
            if data >= st.size {
                break;
            }
            let hole = sf.seek_hole(data).map_err(at(src))?.min(st.size);
            ctx.add_bytes(data - pos); // the hole before this extent
            sf.seek(data).map_err(at(src))?;
            df.seek(data).map_err(at(dest))?;
            let want = hole.saturating_sub(data);
            let got = self.copy_stream(ctx, sf, df, Some(want), src, dest)?;
            if got < want {
                break; // the source shrank: the verify step reports it
            }
            pos = hole;
        }
        ctx.add_bytes(st.size.saturating_sub(pos.min(st.size)));
        df.set_len(st.size).map_err(at(dest))?;
        Ok(())
    }

    /// Copy from the current offsets until EOF or `limit` bytes: `copy_file_range` in chunks of
    /// at most 1 MiB, falling back to read/write with a 1 MiB buffer. `ctx.checkpoint()` runs
    /// before every chunk. Returns the bytes copied.
    fn copy_stream(
        &mut self,
        ctx: &mut JobCtx,
        sf: &mut dyn FsFile,
        df: &mut dyn FsFile,
        limit: Option<u64>,
        src: &Path,
        dest: &Path,
    ) -> Result<u64, Fail> {
        let mut done: u64 = 0;
        let mut kernel = true;
        loop {
            ctx.checkpoint()?;
            let want = match limit {
                Some(l) => (l - done).min(CHUNK as u64),
                None => CHUNK as u64,
            };
            if want == 0 {
                return Ok(done);
            }
            if kernel {
                match df.copy_range_from(sf, want) {
                    Ok(Some(0) | None) => kernel = false, // EOF or unsupported: let read() decide
                    Ok(Some(n)) => {
                        self.range_copied = true;
                        done += n;
                        ctx.add_bytes(n);
                    }
                    // A failed call copies nothing, so both offsets are still where they were.
                    Err(e) if kernel_tier_unsupported(&e, !self.range_copied) => {
                        tracing::debug!(path = %dest.display(), error = %e, "copy_file_range failed; read/write");
                        kernel = false;
                    }
                    Err(e) => return Err(at(dest)(e)),
                }
                continue;
            }
            if self.buf.len() < CHUNK {
                self.buf.resize(CHUNK, 0);
            }
            // `want` <= CHUNK, so this conversion and slice are in bounds.
            let want = usize::try_from(want).unwrap_or(CHUNK).min(CHUNK);
            let n = loop {
                match sf.read(&mut self.buf[..want]) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    other => break other.map_err(at(src))?,
                }
            };
            if n == 0 {
                return Ok(done);
            }
            write_all(df, &self.buf[..n.min(want)]).map_err(at(dest))?;
            done += n as u64;
            ctx.add_bytes(n as u64);
        }
    }

    // ---- symlinks -----------------------------------------------------------------------------

    /// Recreate the symlink `src` at `dest` (temp link + rename, like files).
    pub(super) fn copy_symlink(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        st: &FsStat,
        dest: &Path,
        replace: bool,
    ) -> Result<Written, JobError> {
        loop {
            match self.try_symlink(ctx, src, st, dest, replace) {
                Ok(p) => {
                    self.note_published(p.published.dest_id);
                    if self.mode == Mode::Move {
                        self.finish_move(ctx, src, &p)?;
                    }
                    ctx.file_done();
                    return Ok(Written::Done);
                }
                Err(Fail::Job(e)) => return Err(e),
                Err(Fail::Exists) => return Ok(Written::Exists),
                Err(Fail::Io { path, err }) => match ctx.resolve_error(&path, self.op(), &err)? {
                    ErrorResolution::Retry => {}
                    ErrorResolution::Skip => return Ok(Written::Skipped),
                },
            }
        }
    }

    fn try_symlink(
        &mut self,
        ctx: &mut JobCtx,
        src: &Path,
        st: &FsStat,
        dest: &Path,
        replace: bool,
    ) -> Result<PublishedItem, Fail> {
        ctx.checkpoint()?;
        let fs = ctx.fs();
        let target = fs.read_link(src).map_err(at(src))?;
        let (dir, name) = split(dest)?;
        let mut created = None;
        for _ in 0..TEMP_TRIES {
            let tmp = dir.join(temp_name(name, &ctx.unique_suffix()));
            match fs.symlink(&target, &tmp) {
                Ok(()) => {
                    created = Some(tmp);
                    break;
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
                Err(e) => return Err(at(dest)(e)),
            }
        }
        let Some(tmp) = created else {
            return Err(at(dest)(io::Error::from(io::ErrorKind::AlreadyExists)));
        };
        let mut guard = TempGuard::new(Arc::clone(&fs), tmp.clone());
        let result = (|| {
            if self.is_root
                && let Err(e) = fs.set_owner(&tmp, Some(st.uid), Some(st.gid), false)
            {
                self.meta_warn(ctx, dest, "owner not preserved", &e);
            }
            // Link times are cosmetic and not supported everywhere: log only.
            if let Err(e) = fs.set_times(&tmp, st.atime_ns, st.mtime_ns, false) {
                tracing::debug!(path = %dest.display(), error = %e, "symlink times not set");
            }
            // The identity of the new link (a rename keeps it): recorded as this job's output.
            let made = fs.lstat(&tmp).map_err(at(dest))?;
            self.publish(ctx, &*fs, &mut guard, dest, replace)?;
            let mut deletable = true;
            if (self.sync || replace)
                && let Err(e) = fs.sync_dir(dir)
            {
                ctx.warn(dir, format!("directory not synced: {e}"));
                deletable = false;
            }
            if self.mode == Mode::Move && fs.read_link(dest).ok().as_ref() != Some(&target) {
                deletable = false;
            }
            Ok(Published {
                source_deletable: deletable,
                dest_id: (made.dev, made.ino),
            })
        })();
        if result.is_err() {
            guard.cleanup(ctx);
        }
        result.map(|published| PublishedItem {
            published,
            source_id: (st.dev, st.ino),
        })
    }
}

/// A published item plus the identity of the source it was made from.
struct PublishedItem {
    published: Published,
    source_id: (u64, u64),
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn temp_names_fit_name_max() {
        let n = temp_name(OsStr::new("a.txt"), "0123456789ab");
        assert_eq!(n, OsString::from(".a.txt.ofm-part-0123456789ab"));
        let long = OsString::from_vec(vec![0xffu8; 255]);
        let n = temp_name(&long, "0123456789ab");
        assert_eq!(n.len(), NAME_MAX);
        assert!(n.as_bytes().starts_with(b".\xff"));
        assert!(n.as_bytes().ends_with(b".ofm-part-0123456789ab"));
    }

    #[test]
    fn write_all_handles_short_writes() {
        struct Short(Vec<u8>);
        impl FsFile for Short {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
            fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
                let n = buf.len().min(3);
                self.0.extend_from_slice(&buf[..n]);
                Ok(n)
            }
            fn stat(&self) -> io::Result<FsStat> {
                Err(io::Error::other("x"))
            }
            fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
                // Never called in this test.
                unreachable!()
            }
            fn try_clone_from(&mut self, _: &dyn FsFile) -> io::Result<bool> {
                Ok(false)
            }
            fn copy_range_from(&mut self, _: &mut dyn FsFile, _: u64) -> io::Result<Option<u64>> {
                Ok(None)
            }
            fn seek_data(&mut self, _: u64) -> io::Result<Option<u64>> {
                Ok(None)
            }
            fn seek_hole(&mut self, _: u64) -> io::Result<u64> {
                Ok(0)
            }
            fn seek(&mut self, _: u64) -> io::Result<()> {
                Ok(())
            }
            fn set_len(&mut self, _: u64) -> io::Result<()> {
                Ok(())
            }
            fn set_mode(&mut self, _: u32) -> io::Result<()> {
                Ok(())
            }
            fn set_times(&mut self, _: i128, _: i128) -> io::Result<()> {
                Ok(())
            }
            fn set_owner(&mut self, _: Option<u32>, _: Option<u32>) -> io::Result<()> {
                Ok(())
            }
            fn sync(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut f = Short(Vec::new());
        write_all(&mut f, b"0123456789").unwrap();
        assert_eq!(f.0, b"0123456789");
    }
}

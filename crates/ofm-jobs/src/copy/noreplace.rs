//! "Rename, but never replace" on every file system (T3.6 M3, ADR-0004 "T3.6 review fixes").
//!
//! `renameat2(RENAME_NOREPLACE)` is not implemented by NFS and many FUSE file systems (ntfs-3g,
//! sshfs, gvfs): they answer `EINVAL`, `ENOSYS` or `EOPNOTSUPP`. Then, for a non-directory,
//! `link(from, to)` + `unlink(from)` gives the same guarantee (the kernel's `link` never replaces;
//! `EEXIST` is the usual conflict). Where hard links are not available either (FAT, exFAT, some
//! FUSE: `EPERM`, `ENOSYS`, ...) and for directories, the last resort is "`lstat` says `to` is
//! absent, then plain `rename`": a name created by another program in the microseconds between the
//! two calls could be replaced. That race is documented in ADR-0004; it needs a concurrent writer
//! choosing exactly that name, and our own job never creates a name it did not check.
//!
//! Every call goes through the [`Fs`], so `FaultyFs` can drive each branch.

use std::io;
use std::path::Path;

use crate::fs::Fs;

/// `EPERM`.
const EPERM: i32 = 1;
/// `EINVAL`.
const EINVAL: i32 = 22;
/// `EMLINK`: too many links.
const EMLINK: i32 = 31;
/// `ENOSYS`.
const ENOSYS: i32 = 38;
/// `EOPNOTSUPP`.
const EOPNOTSUPP: i32 = 95;

/// The file system has no `RENAME_NOREPLACE`.
fn noreplace_unsupported(e: &io::Error) -> bool {
    matches!(e.raw_os_error(), Some(EINVAL | ENOSYS | EOPNOTSUPP))
        || e.kind() == io::ErrorKind::Unsupported
}

/// The file system cannot make this hard link (no hard links at all, or not for this file).
fn link_unsupported(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(EPERM | EINVAL | EMLINK | ENOSYS | EOPNOTSUPP)
    ) || e.kind() == io::ErrorKind::Unsupported
}

/// Rename `from` to `to` unless `to` exists (`ErrorKind::AlreadyExists` then, as with
/// `rename_noreplace`). `is_dir`: `from` is a directory (no hard-link step). On success `from` is
/// gone and `to` is the same inode.
pub(crate) fn rename_noreplace(
    fs: &dyn Fs,
    from: &Path,
    to: &Path,
    is_dir: bool,
) -> io::Result<()> {
    match fs.rename_noreplace(from, to) {
        Err(e) if noreplace_unsupported(&e) => {
            tracing::debug!(
                from = %from.display(), to = %to.display(), error = %e,
                "RENAME_NOREPLACE unsupported; falling back"
            );
        }
        other => return other,
    }
    if !is_dir {
        match fs.hard_link(from, to) {
            Ok(()) => return drop_old_name(fs, from, to),
            Err(e) if link_unsupported(&e) => {
                tracing::debug!(to = %to.display(), error = %e, "no hard link; checked rename");
            }
            // EEXIST (a conflict) and real errors.
            Err(e) => return Err(e),
        }
    }
    match fs.lstat(to) {
        Ok(_) => Err(io::Error::from(io::ErrorKind::AlreadyExists)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => fs.rename_replace(from, to),
        Err(e) => Err(e),
    }
}

/// After `link(from, to)`: remove `from`. If that fails, undo the link (when `to` still is the
/// same inode) so the call has no effect, and report the error.
fn drop_old_name(fs: &dyn Fs, from: &Path, to: &Path) -> io::Result<()> {
    let err = match fs.unlink(from) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => e,
    };
    match (fs.lstat(from), fs.lstat(to)) {
        (Ok(a), Ok(b)) if (a.dev, a.ino) == (b.dev, b.ino) => {
            if let Err(u) = fs.unlink(to) {
                tracing::warn!(path = %to.display(), error = %u, "could not undo hard link");
            }
        }
        _ => tracing::warn!(path = %to.display(), "hard link not undone (name changed)"),
    }
    Err(err)
}

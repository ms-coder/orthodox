//! "Rename, but never replace" on every file system (T3.6 M3, ADR-0004 "T3.6 review fixes").
//!
//! `renameat2(RENAME_NOREPLACE)` is not implemented by NFS and many FUSE file systems (ntfs-3g,
//! sshfs, gvfs): they answer `EINVAL`, `ENOSYS` or `EOPNOTSUPP`. Then, for a non-directory,
//! `link(from, to)` + `unlink(from)` gives the same guarantee (the kernel's `link` never replaces;
//! `EEXIST` is the usual conflict). One `EEXIST` is not a conflict: NFS (v3) retransmits a `link`
//! whose reply was lost, and the second attempt finds the name the first one made. That is told
//! apart by identity AND link count ([`link_was_made`]). Where hard links are not available either (FAT, exFAT, some
//! FUSE: `EPERM`, `ENOSYS`, ...) and for directories, the last resort is "`lstat` says `to` is
//! absent, then plain `rename`": a name created by another program in the microseconds between the
//! two calls could be replaced. That race is documented in ADR-0004; it needs a concurrent writer
//! choosing exactly that name, and our own job never creates a name it did not check.
//!
//! Every call goes through the [`Fs`], so `FaultyFs` can drive each branch.

use std::io;
use std::path::Path;

use crate::fs::{Fs, FsStat};

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
        // The state before the call, to recognise our own link in an `EEXIST` answer.
        let before = fs.lstat(from).ok();
        match fs.hard_link(from, to) {
            Ok(()) => return drop_old_name(fs, from, to),
            Err(e)
                if e.kind() == io::ErrorKind::AlreadyExists
                    && link_was_made(fs, before.as_ref(), from, to) =>
            {
                tracing::debug!(to = %to.display(), "link reply lost; the link was made");
                return drop_old_name(fs, from, to);
            }
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

/// Whether `link(from, to)` happened although it reported `EEXIST`: both names are the object
/// `from` was `before` the call, and that object gained exactly one name since. The identity alone
/// would not do: on a case-insensitive file system `a` and `A` are ONE directory entry (`link`
/// says `EEXIST`, both `lstat`s agree) and unlinking `from` would delete the only copy; there the
/// link count does not change. Anything unsure counts as "not made": the caller then reports the
/// conflict and nothing is removed.
fn link_was_made(fs: &dyn Fs, before: Option<&FsStat>, from: &Path, to: &Path) -> bool {
    let Some(before) = before else { return false };
    let (Ok(a), Ok(b)) = (fs.lstat(from), fs.lstat(to)) else {
        return false;
    };
    let id = (before.dev, before.ino);
    (a.dev, a.ino) == id && (b.dev, b.ino) == id && b.nlink == before.nlink + 1
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
    use ofm_testkit::temp::TempDir;

    use super::*;
    use crate::fs::faulty::FaultyFs;

    const EEXIST: i32 = 17;

    /// No `RENAME_NOREPLACE`.
    fn no_noreplace() -> FaultPlan {
        FaultPlan::new().fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(EINVAL))
    }

    /// ... and the reply of the first `link` gets lost.
    fn retransmitting() -> FaultyFs {
        FaultyFs::over_real(no_noreplace().lose_reply_nth(
            FaultOp::HardLink,
            1,
            FaultErrno::Raw(EEXIST),
        ))
    }

    #[test]
    fn a_retransmitted_link_of_a_file_or_symlink_is_success() {
        let t = TempDir::new().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::write(&a, b"data").unwrap();
        rename_noreplace(&retransmitting(), &a, &b, false).unwrap();
        assert!(!a.exists());
        assert_eq!(std::fs::read(&b).unwrap(), b"data");

        let (l, m) = (t.path().join("l"), t.path().join("m"));
        std::os::unix::fs::symlink("nowhere", &l).unwrap();
        rename_noreplace(&retransmitting(), &l, &m, false).unwrap();
        assert!(std::fs::symlink_metadata(&l).is_err());
        assert_eq!(std::fs::read_link(&m).unwrap(), Path::new("nowhere"));
    }

    /// The same object under both names already (as when `a` and `A` are one entry): the link
    /// count does not grow, so nothing may be removed.
    #[test]
    fn an_existing_name_of_the_same_file_stays_a_conflict() {
        let t = TempDir::new().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::write(&a, b"data").unwrap();
        std::fs::hard_link(&a, &b).unwrap();
        let fs = FaultyFs::over_real(no_noreplace());
        let e = rename_noreplace(&fs, &a, &b, false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert!(a.exists() && b.exists());
        assert_eq!(fs.calls_of(FaultOp::Unlink), 0);
    }

    #[test]
    fn another_file_under_the_name_stays_a_conflict() {
        let t = TempDir::new().unwrap();
        let (a, b) = (t.path().join("a"), t.path().join("b"));
        std::fs::write(&a, b"mine").unwrap();
        std::fs::write(&b, b"theirs").unwrap();
        let fs = FaultyFs::over_real(no_noreplace());
        let e = rename_noreplace(&fs, &a, &b, false).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&a).unwrap(), b"mine");
        assert_eq!(std::fs::read(&b).unwrap(), b"theirs");
    }
}

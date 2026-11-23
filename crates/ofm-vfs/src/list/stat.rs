//! `lstat` of one real path to an [`Entry`] (T7.2, ADR-0009): panelized-list rows and refresh.
//!
//! Symlinks are not followed; a second, following stat determines the target kind (`None` =
//! broken link). This mirrors `local::stat::entry_at` but takes a full path, because list rows
//! are arbitrary real paths, not names inside one open directory (and `local::stat` is owned by
//! T1.2).

use std::ffi::OsString;
use std::path::Path;

use ofm_core::{Entry, EntryKind, TargetKind};
use rustix::fs::{AtFlags, CWD, FileType};
use rustix::io::Errno;

use crate::error::VfsError;

fn target_kind(mode: u32) -> Option<TargetKind> {
    match FileType::from_raw_mode(mode) {
        FileType::RegularFile => Some(TargetKind::File),
        FileType::Directory => Some(TargetKind::Dir),
        _ => Some(TargetKind::Other),
    }
}

/// `lstat` `path` and build an [`Entry`] named `display` with fresh metadata (size, mtime, mode,
/// uid, gid). `Errno::NOTDIR` maps to [`VfsError::NotFound`] (a path component that is not a
/// directory is as unusable as a missing one for list rows).
pub(crate) fn stat_path(path: &Path, display: OsString) -> Result<Entry, VfsError> {
    let raw =
        rustix::fs::statat(CWD, path, AtFlags::SYMLINK_NOFOLLOW).map_err(|e| map_errno(path, e))?;
    #[allow(clippy::useless_conversion, reason = "field widths differ per target")]
    let mode: u32 = raw.st_mode.into();
    let kind = match FileType::from_raw_mode(mode) {
        FileType::RegularFile => EntryKind::File,
        FileType::Directory => EntryKind::Dir,
        FileType::Symlink => {
            #[allow(clippy::useless_conversion, reason = "field widths differ per target")]
            let target_mode: Option<u32> = rustix::fs::statat(CWD, path, AtFlags::empty())
                .ok()
                .map(|t| t.st_mode.into());
            EntryKind::Symlink {
                target: target_mode.and_then(target_kind),
            }
        }
        _ => EntryKind::Other,
    };
    let mut e = Entry::new(display, kind);
    e.size = u64::try_from(raw.st_size).unwrap_or(0);
    #[allow(clippy::useless_conversion, reason = "field widths differ per target")]
    {
        e.mtime_ns = i128::from(raw.st_mtime) * 1_000_000_000 + i128::from(raw.st_mtime_nsec);
    }
    e.mode = mode;
    e.uid = raw.st_uid;
    e.gid = raw.st_gid;
    Ok(e)
}

fn map_errno(path: &Path, e: Errno) -> VfsError {
    match e {
        Errno::NOENT | Errno::NOTDIR => VfsError::NotFound(path.to_path_buf()),
        Errno::ACCESS | Errno::PERM => VfsError::PermissionDenied(path.to_path_buf()),
        _ => VfsError::from_io(path, e.into()),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn broken_symlink_has_no_target_kind() {
        let tmp = std::env::temp_dir().join(format!("ofm-list-stat-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let link = tmp.join("broken");
        std::os::unix::fs::symlink("no-such-target", &link).unwrap();

        let e = stat_path(&link, "broken".into()).unwrap();
        assert_eq!(e.kind, EntryKind::Symlink { target: None });

        // Non-UTF-8 name: bytes survive untouched.
        let weird = tmp.join(std::ffi::OsString::from_vec(b"w\xffid".to_vec()));
        std::fs::write(&weird, b"x").unwrap();
        let e = stat_path(&weird, weird.file_name().unwrap().to_owned()).unwrap();
        assert_eq!(e.kind, EntryKind::File);
        assert_eq!(e.size, 1);

        let gone = stat_path(&tmp.join("gone"), "gone".into());
        assert!(matches!(gone, Err(VfsError::NotFound(_))));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}

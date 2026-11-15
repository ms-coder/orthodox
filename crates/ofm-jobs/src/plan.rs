//! Plan phase of copy/move (T3.1 owns this file): scan sources for totals, detect
//! destination-inside-source and same-file, resolve names. Signatures are the T3.0 stubs; T3.1 may
//! reshape anything below as long as `copy::run_copy` / `move_::run_move` keep their signatures.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

use ofm_core::Location;
use ofm_core::mask::RenameMask;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::fs::Fs;

/// Totals found by [`scan`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Non-directory entries.
    pub files: u64,
    /// Directories.
    pub dirs: u64,
    /// Sum of file sizes.
    pub bytes: u64,
}

/// Resolve job sources to local paths. v1.0/M3 copies and moves local items only; archive entries
/// (M6) and list locations (resolved to real paths by the UI before submitting) are
/// `JobError::Unsupported` here.
pub fn local_paths(sources: &[Location]) -> Result<Vec<PathBuf>, JobError> {
    if sources.is_empty() {
        return Err(JobError::Invalid("no sources".into()));
    }
    sources
        .iter()
        .map(|s| match s {
            Location::Local(p) => Ok(p.clone()),
            other => Err(JobError::Unsupported(format!(
                "copy/move from {}",
                other.display_lossy()
            ))),
        })
        .collect()
}

/// Count files/dirs/bytes under `sources` (never follows symlinks unless `follow_symlinks`; guards
/// against symlink loops when following). Reports through `ctx.add_total` (so the UI shows a
/// growing total with `planning = true`) and checks `ctx.checkpoint()` per directory.
pub fn scan(
    _ctx: &mut JobCtx,
    _sources: &[PathBuf],
    _follow_symlinks: bool,
) -> Result<Totals, JobError> {
    todo!("T3.1")
}

/// Refuse copying/moving a directory into itself or its own subtree (`DestInsideSource`) and
/// detect "same file" cases by dev+ino of the destination directory vs each source's parent.
pub fn check_destination(
    _ctx: &mut JobCtx,
    _sources: &[PathBuf],
    _dest_dir: &Path,
) -> Result<(), JobError> {
    todo!("T3.1")
}

/// Destination name for a top-level source: apply the rename mask if any.
pub fn target_name(source: &Path, mask: Option<&RenameMask>) -> OsString {
    let name = source.file_name().unwrap_or(source.as_os_str());
    match mask {
        Some(m) => m.apply(name),
        None => name.to_owned(),
    }
}

/// First free name `name (N).ext` (N >= 1) in `dir`, probing with `lstat` (ADR-0006: the scheme for
/// `RenameNew`/`RenameExisting`; `.bashrc` -> `.bashrc (1)`, `a.tar.gz` -> `a.tar (1).gz`).
pub fn unique_name(_fs: &dyn Fs, _dir: &Path, _name: &OsStr) -> io::Result<OsString> {
    todo!("T3.1")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_paths_rejects_non_local_and_empty() {
        assert!(matches!(local_paths(&[]), Err(JobError::Invalid(_))));
        let arch = Location::Archive {
            archive: "/a.zip".into(),
            inner: "x".into(),
        };
        assert!(matches!(
            local_paths(&[arch]),
            Err(JobError::Unsupported(_))
        ));
        assert_eq!(
            local_paths(&[Location::Local("/a".into())]).unwrap_or_default(),
            vec![PathBuf::from("/a")]
        );
    }

    #[test]
    fn target_name_applies_mask() {
        let m = RenameMask::parse("*.bak").ok();
        assert_eq!(
            target_name(Path::new("/x/a.txt"), m.as_ref()),
            OsString::from("a.bak")
        );
        assert_eq!(
            target_name(Path::new("/x/a.txt"), None),
            OsString::from("a.txt")
        );
    }

    #[test]
    #[ignore = "T3.1"]
    fn scan_counts_tree_and_reports_planning_progress() {}

    #[test]
    #[ignore = "T3.1"]
    fn dest_inside_source_and_same_file_are_refused() {}

    #[test]
    #[ignore = "T3.1"]
    fn unique_name_scheme() {}
}

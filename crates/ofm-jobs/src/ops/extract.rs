//! Extract from an archive (M6): copy from the archive VFS with the copy machinery; not part of M3.

use std::path::{Path, PathBuf};

use crate::ctx::JobCtx;
use crate::error::JobError;

/// Extract `entries` of `archive` into `dest_dir`.
pub fn run(
    _ctx: &mut JobCtx,
    _archive: &Path,
    _entries: &[PathBuf],
    _dest_dir: &Path,
) -> Result<(), JobError> {
    Err(JobError::Unsupported("archive extraction (M6)".into()))
}

//! Batch rename job (F20, M8): not part of M3. `Rename { pairs }` is validated for conflicts and
//! cycles by the caller; the M8 task implements it with `rename_noreplace` per pair (cycles via
//! temp names) and owns this file then. Single renames in place are `Move` jobs.

use std::path::PathBuf;

use crate::ctx::JobCtx;
use crate::error::JobError;

/// Apply `(from, to)` renames.
pub fn run(_ctx: &mut JobCtx, _pairs: &[(PathBuf, PathBuf)]) -> Result<(), JobError> {
    Err(JobError::Unsupported("batch rename (M8)".into()))
}

//! Move engine (T3.1 owns this file; ADR-0004 "Move").
//!
//! Contract for `run_move`: same `st_dev` -> `rename_noreplace` per top-level item (a conflict goes
//! through `ctx.resolve_conflict`; `Overwrite` uses `rename_replace` only for file-over-file);
//! cross-device -> `copy` module machinery with fsync, verify size+mtime, then delete each source
//! item only after its copy succeeded (depth-first, never following symlinks); a failure leaves the
//! sources intact. "Rename in place" (Shift+F6) arrives as `Move` into the same directory with a
//! literal `rename_mask`.

use std::path::Path;

use ofm_core::Location;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::spec::CopyOpts;

/// Move `sources` into `dest_dir`.
pub fn run_move(
    _ctx: &mut JobCtx,
    _sources: &[Location],
    _dest_dir: &Path,
    _opts: &CopyOpts,
) -> Result<(), JobError> {
    todo!("T3.1")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.1"]
    fn same_fs_move_is_an_atomic_rename() {}

    #[test]
    #[ignore = "T3.1"]
    fn cross_device_failure_keeps_sources() {}
}

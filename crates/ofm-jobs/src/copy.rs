//! Copy engine (T3.1 owns this file): exactly the algorithm of ADR-0004.
//!
//! Contract for `run_copy` (acceptance for T3.1, see PLAN T3.1 + the T3.5 checklist):
//! - parse `opts.rename_mask` first (`RenameMask::parse`, error -> `JobError::Invalid`), resolve
//!   `sources` with `plan::local_paths`, `plan::check_destination`, `plan::scan` (planning
//!   progress), then `ctx.set_totals`;
//! - per file: temp `.<name>.ofm-part-<ctx.unique_suffix()>` created with `create_excl` mode 0600,
//!   data (FICLONE -> `copy_file_range` -> 1 MiB read/write; sparse via SEEK_DATA/HOLE), metadata
//!   (mode, ns times, xattrs, owner only if `fs.is_root()`), fsync when `opts.sync`, publish with
//!   `rename_noreplace` (or `rename_replace` after an `Overwrite` resolution), unlink the temp on
//!   EVERY error/cancel path;
//! - `ctx.checkpoint()` between items and every 1 MiB; progress via `ctx.add_bytes/file_done/
//!   set_current`;
//! - conflicts via `ctx.resolve_conflict` (kind: `SameFile` when dev+ino equal, `TypeMismatch`
//!   file vs dir; dir vs dir merges silently); per-syscall failures via `ctx.attempt`/
//!   `ctx.resolve_error`; `RenameNew`/`RenameExisting` use `plan::unique_name`;
//! - record final top-level destination paths with `ctx.note_created`.

use std::path::Path;

use ofm_core::Location;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::spec::CopyOpts;

/// Copy `sources` into `dest_dir`.
pub fn run_copy(
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
    fn copies_file_via_temp_and_rename() {}

    #[test]
    #[ignore = "T3.1"]
    fn cancel_leaves_no_temp_and_no_final_file() {}
}

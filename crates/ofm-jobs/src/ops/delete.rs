//! Delete (T3.2 owns this file; F14, ADR-0004 "Delete").
//!
//! `to_trash`: per top-level path `ctx.fs().trash(path)` via `ctx.attempt(.., Operation::Trash, ..)`;
//! `TrashError::Unavailable` -> `ctx.confirm_permanent_delete(path)` (true = permanent delete of
//! that path, false = skipped). Permanent: depth-first, `lstat` then `unlink` for non-dirs
//! (symlinks included, NEVER followed), `rmdir` after the children; read-only directories are not
//! chmod-ed behind the user's back (the error goes to `resolve_error`). Missing path = skip with a
//! warning, not an error. Progress: `set_totals` after counting (or incremental `add_total`).

use std::path::PathBuf;

use crate::ctx::JobCtx;
use crate::error::JobError;

/// Delete `paths`.
pub fn run(_ctx: &mut JobCtx, _paths: &[PathBuf], _to_trash: bool) -> Result<(), JobError> {
    todo!("T3.2")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.2"]
    fn permanent_delete_never_follows_symlinks() {}

    #[test]
    #[ignore = "T3.2"]
    fn unavailable_trash_asks_then_deletes_or_skips() {}

    #[test]
    #[ignore = "T3.2"]
    fn per_entry_error_offers_retry_skip_abort() {}
}

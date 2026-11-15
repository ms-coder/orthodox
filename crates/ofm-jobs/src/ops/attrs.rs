//! chmod / chown (T3.2 owns this file; F19). `ModeChange { set, clear }`: new = (old & !clear) | set,
//! computed per object from `lstat`; the UI sends the capital-`X` semantics for recursion as two
//! separate `SetAttrs` jobs is NOT needed: when `recursive`, apply the change to directories as
//! given and to files with the execute bits (0o111) masked out of `set` unless the file already
//! has any execute bit ("X for dirs only" rule from PLAN T3.2). Symlinks are skipped for chmod and
//! never followed in recursion; chown uses `lchown` semantics. Failures: `ctx.attempt`.

use std::path::PathBuf;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::spec::{ModeChange, OwnerChange};

/// Apply `mode` and/or `owner` to `paths` (recursively into directories if `recursive`).
pub fn run(
    _ctx: &mut JobCtx,
    _paths: &[PathBuf],
    _mode: Option<ModeChange>,
    _owner: Option<OwnerChange>,
    _recursive: bool,
) -> Result<(), JobError> {
    todo!("T3.2")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.2"]
    fn chmod_set_and_clear_bits() {}

    #[test]
    #[ignore = "T3.2"]
    fn recursive_chmod_does_not_make_plain_files_executable() {}

    #[test]
    #[ignore = "T3.2"]
    fn chown_failure_without_privilege_is_reported_per_item() {}
}

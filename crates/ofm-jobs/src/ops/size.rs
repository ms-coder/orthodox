//! Directory size calculation (T3.2 owns this file; F19, "F3 on directory"). Read-only, cancellable
//! (`ctx.checkpoint()` per directory entry batch), hardlinks counted once by (dev, ino) when
//! `nlink > 1`, symlinks counted as entries of `lstat` size and never followed, unreadable
//! directories are warnings (`ctx.warn`), not errors. Progress: `ctx.add_total(files, bytes)` while
//! walking (UI shows a running total), `set_current(dir)`.

use std::path::PathBuf;

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::event::SizeTotals;

/// Total size of `paths`.
pub fn run(_ctx: &mut JobCtx, _paths: &[PathBuf]) -> Result<SizeTotals, JobError> {
    todo!("T3.2")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.2"]
    fn counts_files_dirs_bytes_and_dedupes_hardlinks() {}

    #[test]
    #[ignore = "T3.2"]
    fn cancel_stops_the_walk() {}
}

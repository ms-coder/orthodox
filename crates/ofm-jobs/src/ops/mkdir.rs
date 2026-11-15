//! Make directory (T3.2 owns this file; F13): `mkdir -p` semantics, mode 0777 (umask applies),
//! an already existing *directory* is success, an existing non-directory is an error. Intermediate
//! directories are created one by one through `ctx.fs().mkdir`. `ctx.note_created(path)` on
//! success (the UI puts the cursor on the first created component below the panel directory).

use std::path::Path;

use crate::ctx::JobCtx;
use crate::error::JobError;

/// Create `path` and missing parents.
pub fn run(_ctx: &mut JobCtx, _path: &Path) -> Result<(), JobError> {
    todo!("T3.2")
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.2"]
    fn creates_parents_and_accepts_existing_dir() {}

    #[test]
    #[ignore = "T3.2"]
    fn existing_file_is_an_error() {}
}

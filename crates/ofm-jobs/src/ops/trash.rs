//! Freedesktop trash (T3.2 owns this file; F14): implemented with the `trash` crate (T3.2 adds the
//! dependency to `crates/ofm-jobs/Cargo.toml`) incl. per-mount `.Trash-$UID`. Called only through
//! `Fs::trash` (so `FaultyFs` can intercept it). Map "no trash for this mount / sandbox" to
//! `TrashError::Unavailable`, everything else to `TrashError::Io`. Tests must use a temporary
//! `XDG_DATA_HOME` and must never touch the real trash.

use std::path::Path;

use crate::fs::TrashError;

/// Move `path` to the trash.
pub fn trash_path(_path: &Path) -> Result<(), TrashError> {
    Err(TrashError::Unavailable(
        "trash not implemented yet (T3.2)".into(),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "T3.2"]
    fn trashed_file_lands_in_xdg_trash_with_trashinfo() {}
}

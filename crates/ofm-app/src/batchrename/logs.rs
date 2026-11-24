//! Undo-log files of batch renames (blocking; run on the I/O pool).
//!
//! Each batch's job writes its own log at a fresh `pending-<id>.toml` (so a log of an earlier
//! batch is never overwritten while the job runs). When the batch is over, the log becomes
//! `last.toml` by an atomic `rename`: only the last batch can be undone, and the preview says
//! when that replaces the undo information of a previous one.

use std::io;
use std::path::Path;

use ofm_core::batch_rename::load_undo_log;

/// `last.toml` exists: describe the batch it would let the user undo (its directory), for the
/// "this replaces the previous undo" note. `None` when there is no previous log.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn previous_batch(last: &Path) -> Option<String> {
    if !last.exists() {
        return None;
    }
    Some(match load_undo_log(last) {
        Ok(log) => log.dir.to_string_lossy().into_owned(),
        Err(_) => last.to_string_lossy().into_owned(),
    })
}

/// Make the batch's log the undo log (`pending` -> `last`, replacing the older one). A missing
/// `pending` (the job wrote nothing) is not an error.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn promote(pending: &Path, last: &Path) -> io::Result<()> {
    match std::fs::rename(pending, last) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// Remove a log that has nothing to undo.
#[allow(clippy::disallowed_methods)] // worker thread, not the GTK main thread
pub fn discard(pending: &Path) {
    let _ = std::fs::remove_file(pending);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use std::collections::HashSet;
    use std::ffi::OsString;

    use ofm_core::batch_rename::{RenameOp, UndoLog, plan, save_undo_log};

    use super::*;

    fn make_log(path: &Path, dir: &Path) {
        let ops = [RenameOp {
            from: OsString::from("a"),
            to: OsString::from("b"),
        }];
        let existing: HashSet<OsString> = [OsString::from("a")].into_iter().collect();
        let p = plan(&ops, &existing).unwrap();
        save_undo_log(&UndoLog::new(dir.to_owned(), &p), path).unwrap();
    }

    #[test]
    fn promote_replaces_the_previous_log_and_previous_batch_is_described() {
        let base = std::env::temp_dir().join(format!("ofm-logs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        let (last, p1, p2) = (
            base.join("last.toml"),
            base.join("p1.toml"),
            base.join("p2.toml"),
        );
        assert_eq!(previous_batch(&last), None);
        make_log(&p1, Path::new("/first"));
        promote(&p1, &last).unwrap();
        assert!(!p1.exists());
        assert_eq!(previous_batch(&last).as_deref(), Some("/first"));
        make_log(&p2, Path::new("/second"));
        promote(&p2, &last).unwrap();
        assert_eq!(load_undo_log(&last).unwrap().dir, Path::new("/second"));
        // A missing pending log is fine; discard is idempotent.
        promote(&base.join("absent.toml"), &last).unwrap();
        discard(&base.join("absent.toml"));
        std::fs::remove_dir_all(&base).unwrap();
    }
}

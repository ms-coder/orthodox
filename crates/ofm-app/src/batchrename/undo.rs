//! `file.batch_rename_undo` (Ctrl+Alt+Z): undo the last batch rename.
//!
//! The log of the last batch (`last.toml`, see [`super::logs`]) is loaded on the pool and checked
//! against the directory with `ofm_core::batch_rename::verify_undo`. A log that no longer matches
//! (a file was renamed again, deleted, or a new file took a name the undo needs) is explained and
//! never guessed at. Otherwise the reverse renames are previewed; Apply submits
//! `JobSpec::RenameRollback`, which removes the log when everything is undone.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io;
use std::ops::ControlFlow;
use std::path::Path;
use std::rc::Rc;

use ofm_core::batch_rename::{BatchError, UndoLog, load_undo_log, undo_plan, verify_undo};
use ofm_jobs::{Fs, RealFs};

use super::{Phase, flow, glue, state};
use crate::context::AppContext;
use crate::dialogs::batchrename_preview;
use crate::i18n::tr;
use crate::wiring;

/// What the worker found.
#[derive(Debug)]
enum Loaded {
    /// There is no log: nothing to undo.
    Nothing,
    /// The log cannot be read or is no longer applicable; the text says why.
    Refused(String),
    /// Verified: the undo can run.
    Ready(UndoLog),
}

/// Load and verify the last batch's log (blocking; call from the pool).
fn load_blocking(last: &Path) -> Loaded {
    let log = match load_undo_log(last) {
        Ok(log) => log,
        Err(BatchError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            return Loaded::Nothing;
        }
        Err(e) => {
            return Loaded::Refused(format!(
                "{} {}",
                tr("The undo log cannot be read:"),
                glue::problem_text(&e)
            ));
        }
    };
    let names: HashSet<OsString> = match RealFs.read_dir(&log.dir) {
        Ok(n) => n.into_iter().collect(),
        Err(e) => {
            return Loaded::Refused(format!("{}: {e}", log.dir.to_string_lossy()));
        }
    };
    match verify_undo(&log, &names) {
        Ok(()) => Loaded::Ready(log),
        Err(e) => Loaded::Refused(glue::problem_text(&e)),
    }
}

/// `file.batch_rename_undo`.
pub fn start(ctx: &Rc<AppContext>) {
    let last = glue::last_log(&ctx.paths.state_dir);
    let worker_last = last.clone();
    let rx = wiring::run_on_pool(&ctx.services.pool, move |tx| {
        let _ = tx.send_blocking(load_blocking(&worker_last));
    });
    let weak = Rc::downgrade(ctx);
    wiring::attach(
        rx,
        move |loaded| {
            let Some(ctx) = weak.upgrade() else {
                return ControlFlow::Break(());
            };
            match loaded {
                Loaded::Nothing => flow::notice(&ctx, tr("There is no batch rename to undo.")),
                Loaded::Refused(why) => flow::notice(&ctx, &why),
                Loaded::Ready(log) => preview(&ctx, log, last.clone()),
            }
            ControlFlow::Break(())
        },
        || {},
    );
}

fn preview(ctx: &Rc<AppContext>, log: UndoLog, last: std::path::PathBuf) {
    let plan = undo_plan(&log);
    let lines = glue::preview_lines(&plan.preview);
    let n = lines.len();
    let heading = format!(
        "{} {n} {} {}",
        tr("Undo the last batch rename:"),
        if n == 1 {
            tr("item in")
        } else {
            tr("items in")
        },
        log.dir.to_string_lossy()
    );
    let mut notes = Vec::new();
    let temps = glue::temp_steps(&plan);
    if temps > 0 {
        notes.push(format!(
            "{} {temps} {}",
            tr("Swapped or chained names need"),
            tr("temporary rename step(s); they are done and undone automatically.")
        ));
    }
    if let Some(s) = state(ctx) {
        s.set_phase(Phase::Preview(lines.clone()));
    }
    let weak = Rc::downgrade(ctx);
    batchrename_preview::present(
        ctx,
        batchrename_preview::PreviewView {
            title: tr("Undo batch rename"),
            heading,
            lines,
            notes,
            apply_label: tr("_Undo"),
        },
        move |apply| {
            let Some(ctx) = weak.upgrade() else {
                return;
            };
            if let Some(s) = state(&ctx) {
                s.set_phase(if apply { Phase::Applying } else { Phase::Idle });
            }
            if apply {
                flow::submit_rollback(&ctx, log, last);
            }
        },
    );
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]
mod tests {
    use ofm_core::batch_rename::{RenameOp, plan, save_undo_log};

    use super::*;

    fn os(s: &str) -> OsString {
        OsString::from(s)
    }

    /// A directory in the state "after renaming a -> b".
    fn after_rename(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("ofm-undo-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let dir = base.join("d");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("b"), "x").unwrap();
        let ops = [RenameOp {
            from: os("a"),
            to: os("b"),
        }];
        let existing: HashSet<OsString> = [os("a")].into_iter().collect();
        let log = UndoLog::new(dir.clone(), &plan(&ops, &existing).unwrap());
        save_undo_log(&log, &base.join("last.toml")).unwrap();
        (base, dir)
    }

    #[test]
    fn no_log_means_nothing_to_undo() {
        let base = std::env::temp_dir().join(format!("ofm-undo-test-none-{}", std::process::id()));
        assert!(matches!(
            load_blocking(&base.join("last.toml")),
            Loaded::Nothing
        ));
    }

    #[test]
    fn a_matching_directory_is_ready() {
        let (base, _dir) = after_rename("ready");
        assert!(matches!(
            load_blocking(&base.join("last.toml")),
            Loaded::Ready(_)
        ));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_stale_log_is_explained_not_applied() {
        let (base, dir) = after_rename("stale");
        // The renamed file was renamed again meanwhile.
        std::fs::rename(dir.join("b"), dir.join("moved")).unwrap();
        match load_blocking(&base.join("last.toml")) {
            Loaded::Refused(why) => assert!(why.contains("no longer"), "{why}"),
            other => panic!("expected a refusal, got {other:?}"),
        }
        // Nothing was touched.
        assert!(dir.join("moved").exists());
        // A name the undo needs is occupied.
        std::fs::rename(dir.join("moved"), dir.join("b")).unwrap();
        std::fs::write(dir.join("a"), "new").unwrap();
        assert!(matches!(
            load_blocking(&base.join("last.toml")),
            Loaded::Refused(_)
        ));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_garbled_log_or_missing_directory_is_refused() {
        let (base, dir) = after_rename("garbled");
        std::fs::write(base.join("bad.toml"), "not = [toml").unwrap();
        assert!(matches!(
            load_blocking(&base.join("bad.toml")),
            Loaded::Refused(_)
        ));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(
            load_blocking(&base.join("last.toml")),
            Loaded::Refused(_)
        ));
        std::fs::remove_dir_all(&base).unwrap();
    }
}

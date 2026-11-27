//! Starting a copy or move of paths that arrive from outside the panels' own dialogs (clipboard
//! paste, drag and drop): the same `JobSpec::Copy`/`Move` the F5/F6 dialogs submit, run by the
//! jobs controller, so progress, conflict questions and panel refresh behave exactly as there.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use ofm_core::Location;
use ofm_jobs::{ConflictAnswer, CopyOpts, JobSpec};

use crate::context::AppContext;
use crate::i18n::tr;
use crate::jobs::{self, SubmitOpts, ui};

/// Sources that are worth transferring into `dest`: a folder dropped on itself and (for a move)
/// items that already live in `dest` are dropped (nothing to do; a copy into the own folder
/// stays, the engine then asks how to resolve the name clash).
pub fn effective_sources(sources: &[PathBuf], dest: &Path, mv: bool) -> Vec<PathBuf> {
    sources
        .iter()
        .filter(|s| s.as_path() != dest)
        .filter(|s| !(mv && s.parent() == Some(dest)))
        .cloned()
        .collect()
}

/// Submit a copy (or move) of `sources` into the local directory `dest`. Shows a message when
/// nothing is left to do for a reason the user should know about; returns whether a job was
/// submitted.
pub fn submit(ctx: &Rc<AppContext>, sources: &[PathBuf], dest: &Path, mv: bool) -> bool {
    let sources = effective_sources(sources, dest, mv);
    if sources.is_empty() {
        return false;
    }
    let Some(controller) = jobs::controller(ctx) else {
        return false;
    };
    // The `[jobs]` defaults of the F5/F6 dialogs apply here too: there is no dialog to ask.
    let (opts, mode) = {
        let cfg = ctx.config.borrow();
        let opts = CopyOpts {
            rename_mask: None,
            follow_symlinks: cfg.jobs.follow_symlinks,
            sync: cfg.jobs.fsync_after_copy,
            on_conflict: (!cfg.general.confirm_overwrite).then_some(ConflictAnswer::Overwrite),
        };
        (opts, jobs::default_run_mode(&cfg))
    };
    let sources: Vec<Location> = sources.into_iter().map(Location::Local).collect();
    let dest_dir = dest.to_owned();
    let spec = if mv {
        JobSpec::Move {
            sources,
            dest_dir,
            opts,
        }
    } else {
        JobSpec::Copy {
            sources,
            dest_dir,
            opts,
        }
    };
    controller.submit_with(ctx, spec, mode, SubmitOpts::default());
    true
}

/// The directory of the active panel when files can be written there; otherwise a message is
/// shown and `None` is returned.
pub fn active_dest_dir(ctx: &Rc<AppContext>, title: &str) -> Option<PathBuf> {
    match ctx.active_controller().location() {
        Location::Local(p) => Some(p),
        _ => {
            ui::message(
                ctx,
                title,
                tr(
                    "Files can only be put into a folder on this computer, not into an archive or list.",
                ),
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn move_into_own_folder_is_nothing() {
        let got = effective_sources(&[p("/a/x"), p("/b/y")], Path::new("/a"), true);
        assert_eq!(got, vec![p("/b/y")]);
    }

    #[test]
    fn copy_into_own_folder_is_kept_for_the_conflict_dialog() {
        let got = effective_sources(&[p("/a/x")], Path::new("/a"), false);
        assert_eq!(got, vec![p("/a/x")]);
    }

    #[test]
    fn folder_onto_itself_is_dropped() {
        assert!(effective_sources(&[p("/a/d")], Path::new("/a/d"), false).is_empty());
        assert!(effective_sources(&[p("/a/d")], Path::new("/a/d"), true).is_empty());
    }
}

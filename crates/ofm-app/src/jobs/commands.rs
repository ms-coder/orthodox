//! Command handlers of the file-operation UI (each id is registered exactly once, here).

use std::path::PathBuf;
use std::rc::Rc;

use ofm_core::{EntryKind, cmd};
use ofm_jobs::{JobSpec, Outcome, RunMode};

use super::actions::{self, Action};
use super::targets::Targets;
use super::{SubmitOpts, ui, view};
use crate::chrome::statusline::{SizeFormat, format_size};
use crate::context::AppContext;
use crate::dialogs::copy::TransferKind;
use crate::dialogs::{copy, delete, link, mkdir, properties};
use crate::dispatch::CommandRegistry;
use crate::i18n::tr;
use crate::panel::PanelController;

/// More directories than this are summed by one job instead of one job per directory.
const PER_DIR_JOBS: usize = 20;

/// Register the commands.
pub fn install(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_COPY, |ctx| copy::present(ctx, TransferKind::Copy));
    reg.register(cmd::FILE_MOVE, |ctx| copy::present(ctx, TransferKind::Move));
    reg.register(cmd::FILE_RENAME, |ctx| {
        copy::present(ctx, TransferKind::Rename);
    });
    reg.register(cmd::FILE_MKDIR, mkdir::present);
    reg.register(cmd::FILE_DELETE, |ctx| delete::present(ctx, false));
    reg.register(cmd::FILE_DELETE_PERMANENT, |ctx| delete::present(ctx, true));
    reg.register(cmd::FILE_LINK, link::present);
    reg.register(cmd::FILE_PROPERTIES, properties::present);
    reg.register(cmd::FILE_CALC_SIZE, calc_size);
    reg.register(cmd::APP_JOBS, view::toggle);

    // Keys bound in the `dialog` section: the topmost dialog that cares gets them.
    for (id, action) in [
        (cmd::DIALOG_QUEUE, Action::Queue),
        (cmd::DIALOG_APPLY_ALL, Action::ApplyAll),
        (cmd::JOBS_PAUSE_RESUME, Action::PauseResume),
        (cmd::JOBS_CANCEL, Action::Cancel),
        (cmd::JOBS_CLEAR_FINISHED, Action::ClearFinished),
    ] {
        reg.register(id, move |ctx| {
            if !actions::dispatch(ctx, action) {
                actions::fallback(ctx, action);
            }
        });
    }
}

/// `file.calc_size`: size of the selected folders (or the one under the cursor), shown in the
/// panel's size column when each calculation finishes. With only files selected the folder under
/// the cursor is measured, so F3 on a folder (F19) always does something.
fn calc_size(ctx: &Rc<AppContext>) {
    let Some(mut targets) = Targets::from_active(ctx) else {
        return;
    };
    if targets.from_selection && !targets.has_dirs() {
        let cursor = targets.controller.model().cursor_entry().cloned();
        if let Some(dir) = cursor.filter(|e| e.kind == EntryKind::Dir && e.name != "..") {
            targets.entries = vec![dir];
            targets.from_selection = false;
        }
    }
    let Some(controller) = super::controller(ctx) else {
        return;
    };
    let dirs: Vec<(String, PathBuf)> = targets
        .entries
        .iter()
        .zip(targets.sources())
        .filter(|(e, _)| e.kind == EntryKind::Dir)
        .filter_map(|(e, loc)| match loc {
            ofm_core::Location::Local(p) => Some((e.name.to_string_lossy().into_owned(), p)),
            _ => None,
        })
        .collect();
    if dirs.is_empty() {
        return;
    }
    if dirs.len() > PER_DIR_JOBS {
        // Many folders: one job, one message with the total.
        let paths = dirs.into_iter().map(|(_, p)| p).collect();
        controller.submit_with(
            ctx,
            JobSpec::CalcSize { paths },
            RunMode::Parallel,
            SubmitOpts {
                origin: None,
                on_finished: Some(Box::new(|ctx, outcome| {
                    if let Outcome::Size(t) = outcome {
                        ui::message(
                            ctx,
                            tr("Size"),
                            &format!(
                                "{} \u{b7} {} {}, {} {}",
                                format_size(t.bytes, SizeFormat::Binary),
                                t.files,
                                tr("files"),
                                t.dirs,
                                tr("folders")
                            ),
                        );
                    }
                })),
            },
        );
        return;
    }
    for (name, path) in dirs {
        let panel = Rc::downgrade(&targets.controller);
        let location = targets.dir.clone();
        controller.submit_with(
            ctx,
            JobSpec::CalcSize { paths: vec![path] },
            RunMode::Parallel,
            SubmitOpts {
                origin: None,
                on_finished: Some(Box::new(move |ctx, outcome| {
                    if let (Outcome::Size(t), Some(panel)) = (outcome, panel.upgrade()) {
                        show_dir_size(ctx, &panel, &location, &name, t.bytes);
                    }
                })),
            },
        );
    }
}

/// Put a calculated folder size into the panel's entry (only while the panel still shows the
/// same folder and the entry is still listed).
fn show_dir_size(
    ctx: &Rc<AppContext>,
    panel: &Rc<PanelController>,
    location: &ofm_core::Location,
    name: &str,
    bytes: u64,
) {
    if &panel.location() != location {
        return;
    }
    let entry = {
        let m = panel.model();
        (0..m.visible_len())
            .filter_map(|i| m.visible(i))
            .find(|e| e.name == name)
            .cloned()
    };
    if let Some(mut entry) = entry {
        // Keep the number across refreshes (watcher re-stat, reload) while the mtime is the same.
        if let Some(cache) = ctx.ext.get::<super::dirsize::DirSizeCache>() {
            cache.store(panel, location, entry.name.clone(), entry.mtime_ns, bytes);
        }
        entry.size = bytes;
        entry.size_known = true;
        panel.mutate_view(|m| m.apply_update(vec![entry], &[]));
    }
}

//! What the results list does with a hit: go to it in the active panel, view it (F3), edit it
//! (F4). The Find window calls these; they use the existing panel navigation, viewer and editor
//! APIs and never touch the file system themselves.

use std::rc::Rc;

use gtk::prelude::*;
use ofm_search::Hit;

use super::rows::{self, FileTarget};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::navigation::goto::navigate;

/// Open the directory containing `hit` in the active panel with the cursor on it. Returns false
/// for hits that have no location a panel can show.
pub fn go_to(ctx: &Rc<AppContext>, hit: &Hit) -> bool {
    let Some((loc, name)) = rows::goto_target(hit) else {
        return false;
    };
    let ctrl = ctx.active_controller();
    if ctrl.location() == loc {
        // Already there: `navigate` would do nothing, so just move the cursor (and refresh, the
        // file may be new).
        ctrl.focus_name(name);
    } else {
        navigate(ctx, &ctrl, loc, Some(name));
    }
    true
}

/// Bring the main window to the front (after "go to" while the Find window stays open).
pub fn raise_main(ctx: &Rc<AppContext>) {
    if let Some(win) = ctx.window() {
        win.window.present();
    }
}

/// F3 on a hit: the viewer window on its file; for a content hit it scrolls to the first match
/// once the file is open. `Err` carries a message for hits that cannot be viewed.
pub fn view(ctx: &Rc<AppContext>, hit: &Hit) -> Result<(), String> {
    let Some(target) = rows::file_target(hit) else {
        return Err(tr("Only files can be viewed from the results.").to_owned());
    };
    match target {
        FileTarget::Local(path, offset) => {
            let Some(session) = crate::viewer::window::open(ctx, vec![path], 0) else {
                return Ok(());
            };
            if let Some(offset) = offset {
                crate::viewer::window::jump_to_offset_when_ready(&session, offset);
            }
        }
        FileTarget::Archive(archive, inner, offset) => {
            crate::panel::archive_extract::view_file_at(ctx, archive, inner, offset);
        }
    }
    Ok(())
}

/// F4 on a hit: the configured editor on a local file. Archive entries use the archive panel's
/// read-only temp-extract-and-view path; edits are never presented as archive write-back.
pub fn edit(ctx: &Rc<AppContext>, hit: &Hit) -> Result<(), String> {
    let Some(target) = rows::file_target(hit) else {
        return Err(tr("Only files can be edited from the results.").to_owned());
    };
    match target {
        FileTarget::Local(path, _) => crate::desktop::editor::open_in_editor(ctx, &path),
        FileTarget::Archive(archive, inner, offset) => {
            crate::panel::archive_extract::view_file_at(ctx, archive, inner, offset);
        }
    }
    Ok(())
}

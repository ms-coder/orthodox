//! What the results list does with a hit: go to it in the active panel, view it (F3), edit it
//! (F4). The Find window calls these; they use the existing panel navigation, viewer and editor
//! APIs and never touch the file system themselves.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use ofm_search::Hit;

use super::rows;
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
    let Some((path, offset)) = rows::file_target(hit) else {
        return Err(tr("Only local files can be viewed from the results.").to_owned());
    };
    let Some(session) = crate::viewer::window::open(ctx, vec![path], 0) else {
        return Ok(());
    };
    if let Some(off) = offset.filter(|o| *o > 0) {
        // The viewer opens files on a worker and has no start-offset argument: jump as soon as
        // the document and the viewport exist (a few ticks), give up after ~3 s.
        let mut tries = 0u32;
        glib::timeout_add_local(Duration::from_millis(75), move || {
            tries += 1;
            if session.len() > 0 {
                session.jump_to(off);
                if session.top() > 0 || tries > 40 {
                    return glib::ControlFlow::Break;
                }
            }
            if tries > 40 || !session.window.is_visible() {
                return glib::ControlFlow::Break;
            }
            glib::ControlFlow::Continue
        });
    }
    Ok(())
}

/// F4 on a hit: the configured editor on its file.
pub fn edit(ctx: &Rc<AppContext>, hit: &Hit) -> Result<PathBuf, String> {
    let Some((path, _)) = rows::file_target(hit) else {
        return Err(tr("Only local files can be edited from the results.").to_owned());
    };
    crate::desktop::editor::open_in_editor(ctx, &path);
    Ok(path)
}

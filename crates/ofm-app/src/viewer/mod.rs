//! The F3 viewer (F21, T5.2a): a window per file with a custom-drawn text / hex surface.
//!
//! Layering:
//! - [`scan`], [`layout`], [`worker`]: GTK-free logic and background work. Everything that reads
//!   the file (open, layout of the visible page, line counting, search, go to) runs on worker
//!   threads and answers with [`worker::Msg`]; the GTK thread never touches the file system and a
//!   10 GB sparse file opens at once (mmap through `ofm-viewer`, page faults on the worker).
//! - [`view`]: the `ViewerView` widget that only draws a given page (snapshot-based, layouts cached).
//! - [`session`]: per-window state, turns commands into page requests, applies answers.
//! - [`window`], [`dialogs`]: window chrome and the search / go-to / encoding dialogs.
//!
//! Keys: the window uses the central dispatcher with `WindowKind::Viewer`; every user action is
//! a `viewer.*` command registered here (plus `file.view` that opens the window).

mod dialogs;
mod layout;
mod scan;
pub mod session;
#[cfg(test)]
mod testutil;
mod view;
pub mod window;
mod worker;

use std::cell::RefCell;

use adw::prelude::*;
use std::path::PathBuf;
use std::rc::Rc;

use ofm_core::{Entry, EntryKind, Location, TargetKind, cmd};

use crate::context::AppContext;
use crate::dispatch::CommandRegistry;
use crate::panel::PanelController;
use session::Session;
pub use worker::{Dir, GotoTarget, SearchSpec};

/// Open viewer windows (state in `ctx.ext`).
#[derive(Default)]
pub struct ViewerHost {
    sessions: RefCell<Vec<Rc<Session>>>,
}

impl ViewerHost {
    pub(crate) fn add(&self, s: Rc<Session>) {
        self.sessions.borrow_mut().push(s);
    }

    pub(crate) fn remove(&self, s: &Rc<Session>) {
        self.sessions.borrow_mut().retain(|x| !Rc::ptr_eq(x, s));
    }

    /// The viewer commands act on: the focused viewer window, else the newest one.
    pub fn current(&self) -> Option<Rc<Session>> {
        let sessions = self.sessions.borrow();
        sessions
            .iter()
            .find(|s| s.window.is_active())
            .or_else(|| sessions.last())
            .cloned()
    }

    /// Number of open viewers.
    pub fn len(&self) -> usize {
        self.sessions.borrow().len()
    }

    /// True when no viewer is open.
    pub fn is_empty(&self) -> bool {
        self.sessions.borrow().is_empty()
    }
}

/// The viewer command target of `ctx`, if a viewer is open.
pub fn current(ctx: &AppContext) -> Option<Rc<Session>> {
    ctx.ext.get::<ViewerHost>()?.current()
}

/// Create the viewer state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(ViewerHost::default()));
}

/// Files in the order of the panel list that `viewer.next_file` steps through, and the position of
/// the cursor file; `None` when the cursor is not on a viewable file.
fn viewable(controller: &PanelController) -> Option<(Vec<PathBuf>, usize)> {
    let dir = controller.location();
    let model = controller.model();
    let cursor = model.cursor_entry()?.clone();
    let path_of = |e: &Entry| -> Option<PathBuf> {
        e.real_path.clone().or_else(|| match &dir {
            Location::Local(d) => Some(d.join(&e.name)),
            _ => None,
        })
    };
    let is_file = |e: &Entry| {
        matches!(
            e.kind,
            EntryKind::File
                | EntryKind::Symlink {
                    target: Some(TargetKind::File)
                }
        )
    };
    if !is_file(&cursor) {
        return None;
    }
    let mut list = Vec::new();
    let mut idx = None;
    for i in 0..model.visible_len() {
        let Some(e) = model.visible(i) else { continue };
        if !is_file(e) {
            continue;
        }
        let Some(p) = path_of(e) else { continue };
        if e.name == cursor.name {
            idx = Some(list.len());
        }
        list.push(p);
    }
    idx.map(|i| (list, i))
}

/// Open the viewer on the file under the active panel's cursor (directories: nothing happens).
fn file_view(ctx: &Rc<AppContext>) {
    let Some((files, idx)) = viewable(&ctx.active_controller()) else {
        return;
    };
    window::open(ctx, files, idx);
}

fn with_session(ctx: &Rc<AppContext>, f: impl FnOnce(&Rc<Session>)) {
    if let Some(s) = current(ctx) {
        f(&s);
    }
}

/// Register `file.view` and every `viewer.*` command.
pub fn install_commands(reg: &mut CommandRegistry) {
    reg.register(cmd::FILE_VIEW, file_view);
    reg.register(cmd::VIEWER_CLOSE, |ctx| {
        with_session(ctx, |s| {
            // Esc first stops a running search; a second press closes the window.
            if !s.cancel_search() {
                s.window.close();
            }
        });
    });
    reg.register(cmd::VIEWER_TOGGLE_HEX, |ctx| {
        with_session(ctx, |s| s.toggle_hex());
    });
    reg.register(cmd::VIEWER_WRAP, |ctx| {
        with_session(ctx, |s| s.toggle_wrap())
    });
    reg.register(cmd::VIEWER_ENCODING, |ctx| {
        with_session(ctx, |s| dialogs::encoding(ctx, s));
    });
    reg.register(cmd::VIEWER_SEARCH, |ctx| {
        with_session(ctx, |s| dialogs::search(ctx, s));
    });
    reg.register(cmd::VIEWER_SEARCH_NEXT, |ctx| {
        with_session(ctx, |s| {
            if !s.search_again(Dir::Forward) {
                dialogs::search(ctx, s);
            }
        });
    });
    reg.register(cmd::VIEWER_SEARCH_PREV, |ctx| {
        with_session(ctx, |s| {
            if !s.search_again(Dir::Backward) {
                dialogs::search(ctx, s);
            }
        });
    });
    reg.register(cmd::VIEWER_GOTO, |ctx| {
        with_session(ctx, |s| dialogs::goto(ctx, s));
    });
    reg.register(cmd::VIEWER_LINE_UP, |ctx| {
        with_session(ctx, |s| s.move_rows(-1))
    });
    reg.register(cmd::VIEWER_LINE_DOWN, |ctx| {
        with_session(ctx, |s| s.move_rows(1))
    });
    reg.register(cmd::VIEWER_PAGE_UP, |ctx| {
        with_session(ctx, |s| s.move_pages(-1))
    });
    reg.register(cmd::VIEWER_PAGE_DOWN, |ctx| {
        with_session(ctx, |s| s.move_pages(1))
    });
    reg.register(cmd::VIEWER_TOP, |ctx| with_session(ctx, |s| s.go_top()));
    reg.register(cmd::VIEWER_BOTTOM, |ctx| {
        with_session(ctx, |s| s.go_bottom())
    });
    reg.register(cmd::VIEWER_SCROLL_LEFT, |ctx| {
        with_session(ctx, |s| s.scroll_cols(-8));
    });
    reg.register(cmd::VIEWER_SCROLL_RIGHT, |ctx| {
        with_session(ctx, |s| s.scroll_cols(8));
    });
    reg.register(cmd::VIEWER_NEXT_FILE, |ctx| {
        with_session(ctx, |s| s.step_file(1))
    });
    reg.register(cmd::VIEWER_PREV_FILE, |ctx| {
        with_session(ctx, |s| s.step_file(-1))
    });
}

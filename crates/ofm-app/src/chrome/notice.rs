//! Transient status messages above the F-key bar: a slim revealer row (like the job progress
//! and compare-result rows) that hides itself after a few seconds. Users of [`show`]: dispatching
//! a command without a handler ("Not available yet") and the config-problems hint at startup.
//!
//! The row is built lazily on the first [`show`] after the main window exists, so nothing has to
//! be added to `hooks.rs`; the state itself is created by `chrome::install_state`. Without a
//! window (headless tests) the text is only remembered in [`NoticeState::last`].

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;

use crate::context::AppContext;
use crate::i18n::tr;

/// How long a message stays visible.
const SHOWN_FOR: Duration = Duration::from_secs(6);

/// Notice state in `ctx.ext`.
#[derive(Default)]
pub struct NoticeState {
    row: RefCell<Option<Rc<Row>>>,
    /// Text of the last message (tests; also shown when the window appears later).
    pub last: RefCell<Option<String>>,
}

struct Row {
    revealer: gtk::Revealer,
    label: gtk::Label,
    /// Bumped per message so an older timeout cannot hide a newer message.
    serial: Cell<u64>,
}

/// Create the state.
pub fn install_state(ctx: &Rc<AppContext>) {
    ctx.ext.insert(Rc::new(NoticeState::default()));
}

/// Show `text` for a few seconds. Without a main window the text is only stored.
pub fn show(ctx: &Rc<AppContext>, text: &str) {
    let Some(state) = ctx.ext.get::<NoticeState>() else {
        return;
    };
    *state.last.borrow_mut() = Some(text.to_owned());
    tracing::info!(target: "ofm::notice", "{text}");
    let Some(row) = state.row.borrow().clone() else {
        return;
    };
    row.label.set_text(text);
    row.revealer.set_reveal_child(true);
    let serial = row.serial.get().wrapping_add(1);
    row.serial.set(serial);
    glib::timeout_add_local_once(SHOWN_FOR, move || {
        if row.serial.get() == serial {
            row.revealer.set_reveal_child(false);
        }
    });
}

/// Text of the last message (tests).
pub fn last_text(ctx: &AppContext) -> Option<String> {
    ctx.ext
        .get::<NoticeState>()
        .and_then(|s| s.last.borrow().clone())
}

/// Whether the message row is showing (tests).
pub fn is_shown(ctx: &AppContext) -> bool {
    ctx.ext
        .get::<NoticeState>()
        .and_then(|s| s.row.borrow().clone())
        .is_some_and(|r| r.revealer.reveals_child())
}

/// Build the row on first use, above the F-key bar.
fn ensure_row(ctx: &Rc<AppContext>, state: &Rc<NoticeState>) -> Option<Rc<Row>> {
    if let Some(row) = state.row.borrow().clone() {
        return Some(row);
    }
    let win = ctx.window()?;
    let label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["ofm-status"])
        .name("ofm-notice")
        .build();
    label.update_property(&[gtk::accessible::Property::Label(tr("Status message"))]);
    label.set_accessible_role(gtk::AccessibleRole::Status);
    let box_ = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    box_.add_css_class("ofm-statusbox");
    box_.append(&label);
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
    revealer.set_child(Some(&box_));
    revealer.set_reveal_child(false);
    win.fkeybar_slot.prepend(&revealer);
    let row = Rc::new(Row {
        revealer,
        label,
        serial: Cell::new(0),
    });
    *state.row.borrow_mut() = Some(Rc::clone(&row));
    Some(row)
}

/// Show the stored message once the window exists (startup path: the warnings are stored before
/// `MainWindow` is built). Called from `chrome::install_ui`.
pub fn show_pending(ctx: &Rc<AppContext>) {
    let Some(state) = ctx.ext.get::<NoticeState>() else {
        return;
    };
    if ensure_row(ctx, &state).is_some()
        && let Some(text) = state.last.borrow().clone()
    {
        show(ctx, &text);
    }
}

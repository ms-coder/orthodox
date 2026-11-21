//! A transient one-line message above the F-key bar ("3 differing on the left, ...").
//!
//! The window has no general status-message mechanism yet, so the compare feature brings the
//! smallest one it needs: a revealer in `MainWindow::fkeybar_slot` (the same slot the job
//! progress row uses) that hides itself after a few seconds. Colours come from the theme through
//! the `ofm-statusbox` / `ofm-status` classes.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;

use crate::context::AppContext;
use crate::window::MainWindow;

use super::CompareState;

/// How long a message stays visible.
const SHOWN_FOR: Duration = Duration::from_secs(8);

/// The widgets of the message row.
pub struct MessageRow {
    revealer: gtk::Revealer,
    label: gtk::Label,
    /// Bumped per message so an older timeout cannot hide a newer message.
    serial: Cell<u64>,
}

/// Slot for the row in [`CompareState`].
pub type RowSlot = RefCell<Option<Rc<MessageRow>>>;

/// Build the row into the F-key slot.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    let Some(state) = ctx.ext.get::<CompareState>() else {
        return;
    };
    let label = gtk::Label::builder()
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .css_classes(["ofm-status"])
        .build();
    label.update_property(&[gtk::accessible::Property::Label("Compare result")]);
    label.set_accessible_role(gtk::AccessibleRole::Status);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    row.add_css_class("ofm-statusbox");
    row.append(&label);
    let revealer = gtk::Revealer::new();
    revealer.set_transition_type(gtk::RevealerTransitionType::SlideUp);
    revealer.set_child(Some(&row));
    revealer.set_reveal_child(false);
    win.fkeybar_slot.prepend(&revealer);
    *state.row.borrow_mut() = Some(Rc::new(MessageRow {
        revealer,
        label,
        serial: Cell::new(0),
    }));
}

/// Show `text` for a few seconds (and remember it in [`CompareState::last_message`]).
pub fn show(ctx: &Rc<AppContext>, text: &str) {
    show_with(ctx, text, true);
}

/// Show `text` until the next message replaces it (progress of a running comparison).
pub fn show_sticky(ctx: &Rc<AppContext>, text: &str) {
    show_with(ctx, text, false);
}

fn show_with(ctx: &Rc<AppContext>, text: &str, auto_hide: bool) {
    let Some(state) = ctx.ext.get::<CompareState>() else {
        return;
    };
    *state.last_message.borrow_mut() = Some(text.to_owned());
    tracing::info!(target: "ofm::compare", "{text}");
    let Some(row) = state.row.borrow().clone() else {
        return;
    };
    row.label.set_text(text);
    row.revealer.set_reveal_child(true);
    let serial = row.serial.get().wrapping_add(1);
    row.serial.set(serial);
    if auto_hide {
        gtk::glib::timeout_add_local_once(SHOWN_FOR, move || {
            if row.serial.get() == serial {
                row.revealer.set_reveal_child(false);
            }
        });
    }
}

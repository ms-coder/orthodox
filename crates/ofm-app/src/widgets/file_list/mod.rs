//! The custom-drawn file list (ADR-0002). Owner: T2.1 (everything under `widgets/file_list/`).
//!
//! T2.0 ships a **stub** with the final public surface: a `gtk::Widget` subclass that draws plain
//! single-column rows (name + size) from a bound [`PanelController`], follows the cursor, scrolls
//! with the mouse wheel and reports its geometry to the controller. T2.1 replaces the internals
//! (Brief/Full/Custom columns, header, frames, highlight rules, mouse, accessibility, caching)
//! and may extend the public API; the items marked *stable* below must keep working because
//! `window/panel_view.rs` (T2.2) and the commands use them.
//!
//! Key handling does NOT belong here: keys arrive as commands (`dispatch.rs`); the cursor
//! commands are registered in [`install_commands`] (`commands.rs`, also T2.1).

mod commands;
mod imp;

use std::rc::Rc;

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use crate::panel::PanelController;
use crate::theme::Palette;

pub use commands::install_commands;

glib::wrapper! {
    /// Custom-drawn list of one panel tab.
    pub struct FileList(ObjectSubclass<imp::FileList>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for FileList {
    fn default() -> Self {
        Self::new()
    }
}

impl FileList {
    /// New empty list (bind a controller and a palette before it shows anything).
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// *Stable.* Show `ctrl`'s model; `None` unbinds. Subscribes to `ctrl.changed`.
    pub fn bind(&self, ctrl: Option<Rc<PanelController>>) {
        self.imp().bind(self, ctrl);
    }

    /// *Stable.* Use `palette` for painting and redraw.
    pub fn set_palette(&self, palette: Rc<Palette>) {
        self.imp().set_palette(palette);
        self.queue_draw();
    }

    /// *Stable.* Whether this is the active panel (cursor colours) and redraw.
    pub fn set_active(&self, active: bool) {
        self.imp().set_active(active);
        self.queue_draw();
    }
}

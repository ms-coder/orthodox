//! The custom-drawn file list (ADR-0002). Owner: T2.1 (everything under `widgets/file_list/`).
//!
//! A `gtk::Widget` subclass that draws one panel tab from a bound [`PanelController`]:
//! Brief (column-major names), Full and Custom (header + columns) views, the cursor and marked
//! rows, theme highlight rules and frames. It reports its geometry to the controller, follows
//! the cursor, scrolls with the wheel and handles the mouse. Pango layouts are cached and only
//! the visible window of the model is touched, so 100k entries scroll like 100.
//!
//! Key handling does NOT belong here: keys arrive as commands (`dispatch.rs`); the commands of
//! this module (cursor, view mode, sort, hidden files) are registered in [`install_commands`].
//!
//! Modules: `layout` (grid/scroll math), `geo` (frame geometry, hit testing), `format` (cell text),
//! `settings` (view modes, `[panels]` settings), `text` (layout cache), `paint` (drawing),
//! `imp` (the widget).
//!
//! # Wiring points for other modules
//!
//! * [`FileList::install_mouse`] connects clicks to the application (active panel, open, context
//!   menu); `window::panel_view` calls it once per side.
//! * [`configure`] installs the `[panels]` config section (startup and `app.reload_config`).
//! * [`view_mode`] / [`set_view_mode`] read and set the per-tab view mode (session save/restore).
//! * Signals: `activate-panel`, `open-entry(u32)`, `context-menu(f64, f64)` and the
//!   [`FileList::set_drag_provider`] hook for custom drag payloads.

mod commands;
mod format;
mod geo;
mod imp;
mod layout;
mod paint;
mod settings;
mod text;

#[cfg(test)]
mod tests;

use std::rc::{Rc, Weak};

use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ofm_core::cmd;

use crate::context::AppContext;
use crate::panel::{PanelController, Side};
use crate::theme::Palette;

pub use commands::install_commands;
pub use settings::{
    Column, Settings, SizeFormat, ViewMode, configure, set_view_mode, settings, view_mode,
};

/// Frame timing counters of a list (see the `ofm::file_list` tracing target).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Frames painted.
    pub frames: u64,
    /// Duration of the last frame, microseconds.
    pub last_us: u64,
    /// Slowest frame, microseconds.
    pub max_us: u64,
    /// Sum of all frame durations, microseconds.
    pub total_us: u64,
}

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

    /// Accessible name of the list (e.g. "Left panel").
    pub fn set_accessible_label(&self, label: &str) {
        self.update_property(&[gtk::accessible::Property::Label(label)]);
    }

    /// Frame timing of this list.
    pub fn frame_stats(&self) -> FrameStats {
        self.imp().stats()
    }

    /// `(first visible entry, rows per column, columns)` of the current layout.
    pub fn window(&self) -> (u32, u32, u32) {
        self.imp().window()
    }

    /// Replace the drag payload builder: called with the press position, returns the content to
    /// drag (`None` cancels the drag). The default drags the marked files (or the one under the
    /// pointer) of a local directory as a `gdk::FileList`.
    pub fn set_drag_provider(
        &self,
        provider: impl Fn(&FileList, f64, f64) -> Option<gtk::gdk::ContentProvider> + 'static,
    ) {
        self.imp()
            .set_drag_provider(Some(Rc::new(provider) as imp::DragProvider));
    }

    /// Back to the default drag payload.
    pub fn clear_drag_provider(&self) {
        self.imp().set_drag_provider(None);
    }

    /// A mouse press landed in the list: its panel should become active.
    pub fn connect_activate_panel(&self, f: impl Fn(&FileList) + 'static) -> glib::SignalHandlerId {
        self.connect_closure(
            "activate-panel",
            false,
            glib::closure_local!(move |list: FileList| f(&list)),
        )
    }

    /// A row was double-clicked (visible index).
    pub fn connect_open_entry(
        &self,
        f: impl Fn(&FileList, u32) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "open-entry",
            false,
            glib::closure_local!(move |list: FileList, idx: u32| f(&list, idx)),
        )
    }

    /// Secondary click at (x, y) in widget coordinates.
    pub fn connect_context_menu(
        &self,
        f: impl Fn(&FileList, f64, f64) + 'static,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "context-menu",
            false,
            glib::closure_local!(move |list: FileList, x: f64, y: f64| f(&list, x, y)),
        )
    }

    /// Route the mouse to the application: a press activates panel `side`, a double click runs
    /// `panel.open`, a secondary click runs `panel.context_menu`. (Header clicks sort directly.)
    pub fn install_mouse(&self, ctx: &Rc<AppContext>, side: Side) {
        let weak: Weak<AppContext> = Rc::downgrade(ctx);
        self.connect_activate_panel({
            let weak = weak.clone();
            move |_| {
                if let Some(ctx) = weak.upgrade() {
                    ctx.set_active_side(side);
                }
            }
        });
        self.connect_open_entry({
            let weak = weak.clone();
            move |_, _| {
                if let Some(ctx) = weak.upgrade() {
                    ctx.execute(cmd::PANEL_OPEN);
                }
            }
        });
        self.connect_context_menu(move |_, _, _| {
            if let Some(ctx) = weak.upgrade() {
                ctx.execute(cmd::PANEL_CONTEXT_MENU);
            }
        });
    }
}

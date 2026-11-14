//! `MainWindow`: the `adw::ApplicationWindow` with its layout regions.

use std::rc::Rc;

use adw::prelude::*;

use gtk::glib;

use super::layout;
use super::panel_view::PanelView;
use super::session::{self, SessionState};
use crate::context::AppContext;
use crate::dispatch::{self, WindowKind};
use crate::panel::{LEFT, RIGHT};
use crate::theme;

/// The main window and handles to its regions. All fields are public so hook functions can fill
/// the slots; do not remove fields (T2.2 may add more).
pub struct MainWindow {
    /// The toplevel.
    pub window: adw::ApplicationWindow,
    /// Top slot: F9 menu bar (chrome).
    pub menubar_slot: gtk::Box,
    /// Horizontal split holding the two panel views.
    pub panels: gtk::Paned,
    /// Left panel view.
    pub left: PanelView,
    /// Right panel view.
    pub right: PanelView,
    /// Terminal output area above the command line (M4).
    pub terminal_slot: gtk::Box,
    /// Command line row (M4).
    pub cmdline_slot: gtk::Box,
    /// Bottom slot: F-key bar (chrome).
    pub fkeybar_slot: gtk::Box,
}

impl MainWindow {
    /// Build the window for `app`, wire the key controller, run the install hooks, register the
    /// window in `ctx` and return it (not yet presented).
    pub fn new(app: &adw::Application, ctx: &Rc<AppContext>) -> Rc<MainWindow> {
        let left = PanelView::new(ctx, LEFT);
        let right = PanelView::new(ctx, RIGHT);

        let panels = gtk::Paned::new(gtk::Orientation::Horizontal);
        panels.set_wide_handle(false);
        panels.set_resize_start_child(true);
        panels.set_resize_end_child(true);
        panels.set_shrink_start_child(false);
        panels.set_shrink_end_child(false);
        panels.set_start_child(Some(&left.root));
        panels.set_end_child(Some(&right.root));
        panels.set_vexpand(true);

        let slot = || gtk::Box::new(gtk::Orientation::Vertical, 0);
        let menubar_slot = slot();
        let terminal_slot = slot();
        let cmdline_slot = slot();
        let fkeybar_slot = slot();

        let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
        column.append(&menubar_slot);
        column.append(&panels);
        column.append(&terminal_slot);
        column.append(&cmdline_slot);
        column.append(&fkeybar_slot);

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("OFM")
            .default_width(1200)
            .default_height(800)
            .content(&column)
            .build();
        window.add_css_class("ofm-window");

        // One key controller for the whole window (invariant 5).
        dispatch::install_key_controller(&window, ctx, WindowKind::Main);

        let win = Rc::new(MainWindow {
            window,
            menubar_slot,
            panels,
            left,
            right,
            terminal_slot,
            cmdline_slot,
            fkeybar_slot,
        });
        crate::hooks::install_panel_view(ctx, &win.left);
        crate::hooks::install_panel_view(ctx, &win.right);
        crate::hooks::install_ui(ctx, &win);
        ctx.set_window(Rc::clone(&win));
        win
    }

    /// Show the window.
    pub fn present(&self) {
        self.window.present();
    }
}

/// Window-level UI after construction: restored size, split ratio, one-panel mode, live theme
/// colours and the save-on-close handler.
pub fn install_ui(ctx: &Rc<AppContext>, win: &MainWindow) {
    if let Some(state) = ctx.ext.get::<SessionState>() {
        let w = &state.loaded.window;
        if w.width > 0 && w.height > 0 {
            win.window.set_default_size(w.width, w.height);
        }
        if w.maximized {
            win.window.set_maximized(true);
        }
    }
    layout::install_ui(ctx, win);
    session::restore_on_start(ctx);
    theme::registry::install_ui(ctx, win.window.upcast_ref());

    // Close request: save the session on a worker, then destroy the window (never block the UI).
    let weak = Rc::downgrade(ctx);
    win.window.connect_close_request(move |w| {
        let Some(ctx) = weak.upgrade() else {
            return glib::Propagation::Proceed;
        };
        let Some(state) = ctx.ext.get::<SessionState>() else {
            return glib::Propagation::Proceed;
        };
        if state.closing.replace(true) {
            return glib::Propagation::Stop; // a save is already running
        }
        let w = w.clone();
        session::save_async(&ctx, move || w.destroy());
        glib::Propagation::Stop
    });
}

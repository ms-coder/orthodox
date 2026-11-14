//! `PanelView`: one side of the window: tab bar slot, path header, file list, status line.

use std::rc::Rc;

use gtk::prelude::*;

use crate::context::AppContext;
use crate::i18n::tr;
use crate::panel::{LoadState, PanelController, PanelHost, Side};
use crate::widgets::file_list::FileList;

/// Widgets of one panel side. Fields are public so other modules can decorate the view in their
/// `install_panel_view` hooks.
pub struct PanelView {
    /// 0 = left, 1 = right.
    pub side: Side,
    /// Root widget (put into the paned).
    pub root: gtk::Box,
    /// Above the path header: the tab bar goes here (T2.3).
    pub tab_bar_slot: gtk::Box,
    /// Current location as text.
    pub path_label: gtk::Label,
    /// Overlay around the list: popovers/overlays (quick search) attach here.
    pub overlay: gtk::Overlay,
    /// The custom-drawn list.
    pub list: FileList,
    /// Status text of the panel.
    pub status_label: gtk::Label,
    /// Row below the list for extra status widgets (progress, free space).
    pub status_slot: gtk::Box,
    host: Rc<PanelHost>,
}

impl PanelView {
    /// Build the view and bind it to the side's active tab; keeps following tab switches, the
    /// active-panel state and theme changes.
    pub fn new(ctx: &Rc<AppContext>, side: Side) -> PanelView {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("ofm-panel");
        root.set_hexpand(true);
        let tab_bar_slot = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let path_label = gtk::Label::new(None);
        path_label.set_xalign(0.0);
        path_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        path_label.add_css_class("ofm-path");
        let list = FileList::new();
        list.set_palette(ctx.theme.palette());
        let overlay = gtk::Overlay::new();
        overlay.set_child(Some(&list));
        overlay.set_vexpand(true);
        let status_label = gtk::Label::new(None);
        status_label.set_xalign(0.0);
        status_label.set_hexpand(true);
        status_label.add_css_class("ofm-status");
        let status_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_slot.append(&status_label);
        root.append(&tab_bar_slot);
        root.append(&path_label);
        root.append(&overlay);
        root.append(&status_slot);

        let host = Rc::clone(ctx.host(side));
        let view = PanelView {
            host: Rc::clone(&host),
            side,
            root,
            tab_bar_slot,
            path_label,
            overlay,
            list,
            status_label,
            status_slot,
        };
        view.sync_active(ctx.active_side() == side);

        // Follow the active tab of this side.
        view.bind_controller(host.active());
        {
            let v = view.handles();
            host.active_changed
                .connect(move |ctrl| v.bind_controller(Rc::clone(ctrl)));
        }
        // Follow the active panel.
        {
            let v = view.handles();
            ctx.active_changed
                .connect(move |active| v.sync_active(*active == side));
        }
        // Follow the theme.
        {
            let list = view.list.clone();
            ctx.theme
                .changed
                .connect(move |palette| list.set_palette(Rc::clone(palette)));
        }
        view
    }

    /// Cheap clone of the widget handles for use in closures.
    fn handles(&self) -> PanelViewHandles {
        PanelViewHandles {
            root: self.root.clone(),
            path_label: self.path_label.clone(),
            status_label: self.status_label.clone(),
            list: self.list.clone(),
            host: Rc::clone(&self.host),
        }
    }

    fn sync_active(&self, active: bool) {
        self.handles().sync_active(active);
    }

    fn bind_controller(&self, ctrl: Rc<PanelController>) {
        self.handles().bind_controller(ctrl);
    }
}

#[derive(Clone)]
struct PanelViewHandles {
    root: gtk::Box,
    path_label: gtk::Label,
    status_label: gtk::Label,
    list: FileList,
    host: Rc<PanelHost>,
}

impl PanelViewHandles {
    fn sync_active(&self, active: bool) {
        if active {
            self.root.add_css_class("active");
        } else {
            self.root.remove_css_class("active");
        }
        self.list.set_active(active);
    }

    fn bind_controller(&self, ctrl: Rc<PanelController>) {
        self.list.bind(Some(Rc::clone(&ctrl)));
        self.update_texts(&ctrl);
        let this = self.clone();
        let weak = Rc::downgrade(&ctrl);
        ctrl.changed.connect(move |_ev| {
            // Only the tab currently shown by this side updates the texts.
            if let Some(ctrl) = weak.upgrade()
                && Rc::ptr_eq(&this.host.active(), &ctrl)
            {
                this.update_texts(&ctrl);
            }
        });
    }

    fn update_texts(&self, ctrl: &PanelController) {
        self.path_label.set_text(&ctrl.location().display_lossy());
        let status = match ctrl.state() {
            LoadState::Loading => tr("Loading...").to_owned(),
            LoadState::Failed(msg) => msg,
            LoadState::Idle => {
                let m = ctrl.model();
                format!("{} items, {} selected", m.total_len(), m.selected_count())
            }
        };
        self.status_label.set_text(&status);
    }
}

/// Hook: decorate a freshly built panel view (T2.2: active-panel frame, header buttons).
pub fn install_panel_view(_ctx: &Rc<AppContext>, _view: &PanelView) {}

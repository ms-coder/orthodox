//! `PanelView`: one side of the window: tab bar slot, path header, file list, status line.

use std::cell::{Cell, RefCell};
use std::ops::ControlFlow;
use std::rc::{Rc, Weak};

use gtk::prelude::*;
use ofm_core::Location;

use crate::chrome::statusline::{self, SizeFormat, StatusFormat};
use crate::context::AppContext;
use crate::i18n::tr;
use crate::panel::{LoadState, PanelController, PanelEvent, PanelHost, Side};
use crate::widgets::file_list::FileList;
use crate::wiring;

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
    /// Status text of the panel: the file under the cursor (or the load state).
    pub status_label: gtk::Label,
    /// Selection summary ("3 selected (1.2 MiB) / 120").
    pub summary_label: gtk::Label,
    /// Free space of the panel's file system.
    pub space_label: gtk::Label,
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
        status_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
        status_label.add_css_class("ofm-status");
        let summary_label = gtk::Label::new(None);
        summary_label.add_css_class("ofm-status-dim");
        let space_label = gtk::Label::new(None);
        space_label.add_css_class("ofm-status-dim");
        let status_slot = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        status_slot.add_css_class("ofm-statusbox");
        status_slot.append(&status_label);
        status_slot.append(&summary_label);
        status_slot.append(&space_label);
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
            summary_label,
            space_label,
            status_slot,
        };
        view.sync_active(ctx.active_side() == side);

        // Follow the active tab of this side.
        let handles = view.handles(ctx);
        handles.bind_controller(host.active());
        {
            let v = handles.clone();
            host.active_changed
                .connect(move |ctrl| v.bind_controller(Rc::clone(ctrl)));
        }
        // Follow the active panel.
        {
            let v = handles.clone();
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
    fn handles(&self, ctx: &Rc<AppContext>) -> PanelViewHandles {
        PanelViewHandles {
            root: self.root.clone(),
            path_label: self.path_label.clone(),
            status_label: self.status_label.clone(),
            summary_label: self.summary_label.clone(),
            space_label: self.space_label.clone(),
            list: self.list.clone(),
            host: Rc::clone(&self.host),
            ctx: Rc::downgrade(ctx),
            space_generation: Rc::new(Cell::new(0)),
            space_for: Rc::new(RefCell::new(None)),
        }
    }

    fn sync_active(&self, active: bool) {
        set_active_look(&self.root, &self.list, active);
    }
}

fn set_active_look(root: &gtk::Box, list: &FileList, active: bool) {
    if active {
        root.add_css_class("active");
    } else {
        root.remove_css_class("active");
    }
    list.set_active(active);
}

#[derive(Clone)]
struct PanelViewHandles {
    root: gtk::Box,
    path_label: gtk::Label,
    status_label: gtk::Label,
    summary_label: gtk::Label,
    space_label: gtk::Label,
    list: FileList,
    host: Rc<PanelHost>,
    ctx: Weak<AppContext>,
    /// Bumped per free-space request; older answers are dropped.
    space_generation: Rc<Cell<u64>>,
    /// Location the shown free-space value belongs to.
    space_for: Rc<RefCell<Option<Location>>>,
}

impl PanelViewHandles {
    fn sync_active(&self, active: bool) {
        set_active_look(&self.root, &self.list, active);
    }

    fn bind_controller(&self, ctrl: Rc<PanelController>) {
        self.list.bind(Some(Rc::clone(&ctrl)));
        // A different tab may live somewhere else: forget the old free-space value.
        self.space_for.borrow_mut().take();
        self.update_texts(&ctrl);
        self.update_space(&ctrl);
        let this = self.clone();
        let weak = Rc::downgrade(&ctrl);
        ctrl.changed.connect(move |ev| {
            // Only the tab currently shown by this side updates the texts.
            if let Some(ctrl) = weak.upgrade()
                && Rc::ptr_eq(&this.host.active(), &ctrl)
            {
                this.update_texts(&ctrl);
                if matches!(ev, PanelEvent::Location | PanelEvent::State) {
                    this.update_space(&ctrl);
                }
            }
        });
    }

    fn status_format(&self) -> StatusFormat {
        match self.ctx.upgrade() {
            Some(ctx) => {
                let cfg = ctx.config.borrow();
                StatusFormat {
                    size: SizeFormat::from_config(&cfg.panels.size_format),
                    time: cfg.panels.time_format.clone(),
                }
            }
            None => StatusFormat::default(),
        }
    }

    fn update_texts(&self, ctrl: &PanelController) {
        self.path_label.set_text(&ctrl.location().display_lossy());
        let fmt = self.status_format();
        let (left, summary) = match ctrl.state() {
            LoadState::Loading => (tr("Loading...").to_owned(), String::new()),
            LoadState::Failed(msg) => (msg, String::new()),
            LoadState::Idle => {
                let m = ctrl.model();
                let left = m
                    .cursor_entry()
                    .map(|e| statusline::cursor_info(e, &fmt))
                    .unwrap_or_default();
                let summary = statusline::summary(
                    m.total_len(),
                    m.selected_count(),
                    m.selected_size(),
                    fmt.size,
                );
                (left, summary)
            }
        };
        self.status_label.set_text(&left);
        self.summary_label.set_text(&summary);
    }

    /// Ask a worker for the free space of the tab's file system (statvfs is file I/O).
    fn update_space(&self, ctrl: &PanelController) {
        let loc = ctrl.location();
        if self.space_for.borrow().as_ref() == Some(&loc) && ctrl.state() != LoadState::Idle {
            return;
        }
        if matches!(loc, Location::List { .. }) {
            self.space_label.set_text("");
            return;
        }
        let Ok(vfs) = ctrl.services().vfs.get(&loc) else {
            self.space_label.set_text("");
            return;
        };
        *self.space_for.borrow_mut() = Some(loc.clone());
        let generation = self.space_generation.get() + 1;
        self.space_generation.set(generation);
        let rx = wiring::run_on_pool(&ctrl.services().pool, move |tx| {
            let _ = tx.send_blocking(vfs.space(&loc).ok().map(|s| s.free));
        });
        let this = self.clone();
        wiring::attach(
            rx,
            move |free| {
                if this.space_generation.get() == generation {
                    let text = free.map_or_else(String::new, |f| {
                        statusline::free_text(f, this.status_format().size)
                    });
                    this.space_label.set_text(&text);
                }
                ControlFlow::Break(())
            },
            || {},
        );
    }
}

/// Hook: decorate a freshly built panel view: clicking anywhere in a panel makes it the active
/// one (the click itself still reaches the list).
pub fn install_panel_view(ctx: &Rc<AppContext>, view: &PanelView) {
    let gesture = gtk::GestureClick::new();
    gesture.set_button(0);
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(ctx);
    let side = view.side;
    gesture.connect_pressed(move |_, _, _, _| {
        if let Some(ctx) = weak.upgrade() {
            ctx.set_active_side(side);
        }
    });
    view.root.add_controller(gesture);
}

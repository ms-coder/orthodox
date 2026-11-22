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
    /// Shown instead of `path_label` while a [`TextHold`] is active (quick view).
    held_path_label: gtk::Label,
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
    /// Set while a [`TextHold`] owns the path header and the status line.
    held: Rc<Cell<bool>>,
}

/// While held, the controller-driven texts (path header, status line, selection summary) of a
/// panel side are not rewritten; [`TextHold::release`] shows the controller's texts again.
#[derive(Clone)]
pub struct TextHold {
    handles: PanelViewHandles,
}

impl TextHold {
    /// Take over the path header and clear the selection summary; the status line shows `status`.
    pub fn hold(&self, path: &str, status: &str) {
        let h = &self.handles;
        h.held.set(true);
        // A second label takes the header's place: re-setting the text of the real path label
        // leaves a 2-pixel rendering difference behind (found by the pixel-exact restore test).
        h.held_path_label.set_text(path);
        h.held_path_label.set_visible(true);
        h.status_label.set_text(status);
        h.summary_label.set_text("");
    }

    /// Change the status line while held (a no-op otherwise).
    pub fn set_status(&self, status: &str) {
        if self.handles.held.get() {
            self.handles.status_label.set_text(status);
        }
    }

    /// Give the texts back: they are rebuilt from the side's active tab.
    pub fn release(&self) {
        let h = &self.handles;
        if h.held.replace(false) {
            h.held_path_label.set_visible(false);
            h.update_texts(&h.host.active());
        }
    }
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
        let held_path_label = gtk::Label::new(None);
        held_path_label.set_xalign(0.0);
        held_path_label.set_ellipsize(gtk::pango::EllipsizeMode::Start);
        held_path_label.add_css_class("ofm-path");
        held_path_label.set_visible(false);
        let list = FileList::new();
        list.set_palette(ctx.theme.palette());
        list.install_mouse(ctx, side);
        list.set_accessible_label(if side == crate::panel::LEFT {
            tr("Left panel")
        } else {
            tr("Right panel")
        });
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
        // The held label is drawn over the path label, which keeps its allocation (hiding or
        // re-measuring it changes how its glyphs are clipped by a pixel row).
        let path_overlay = gtk::Overlay::new();
        path_overlay.set_child(Some(&path_label));
        path_overlay.add_overlay(&held_path_label);
        path_overlay.set_measure_overlay(&held_path_label, false);
        root.append(&path_overlay);
        root.append(&overlay);
        root.append(&status_slot);

        let host = Rc::clone(ctx.host(side));
        let view = PanelView {
            host: Rc::clone(&host),
            side,
            root,
            tab_bar_slot,
            path_label,
            held_path_label,
            overlay,
            list,
            status_label,
            summary_label,
            space_label,
            status_slot,
            held: Rc::new(Cell::new(false)),
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

    /// Handle that lets another module (quick view) take over the path header and the status
    /// line of this side and give them back exactly as they were.
    pub fn text_hold(&self, ctx: &Rc<AppContext>) -> TextHold {
        TextHold {
            handles: self.handles(ctx),
        }
    }

    /// Cheap clone of the widget handles for use in closures.
    fn handles(&self, ctx: &Rc<AppContext>) -> PanelViewHandles {
        PanelViewHandles {
            held: Rc::clone(&self.held),
            root: self.root.clone(),
            path_label: self.path_label.clone(),
            held_path_label: self.held_path_label.clone(),
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
    held: Rc<Cell<bool>>,
    root: gtk::Box,
    path_label: gtk::Label,
    held_path_label: gtk::Label,
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
        if self.held.get() {
            return;
        }
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
                // A quick filter that outlives its bar (Enter) stays visible until cleared.
                let summary = match m.filter() {
                    Some(f) => statusline::with_filter(f, &summary),
                    None => summary,
                };
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

/// Hook: decorate a freshly built panel view. Mouse handling (click activates the panel,
/// double-click opens, right-click context menu) is installed by `FileList::install_mouse`.
pub fn install_panel_view(_ctx: &Rc<AppContext>, _view: &PanelView) {}

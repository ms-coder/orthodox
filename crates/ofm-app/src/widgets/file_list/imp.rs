//! Stub implementation of the `FileList` widget (replaced by T2.1).

use std::cell::{Cell, RefCell};
use std::os::unix::ffi::OsStrExt;
use std::rc::Rc;

use gtk::glib;
use gtk::graphene;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ofm_core::{Entry, EntryKind};

use super::FileList as Obj;
use crate::panel::{PanelController, PanelEvent, ViewGeometry};
use crate::signal::SignalId;

/// Row padding in px (stub).
const PAD: f32 = 2.0;

#[derive(Default)]
pub struct FileList {
    ctrl: RefCell<Option<Rc<PanelController>>>,
    conn: Cell<Option<SignalId>>,
    palette: RefCell<Option<Rc<crate::theme::Palette>>>,
    active: Cell<bool>,
    /// Index of the first visible row.
    top: Cell<u32>,
    row_height: Cell<f32>,
}

#[glib::object_subclass]
impl ObjectSubclass for FileList {
    const NAME: &'static str = "OfmFileList";
    type Type = Obj;
    type ParentType = gtk::Widget;
}

impl ObjectImpl for FileList {
    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.set_hexpand(true);
        obj.set_vexpand(true);
        obj.set_overflow(gtk::Overflow::Hidden);
        obj.add_css_class("ofm-file-list");

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        let weak = obj.downgrade();
        scroll.connect_scroll(move |_, _dx, dy| {
            if let Some(list) = weak.upgrade() {
                list.imp().scroll_by(dy.round() as i32 * 3);
                list.queue_draw();
            }
            glib::Propagation::Stop
        });
        obj.add_controller(scroll);
    }

    fn dispose(&self) {
        self.unbind();
    }
}

impl WidgetImpl for FileList {
    fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
        self.parent_size_allocate(width, height, baseline);
        if let Some(ctrl) = self.ctrl.borrow().as_ref() {
            let rows = self.visible_rows(height as f32).max(1);
            ctrl.set_geometry(ViewGeometry {
                rows_per_page: rows,
                columns: 1,
            });
        }
        self.ensure_cursor_visible();
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let obj = self.obj();
        let (w, h) = (obj.width() as f32, obj.height() as f32);
        let palette = self.palette.borrow().clone();
        let ctrl = self.ctrl.borrow().clone();
        let (Some(palette), Some(ctrl)) = (palette, ctrl) else {
            return;
        };
        let bounds = graphene::Rect::new(0.0, 0.0, w, h);
        snapshot.append_color(&palette.panel.bg, &bounds);

        let font = pango::FontDescription::from_string(&palette.font);
        let probe = obj.create_pango_layout(Some("Ag"));
        probe.set_font_description(Some(&font));
        let (_, text_h) = probe.pixel_size();
        let row_h = text_h as f32 + 2.0 * PAD;
        self.row_height.set(row_h);

        let model = ctrl.model();
        let len = model.visible_len();
        let cursor = model.cursor();
        let first = self.top.get().min(len.saturating_sub(1));
        let rows = self.visible_rows(h) + 1;
        for (n, idx) in (first..len.min(first + rows)).enumerate() {
            let Some(entry) = model.visible(idx) else {
                continue;
            };
            let y = n as f32 * row_h;
            let is_cursor = cursor == Some(idx);
            let selected = model.is_selected(idx);
            let mut fg = if entry.kind.is_dir_like() {
                palette.panel.dir_fg
            } else {
                palette.panel.fg
            };
            if selected {
                fg = palette.marked_fg;
            }
            if is_cursor {
                if self.active.get() {
                    snapshot.append_color(
                        &palette.cursor.active_bg,
                        &graphene::Rect::new(0.0, y, w, row_h),
                    );
                    fg = palette.cursor.active_fg;
                } else if let Some(bg) = palette.cursor.inactive_bg {
                    snapshot.append_color(&bg, &graphene::Rect::new(0.0, y, w, row_h));
                    fg = palette.cursor.inactive_fg.unwrap_or(fg);
                }
            }
            let name = display_name(entry);
            let size = size_text(entry);
            draw_text(snapshot, &obj, &font, &name, &fg, PAD * 3.0, y + PAD, None);
            draw_text(
                snapshot,
                &obj,
                &font,
                &size,
                &fg,
                0.0,
                y + PAD,
                Some(w - PAD * 3.0),
            );
        }
    }
}

impl FileList {
    pub(super) fn bind(&self, obj: &Obj, ctrl: Option<Rc<PanelController>>) {
        self.unbind();
        if let Some(c) = &ctrl {
            let weak = obj.downgrade();
            let id = c.changed.connect(move |ev| {
                if let Some(list) = weak.upgrade() {
                    if matches!(ev, PanelEvent::Location) {
                        list.imp().top.set(0);
                    }
                    list.imp().ensure_cursor_visible();
                    list.queue_draw();
                }
            });
            self.conn.set(Some(id));
        }
        self.top.set(0);
        *self.ctrl.borrow_mut() = ctrl;
        obj.queue_draw();
    }

    fn unbind(&self) {
        if let (Some(c), Some(id)) = (self.ctrl.borrow_mut().take(), self.conn.take()) {
            c.changed.disconnect(id);
        }
    }

    pub(super) fn set_palette(&self, palette: Rc<crate::theme::Palette>) {
        *self.palette.borrow_mut() = Some(palette);
    }

    pub(super) fn set_active(&self, active: bool) {
        self.active.set(active);
    }

    fn visible_rows(&self, height: f32) -> u32 {
        let rh = self.row_height.get();
        if rh <= 0.0 {
            return 20;
        }
        (height / rh).floor() as u32
    }

    fn scroll_by(&self, rows: i32) {
        let Some(ctrl) = self.ctrl.borrow().clone() else {
            return;
        };
        let max_top = ctrl.model().visible_len().saturating_sub(1);
        let top = i64::from(self.top.get()) + i64::from(rows);
        self.top.set(top.clamp(0, i64::from(max_top)) as u32);
    }

    fn ensure_cursor_visible(&self) {
        let Some(ctrl) = self.ctrl.borrow().clone() else {
            return;
        };
        let Some(cursor) = ctrl.model().cursor() else {
            return;
        };
        let rows = self.visible_rows(self.obj().height() as f32).max(1);
        let top = self.top.get();
        if cursor < top {
            self.top.set(cursor);
        } else if cursor >= top + rows {
            self.top.set(cursor + 1 - rows);
        }
    }
}

fn display_name(e: &Entry) -> String {
    let mut s = String::from_utf8_lossy(e.name.as_bytes()).into_owned();
    if matches!(e.kind, EntryKind::Dir) && s != ".." {
        s.insert(0, '/');
    }
    s
}

fn size_text(e: &Entry) -> String {
    match e.kind {
        EntryKind::Dir => "<DIR>".to_owned(),
        _ if e.name.as_bytes() == b".." => "<UP>".to_owned(),
        _ => e.size.to_string(),
    }
}

/// Draw `text` at (`x`, `y`); with `right_edge` the text ends at that x coordinate.
#[allow(clippy::too_many_arguments)]
fn draw_text(
    snapshot: &gtk::Snapshot,
    widget: &Obj,
    font: &pango::FontDescription,
    text: &str,
    color: &gtk::gdk::RGBA,
    x: f32,
    y: f32,
    right_edge: Option<f32>,
) {
    let layout = widget.create_pango_layout(Some(text));
    layout.set_font_description(Some(font));
    let (tw, _) = layout.pixel_size();
    let x = right_edge.map_or(x, |r| r - tw as f32);
    snapshot.save();
    snapshot.translate(&graphene::Point::new(x, y));
    snapshot.append_layout(&layout, color);
    snapshot.restore();
}

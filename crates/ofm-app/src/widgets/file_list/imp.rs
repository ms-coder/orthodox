//! `FileList` implementation: widget plumbing, scrolling, mouse. Drawing is in `paint.rs`,
//! geometry in `geo.rs`/`layout.rs`.

use std::cell::{Cell, RefCell};
use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Instant;

use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ofm_core::panel::{CursorMove, ModelChange};
use ofm_core::{Location, panel::SortKey};

use super::FileList as Obj;
use super::FrameStats;
use super::commands::toggle_sort;
use super::format::is_parent;
use super::geo::{Geo, Hit, Metrics};
use super::icons::{IconCache, icon_px};
use super::paint::Painter;
use super::settings;
use super::text::TextCache;
use crate::i18n::tr;
use crate::panel::{LoadState, PanelController, PanelEvent, ViewGeometry};
use crate::signal::SignalId;
use crate::theme::Palette;

/// Rows scrolled per wheel notch in Full/Custom mode.
const WHEEL_ROWS: f64 = 3.0;
/// A frame slower than this is logged at debug level.
const SLOW_FRAME_US: u64 = 16_000;

/// Hook that builds the drag payload for a press at (x, y); see [`Obj::set_drag_provider`].
pub type DragProvider = Rc<dyn Fn(&Obj, f64, f64) -> Option<gdk::ContentProvider>>;

#[derive(Default)]
pub struct FileList {
    ctrl: RefCell<Option<Rc<PanelController>>>,
    conn: Cell<Option<SignalId>>,
    palette: RefCell<Option<Rc<Palette>>>,
    active: Cell<bool>,
    /// Allocated size (set in `size_allocate`).
    size: Cell<(i32, i32)>,
    /// Index of the first visible entry.
    first: Cell<u32>,
    /// Anchor of Shift-click range selection.
    anchor: Cell<Option<u32>>,
    /// Cursor at the last sync (only a *changed* cursor scrolls the view).
    last_cursor: Cell<Option<u32>>,
    metrics: Cell<Option<Metrics>>,
    geo: RefCell<Option<Geo>>,
    cache: RefCell<TextCache>,
    /// Rebuilt together with `metrics` (theme, font, scale, system settings).
    icons: RefCell<IconCache>,
    settings_gen: Cell<u64>,
    scroll_acc: Cell<(f64, f64)>,
    stats: Cell<FrameStats>,
    drag_provider: RefCell<Option<DragProvider>>,
}

#[glib::object_subclass]
impl ObjectSubclass for FileList {
    const NAME: &'static str = "OfmFileList";
    type Type = Obj;
    type ParentType = gtk::Widget;

    fn class_init(klass: &mut Self::Class) {
        klass.set_accessible_role(gtk::AccessibleRole::List);
        klass.set_css_name("ofm-file-list");
    }
}

impl ObjectImpl for FileList {
    fn signals() -> &'static [glib::subclass::Signal] {
        static SIGNALS: OnceLock<Vec<glib::subclass::Signal>> = OnceLock::new();
        SIGNALS.get_or_init(|| {
            vec![
                // A mouse press landed in the list: its panel should become the active one.
                glib::subclass::Signal::builder("activate-panel").build(),
                // Double click on a row (visible index): open it.
                glib::subclass::Signal::builder("open-entry")
                    .param_types([u32::static_type()])
                    .build(),
                // Secondary click at (x, y) in widget coordinates.
                glib::subclass::Signal::builder("context-menu")
                    .param_types([f64::static_type(), f64::static_type()])
                    .build(),
            ]
        })
    }

    fn constructed(&self) {
        self.parent_constructed();
        let obj = self.obj();
        obj.set_hexpand(true);
        obj.set_vexpand(true);
        obj.set_overflow(gtk::Overflow::Hidden);
        obj.set_size_request(120, 60);
        obj.add_css_class("ofm-file-list");
        obj.update_property(&[gtk::accessible::Property::Label(tr("File list"))]);

        // Wheel / touchpad.
        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
        let weak = obj.downgrade();
        scroll.connect_scroll(move |_, dx, dy| {
            if let Some(list) = weak.upgrade() {
                list.imp().on_scroll(dx, dy);
            }
            glib::Propagation::Stop
        });
        obj.add_controller(scroll);

        // Clicks of every button.
        let click = gtk::GestureClick::builder().button(0).build();
        let weak = obj.downgrade();
        click.connect_pressed(move |g, n_press, x, y| {
            if let Some(list) = weak.upgrade() {
                let state = g.current_event_state();
                list.imp()
                    .on_press(g.current_button(), state, n_press, x, y);
            }
        });
        obj.add_controller(click);

        // Drag source (files out of the list).
        let drag = gtk::DragSource::new();
        drag.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE);
        let weak = obj.downgrade();
        drag.connect_prepare(move |_, x, y| weak.upgrade()?.imp().drag_content(x, y));
        obj.add_controller(drag);

        // A new scale factor changes glyph metrics and icon assets.
        let weak = obj.downgrade();
        obj.connect_scale_factor_notify(move |_| {
            if let Some(list) = weak.upgrade() {
                list.imp().invalidate_fonts();
            }
        });
    }

    fn dispose(&self) {
        self.unbind();
    }
}

impl WidgetImpl for FileList {
    fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
        self.parent_size_allocate(width, height, baseline);
        self.size.set((width, height));
        self.sync(false);
    }

    fn realize(&self) {
        self.parent_realize();
        // The Pango context is final now: measure again.
        self.invalidate_fonts();
    }

    fn system_setting_changed(&self, setting: &gtk::SystemSetting) {
        self.parent_system_setting_changed(setting);
        self.invalidate_fonts();
    }

    fn snapshot(&self, snapshot: &gtk::Snapshot) {
        let started = Instant::now();
        let obj = self.obj();
        let palette = self.palette.borrow().clone();
        let ctrl = self.ctrl.borrow().clone();
        let (Some(palette), Some(ctrl)) = (palette, ctrl) else {
            return;
        };
        let g = settings::generation();
        if g != self.settings_gen.replace(g) {
            self.cache.borrow_mut().clear();
            self.sync(false);
        }
        if self.geo.borrow().is_none() {
            self.sync(false);
        }
        let geo_ref = self.geo.borrow();
        let Some(geo) = geo_ref.as_ref() else {
            return;
        };
        let settings = settings::settings();
        let model = ctrl.model();
        let message = (model.visible_len() == 0).then(|| match ctrl.state() {
            LoadState::Loading => tr("Loading...").to_owned(),
            LoadState::Failed(msg) => msg,
            LoadState::Idle => tr("(empty)").to_owned(),
        });
        let mut cache = self.cache.borrow_mut();
        let icons = self.icons.borrow();
        let mut painter = Painter {
            snapshot,
            widget: &obj,
            palette: &palette,
            cache: &mut cache,
            icons: &icons,
            geo,
            metrics: self.metrics.get().unwrap_or_default(),
            settings: &settings,
            active: self.active.get(),
        };
        let drawn = painter.paint(&model, self.first.get(), message.as_deref());
        let cached = cache.len();
        drop(cache);

        let us = started.elapsed().as_micros() as u64;
        let mut st = self.stats.get();
        st.frames += 1;
        st.last_us = us;
        st.max_us = st.max_us.max(us);
        st.total_us += us;
        self.stats.set(st);
        tracing::trace!(target: "ofm::file_list", frame_us = us, rows = drawn, cached, "frame");
        if us > SLOW_FRAME_US {
            tracing::debug!(target: "ofm::file_list", frame_us = us, rows = drawn, "slow frame");
        }
    }
}

impl FileList {
    // ---- binding ------------------------------------------------------------------------------
    pub(super) fn bind(&self, obj: &Obj, ctrl: Option<Rc<PanelController>>) {
        self.unbind();
        if let Some(c) = &ctrl {
            let weak = obj.downgrade();
            let id = c.changed.connect(move |ev| {
                if let Some(list) = weak.upgrade() {
                    list.imp().on_event(ev);
                }
            });
            self.conn.set(Some(id));
        }
        self.first.set(0);
        self.anchor.set(None);
        self.last_cursor.set(None);
        *self.geo.borrow_mut() = None;
        *self.ctrl.borrow_mut() = ctrl;
        self.sync(true);
        self.update_a11y();
    }

    fn unbind(&self) {
        if let (Some(c), Some(id)) = (self.ctrl.borrow_mut().take(), self.conn.take()) {
            c.changed.disconnect(id);
        }
    }

    pub(super) fn set_palette(&self, palette: Rc<Palette>) {
        self.cache.borrow_mut().set_font(&palette.font);
        *self.palette.borrow_mut() = Some(palette);
        self.metrics.set(None);
        self.sync(false);
    }

    pub(super) fn set_active(&self, active: bool) {
        self.active.set(active);
    }

    pub(super) fn set_drag_provider(&self, p: Option<DragProvider>) {
        *self.drag_provider.borrow_mut() = p;
    }

    pub(super) fn stats(&self) -> FrameStats {
        self.stats.get()
    }

    /// `(first visible index, rows per column, columns)` of the current layout.
    pub(super) fn window(&self) -> (u32, u32, u32) {
        let (rows, cols) = self
            .geo
            .borrow()
            .as_ref()
            .map_or((0, 0), |g| (g.grid.rows, g.grid.cols));
        (self.first.get(), rows, cols)
    }

    /// Copy of the current geometry (tests, hit-testing helpers).
    #[cfg(test)]
    pub(super) fn geo(&self) -> Option<Geo> {
        self.geo.borrow().clone()
    }

    pub(super) fn icon_at(&self, index: u32) -> Option<(String, i32)> {
        let ctrl = self.ctrl.borrow().clone()?;
        let palette = self.palette.borrow().clone()?;
        let model = ctrl.model();
        let entry = model.visible(index)?;
        let rule = ofm_config::highlight::matching_rule(&palette.theme.highlight, entry);
        let icons = self.icons.borrow();
        let (name, _) = icons.get(rule, entry)?;
        Some((name.to_owned(), icons.size()))
    }

    fn invalidate_fonts(&self) {
        self.cache.borrow_mut().clear();
        self.metrics.set(None);
        self.sync(false);
    }

    fn on_event(&self, ev: &PanelEvent) {
        match ev {
            PanelEvent::Location => {
                self.first.set(0);
                self.anchor.set(None);
                self.last_cursor.set(None);
                self.sync(true);
                self.update_a11y();
            }
            PanelEvent::Model(change) => self.sync(*change == ModelChange::Reset),
            PanelEvent::Redraw => self.sync(false),
            PanelEvent::State => self.obj().queue_draw(),
        }
    }

    // ---- metrics and layout --------------------------------------------------------------------
    fn metrics(&self) -> Metrics {
        if let Some(m) = self.metrics.get() {
            return m;
        }
        let obj = self.obj();
        let cache = self.cache.borrow();
        let measure = |text: &str| {
            let layout = obj.create_pango_layout(Some(text));
            layout.set_font_description(Some(cache.regular_font()));
            layout
        };
        let digits = measure("0000000000");
        let char_w = (digits.size().0 as f32 / pango::SCALE as f32 / 10.0).max(1.0);
        let text_h = measure("Ag\u{25B2}").pixel_size().1 as f32;
        let row_h = (text_h + 2.0).ceil();
        let icons = self
            .palette
            .borrow()
            .as_ref()
            .map_or_else(IconCache::default, |p| {
                IconCache::build(
                    &p.theme.highlight,
                    &obj.display(),
                    icon_px(row_h),
                    obj.scale_factor(),
                )
            });
        let m = Metrics {
            row_h,
            char_w,
            text_h,
            icon_w: icons.advance(),
        };
        *self.icons.borrow_mut() = icons;
        self.metrics.set(Some(m));
        m
    }

    /// Recompute the geometry for the current size, mode and font; clamp (and, when `reveal` or
    /// the cursor moved or the layout changed shape, scroll to) the window; redraw.
    fn sync(&self, reveal: bool) {
        let Some(ctrl) = self.ctrl.borrow().clone() else {
            return;
        };
        let Some(palette) = self.palette.borrow().clone() else {
            return;
        };
        let (w, h) = self.size.get();
        if w <= 0 || h <= 0 {
            return;
        }
        let m = self.metrics();
        let mode = settings::view_mode(&ctrl);
        let cfg = settings::settings();
        let geo = Geo::compute(
            w as f32,
            h as f32,
            &m,
            mode,
            &cfg.columns(mode),
            palette.theme.retro.frames,
        );
        let shape_changed = self
            .geo
            .borrow()
            .as_ref()
            .is_none_or(|g| g.grid != geo.grid || g.mode != geo.mode);
        ctrl.set_geometry(ViewGeometry {
            rows_per_page: geo.grid.rows,
            columns: geo.grid.cols,
        });
        let (len, cursor) = {
            let model = ctrl.model();
            (model.visible_len(), model.cursor())
        };
        let mut first = geo.grid.clamp_first(self.first.get(), len);
        if let Some(c) = cursor
            && (reveal || shape_changed || cursor != self.last_cursor.get())
        {
            first = geo.grid.reveal(first, c, len);
        }
        let cursor_moved = cursor != self.last_cursor.replace(cursor);
        self.first.set(first);
        *self.geo.borrow_mut() = Some(geo);
        if cursor_moved {
            self.update_a11y();
        }
        self.obj().queue_draw();
    }

    fn update_a11y(&self) {
        let Some(ctrl) = self.ctrl.borrow().clone() else {
            return;
        };
        let model = ctrl.model();
        let desc = match (model.cursor(), model.cursor_entry()) {
            (Some(i), Some(e)) => format!(
                "{}, {} / {}",
                String::from_utf8_lossy(e.name.as_bytes()),
                i + 1,
                model.visible_len()
            ),
            _ => String::new(),
        };
        drop(model);
        self.obj()
            .update_property(&[gtk::accessible::Property::Description(&desc)]);
    }

    // ---- scrolling ----------------------------------------------------------------------------
    pub(super) fn on_scroll(&self, dx: f64, dy: f64) {
        let brief = self.geo.borrow().as_ref().is_some_and(|g| g.grid.cols > 1);
        let (ax, ay) = self.scroll_acc.get();
        // Brief scrolls whole columns, one per notch; Full scrolls three rows per notch.
        let (ax, ay) = if brief {
            (ax + dx + dy, 0.0)
        } else {
            (0.0, ay + dy * WHEEL_ROWS)
        };
        let (whole_x, whole_y) = (ax.trunc(), ay.trunc());
        self.scroll_acc.set((ax - whole_x, ay - whole_y));
        let n = (whole_x + whole_y) as i32;
        if n != 0 {
            self.scroll_units(n);
        }
    }

    fn scroll_units(&self, delta: i32) {
        let (Some(ctrl), Some(grid)) = (
            self.ctrl.borrow().clone(),
            self.geo.borrow().as_ref().map(|g| g.grid),
        ) else {
            return;
        };
        let len = ctrl.model().visible_len();
        self.first.set(grid.scroll(self.first.get(), delta, len));
        self.obj().queue_draw();
    }

    // ---- mouse --------------------------------------------------------------------------------
    fn hit(&self, x: f64, y: f64) -> Hit {
        let Some(ctrl) = self.ctrl.borrow().clone() else {
            return Hit::Blank;
        };
        let len = ctrl.model().visible_len();
        self.geo.borrow().as_ref().map_or(Hit::Blank, |g| {
            g.hit(x as f32, y as f32, self.first.get(), len)
        })
    }

    /// The row under (x, y) and its rectangle (drop targets highlight it).
    pub(super) fn row_rect(&self, x: f64, y: f64) -> Option<super::RowRect> {
        let Hit::Row(index) = self.hit(x, y) else {
            return None;
        };
        let geo = self.geo.borrow();
        let g = geo.as_ref()?;
        let col = ((x as f32 - g.content.x) / g.cell_w) as u32;
        let row = ((y as f32 - g.body.y) / g.row_h) as u32;
        let (ox, oy) = g.cell_origin(col, row);
        Some(super::RowRect {
            index,
            x: f64::from(ox),
            y: f64::from(oy),
            width: f64::from(g.cell_w),
            height: f64::from(g.row_h),
        })
    }

    /// The rectangle of the cursor's row (the context menu points at it when opened from the
    /// keyboard); `None` when the list is empty or not laid out yet.
    pub(super) fn cursor_row_rect(&self) -> Option<super::RowRect> {
        let ctrl = self.ctrl.borrow().clone()?;
        let idx = ctrl.model().cursor()?;
        let geo = self.geo.borrow();
        let g = geo.as_ref()?;
        let (col, row) = g.grid.cell_of(self.first.get(), idx)?;
        let (ox, oy) = g.cell_origin(col, row);
        Some(super::RowRect {
            index: idx,
            x: f64::from(ox),
            y: f64::from(oy),
            width: f64::from(g.cell_w),
            height: f64::from(g.row_h),
        })
    }

    pub(super) fn on_press(
        &self,
        button: u32,
        state: gdk::ModifierType,
        n_press: i32,
        x: f64,
        y: f64,
    ) {
        let obj = self.obj();
        let Some(ctrl) = self.ctrl.borrow().clone() else {
            return;
        };
        obj.emit_by_name::<()>("activate-panel", &[]);
        let hit = self.hit(x, y);
        match (button, hit) {
            (gdk::BUTTON_PRIMARY, Hit::HeaderColumn(c)) => {
                if let Some(key) = c.sort_key() {
                    toggle_sort(&ctrl, key);
                }
            }
            (gdk::BUTTON_PRIMARY, Hit::HeaderBrief) => toggle_sort(&ctrl, SortKey::Name),
            (gdk::BUTTON_PRIMARY, Hit::Row(idx)) => {
                self.select_click(&ctrl, idx, state);
                if n_press == 2 {
                    obj.emit_by_name::<()>("open-entry", &[&idx]);
                }
            }
            (gdk::BUTTON_SECONDARY, hit) => {
                if let Hit::Row(idx) = hit {
                    // A right click inside the marked set keeps it; elsewhere it moves the cursor.
                    if !ctrl.model().is_selected(idx) {
                        move_cursor_to(&ctrl, idx);
                    }
                }
                obj.emit_by_name::<()>("context-menu", &[&x, &y]);
            }
            _ => {}
        }
    }

    /// Click semantics: plain = cursor, Ctrl = toggle mark, Shift = mark the range from the anchor.
    fn select_click(&self, ctrl: &Rc<PanelController>, idx: u32, state: gdk::ModifierType) {
        let shift = state.contains(gdk::ModifierType::SHIFT_MASK);
        let control = state.contains(gdk::ModifierType::CONTROL_MASK);
        if shift {
            let from = self
                .anchor
                .get()
                .or_else(|| ctrl.model().cursor())
                .unwrap_or(idx);
            ctrl.mutate(|m| m.select_range(from, idx, true));
            self.anchor.set(Some(from));
        } else {
            if control {
                ctrl.mutate(|m| m.toggle_select(idx));
            }
            self.anchor.set(Some(idx));
        }
        move_cursor_to(ctrl, idx);
    }

    // ---- drag ---------------------------------------------------------------------------------
    fn drag_content(&self, x: f64, y: f64) -> Option<gdk::ContentProvider> {
        let hook = self.drag_provider.borrow().clone();
        if let Some(hook) = hook {
            return hook(&self.obj(), x, y);
        }
        let Hit::Row(idx) = self.hit(x, y) else {
            return None;
        };
        let ctrl = self.ctrl.borrow().clone()?;
        let Location::Local(base) = ctrl.location() else {
            return None;
        };
        let model = ctrl.model();
        let names: Vec<OsString> = if model.is_selected(idx) {
            model.selected_names()
        } else {
            match model.visible(idx) {
                Some(e) if !is_parent(e) => vec![e.name.clone()],
                _ => return None,
            }
        };
        let files: Vec<gio::File> = names
            .iter()
            .map(|n| gio::File::for_path(base.join(n)))
            .collect();
        let list = gdk::FileList::from_array(&files);
        Some(gdk::ContentProvider::for_value(&list.to_value()))
    }
}

/// Move the model cursor to `idx` through `cursor_move` (which also cancels a pending
/// "cursor on name" request of the controller).
fn move_cursor_to(ctrl: &PanelController, idx: u32) {
    let Some(cur) = ctrl.model().cursor() else {
        return;
    };
    let delta = i64::from(idx) - i64::from(cur);
    if let Ok(delta) = i32::try_from(delta)
        && delta != 0
    {
        ctrl.cursor_move(CursorMove::By(delta));
    }
}

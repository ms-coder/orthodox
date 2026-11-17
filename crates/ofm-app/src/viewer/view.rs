//! `ViewerView`: the custom-drawn text / hex surface of the viewer window (ADR-0002 style).
//!
//! The widget only *draws* what it is given: the session lays pages out on a worker thread
//! ([`super::layout`]) and hands them over with [`ViewerView::show_text`] / [`ViewerView::show_hex`].
//! Shaped `pango::Layout`s are cached by content, so scrolling by a row re-shapes one line; the
//! snapshot touches only the rows of the current page. Colours come from the theme palette
//! (`[colors.viewer]`, falling back to the panel colours). Keys are not handled here.

use std::collections::HashMap;
use std::ops::Range;
use std::rc::Rc;

use gtk::gdk::RGBA;
use gtk::glib;
use gtk::graphene;
use gtk::pango;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::layout::{HexPage, TextPage};
use crate::theme::Palette;

/// Called with `(rows, columns)` whenever the visible grid changes.
pub type ViewportFn = Rc<dyn Fn(usize, usize)>;

glib::wrapper! {
    /// Custom-drawn text / hex surface.
    pub struct ViewerView(ObjectSubclass<imp::ViewerView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl Default for ViewerView {
    fn default() -> Self {
        Self::new()
    }
}

impl ViewerView {
    /// New empty view.
    pub fn new() -> Self {
        glib::Object::new()
    }

    /// Use `palette` for colours and font, and redraw.
    pub fn set_palette(&self, palette: Rc<Palette>) {
        self.imp().set_palette(&self.clone(), palette);
    }

    /// Draw a text page.
    pub fn show_text(&self, page: &TextPage) {
        self.imp().show_text(self, page);
    }

    /// Draw a hex page of a file of `file_len` bytes; `hl` is the search match to mark.
    pub fn show_hex(&self, page: &HexPage, hl: Option<&Range<u64>>, file_len: u64) {
        self.imp().show_hex(self, page, hl, file_len);
    }

    /// Show a centred message instead of the content (`None` clears the view).
    pub fn show_message(&self, msg: Option<&str>) {
        self.imp().show_message(self, msg);
    }

    /// Register the viewport callback (fires on the main loop after a size or font change).
    pub fn set_viewport_callback(&self, f: ViewportFn) {
        self.imp().viewport_cb.replace(Some(f));
    }

    /// Current `(rows, columns)` of the visible grid (0 before the first allocation).
    pub fn viewport(&self) -> (usize, usize) {
        self.imp().viewport()
    }
}

mod imp {
    use std::cell::{Cell, RefCell};

    use super::*;

    /// Layout cache key: text plus attribute ranges `(start, end, kind)`.
    type Key = (String, Vec<(usize, usize, u8)>);
    const SPAN_MATCH: u8 = 0;
    const SPAN_OFFSET: u8 = 1;
    /// Cached layouts before the cache is dropped as a whole.
    const MAX_CACHE: usize = 600;
    const PAD_X: f64 = 6.0;
    const PAD_Y: f64 = 3.0;

    enum Content {
        Empty,
        Message(pango::Layout),
        Rows(Vec<pango::Layout>),
    }

    pub struct ViewerView {
        palette: RefCell<Option<Rc<Palette>>>,
        content: RefCell<Content>,
        cache: RefCell<HashMap<Key, pango::Layout>>,
        font: RefCell<pango::FontDescription>,
        /// `(char width, row height)` in px.
        metrics: Cell<Option<(f64, f64)>>,
        size: Cell<(i32, i32)>,
        last_vp: Cell<(usize, usize)>,
        pub viewport_cb: RefCell<Option<ViewportFn>>,
    }

    impl Default for ViewerView {
        fn default() -> Self {
            ViewerView {
                palette: RefCell::new(None),
                content: RefCell::new(Content::Empty),
                cache: RefCell::new(HashMap::new()),
                font: RefCell::new(pango::FontDescription::from_string("monospace 11")),
                metrics: Cell::new(None),
                size: Cell::new((0, 0)),
                last_vp: Cell::new((0, 0)),
                viewport_cb: RefCell::new(None),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for ViewerView {
        const NAME: &'static str = "OfmViewerView";
        type Type = super::ViewerView;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Document);
            klass.set_css_name("ofm-viewer-view");
        }
    }

    impl ObjectImpl for ViewerView {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_focusable(false);
            let weak = obj.downgrade();
            obj.connect_scale_factor_notify(move |_| {
                if let Some(v) = weak.upgrade() {
                    v.imp().invalidate();
                }
            });
        }
    }

    impl WidgetImpl for ViewerView {
        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            match orientation {
                gtk::Orientation::Horizontal => (160, 640, -1, -1),
                _ => (60, 400, -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.size.set((width, height));
            self.check_viewport();
        }

        fn realize(&self) {
            self.parent_realize();
            self.invalidate();
        }

        fn system_setting_changed(&self, setting: &gtk::SystemSetting) {
            self.parent_system_setting_changed(setting);
            self.invalidate();
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let (w, h) = self.size.get();
            let Some(palette) = self.palette.borrow().clone() else {
                return;
            };
            let (bg, fg, ..) = colors(&palette);
            snapshot.append_color(&bg, &graphene::Rect::new(0.0, 0.0, w as f32, h as f32));
            let (_, line_h) = self.metrics();
            match &*self.content.borrow() {
                Content::Empty => {}
                Content::Message(l) => {
                    let (tw, th) = l.pixel_size();
                    snapshot.save();
                    snapshot.translate(&graphene::Point::new(
                        ((w - tw) / 2).max(0) as f32,
                        ((h - th) / 2).max(0) as f32,
                    ));
                    snapshot.append_layout(l, &fg);
                    snapshot.restore();
                }
                Content::Rows(rows) => {
                    for (i, l) in rows.iter().enumerate() {
                        let y = PAD_Y + i as f64 * line_h;
                        if y > f64::from(h) {
                            break;
                        }
                        snapshot.save();
                        snapshot.translate(&graphene::Point::new(PAD_X as f32, y as f32));
                        snapshot.append_layout(l, &fg);
                        snapshot.restore();
                    }
                }
            }
        }
    }

    /// `(background, text, match bg, match fg, hex offset)` of the viewer.
    fn colors(p: &Palette) -> (RGBA, RGBA, RGBA, RGBA, RGBA) {
        let v = &p.theme.colors.viewer;
        (
            p.resolve(&v.bg).unwrap_or(p.panel.bg),
            p.resolve(&v.fg).unwrap_or(p.panel.fg),
            p.resolve(&v.match_bg).unwrap_or(p.cursor.active_bg),
            p.resolve(&v.match_fg).unwrap_or(p.cursor.active_fg),
            p.resolve(&v.hex_offset_fg).unwrap_or(p.system.dim),
        )
    }

    /// Font for the viewer: the theme's family when it is monospace, else `monospace`.
    pub fn font_for(p: &Palette) -> String {
        let (family, size) = match p.font.rsplit_once(' ') {
            Some((f, s)) if s.parse::<f32>().is_ok() => (f.to_owned(), s.parse::<f32>().ok()),
            _ => (p.font.clone(), None),
        };
        let family = if family.to_lowercase().contains("mono") {
            family
        } else {
            "monospace".to_owned()
        };
        format!("{family} {}", size.filter(|s| *s > 0.0).unwrap_or(11.0))
    }

    fn pango_color(c: RGBA) -> (u16, u16, u16) {
        let q = |v: f32| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
        (q(c.red()), q(c.green()), q(c.blue()))
    }

    impl ViewerView {
        pub fn set_palette(&self, obj: &super::ViewerView, palette: Rc<Palette>) {
            let font = pango::FontDescription::from_string(&font_for(&palette));
            *self.font.borrow_mut() = font;
            *self.palette.borrow_mut() = Some(palette);
            self.invalidate();
            obj.queue_draw();
        }

        /// Fonts or scale changed: measure again and drop shaped layouts.
        fn invalidate(&self) {
            self.metrics.set(None);
            self.cache.borrow_mut().clear();
            self.check_viewport();
            self.obj().queue_draw();
        }

        fn metrics(&self) -> (f64, f64) {
            if let Some(m) = self.metrics.get() {
                return m;
            }
            let obj = self.obj();
            let measure = |text: &str| {
                let l = obj.create_pango_layout(Some(text));
                l.set_font_description(Some(&self.font.borrow()));
                l
            };
            let digits = measure("0000000000");
            let char_w = (f64::from(digits.size().0) / f64::from(pango::SCALE) / 10.0).max(1.0);
            let line_h = f64::from(measure("Ag\u{c9}").pixel_size().1).max(1.0);
            self.metrics.set(Some((char_w, line_h)));
            (char_w, line_h)
        }

        pub fn viewport(&self) -> (usize, usize) {
            let (w, h) = self.size.get();
            let (cw, lh) = self.metrics();
            let cols = ((f64::from(w) - 2.0 * PAD_X) / cw).floor().max(0.0) as usize;
            let rows = ((f64::from(h) - PAD_Y) / lh).floor().max(0.0) as usize;
            (rows, cols)
        }

        /// Tell the owner when the grid changed (deferred: never inside size allocation).
        fn check_viewport(&self) {
            let vp = self.viewport();
            if vp.0 == 0 || vp.1 == 0 || vp == self.last_vp.get() {
                return;
            }
            self.last_vp.set(vp);
            let weak = self.obj().downgrade();
            glib::idle_add_local_once(move || {
                let Some(v) = weak.upgrade() else { return };
                let cb = v.imp().viewport_cb.borrow().clone();
                if let Some(cb) = cb {
                    let vp = v.imp().viewport();
                    cb(vp.0, vp.1);
                }
            });
        }

        fn layout_for(&self, text: &str, spans: &[(usize, usize, u8)]) -> pango::Layout {
            let key: Key = (text.to_owned(), spans.to_vec());
            if let Some(l) = self.cache.borrow().get(&key) {
                return l.clone();
            }
            if self.cache.borrow().len() >= MAX_CACHE {
                self.cache.borrow_mut().clear();
            }
            let l = self.obj().create_pango_layout(Some(text));
            l.set_font_description(Some(&self.font.borrow()));
            l.set_single_paragraph_mode(true);
            if !spans.is_empty()
                && let Some(p) = self.palette.borrow().as_ref()
            {
                let (_, _, mbg, mfg, off) = colors(p);
                let list = pango::AttrList::new();
                for &(a, b, kind) in spans {
                    let (a, b) = (a as u32, b as u32);
                    let mut attrs: Vec<pango::AttrColor> = Vec::new();
                    if kind == SPAN_MATCH {
                        let (r, g, bl) = pango_color(mbg);
                        attrs.push(pango::AttrColor::new_background(r, g, bl));
                        let (r, g, bl) = pango_color(mfg);
                        attrs.push(pango::AttrColor::new_foreground(r, g, bl));
                    } else if kind == SPAN_OFFSET {
                        let (r, g, bl) = pango_color(off);
                        attrs.push(pango::AttrColor::new_foreground(r, g, bl));
                    }
                    for mut attr in attrs {
                        attr.set_start_index(a);
                        attr.set_end_index(b);
                        list.insert(attr);
                    }
                }
                l.set_attributes(Some(&list));
            }
            self.cache.borrow_mut().insert(key, l.clone());
            l
        }

        pub fn show_text(&self, obj: &super::ViewerView, page: &TextPage) {
            let rows = page
                .rows
                .iter()
                .map(|r| {
                    let spans: Vec<_> = r.hl.iter().map(|h| (h.start, h.end, SPAN_MATCH)).collect();
                    self.layout_for(&r.text, &spans)
                })
                .collect();
            *self.content.borrow_mut() = Content::Rows(rows);
            obj.queue_draw();
        }

        pub fn show_hex(
            &self,
            obj: &super::ViewerView,
            page: &HexPage,
            hl: Option<&Range<u64>>,
            file_len: u64,
        ) {
            let digits = hex_digits(file_len.saturating_sub(1));
            let rows = page
                .rows
                .iter()
                .map(|r| {
                    let (text, spans) = hex_line(r.offset, &r.bytes, &r.text, digits, hl);
                    self.layout_for(&text, &spans)
                })
                .collect();
            *self.content.borrow_mut() = Content::Rows(rows);
            obj.queue_draw();
        }

        pub fn show_message(&self, obj: &super::ViewerView, msg: Option<&str>) {
            *self.content.borrow_mut() = match msg {
                Some(m) => {
                    let l = self.layout_for(m, &[]);
                    Content::Message(l)
                }
                None => Content::Empty,
            };
            obj.queue_draw();
        }
    }

    /// Hex digits of the offset column for offsets up to `max_offset` (at least 8).
    pub fn hex_digits(max_offset: u64) -> usize {
        let needed = (64 - max_offset.max(1).leading_zeros() as usize).div_ceil(4);
        needed.max(8)
    }

    /// `00000000  48 65 6C 6C 6F 20 57 6F  72 6C 64 0A 00 00 00 00  |Hello World.....|` with the
    /// attribute spans for the offset column and the part of the row covered by `hl`.
    pub fn hex_line(
        offset: u64,
        bytes: &[u8],
        text: &str,
        digits: usize,
        hl: Option<&Range<u64>>,
    ) -> (String, Vec<(usize, usize, u8)>) {
        let mut s = format!("{offset:0digits$X} ");
        let mut spans = vec![(0, digits, SPAN_OFFSET)];
        let mut cell = vec![(0usize, 0usize); 16];
        for (i, slot) in cell.iter_mut().enumerate() {
            if i == 8 {
                s.push(' ');
            }
            s.push(' ');
            let start = s.len();
            match bytes.get(i) {
                Some(b) => s.push_str(&format!("{b:02X}")),
                None => s.push_str("  "),
            }
            *slot = (start, s.len());
        }
        s.push_str("  |");
        let text_start = s.len();
        s.push_str(text);
        s.push('|');
        if let Some(h) = hl {
            let hit: Vec<usize> = (0..bytes.len())
                .filter(|&i| {
                    let o = offset + i as u64;
                    o >= h.start && o < h.end
                })
                .collect();
            if let (Some(&a), Some(&b)) = (hit.first(), hit.last()) {
                spans.push((cell[a].0, cell[b].1, SPAN_MATCH));
                spans.push((text_start + a, text_start + b + 1, SPAN_MATCH));
            }
        }
        (s, spans)
    }

    #[cfg(test)]
    #[allow(clippy::unwrap_used, clippy::expect_used)]
    mod tests {
        use super::*;

        #[test]
        fn hex_line_layout_and_match_spans() {
            let (s, spans) = hex_line(
                0x10,
                b"Hello World\n\0\0\0\0",
                "Hello World.....",
                8,
                Some(&(0x12..0x14)),
            );
            assert_eq!(
                s,
                "00000010  48 65 6C 6C 6F 20 57 6F  72 6C 64 0A 00 00 00 00  |Hello World.....|"
            );
            assert_eq!(spans[0], (0, 8, SPAN_OFFSET));
            let m = &spans[1];
            assert_eq!(&s[m.0..m.1], "6C 6C");
            let t = &spans[2];
            assert_eq!(&s[t.0..t.1], "ll");
        }

        #[test]
        fn short_last_row_is_padded() {
            let (s, spans) = hex_line(0, b"ab", "ab", 8, None);
            assert!(s.ends_with("|ab|"));
            assert_eq!(spans.len(), 1);
            let (full, _) = hex_line(0, &[0u8; 16], "................", 8, None);
            assert_eq!(
                s.find('|'),
                full.find('|'),
                "a short row keeps the columns aligned"
            );
        }

        #[test]
        fn offset_width_grows_for_big_files() {
            assert_eq!(hex_digits(0), 8);
            assert_eq!(hex_digits(0xFFFF_FFFF), 8);
            assert_eq!(hex_digits(0x1_0000_0000), 9);
            assert_eq!(hex_digits(10 * 1024 * 1024 * 1024), 9);
        }

        #[test]
        fn font_follows_theme_only_when_monospace() {
            use crate::theme::SystemColors;
            let theme = ofm_config::Theme::builtin("far-blue").unwrap();
            let p = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
            assert_eq!(font_for(&p), "monospace 11");
            let theme = ofm_config::Theme::builtin("adwaita").unwrap();
            let p = Palette::from_theme(Rc::new(theme), SystemColors::fallback());
            assert!(font_for(&p).starts_with("monospace "));
        }
    }
}

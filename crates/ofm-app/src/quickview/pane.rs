//! The quick-view pane: a custom widget that draws one [`Content`] (text lines, an information
//! table, an image or a message) with the theme palette. It is laid over the other panel's file
//! list as an overlay child, so the list (its cursor, selection, scroll position) is untouched
//! while the pane is visible.
//!
//! Drawing follows ADR-0002: everything in `snapshot()`, `pango::Layout`s cached until the
//! content, size, palette or scale changes, and only the visible rows are laid out.

use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;

use super::worker::Row;
use crate::theme::Palette;

/// Outer padding of the body, in px.
const PAD: f32 = 8.0;

/// What the pane shows.
#[derive(Debug, Clone, Default)]
pub enum Body {
    /// Nothing (before the first preview).
    #[default]
    Empty,
    /// A centered dim message.
    Message(String),
    /// Monospace text lines.
    Text(Vec<String>),
    /// A label/value table with an optional dim note below.
    Info {
        /// Table rows.
        rows: Vec<Row>,
        /// Note under the table.
        note: Option<String>,
    },
    /// An image scaled to fit, with a caption line.
    Image {
        /// The pixels.
        texture: gdk::Texture,
        /// Caption under the image.
        caption: String,
    },
}

/// Title bar text plus the body.
#[derive(Debug, Clone, Default)]
pub struct Content {
    /// Left side of the title bar (the file name).
    pub title: String,
    /// Right side of the title bar, dim (encoding, "truncated", ...).
    pub subtitle: String,
    /// The body.
    pub body: Body,
}

glib::wrapper! {
    /// See the module docs.
    pub struct PreviewPane(ObjectSubclass<imp::PreviewPane>) @extends gtk::Widget,
        @implements gtk::Accessible, gtk::Buildable, gtk::ConstraintTarget;
}

impl PreviewPane {
    /// New, empty pane (hidden by the caller until quick view is switched on).
    pub fn new() -> Self {
        let pane: Self = glib::Object::new();
        pane.set_visible(false);
        pane
    }

    /// Use `palette` for colours and font; redraws.
    pub fn set_palette(&self, palette: Rc<Palette>) {
        self.imp().set_palette(palette);
        self.queue_draw();
    }

    /// Show `content`; redraws.
    pub fn set_content(&self, content: Content) {
        self.imp().set_content(content);
        self.queue_draw();
    }

    /// The content currently shown (tests and the controller's no-op checks).
    pub fn content(&self) -> Content {
        self.imp().content()
    }

    /// Size the previews should aim for: `(width px, height px, text rows)`, in device pixels
    /// for the image sizes. A sane default before the first allocation.
    pub fn view_box(&self) -> (i32, i32, usize) {
        self.imp().view_box(self)
    }
}

impl Default for PreviewPane {
    fn default() -> Self {
        Self::new()
    }
}

mod imp {
    use std::cell::{Cell, RefCell};

    use gtk::graphene;
    use gtk::pango;

    use super::*;
    use crate::theme::mix;

    /// One drawn text run.
    struct Item {
        layout: pango::Layout,
        x: f32,
        y: f32,
        color: gdk::RGBA,
    }

    #[derive(Default)]
    struct Cache {
        key: Option<(u64, i32, i32, u64, i32)>,
        items: Vec<Item>,
        header: Vec<Item>,
        line_h: f32,
        image: Option<graphene::Rect>,
    }

    #[derive(Default)]
    pub struct PreviewPane {
        palette: RefCell<Option<Rc<Palette>>>,
        content: RefCell<Content>,
        content_gen: Cell<u64>,
        palette_gen: Cell<u64>,
        cache: RefCell<Cache>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for PreviewPane {
        const NAME: &'static str = "OfmQuickViewPane";
        type Type = super::PreviewPane;
        type ParentType = gtk::Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.set_accessible_role(gtk::AccessibleRole::Group);
            klass.set_css_name("ofm-quickview");
        }
    }

    impl ObjectImpl for PreviewPane {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_hexpand(true);
            obj.set_vexpand(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.update_property(&[gtk::accessible::Property::Label(crate::i18n::tr(
                "Quick view",
            ))]);
        }
    }

    impl WidgetImpl for PreviewPane {
        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            let obj = self.obj();
            let Some(palette) = self.palette.borrow().clone() else {
                return;
            };
            let (w, h) = (obj.width(), obj.height());
            if w <= 0 || h <= 0 {
                return;
            }
            let full = graphene::Rect::new(0.0, 0.0, w as f32, h as f32);
            snapshot.append_color(&palette.panel.bg, &full);

            self.rebuild_if_needed(&obj, &palette, w, h);
            let cache = self.cache.borrow();
            // Title bar.
            let bar_h = cache.line_h + 4.0;
            snapshot.append_color(
                &palette.panel.header_bg,
                &graphene::Rect::new(0.0, 0.0, w as f32, bar_h),
            );
            for it in &cache.header {
                draw(snapshot, it);
            }
            // Image.
            if let (Some(rect), Body::Image { texture, .. }) =
                (cache.image.as_ref(), &self.content.borrow().body)
            {
                snapshot.append_scaled_texture(texture, gtk::gsk::ScalingFilter::Linear, rect);
            }
            for it in &cache.items {
                draw(snapshot, it);
            }
        }
    }

    fn draw(snapshot: &gtk::Snapshot, it: &Item) {
        snapshot.save();
        snapshot.translate(&graphene::Point::new(it.x, it.y));
        snapshot.append_layout(&it.layout, &it.color);
        snapshot.restore();
    }

    impl PreviewPane {
        pub fn set_palette(&self, palette: Rc<Palette>) {
            *self.palette.borrow_mut() = Some(palette);
            self.palette_gen.set(self.palette_gen.get() + 1);
        }

        pub fn set_content(&self, content: Content) {
            *self.content.borrow_mut() = content;
            self.content_gen.set(self.content_gen.get() + 1);
        }

        pub fn content(&self) -> Content {
            self.content.borrow().clone()
        }

        pub fn view_box(&self, obj: &super::PreviewPane) -> (i32, i32, usize) {
            let scale = obj.scale_factor().max(1);
            let (w, h) = (obj.width(), obj.height());
            if w <= 0 || h <= 0 {
                return (800 * scale, 600 * scale, 40);
            }
            let line = self.cache.borrow().line_h.max(12.0);
            let rows = ((h as f32 - line - 4.0 - 2.0 * PAD) / line)
                .floor()
                .max(1.0) as usize;
            (
                (w - 2 * PAD as i32) * scale,
                (h - 2 * PAD as i32) * scale,
                rows,
            )
        }

        fn layout(
            &self,
            obj: &super::PreviewPane,
            font: &pango::FontDescription,
            text: &str,
            width: Option<i32>,
        ) -> pango::Layout {
            let l = obj.create_pango_layout(Some(text));
            l.set_font_description(Some(font));
            l.set_single_paragraph_mode(true);
            if let Some(w) = width {
                l.set_width(w * pango::SCALE);
                l.set_ellipsize(pango::EllipsizeMode::End);
            }
            l
        }

        /// Recompute the layouts when content, size, palette or scale changed.
        fn rebuild_if_needed(&self, obj: &super::PreviewPane, palette: &Palette, w: i32, h: i32) {
            let key = (
                self.content_gen.get(),
                w,
                h,
                self.palette_gen.get(),
                obj.scale_factor(),
            );
            if self.cache.borrow().key == Some(key) {
                return;
            }
            let mut ui_font = pango::FontDescription::from_string(&palette.font);
            if ui_font.size() == 0 {
                ui_font.set_size(11 * pango::SCALE);
            }
            let mut mono = ui_font.clone();
            mono.set_family("monospace");
            let fg = palette.panel.fg;
            let bg = palette.panel.bg;
            let dim = mix(fg, bg, 0.55);

            let content = self.content.borrow();
            let probe = self.layout(obj, &ui_font, "Mg", None);
            let line_h = probe.pixel_size().1.max(8) as f32;
            let bar_h = line_h + 4.0;
            let inner_w = (w as f32 - 2.0 * PAD).max(16.0) as i32;

            // Title bar: name left (ellipsized), dim subtitle right.
            let mut header = Vec::new();
            let sub = (!content.subtitle.is_empty())
                .then(|| self.layout(obj, &ui_font, &content.subtitle, None));
            let sub_w = sub.as_ref().map_or(0, |l| l.pixel_size().0);
            let title_w = (inner_w - sub_w - if sub_w > 0 { 12 } else { 0 }).max(16);
            header.push(Item {
                layout: self.layout(obj, &ui_font, &content.title, Some(title_w)),
                x: PAD,
                y: 2.0,
                color: palette.panel.header_fg,
            });
            if let Some(l) = sub {
                header.push(Item {
                    layout: l,
                    x: w as f32 - PAD - sub_w as f32,
                    y: 2.0,
                    color: mix(palette.panel.header_fg, palette.panel.header_bg, 0.7),
                });
            }

            let top = bar_h + PAD;
            let mut items = Vec::new();
            let mut image = None;
            match &content.body {
                Body::Empty => {}
                Body::Message(text) => {
                    let l = self.layout(obj, &ui_font, text, Some(inner_w));
                    l.set_alignment(pango::Alignment::Center);
                    items.push(Item {
                        layout: l,
                        x: PAD,
                        y: top + ((h as f32 - top) / 2.0 - line_h).max(0.0),
                        color: dim,
                    });
                }
                Body::Text(lines) => {
                    let mono_line_h =
                        self.layout(obj, &mono, "Mg", None).pixel_size().1.max(8) as f32;
                    let rows = (((h as f32 - top - PAD) / mono_line_h).floor().max(0.0)) as usize;
                    for (i, line) in lines.iter().take(rows).enumerate() {
                        items.push(Item {
                            layout: self.layout(obj, &mono, line, Some(inner_w)),
                            x: PAD,
                            y: top + i as f32 * mono_line_h,
                            color: fg,
                        });
                    }
                }
                Body::Info { rows, note } => {
                    let label_w = rows
                        .iter()
                        .map(|r| self.layout(obj, &ui_font, &r.label, None).pixel_size().0)
                        .max()
                        .unwrap_or(0) as f32
                        + 16.0;
                    let value_w = (inner_w as f32 - label_w).max(16.0) as i32;
                    let mut y = top;
                    for r in rows {
                        items.push(Item {
                            layout: self.layout(obj, &ui_font, &r.label, None),
                            x: PAD,
                            y,
                            color: dim,
                        });
                        items.push(Item {
                            layout: self.layout(obj, &ui_font, &r.value, Some(value_w)),
                            x: PAD + label_w,
                            y,
                            color: fg,
                        });
                        y += line_h + 2.0;
                    }
                    if let Some(note) = note {
                        items.push(Item {
                            layout: self.layout(obj, &ui_font, note, Some(inner_w)),
                            x: PAD,
                            y: y + line_h / 2.0,
                            color: dim,
                        });
                    }
                }
                Body::Image { texture, caption } => {
                    let cap = self.layout(obj, &ui_font, caption, Some(inner_w));
                    let cap_w = cap.pixel_size().0 as f32;
                    let area_h = (h as f32 - top - PAD - line_h - 4.0).max(16.0);
                    let area_w = inner_w as f32;
                    let (tw, th) = (texture.width() as f32, texture.height() as f32);
                    // The texture is decoded at device-pixel size: draw it at texture/scale_factor.
                    let sf = obj.scale_factor() as f32;
                    let fit = (area_w / tw).min(area_h / th).min(1.0 / sf);
                    let (dw, dh) = (tw * fit, th * fit);
                    image = Some(graphene::Rect::new(
                        PAD + (area_w - dw) / 2.0,
                        top + (area_h - dh) / 2.0,
                        dw,
                        dh,
                    ));
                    items.push(Item {
                        layout: cap,
                        x: PAD + ((area_w - cap_w) / 2.0).max(0.0),
                        y: top + area_h + 4.0,
                        color: dim,
                    });
                }
            }
            *self.cache.borrow_mut() = Cache {
                key: Some(key),
                items,
                header,
                line_h,
                image,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_content_is_empty() {
        let c = Content::default();
        assert!(matches!(c.body, Body::Empty));
        assert!(c.title.is_empty());
    }
}

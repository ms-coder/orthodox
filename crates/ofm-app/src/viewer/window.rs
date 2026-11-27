//! The viewer window: header (path), the text/hex surface with a scroll bar, a status line and
//! a key-hint strip. Keys go through the central dispatcher (`WindowKind::Viewer`).

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::glib;
use ofm_core::{KeyContext, cmd};
use ofm_viewer::Encoding;

use super::ViewerHost;
use super::session::{Initial, Session, Ui};
use super::view::ViewerView;
use crate::context::AppContext;
use crate::dispatch::{self, WindowKind};
use crate::i18n::tr;

/// Rows scrolled per mouse-wheel notch.
const WHEEL_ROWS: f64 = 3.0;

/// Key hint strip text from the active keymap, e.g. `F2 Wrap  F4 Hex  F7 Search`.
fn hints(ctx: &AppContext) -> String {
    let keymap = ctx.keymap.borrow();
    [
        (cmd::VIEWER_CLOSE, tr("Close")),
        (cmd::VIEWER_WRAP, tr("Wrap")),
        (cmd::VIEWER_TOGGLE_HEX, tr("Hex")),
        (cmd::VIEWER_SEARCH, tr("Search")),
        (cmd::VIEWER_ENCODING, tr("Encoding")),
        (cmd::VIEWER_GOTO, tr("Go to")),
    ]
    .iter()
    .filter_map(|(id, label)| {
        let (_, chord) = keymap
            .chords_for(*id)
            .into_iter()
            .filter(|(c, _)| *c == KeyContext::Viewer)
            .min_by_key(|(_, chord)| chord.to_string() != "Esc")?;
        Some(format!("{chord} {label}"))
    })
    .collect::<Vec<_>>()
    .join("   ")
}

fn status_label(css: &str) -> gtk::Label {
    gtk::Label::builder()
        .css_classes([css])
        .xalign(0.0)
        .single_line_mode(true)
        .build()
}

/// Scroll to a byte offset once the asynchronously opened document has a viewport.
/// Gives up after about three seconds or when the viewer closes.
pub fn jump_to_offset_when_ready(session: &Rc<Session>, offset: u64) {
    if offset == 0 {
        return;
    }
    let session = Rc::clone(session);
    let mut tries = 0u32;
    glib::timeout_add_local(Duration::from_millis(75), move || {
        tries += 1;
        if !session.is_empty() {
            session.jump_to(offset);
            if session.top() > 0 || tries > 40 {
                return glib::ControlFlow::Break;
            }
        }
        if tries > 40 || !session.window.is_visible() {
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
}

/// Open a viewer window for `siblings[idx]` (the files `viewer.next_file` steps through).
/// Returns `None` without a main window (headless tests).
pub fn open(ctx: &Rc<AppContext>, siblings: Vec<PathBuf>, idx: usize) -> Option<Rc<Session>> {
    let main = ctx.window()?;
    let host = ctx.ext.get::<ViewerHost>()?;

    let view = ViewerView::new();
    view.set_palette(ctx.theme.palette());
    view.update_property(&[gtk::accessible::Property::Label(tr("File contents"))]);

    let title = status_label("ofm-path");
    title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
    title.set_selectable(false);
    title.update_property(&[gtk::accessible::Property::Label(tr("File path"))]);
    let left = status_label("ofm-status");
    left.update_property(&[gtk::accessible::Property::Label(tr("Encoding and mode"))]);
    let right = status_label("ofm-status");
    right.set_xalign(1.0);
    right.set_hexpand(true);
    right.update_property(&[gtk::accessible::Property::Label(tr("Position"))]);
    let notice = status_label("ofm-status");
    notice.set_visible(false);
    let hint = status_label("ofm-status-dim");
    hint.set_text(&hints(ctx));
    hint.set_hexpand(true);
    hint.set_halign(gtk::Align::Start);
    hint.set_ellipsize(gtk::pango::EllipsizeMode::End);

    let adj = gtk::Adjustment::new(0.0, 0.0, 1.0, 1.0, 1.0, 1.0);
    let scroll = gtk::Scrollbar::new(gtk::Orientation::Vertical, Some(&adj));
    scroll.update_property(&[gtk::accessible::Property::Label(tr("Position in file"))]);
    scroll.set_focusable(false);

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    body.append(&view);
    body.append(&scroll);
    let status = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    status.add_css_class("ofm-statusbox");
    status.append(&left);
    status.append(&notice);
    status.append(&hint);
    status.append(&right);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 0);
    column.append(&title);
    column.append(&body);
    column.append(&status);

    let window = adw::ApplicationWindow::builder()
        .content(&column)
        .default_width(1000)
        .default_height(720)
        .title(tr("Viewer"))
        .build();
    if let Some(app) = main.window.application() {
        window.set_application(Some(&app));
    }
    window.set_transient_for(Some(&main.window));
    window.set_destroy_with_parent(true);
    window.add_css_class("ofm-window");
    dispatch::install_key_controller(&window, ctx, WindowKind::Viewer);

    let initial = {
        let cfg = ctx.config.borrow();
        Initial {
            wrap: cfg.viewer.wrap,
            encoding: cfg.viewer.default_encoding.parse::<Encoding>().ok(),
        }
    };
    let session = Session::new(
        ctx,
        window.clone(),
        view.clone(),
        Ui {
            title,
            left,
            right,
            notice,
            scroll,
        },
        &initial,
    );

    // Mouse wheel = scroll commands.
    let acc = Rc::new(Cell::new(0.0f64));
    let wheel = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
    {
        let weak = Rc::downgrade(&session);
        wheel.connect_scroll(move |_, _dx, dy| {
            let Some(s) = weak.upgrade() else {
                return gtk::glib::Propagation::Proceed;
            };
            acc.set(acc.get() + dy * WHEEL_ROWS);
            let n = acc.get().trunc();
            acc.set(acc.get() - n);
            if n != 0.0 {
                s.move_rows(n as i64);
            }
            gtk::glib::Propagation::Stop
        });
    }
    view.add_controller(wheel);

    // Theme switches repaint the viewer.
    let theme_id = {
        let view = view.clone();
        ctx.theme.changed.connect(move |palette| {
            view.set_palette(Rc::clone(palette));
        })
    };
    host.add(Rc::clone(&session));
    {
        let (weak_ctx, session2) = (Rc::downgrade(ctx), Rc::clone(&session));
        window.connect_close_request(move |_| {
            session2.shutdown();
            session2.run_on_close();
            if let Some(ctx) = weak_ctx.upgrade() {
                ctx.theme.changed.disconnect(theme_id);
                if let Some(host) = ctx.ext.get::<ViewerHost>() {
                    host.remove(&session2);
                }
            }
            gtk::glib::Propagation::Proceed
        });
    }
    session.open(siblings, idx);
    window.present();
    Some(session)
}

//! T8.4 (F31): the file clipboard inside the app, through `gdk::Clipboard` and the commands
//! `file.clipboard_copy/cut/paste/copy_path`, with the real job engine. Interop with GNOME Files
//! is checked at the format level: what OFM publishes carries `x-special/gnome-copied-files` and a
//! file list, and OFM pastes what only GNOME Files' format (or only a file list) offers.
//! Also the drop highlight (screenshots with `OFM_SHOT_DIR`). Needs a display; skips without one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use fileops_support::*;
use gtk::gdk;
use gtk::gio;
use gtk::glib;
use gtk::prelude::*;
use ofm_app::desktop::clip::{self, ClipOp, GNOME_COPIED_FILES};
use ofm_app::desktop::{clipboard, dnd};
use ofm_core::cmd;
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};

const RAW: &[u8] = b"caf\xe9 \xff.txt";

fn clipboard_of(fx: &Fixture) -> gdk::Clipboard {
    fx.win.window.clipboard()
}

fn read_clip(cb: &gdk::Clipboard) -> Option<clip::ClipFiles> {
    glib::MainContext::default().block_on(clipboard::read_files(cb))
}

fn exists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

/// One test function: GTK may be initialised by one thread only, so the scenarios run in turn.
#[test]
fn desktop_clipboard_and_drop() {
    if !init() {
        return;
    }
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"alpha"))
        .file("b.txt", FileSpec::content(b"beta"))
        .file("c.txt", FileSpec::content(b"gamma"))
        .file("d.txt", FileSpec::content(b"delta"))
        .file(raw_name(RAW), FileSpec::content(b"raw"));
    let right = TreeSpec::new().file("keep", FileSpec::content(b"k"));
    let fx = Fixture::new(
        "io.github.mscoder.OfmDesktopClipboard",
        left,
        right,
        theme_from_env().as_deref(),
    );
    let (l, r): (PathBuf, PathBuf) = (fx.left.path().to_owned(), fx.right.path().to_owned());
    let cb = clipboard_of(&fx);

    // ---- Ctrl+C: both formats are offered, the content decodes back. ----
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_CLIPBOARD_COPY);
    let formats = cb.formats();
    assert!(formats.contain_mime_type(GNOME_COPIED_FILES), "{formats:?}");
    assert!(formats.contain_mime_type("text/uri-list"), "{formats:?}");
    let got = read_clip(&cb).expect("files on the clipboard");
    assert_eq!(got.op, ClipOp::Copy);
    assert_eq!(got.paths, vec![l.join("a.txt")]);

    // The raw GNOME payload is what GNOME Files would read.
    let (stream, mime) = glib::MainContext::default()
        .block_on(cb.read_future(&[GNOME_COPIED_FILES], glib::Priority::DEFAULT))
        .unwrap();
    assert_eq!(mime, GNOME_COPIED_FILES);
    let bytes = glib::MainContext::default()
        .block_on(stream.read_bytes_future(4096, glib::Priority::DEFAULT))
        .unwrap();
    assert_eq!(
        &bytes[..],
        format!("copy\nfile://{}/a.txt", l.display()).as_bytes()
    );

    // ---- Ctrl+V in the other panel copies. ----
    fx.ctx.set_active_side(1);
    fx.run(cmd::FILE_CLIPBOARD_PASTE);
    pump_until("a.txt pasted", || exists(&r.join("a.txt")));
    pump_for(200);
    assert_eq!(std::fs::read(r.join("a.txt")).unwrap(), b"alpha");
    assert!(exists(&l.join("a.txt")), "copy must keep the source");

    // ---- Ctrl+X + Ctrl+V moves, with two marked files. ----
    fx.ctx.set_active_side(0);
    fx.ctx.host(0).active().mutate(|m| {
        m.set_cursor_name("b.txt".as_ref());
        m.toggle_select(m.cursor().unwrap());
        m.set_cursor_name("c.txt".as_ref());
        m.toggle_select(m.cursor().unwrap());
    });
    fx.run(cmd::FILE_CLIPBOARD_CUT);
    let got = read_clip(&cb).unwrap();
    assert_eq!(got.op, ClipOp::Cut);
    assert_eq!(got.paths, vec![l.join("b.txt"), l.join("c.txt")]);
    fx.ctx.set_active_side(1);
    fx.run(cmd::FILE_CLIPBOARD_PASTE);
    pump_until("moved", || {
        exists(&r.join("b.txt")) && exists(&r.join("c.txt")) && !exists(&l.join("b.txt"))
    });
    assert_eq!(std::fs::read(r.join("c.txt")).unwrap(), b"gamma");
    assert!(!exists(&l.join("c.txt")), "cut must remove the source");
    pump_for(300);
    assert!(
        !cb.formats().contain_mime_type(GNOME_COPIED_FILES),
        "a pasted cut empties the clipboard"
    );

    // ---- GNOME Files -> OFM: only its own format on the clipboard (cut). ----
    let gnome = clip::encode_gnome(ClipOp::Cut, &[l.join("d.txt")]);
    cb.set_content(Some(&gdk::ContentProvider::for_bytes(
        GNOME_COPIED_FILES,
        &glib::Bytes::from_owned(gnome),
    )))
    .unwrap();
    fx.run(cmd::FILE_CLIPBOARD_PASTE);
    pump_until("gnome cut pasted", || exists(&r.join("d.txt")));
    pump_until("source gone", || !exists(&l.join("d.txt")));

    // ---- Another app that only offers a file list (copy), non-UTF-8 name. ----
    let raw_path = l.join(std::ffi::OsStr::from_bytes(RAW));
    let list = gdk::FileList::from_array(&[gio::File::for_path(&raw_path)]);
    cb.set_content(Some(&gdk::ContentProvider::for_value(&list.to_value())))
        .unwrap();
    let got = read_clip(&cb).unwrap();
    assert_eq!(got.op, ClipOp::Copy);
    assert_eq!(got.paths, vec![raw_path.clone()]);
    fx.run(cmd::FILE_CLIPBOARD_PASTE);
    let raw_copy = r.join(std::ffi::OsStr::from_bytes(RAW));
    pump_until("raw name pasted", || exists(&raw_copy));
    assert_eq!(std::fs::read(&raw_copy).unwrap(), b"raw");
    assert!(exists(&raw_path));

    // ---- Text on the clipboard is not a file paste: nothing happens. ----
    cb.set_text("hello");
    let before = std::fs::read_dir(&r).unwrap().count();
    fx.run(cmd::FILE_CLIPBOARD_PASTE);
    pump_for(300);
    assert_eq!(std::fs::read_dir(&r).unwrap().count(), before);

    // ---- Ctrl+Shift+C: the full path as text. ----
    fx.ctx.set_active_side(0);
    fx.cursor_to(0, "a.txt");
    fx.run(cmd::FILE_COPY_PATH);
    let text = glib::MainContext::default()
        .block_on(cb.read_text_future())
        .unwrap()
        .unwrap();
    assert_eq!(text.as_str(), l.join("a.txt").to_string_lossy());
    drop(fx);

    drop_highlight_follows_the_state();
}

/// The drop highlight widgets (panel frame, folder row) exist per panel and toggle (screenshots
/// with `OFM_SHOT_DIR`).
fn drop_highlight_follows_the_state() {
    let left = TreeSpec::new().file("a.txt", FileSpec::content(b"a")).dir(
        "folder",
        TreeSpec::new().file("in.txt", FileSpec::content(b"i")),
    );
    let right = TreeSpec::new().file("x", FileSpec::content(b"x"));
    let fx = Fixture::new(
        "io.github.mscoder.OfmDesktopDrop",
        left,
        right,
        theme_from_env().as_deref(),
    );
    // Visible highlight widgets of both panels: (frames, rows).
    fn count_named(w: &gtk::Widget, name: &str) -> usize {
        let mut n = usize::from(w.widget_name() == name && w.is_visible());
        let mut c = w.first_child();
        while let Some(ch) = c {
            n += count_named(&ch, name);
            c = ch.next_sibling();
        }
        n
    }
    let frames = |fx: &Fixture| -> (usize, usize) {
        let root: &gtk::Widget = fx.win.window.upcast_ref();
        (
            count_named(root, "ofm-drop-frame"),
            count_named(root, "ofm-drop-row"),
        )
    };
    assert_eq!(frames(&fx), (0, 0));

    dnd::preview_highlight(&fx.ctx, 1, None);
    pump_for(100);
    assert_eq!(frames(&fx), (1, 0));
    fx.shot("drop-panel");
    dnd::clear_highlight(&fx.ctx, 1);

    // Highlight the `folder` row of the left panel the way a drag over it does.
    let list = {
        // The list widget of the left panel is the one named by its accessible label; its row
        // rectangle comes from the public hit test.
        fn find_list(w: &gtk::Widget) -> Option<ofm_app::widgets::file_list::FileList> {
            if let Some(l) = w.downcast_ref::<ofm_app::widgets::file_list::FileList>() {
                return Some(l.clone());
            }
            let mut c = w.first_child();
            while let Some(ch) = c {
                if let Some(l) = find_list(&ch) {
                    return Some(l);
                }
                c = ch.next_sibling();
            }
            None
        }
        find_list(fx.win.window.upcast_ref()).expect("file list")
    };
    pump_for(200);
    // Row 0 is `..`? Probe a few rows for the folder row; the hit test returns the geometry.
    let model_idx = {
        let ctrl = fx.ctx.host(0).active();
        let m = ctrl.model();
        (0..m.visible_len())
            .find(|&i| m.visible(i).is_some_and(|e| e.name == "folder"))
            .unwrap()
    };
    let mut rect = None;
    'scan: for y in (0..600).step_by(4) {
        for x in [20.0, 60.0, 200.0] {
            if let Some(r) = list.row_at(x, f64::from(y))
                && r.index == model_idx
            {
                rect = Some(r);
                break 'scan;
            }
        }
    }
    let rect = rect.expect("row geometry for the folder");
    assert!(rect.width > 10.0 && rect.height > 5.0);
    dnd::preview_highlight(&fx.ctx, 0, Some(rect));
    pump_for(100);
    assert_eq!(frames(&fx), (0, 1));
    fx.shot("drop-row");
    dnd::clear_highlight(&fx.ctx, 0);
    assert_eq!(frames(&fx), (0, 0));
}

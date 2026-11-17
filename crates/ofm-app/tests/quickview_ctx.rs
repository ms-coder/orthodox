//! T5.2b: quick view driven through `AppContext::execute` (needs a display; skips without one):
//! Ctrl+Q's command shows a text preview, follows the cursor (debounced) to a directory summary,
//! bounces the active panel back, and switches off cleanly.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::panel::{LEFT, LoadState, RIGHT};
use ofm_app::quickview::QuickView;
use ofm_app::quickview::pane::Body;
use ofm_app::window::MainWindow;
use ofm_config::ConfigPaths;
use ofm_core::{Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec};

fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn loaded(ctx: &AppContext) -> bool {
    ctx.panels
        .iter()
        .all(|h| h.active().state() == LoadState::Idle && h.active().model().total_len() > 0)
}

#[test]
fn quick_view_shows_text_then_directory_and_switches_off() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"first line\nsecond line\n"))
        .dir(
            "sub",
            TreeSpec::new().file("f", FileSpec::content(b"12345")),
        )
        .build_temp()
        .unwrap();
    let right = TreeSpec::new()
        .file("x", FileSpec::content(b"x"))
        .build_temp()
        .unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    let ctx = build_context(AppInit {
        paths: ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state")),
        left: Location::Local(left.path().to_owned()),
        right: Location::Local(right.path().to_owned()),
    });
    let app = adw::Application::builder()
        .application_id("io.github.mscoder.OfmQuickViewTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE)
        .expect("register app");
    let _win: Rc<MainWindow> = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    pump_until("panels loaded", || loaded(&ctx));
    let qv = ctx.ext.get::<QuickView>().expect("QuickView state");
    assert!(!qv.is_on());

    // `..`, sub, a.txt: move to a.txt and switch quick view on.
    ctx.execute(cmd::PANEL_END);
    ctx.execute(cmd::PANEL_TOGGLE_QUICKVIEW);
    assert_eq!(qv.shown_side(), Some(RIGHT));
    pump_until("text preview", || {
        qv.pane_content(RIGHT).is_some_and(
            |c| matches!(&c.body, Body::Text(l) if l == &["first line", "second line"]),
        )
    });
    assert_eq!(qv.pane_content(RIGHT).unwrap().title, "a.txt");

    // The cursor moves to the directory: the summary follows, with totals.
    ctx.execute(cmd::PANEL_CURSOR_UP);
    pump_until("directory summary", || {
        qv.pane_content(RIGHT).is_some_and(|c| {
            c.title == "sub"
                && matches!(&c.body, Body::Info { rows, .. }
                    if rows.iter().any(|r| r.label == "Files" && r.value == "1")
                        && rows.iter().any(|r| r.label == "Size"))
                && c.subtitle.is_empty()
        })
    });

    // Switching to the preview side bounces back to the file panel.
    ctx.execute(cmd::PANEL_SWITCH);
    assert_eq!(ctx.active_side(), LEFT);

    // Off: pane emptied, left cursor untouched.
    let cursor = ctx.host(LEFT).active().model().cursor();
    ctx.execute(cmd::PANEL_TOGGLE_QUICKVIEW);
    assert!(!qv.is_on());
    assert!(matches!(qv.pane_content(RIGHT).unwrap().body, Body::Empty));
    assert_eq!(ctx.host(LEFT).active().model().cursor(), cursor);
    // And on again works (state was fully reset).
    ctx.execute(cmd::PANEL_TOGGLE_QUICKVIEW);
    pump_until("preview after re-enabling", || {
        qv.pane_content(RIGHT).is_some_and(|c| c.title == "sub")
    });
}

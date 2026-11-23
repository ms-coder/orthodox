//! T6.3 in-process test: F3 on an archive entry extracts it to a private temp dir and opens the
//! viewer; closing the viewer removes the temp dir. Real zip made with the `zip` CLI (skips when
//! it is missing). Needs a display.
//!
//! The sibling real-binary test `archive_real.rs` covers the same flow with real key events; this
//! one runs fast and pinpoints the cleanup contract (the viewer's `destroy` signal is not a viable
//! hook — GTK defers it — so cleanup runs from the close-request path, see
//! `viewer::Session::set_on_close`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixture setup on the test thread
)]

use std::ffi::OsString;
use std::process::Command;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::panel::{LEFT, LoadState};
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
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}

fn loaded(ctx: &AppContext, side: usize) -> bool {
    let c = ctx.host(side).active();
    c.state() == LoadState::Idle && c.model().total_len() > 0
}

fn put_cursor(ctx: &AppContext, side: usize, name: &str) {
    let c = ctx.host(side).active();
    assert!(
        c.model_mut().set_cursor_name(&OsString::from(name)),
        "no entry {name}"
    );
}

fn have(tool: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn make_zip() -> Option<(Vec<u8>, ofm_testkit::temp::TempDir)> {
    if !have("zip") {
        eprintln!("skipping: the zip CLI is not installed");
        return None;
    }
    let scratch = ofm_testkit::temp::TempDir::new().unwrap();
    let src = scratch.path().join("src");
    std::fs::create_dir_all(&src).unwrap();
    std::fs::write(src.join("hello.txt"), b"hello archive\n").unwrap();
    let st = Command::new("zip")
        .current_dir(&src)
        .args(["-q", "test.zip", "hello.txt"])
        .stdout(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(st.success());
    Some((std::fs::read(src.join("test.zip")).unwrap(), scratch))
}

#[test]
fn viewer_temp_dir_is_removed_when_the_viewer_closes() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();
    let Some((zip_bytes, _scratch)) = make_zip() else {
        return;
    };
    let left = TreeSpec::new()
        .file("test.zip", FileSpec::content(&zip_bytes))
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
        .application_id("io.github.mscoder.OfmArchiveViewerCtxTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE).expect("register");
    let _win = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    pump_until("panels loaded", || {
        loaded(&ctx, LEFT) && loaded(&ctx, ofm_app::panel::RIGHT)
    });

    // Enter the archive (real index on the pool, magic-detected zip).
    put_cursor(&ctx, LEFT, "test.zip");
    ctx.execute(cmd::PANEL_OPEN);
    pump_until("archive opened", || {
        matches!(ctx.host(LEFT).active().location(), Location::Archive { .. }) && loaded(&ctx, LEFT)
    });

    // F3 on hello.txt: extracted to a private temp dir, opened in the viewer.
    put_cursor(&ctx, LEFT, "hello.txt");
    ctx.execute(cmd::FILE_VIEW);
    pump_until("viewer open", || ofm_app::viewer::current(&ctx).is_some());
    pump_until("one temp dir", || ctx.services.archives.temp_count() == 1);

    // Esc (viewer.close): the window closes, the temp dir is removed.
    ctx.execute(cmd::VIEWER_CLOSE);
    pump_until("viewer host empty", || {
        ctx.ext
            .get::<ofm_app::viewer::ViewerHost>()
            .unwrap()
            .is_empty()
    });
    pump_until("temp dir removed", || {
        ctx.services.archives.temp_count() == 0
    });
}

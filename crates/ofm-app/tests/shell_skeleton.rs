//! T2.0 smoke test: the shell lists real directories, Tab switches panels, cursor commands move
//! the cursor. Drives the app through `AppContext::execute` (no synthetic key events).
//! Needs a display (Xvfb in `scripts/check.sh`); skips itself without one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dispatch::ExecResult;
use ofm_app::panel::{LEFT, LoadState, RIGHT};
use ofm_app::window::MainWindow;
use ofm_config::ConfigPaths;
use ofm_core::{Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec};
use std::rc::Rc;

fn pump_until(mut done: impl FnMut() -> bool) {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn idle(ctx: &AppContext) -> bool {
    ctx.panels
        .iter()
        .all(|h| h.active().state() == LoadState::Idle && h.active().model().total_len() > 0)
}

#[test]
fn lists_switches_and_moves() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("b.txt", FileSpec::content(b"b"))
        .dir("sub", TreeSpec::new())
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
        .application_id("io.github.mscoder.OfmTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE)
        .expect("register app");
    let _win: Rc<MainWindow> = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    pump_until(|| idle(&ctx));

    // `..` + sub + a.txt + b.txt, directories first.
    assert_eq!(ctx.host(LEFT).active().model().visible_len(), 4);
    assert_eq!(ctx.host(RIGHT).active().model().visible_len(), 2);
    assert_eq!(ctx.active_side(), LEFT);

    assert_eq!(ctx.execute(cmd::PANEL_CURSOR_DOWN), ExecResult::Handled);
    assert_eq!(ctx.host(LEFT).active().model().cursor(), Some(1));
    ctx.execute(cmd::PANEL_END);
    assert_eq!(ctx.host(LEFT).active().model().cursor(), Some(3));
    ctx.execute(cmd::PANEL_CURSOR_UP);
    assert_eq!(ctx.host(LEFT).active().model().cursor(), Some(2));

    ctx.execute(cmd::PANEL_SWITCH);
    assert_eq!(ctx.active_side(), RIGHT);
    ctx.execute(cmd::PANEL_CURSOR_DOWN);
    assert_eq!(ctx.host(RIGHT).active().model().cursor(), Some(1));
    assert_eq!(ctx.host(LEFT).active().model().cursor(), Some(2));
    ctx.execute(cmd::PANEL_SWITCH);
    assert_eq!(ctx.active_side(), LEFT);

    // Commands nobody implemented yet are reported, not crashed on.
    assert_eq!(ctx.execute(cmd::FILE_COPY), ExecResult::NotImplemented);
}

// The former placeholders `file_list_brief_mode_columns` (T2.1) and `navigation_commands` (T2.3) were
// removed: brief/full/custom layout is covered by `src/widgets/file_list/tests.rs`
// (`list_widget_end_to_end`, `commands_drive_sort_view_and_hidden`) and the navigation commands by
// `tests/navigation_commands.rs`. Real key events: `tests/real_binary.rs`.

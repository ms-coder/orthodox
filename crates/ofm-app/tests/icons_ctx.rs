//! M8-icons (F30) smoke test: highlight-rule icons in the real panels across theme switches
//! (`app.theme_next`) and view modes, and the warning for an icon the icon theme does not have.
//! One `#[test]` only: GTK must stay on a single thread. Skips itself without a display.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::AppInit;
use ofm_app::dialogs::preferences::warning_lines;
use ofm_app::dispatch::ExecResult;
use ofm_app::panel::LoadState;
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

#[test]
fn highlight_icons_in_the_panels() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();

    let t = TreeSpec::new()
        .dir("sub", TreeSpec::new())
        .file("pic.png", FileSpec::content(b"x"))
        .build_temp()
        .unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state"));
    std::fs::create_dir_all(paths.themes_dir()).unwrap();
    std::fs::write(
        paths.themes_dir().join("mine.toml"),
        "base = \"adwaita\"\n\
         [[highlight]]\nattr = \"dir\"\nicon = \"folder\"\n\
         [[highlight]]\nattr = \"image\"\nicon = \"no-such-icon-xyz\"\n",
    )
    .unwrap();
    std::fs::write(paths.config_file(), "[theme]\nname = \"mine\"\n").unwrap();

    let ctx = build_context(AppInit {
        paths,
        left: Location::Local(t.path().to_owned()),
        right: Location::Local(t.path().to_owned()),
    });
    let lines = warning_lines(&ctx);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("no-such-icon-xyz"), "{lines:?}");

    let app = adw::Application::builder()
        .application_id("io.github.mscoder.OfmTestIcons")
        .build();
    app.register(gtk::gio::Cancellable::NONE).unwrap();
    let win = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    win.present();
    pump_until("listings", || {
        ctx.panels
            .iter()
            .all(|h| h.active().state() == LoadState::Idle)
    });
    let list = &win.left.list;
    pump_until("layout", || list.window().1 > 0);
    let index = |name: &str| {
        let ctrl = ctx.active_controller();
        let m = ctrl.model();
        (0..m.visible_len())
            .find(|i| m.visible(*i).is_some_and(|e| e.name == *name))
            .unwrap()
    };
    let (sub, pic) = (index("sub"), index("pic.png"));
    let icon = |i: u32| list.icon_at(i).map(|(name, _)| name);

    assert_eq!(ctx.theme.theme().id, "mine");
    assert_eq!(icon(sub).as_deref(), Some("folder"));
    assert_eq!(
        icon(pic).as_deref(),
        Some("text-x-generic"),
        "unknown name: generic icon"
    );

    // mine -> far-blue (text only) -> commander-light -> adwaita.
    for (theme, dir_icon, pic_icon) in [
        ("far-blue", None, None),
        ("commander-light", Some("folder"), Some("image-x-generic")),
        ("adwaita", Some("folder"), Some("image-x-generic")),
    ] {
        assert_eq!(ctx.execute(cmd::APP_THEME_NEXT), ExecResult::Handled);
        assert_eq!(ctx.theme.theme().id, theme);
        for view in [
            cmd::PANEL_VIEW_BRIEF,
            cmd::PANEL_VIEW_FULL,
            cmd::PANEL_VIEW_CUSTOM,
        ] {
            ctx.execute(view);
            assert_eq!(icon(sub).as_deref(), dir_icon, "{theme}");
            assert_eq!(icon(pic).as_deref(), pic_icon, "{theme}");
        }
    }
    win.window.close();
}

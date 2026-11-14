//! T2.2 smoke test: window chrome (F-key bar, menu bar, one-panel mode, split ratio), runtime theme
//! switching, session restore/save. Drives everything through `AppContext::execute`.
//! One `#[test]` only: GTK must stay on a single thread. Skips itself without a display.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::chrome::ChromeState;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dispatch::ExecResult;
use ofm_app::panel::{LEFT, LoadState, RIGHT};
use ofm_app::theme::ThemeState;
use ofm_app::widgets::file_list::{self, ViewMode};
use ofm_app::window::MainWindow;
use ofm_app::window::layout::LayoutState;
use ofm_app::window::session;
use ofm_config::session::{PanelSession, Session, TabSession, WindowState};
use ofm_config::{ConfigPaths, Theme};
use ofm_core::panel::SortKey;
use ofm_core::{Location, Mods, cmd};
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

fn pump_for(ms: u64) {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn tab(
    path: &std::path::Path,
    sort: &str,
    reverse: bool,
    hidden: bool,
    locked: bool,
) -> TabSession {
    let mut t = TabSession {
        view: "full".into(),
        sort: sort.into(),
        sort_reverse: reverse,
        dirs_first: true,
        show_hidden: hidden,
        locked,
        ..TabSession::default()
    };
    t.set_path(path);
    t
}

#[test]
fn chrome_themes_and_session() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();

    let t = TreeSpec::new()
        .dir("a", TreeSpec::new().file("x", FileSpec::content(b"x")))
        .dir("b", TreeSpec::new())
        .dir("c", TreeSpec::new())
        .build_temp()
        .unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state"));
    let dir = |n: &str| t.path().join(n);

    // A stored session with deliberately broken indices (deferred M1 finding).
    let stored = Session {
        window: WindowState {
            width: 900,
            height: 600,
            maximized: false,
        },
        split_ratio: 0.3,
        active_panel: 9,
        panels: vec![
            PanelSession {
                active_tab: 42,
                tabs: vec![
                    tab(&dir("a"), "mtime", true, true, false),
                    tab(&dir("b"), "size", false, false, true),
                ],
            },
            PanelSession {
                active_tab: 0,
                tabs: vec![tab(&dir("c"), "name", false, false, false)],
            },
        ],
        ..Session::default()
    };
    stored.save(&paths).unwrap();

    let ctx: Rc<AppContext> = build_context(AppInit {
        paths: paths.clone(),
        left: Location::Local(t.path().to_owned()),
        right: Location::Local(t.path().to_owned()),
    });
    // ---- window ------------------------------------------------------------------------------
    let app = adw::Application::builder()
        .application_id("io.github.mscoder.OfmTestChrome")
        .build();
    app.register(gtk::gio::Cancellable::NONE).unwrap();
    let win: Rc<MainWindow> = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    win.present();
    pump_until("window mapped", || win.window.is_mapped());
    pump_until("listings", || {
        ctx.panels
            .iter()
            .all(|h| h.active().state() == LoadState::Idle)
    });
    assert_eq!(
        win.window.default_size(),
        (900, 600),
        "window size restored"
    );
    // The window restores the stored paths itself unless the test binary got CLI arguments;
    // restore explicitly in that case so the assertions are independent of how we were started.
    if ctx.host(LEFT).tab_count() == 1 {
        let mut s = stored.clone();
        session::sanitize(&mut s);
        session::restore_panels(&ctx, &s);
    }

    // ---- restore -----------------------------------------------------------------------------
    assert_eq!(ctx.host(LEFT).tab_count(), 2);
    assert_eq!(
        ctx.host(LEFT).active_index(),
        1,
        "active_tab 42 clamped to the last tab"
    );
    assert_eq!(ctx.active_side(), RIGHT, "active_panel 9 clamped to 1");
    let tabs = ctx.host(LEFT).tabs();
    assert_eq!(tabs[0].location(), Location::Local(dir("a")));
    assert!(tabs[0].model().show_hidden());
    assert_eq!(tabs[0].model().sort_spec().key, SortKey::Mtime);
    assert!(tabs[0].model().sort_spec().reverse);
    assert!(tabs[1].locked());
    assert_eq!(
        file_list::view_mode(&tabs[0]),
        ViewMode::Full,
        "view mode restored"
    );
    assert_eq!(
        ctx.host(RIGHT).active().location(),
        Location::Local(dir("c"))
    );

    let layout = ctx.ext.get::<LayoutState>().unwrap();
    assert!((layout.ratio() - 0.3).abs() < 1e-6);
    assert!(win.panels.position() > 0 && win.panels.position() < 450);

    // ---- one-panel mode ----------------------------------------------------------------------
    assert!(win.left.root.is_visible() && win.right.root.is_visible());
    assert_eq!(ctx.execute(cmd::PANEL_TOGGLE_OTHER), ExecResult::Handled);
    assert!(layout.one_panel());
    assert!(
        !win.left.root.is_visible() && win.right.root.is_visible(),
        "right is active"
    );
    ctx.execute(cmd::PANEL_SWITCH);
    assert!(
        win.left.root.is_visible() && !win.right.root.is_visible(),
        "follows the active panel"
    );
    ctx.execute(cmd::PANEL_TOGGLE_OTHER);
    assert!(win.left.root.is_visible() && win.right.root.is_visible());

    // ---- active panel highlight --------------------------------------------------------------
    assert!(win.left.root.has_css_class("active") && !win.right.root.has_css_class("active"));

    // ---- theme switching ---------------------------------------------------------------------
    assert_eq!(ctx.theme.theme().id, "far-blue");
    let seen = Rc::new(std::cell::Cell::new(0));
    {
        let seen = Rc::clone(&seen);
        ctx.theme.changed.connect(move |_| seen.set(seen.get() + 1));
    }
    let far_bg = ctx.theme.palette().panel.bg;
    assert_eq!(ctx.execute(cmd::APP_THEME_NEXT), ExecResult::Handled);
    assert_eq!(ctx.theme.theme().id, "commander-light");
    assert_eq!(seen.get(), 1, "one instant switch, one notification");
    assert_ne!(ctx.theme.palette().panel.bg, far_bg);
    ctx.execute(cmd::APP_THEME_NEXT);
    assert_eq!(ctx.theme.theme().id, "adwaita");
    // Adwaita colours come from the style manager / stylesheet, not magenta.
    assert_ne!(
        ctx.theme.palette().panel.bg,
        gtk::gdk::RGBA::new(1.0, 0.0, 1.0, 1.0)
    );
    ctx.execute(cmd::APP_THEME_NEXT);
    assert_eq!(ctx.theme.theme().id, "far-blue");
    assert_eq!(ctx.ext.get::<ThemeState>().unwrap().themes().len(), 3);
    // Rapid switching: the last choice wins in config.toml (saves are serialised).
    ctx.execute(cmd::APP_THEME_NEXT);
    assert_eq!(ctx.theme.theme().id, "commander-light");

    // ---- F-key bar ---------------------------------------------------------------------------
    let chrome = ctx.ext.get::<ChromeState>().unwrap();
    let fkeys = chrome.fkeybar.borrow().clone().unwrap();
    let labels = fkeys.labels();
    assert_eq!(labels.len(), 10);
    assert_eq!(labels[4], "Copy");
    assert_eq!(labels[9], "Quit");
    fkeys.set_mods(Mods {
        shift: true,
        ..Mods::default()
    });
    assert_eq!(fkeys.labels()[5], "Rename", "Shift+F6");
    fkeys.set_mods(Mods::default());
    assert_eq!(fkeys.labels()[4], "Copy");
    fkeys.click(5); // F5 -> file.copy (not implemented in M2, must not crash)

    // ---- menu bar ----------------------------------------------------------------------------
    let menubar = chrome.menubar.borrow().clone().unwrap();
    assert!(menubar.menu_count() >= 5);
    assert!(menubar.row_commands(0).contains(&cmd::FILE_COPY));
    assert!(
        !menubar.handle_key(gtk::gdk::Key::Down),
        "closed menu ignores keys"
    );
    assert_eq!(ctx.execute(cmd::APP_MENU), ExecResult::Handled);
    pump_until("menu opens", || menubar.is_open());
    assert_eq!(menubar.row_accel(0, cmd::FILE_COPY).as_deref(), Some("F5"));
    assert_eq!(menubar.row_accel(0, cmd::APP_QUIT).as_deref(), Some("F10"));
    pump_for(100); // first row gets the focus
    // While open, keys are consumed by the menu (navigation, switch, close).
    assert!(menubar.handle_key(gtk::gdk::Key::Down));
    assert!(menubar.handle_key(gtk::gdk::Key::Right));
    pump_until("next menu", || menubar.is_open());
    assert!(menubar.handle_key(gtk::gdk::Key::Escape));
    pump_until("menu closes", || !menubar.is_open());
    // F9 toggles.
    ctx.execute(cmd::APP_MENU);
    pump_until("menu opens again", || menubar.is_open());
    assert!(menubar.handle_key(gtk::gdk::Key::F9));
    pump_until("menu closes by F9", || !menubar.is_open());

    // ---- theme choice is persisted (worker thread) --------------------------------------------
    pump_until("config saved", || {
        std::fs::read_to_string(paths.config_file()).is_ok_and(|s| s.contains("commander-light"))
    });
    pump_for(100);
    let text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(text.contains("commander-light"), "{text}");
    assert_eq!(Theme::builtin("far-blue").unwrap().id, "far-blue");

    // ---- save on close -----------------------------------------------------------------------
    ctx.set_active_side(LEFT);
    ctx.execute(cmd::PANEL_CURSOR_DOWN);
    assert_eq!(ctx.execute(cmd::APP_QUIT), ExecResult::Handled);
    pump_until("session saved", || {
        let (s, _) = Session::load(&paths);
        s.active_panel == 0 && s.panels.first().is_some_and(|p| p.active_tab == 1)
    });
    let (saved, warnings) = Session::load(&paths);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(saved.panels.len(), 2);
    assert_eq!(saved.panels[0].tabs.len(), 2);
    assert_eq!(saved.panels[0].tabs[0].path_buf().unwrap(), dir("a"));
    assert_eq!(saved.panels[0].tabs[0].sort, "mtime");
    assert!(saved.panels[0].tabs[0].sort_reverse);
    assert!(saved.panels[0].tabs[0].show_hidden);
    assert!(saved.panels[0].tabs[1].locked);
    assert_eq!(saved.panels[0].tabs[0].view, "full");
    assert_eq!(saved.panels[1].tabs[0].path_buf().unwrap(), dir("c"));
    assert!(
        (saved.split_ratio - 0.3).abs() < 0.02,
        "{}",
        saved.split_ratio
    );
    assert!(saved.window.width >= 320 && saved.window.height >= 320);
    pump_for(50);
}

//! Shared helpers of the file-operation UI tests (`fileops_*.rs`). Each test binary has ONE
//! `#[test]` that owns the GTK thread (GTK may only be initialised once per process).
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods
)]

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dispatch::ExecResult;
use ofm_app::navigation::modal::ModalStack;
use ofm_app::panel::LoadState;
use ofm_app::window::MainWindow;
use ofm_config::ConfigPaths;
use ofm_core::{CommandId, Location};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::TreeSpec;

/// A running application over two fixture directories.
pub struct Fixture {
    pub ctx: Rc<AppContext>,
    pub win: Rc<MainWindow>,
    pub left: TempDir,
    pub right: TempDir,
    pub _cfg: TempDir,
}

/// Theme for screenshot runs: `OFM_TEST_THEME` (default far-blue).
pub fn theme_from_env() -> Option<String> {
    Some(std::env::var("OFM_TEST_THEME").unwrap_or_else(|_| "far-blue".to_owned()))
}

/// Initialise GTK (false: no display, the test should skip itself).
pub fn init() -> bool {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return false;
    }
    let _ = adw::init();
    true
}

pub fn pump_for(ms: u64) {
    let main = glib::MainContext::default();
    let start = Instant::now();
    while start.elapsed() < Duration::from_millis(ms) {
        main.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let main = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "timed out: {what}"
        );
        main.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

pub fn settle(ctx: &AppContext) {
    pump_until("panels loaded", || {
        ctx.panels
            .iter()
            .all(|h| h.tabs().iter().all(|t| t.state() != LoadState::Loading))
    });
}

impl Fixture {
    /// `left` / `right` build the two panel directories; `theme` is a theme id.
    pub fn new(app_id: &str, left: TreeSpec, right: TreeSpec, theme: Option<&str>) -> Fixture {
        let left = left.build_temp().unwrap();
        let right = right.build_temp().unwrap();
        let cfg = TreeSpec::new().build_temp().unwrap();
        let config_dir = cfg.path().join("config");
        if let Some(t) = theme {
            std::fs::create_dir_all(&config_dir).unwrap();
            std::fs::write(
                config_dir.join("config.toml"),
                format!("[theme]\nname = \"{t}\"\n"),
            )
            .unwrap();
        }
        let paths = ConfigPaths::with_dirs(config_dir, cfg.path().join("state"));
        let ctx = build_context(AppInit {
            paths,
            left: Location::Local(left.path().to_owned()),
            right: Location::Local(right.path().to_owned()),
        });
        let app = adw::Application::builder().application_id(app_id).build();
        app.register(gtk::gio::Cancellable::NONE).unwrap();
        let win = MainWindow::new(&app, &ctx);
        win.present();
        ctx.start_loading();
        settle(&ctx);
        Fixture {
            ctx,
            win,
            left,
            right,
            _cfg: cfg,
        }
    }

    pub fn run(&self, c: CommandId) {
        assert_eq!(self.ctx.execute(c), ExecResult::Handled, "{c}");
    }

    pub fn modal_depth(&self) -> usize {
        self.ctx.ext.get::<ModalStack>().unwrap().depth()
    }

    /// Widget with `name` anywhere under the main window (dialogs included).
    pub fn find(&self, name: &str) -> gtk::Widget {
        find_by_name(self.win.window.upcast_ref(), name)
            .unwrap_or_else(|| panic!("no widget named {name}"))
    }

    pub fn try_find(&self, name: &str) -> Option<gtk::Widget> {
        find_by_name(self.win.window.upcast_ref(), name)
    }

    pub fn entry(&self, name: &str) -> gtk::Entry {
        self.find(name).downcast().unwrap()
    }

    pub fn check(&self, name: &str) -> gtk::CheckButton {
        self.find(name).downcast().unwrap()
    }

    pub fn button(&self, name: &str) -> gtk::Button {
        self.find(name).downcast().unwrap()
    }

    pub fn cursor_to(&self, side: usize, name: &str) {
        assert!(
            self.ctx
                .host(side)
                .active()
                .mutate(|m| m.set_cursor_name(name.as_ref())),
            "no entry {name}"
        );
    }

    /// Screenshot of the whole X screen into `$OFM_SHOT_DIR/<name>.png` (only when the variable
    /// is set; needs ImageMagick's `import`).
    pub fn shot(&self, name: &str) {
        let Ok(dir) = std::env::var("OFM_SHOT_DIR") else {
            return;
        };
        pump_for(500);
        std::fs::create_dir_all(&dir).unwrap();
        let out = Path::new(&dir).join(format!("{name}.png"));
        let st = std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(&out)
            .status();
        eprintln!("screenshot {}: {st:?}", out.display());
    }
}

/// The LAST widget named `name` in tree order: a dialog that is still animating away stays in
/// the tree for a moment, the dialog just opened comes after it.
pub fn find_by_name(root: &gtk::Widget, name: &str) -> Option<gtk::Widget> {
    let mut found = None;
    let mut child = root.first_child();
    while let Some(c) = child {
        if let Some(f) = find_by_name(&c, name) {
            found = Some(f);
        }
        child = c.next_sibling();
    }
    if found.is_none() && root.widget_name() == name {
        return Some(root.clone());
    }
    found
}

/// Number of children of a `gtk::ListBox`.
pub fn list_len(list: &gtk::ListBox) -> usize {
    let mut n = 0;
    let mut c = list.first_child();
    while let Some(w) = c {
        n += 1;
        c = w.next_sibling();
    }
    n
}

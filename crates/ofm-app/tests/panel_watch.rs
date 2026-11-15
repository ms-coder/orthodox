//! T3.0: directory watching of `PanelController` (live updates, cursor-on-name, DirGone, stop on
//! shutdown). Headless: no display needed; each test runs its own glib `MainContext` as the
//! thread default, so the tests are independent of GTK initialisation and of each other.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use ofm_app::context::Services;
use ofm_app::panel::{LoadState, PanelController};
use ofm_core::Location;
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec};

/// Run `body` with a private main context as the thread default.
fn with_loop(body: impl FnOnce(&glib::MainContext)) {
    let ctx = glib::MainContext::new();
    ctx.with_thread_default(|| body(&ctx)).unwrap();
}

fn pump_until(ctx: &glib::MainContext, what: &str, mut done: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn names(c: &PanelController) -> Vec<OsString> {
    let m = c.model();
    (0..m.visible_len())
        .filter_map(|i| m.visible(i).map(|e| e.name.clone()))
        .filter(|n| n != "..")
        .collect()
}

fn has(c: &PanelController, name: &str) -> bool {
    names(c).iter().any(|n| n == OsStr::new(name))
}

fn cursor_name(c: &PanelController) -> Option<OsString> {
    c.model().cursor_entry().map(|e| e.name.clone())
}

/// Controller listing `dir`, loaded and watching.
fn opened(ctx: &glib::MainContext, dir: &Path) -> Rc<PanelController> {
    let c = PanelController::new(Services::new(), Location::Local(dir.to_path_buf()));
    c.reload();
    pump_until(ctx, "listing + watch", || {
        c.state() == LoadState::Idle && c.watching()
    });
    c
}

fn fixture() -> TempDir {
    TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("b.txt", FileSpec::content(b"b"))
        .build_temp()
        .unwrap()
}

#[test]
fn created_and_removed_files_show_up_without_reload() {
    with_loop(|ctx| {
        let t = fixture();
        let c = opened(ctx, t.path());
        assert!(has(&c, "a.txt") && !has(&c, "new.txt"));

        std::fs::write(t.path().join("new.txt"), b"n").unwrap();
        pump_until(ctx, "new.txt appears", || has(&c, "new.txt"));

        std::fs::remove_file(t.path().join("a.txt")).unwrap();
        pump_until(ctx, "a.txt disappears", || !has(&c, "a.txt"));

        std::fs::rename(t.path().join("b.txt"), t.path().join("c.txt")).unwrap();
        pump_until(ctx, "rename seen", || has(&c, "c.txt") && !has(&c, "b.txt"));
        c.shutdown();
    });
}

#[test]
fn focus_name_waits_for_the_new_item() {
    with_loop(|ctx| {
        let t = fixture();
        let c = opened(ctx, t.path());
        // Already listed: immediate.
        c.focus_name("b.txt".into());
        assert_eq!(cursor_name(&c), Some("b.txt".into()));
        // Not there yet: the cursor follows once the watcher delivers the file.
        c.focus_name("zzz-copy.txt".into());
        assert_eq!(cursor_name(&c), Some("b.txt".into()));
        std::fs::write(t.path().join("zzz-copy.txt"), b"x").unwrap();
        pump_until(ctx, "cursor on new file", || {
            cursor_name(&c) == Some("zzz-copy.txt".into())
        });
        c.shutdown();
    });
}

#[test]
fn user_cursor_move_cancels_pending_focus() {
    with_loop(|ctx| {
        let t = fixture();
        let c = opened(ctx, t.path());
        c.focus_name("later.txt".into());
        c.cursor_move(ofm_core::panel::CursorMove::Home);
        let at = cursor_name(&c);
        std::fs::write(t.path().join("later.txt"), b"x").unwrap();
        pump_until(ctx, "later.txt appears", || has(&c, "later.txt"));
        assert_eq!(cursor_name(&c), at, "the user's cursor must not be yanked");
        c.shutdown();
    });
}

#[test]
fn reload_and_focus_puts_cursor_on_name() {
    with_loop(|ctx| {
        let t = fixture();
        let c = opened(ctx, t.path());
        c.reload_and_focus("b.txt".into());
        pump_until(ctx, "reloaded", || c.state() == LoadState::Idle);
        assert_eq!(cursor_name(&c), Some("b.txt".into()));
        assert!(
            c.watching(),
            "a reload of the same directory keeps the watch"
        );
        c.shutdown();
    });
}

#[test]
fn shutdown_stops_watching() {
    with_loop(|ctx| {
        let t = fixture();
        let c = opened(ctx, t.path());
        c.shutdown();
        assert!(!c.watching());
        std::fs::write(t.path().join("late.txt"), b"x").unwrap();
        let end = Instant::now() + Duration::from_millis(600);
        while Instant::now() < end {
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!has(&c, "late.txt"));
    });
}

#[test]
fn navigation_moves_the_watch_to_the_new_directory() {
    with_loop(|ctx| {
        let t = TreeSpec::new()
            .dir(
                "sub",
                ofm_testkit::tree::DirSpec::new(TreeSpec::new().file("in.txt", FileSpec::size(1))),
            )
            .build_temp()
            .unwrap();
        let c = opened(ctx, t.path());
        c.load(
            Location::Local(t.path().join("sub")),
            ofm_app::panel::LoadOpts::default(),
        );
        pump_until(ctx, "sub listed", || {
            c.state() == LoadState::Idle && has(&c, "in.txt")
        });
        std::fs::write(t.path().join("sub").join("n2.txt"), b"x").unwrap();
        pump_until(ctx, "sub watched", || has(&c, "n2.txt"));
        // The old directory is no longer watched.
        std::fs::write(t.path().join("elsewhere.txt"), b"x").unwrap();
        let end = Instant::now() + Duration::from_millis(500);
        while Instant::now() < end {
            ctx.iteration(false);
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!has(&c, "elsewhere.txt"));
        c.shutdown();
    });
}

#[test]
fn deleting_the_watched_directory_goes_to_the_parent() {
    with_loop(|ctx| {
        let t = TreeSpec::new()
            .dir(
                "victim",
                ofm_testkit::tree::DirSpec::new(TreeSpec::new().file("x", FileSpec::size(1))),
            )
            .file("keep", FileSpec::size(1))
            .build_temp()
            .unwrap();
        let victim = t.path().join("victim");
        let c = opened(ctx, &victim);
        std::fs::remove_dir_all(&victim).unwrap();
        pump_until(ctx, "moved to parent", || {
            c.location() == Location::Local(t.path().to_path_buf())
                && c.state() == LoadState::Idle
                && has(&c, "keep")
        });
        c.shutdown();
    });
}

#[test]
fn event_flood_collapses_into_a_rescan() {
    with_loop(|ctx| {
        let t = fixture();
        let c = opened(ctx, t.path());
        for i in 0..2300 {
            std::fs::write(t.path().join(format!("f{i:04}")), b"").unwrap();
        }
        pump_until(ctx, "all files visible", || c.model().total_len() >= 2302);
        c.shutdown();
    });
}

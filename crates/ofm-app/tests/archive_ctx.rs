//! T6.3 in-process tests of the archive UI (needs a display; skips without one). They drive the
//! app through `AppContext::execute` and use a *preloaded index* (`ArchiveVfs::preload_index`)
//! over a file that only has a zip magic, so everything here is independent of the format
//! backends: opening by magic bytes, navigating, the cursor on the archive after leaving it,
//! read-only refusals, the error path, and the password prompt (dialog, retry line, cancel).
//! Real archives (extract, F3, passwords end to end) are in `archive_real.rs`.
//!
//! One `#[test]`: GTK is initialised once on the main test thread.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixture setup on the test thread
)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::compare::CompareState;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dialogs::password::{Origin, PasswordUi, abort_pending};
use ofm_app::navigation::modal::ModalStack;
use ofm_app::panel::archive::{ArchiveState, state as archive_state};
use ofm_app::panel::{LEFT, LoadState, RIGHT};
use ofm_app::window::MainWindow;
use ofm_config::ConfigPaths;
use ofm_core::{Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec};
use ofm_vfs::archive::{
    ArchiveEntryKind, ArchiveFormat, ArchiveLimits, IndexBuilder, PasswordProvider, RawEntry,
};

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

fn settle(ms: u64) {
    let ctx = glib::MainContext::default();
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn loaded(ctx: &AppContext, side: usize) -> bool {
    let c = ctx.host(side).active();
    c.state() == LoadState::Idle && c.model().total_len() > 0
}

fn names(ctx: &AppContext, side: usize) -> Vec<String> {
    let c = ctx.host(side).active();
    let m = c.model();
    (0..m.visible_len())
        .filter_map(|i| m.visible(i))
        .map(|e| e.name.to_string_lossy().into_owned())
        .collect()
}

fn cursor_name(ctx: &AppContext, side: usize) -> Option<String> {
    let c = ctx.host(side).active();
    let m = c.model();
    m.cursor_entry()
        .map(|e| e.name.to_string_lossy().into_owned())
}

fn put_cursor(ctx: &AppContext, side: usize, name: &str) {
    let c = ctx.host(side).active();
    assert!(
        c.model_mut().set_cursor_name(&OsString::from(name)),
        "no entry {name}"
    );
}

fn archive_loc(archive: &Path, inner: &str) -> Location {
    Location::Archive {
        archive: archive.to_owned(),
        inner: inner.into(),
    }
}

fn modals(ctx: &AppContext) -> usize {
    ctx.ext.get::<ModalStack>().unwrap().depth()
}

fn last_message(ctx: &AppContext) -> Option<String> {
    ctx.ext.get::<CompareState>().unwrap().last_message()
}

fn close_dialogs(ctx: &Rc<AppContext>) {
    while modals(ctx) > 0 {
        ctx.execute(cmd::DIALOG_CANCEL);
        settle(30);
    }
}

fn zip_index(archive: &Path) -> Arc<ofm_vfs::archive::ArchiveIndex> {
    let mut b = IndexBuilder::new(archive, ArchiveFormat::Zip, ArchiveLimits::default());
    let mut f = RawEntry::new("d/f.txt", ArchiveEntryKind::File);
    f.size = 5;
    f.mtime_ns = Some(1_700_000_000_000_000_000);
    f.mode = Some(0o640);
    b.push(f).unwrap();
    let mut g = RawEntry::new("top.txt", ArchiveEntryKind::File);
    g.size = 7;
    b.push(g).unwrap();
    let mut s = RawEntry::new("d/secret.bin", ArchiveEntryKind::File);
    s.size = 100;
    s.encrypted = true;
    b.push(s).unwrap();
    Arc::new(b.finish().unwrap())
}

struct Fixture {
    ctx: Rc<AppContext>,
    _win: Rc<MainWindow>,
    left: PathBuf,
    right: PathBuf,
    _dirs: (
        ofm_testkit::temp::TempDir,
        ofm_testkit::temp::TempDir,
        ofm_testkit::temp::TempDir,
    ),
}

fn fixture() -> Option<Fixture> {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return None;
    }
    let _ = adw::init();
    // `fake.zip` has the zip magic only: its index is preloaded. `plain.txt` is no archive.
    let left = TreeSpec::new()
        .file(
            "fake.zip",
            FileSpec::content(b"PK\x03\x04 not really a zip"),
        )
        .file("plain.txt", FileSpec::content(b"hello"))
        .file(
            "broken.zip",
            FileSpec::content(b"PK\x03\x04 stub backend fails"),
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
        .application_id("io.github.mscoder.OfmArchiveCtxTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE)
        .expect("register app");
    let win = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    pump_until("panels loaded", || {
        loaded(&ctx, LEFT) && loaded(&ctx, RIGHT)
    });
    let (l, r) = (left.path().to_owned(), right.path().to_owned());
    Some(Fixture {
        ctx,
        _win: win,
        left: l,
        right: r,
        _dirs: (left, right, cfg),
    })
}

#[test]
fn archive_ui_in_process() {
    let Some(fx) = fixture() else { return };
    let ctx = &fx.ctx;
    let fake = fx.left.join("fake.zip");
    ctx.services
        .archives
        .vfs()
        .preload_index(&fake, zip_index(&fake));

    navigation_and_cursor(&fx, &fake);
    read_only_refusals(&fx, &fake);
    broken_archive_leaves_the_panel(&fx);
    password_prompt(&fx);
}

/// Enter on an archive (by magic, not extension) opens it as a folder; `..`/Backspace-style
/// go-up at the root returns to the folder with the cursor ON the archive file.
fn navigation_and_cursor(fx: &Fixture, fake: &Path) {
    let ctx = &fx.ctx;
    let left = ctx.host(LEFT).active();
    put_cursor(ctx, LEFT, "fake.zip");
    ctx.execute(cmd::PANEL_OPEN);
    pump_until("archive opened", || {
        left.location() == archive_loc(fake, "") && loaded(ctx, LEFT)
    });
    assert_eq!(names(ctx, LEFT), ["..", "d", "top.txt"]);
    {
        let m = left.model();
        let top = (0..m.visible_len())
            .filter_map(|i| m.visible(i))
            .find(|e| e.name == "top.txt")
            .unwrap();
        assert_eq!(top.size, 7, "stored size is shown");
    }

    // Into a folder and back up: the cursor lands on the folder we left.
    put_cursor(ctx, LEFT, "d");
    ctx.execute(cmd::PANEL_OPEN);
    pump_until("inside d", || {
        left.location() == archive_loc(fake, "d") && loaded(ctx, LEFT)
    });
    assert_eq!(names(ctx, LEFT), ["..", "f.txt", "secret.bin"]);
    {
        // Stored mode/mtime are shown; the encrypted entry carries the lock flag.
        let m = left.model();
        let find = |n: &str| {
            (0..m.visible_len())
                .filter_map(|i| m.visible(i))
                .find(|e| e.name == n)
                .unwrap()
                .clone()
        };
        let (f, secret) = (find("f.txt"), find("secret.bin"));
        assert_eq!(f.mode & 0o7777, 0o640);
        assert_eq!(f.mtime_ns, 1_700_000_000_000_000_000);
        assert_eq!(f.mode & ofm_app::panel::archive::ARCHIVE_ENCRYPTED, 0);
        assert_ne!(secret.mode & ofm_app::panel::archive::ARCHIVE_ENCRYPTED, 0);
    }
    ctx.execute(cmd::PANEL_GO_UP);
    pump_until("back at root", || {
        left.location() == archive_loc(fake, "") && loaded(ctx, LEFT)
    });
    assert_eq!(cursor_name(ctx, LEFT).as_deref(), Some("d"));

    // `..` row at the root leaves the archive; the cursor is on the archive file.
    put_cursor(ctx, LEFT, "..");
    ctx.execute(cmd::PANEL_OPEN);
    pump_until("left the archive", || {
        left.location() == Location::Local(fx.left.clone()) && loaded(ctx, LEFT)
    });
    assert_eq!(cursor_name(ctx, LEFT).as_deref(), Some("fake.zip"));

    // A session saved while inside an archive restores to the archive's folder, never inside it.
    {
        put_cursor(ctx, LEFT, "fake.zip");
        ctx.execute(cmd::PANEL_OPEN);
        pump_until("inside again", || {
            matches!(left.location(), Location::Archive { .. }) && loaded(ctx, LEFT)
        });
        let saved = ofm_app::navigation::session::capture(ctx.host(LEFT));
        assert_eq!(saved.tabs[0].path_buf().as_deref(), Some(fx.left.as_path()));
        ctx.execute(cmd::PANEL_GO_UP);
        pump_until("out again", || {
            left.location() == Location::Local(fx.left.clone()) && loaded(ctx, LEFT)
        });
    }

    // History knows archive locations: back goes into the archive again.
    ctx.execute(cmd::PANEL_HISTORY_BACK);
    pump_until("history back into the archive", || {
        matches!(left.location(), Location::Archive { .. }) && loaded(ctx, LEFT)
    });
    // Ctrl+PgDn (enter_dir) on a folder inside works like Enter.
    put_cursor(ctx, LEFT, "d");
    ctx.execute(cmd::PANEL_ENTER_DIR);
    pump_until("enter_dir", || left.location() == archive_loc(fake, "d"));
    // Back to the real folder for the next scenarios (go up twice).
    ctx.execute(cmd::PANEL_GO_UP);
    pump_until("up 1", || left.location() == archive_loc(fake, ""));
    ctx.execute(cmd::PANEL_GO_UP);
    pump_until("up 2", || {
        left.location() == Location::Local(fx.left.clone()) && loaded(ctx, LEFT)
    });
}

/// F6/F7/F8/... in an archive panel and copying INTO an archive: a message, no dialog, no job.
fn read_only_refusals(fx: &Fixture, fake: &Path) {
    let ctx = &fx.ctx;
    let left = ctx.host(LEFT).active();
    put_cursor(ctx, LEFT, "fake.zip");
    ctx.execute(cmd::PANEL_OPEN);
    pump_until("opened again", || {
        left.location() == archive_loc(fake, "") && loaded(ctx, LEFT)
    });
    put_cursor(ctx, LEFT, "top.txt");
    for c in [
        cmd::FILE_RENAME,
        cmd::FILE_MKDIR,
        cmd::FILE_DELETE,
        cmd::FILE_DELETE_PERMANENT,
        cmd::FILE_LINK,
        cmd::FILE_PROPERTIES,
        cmd::FILE_NEW_FILE,
        cmd::FILE_CLIPBOARD_PASTE,
        cmd::FILE_CLIPBOARD_CUT,
        cmd::FILE_BATCH_RENAME,
        cmd::FILE_BATCH_RENAME_UNDO,
    ] {
        // A fresh message per command, so a stale one cannot make the check pass.
        ofm_app::compare::message::show(ctx, "-");
        ctx.execute(c);
        assert_eq!(modals(ctx), 0, "{c} must not open a dialog");
        assert_eq!(
            last_message(ctx).as_deref(),
            Some("Archives are read-only"),
            "{c}"
        );
    }
    settle(100);
    assert_eq!(
        left.location(),
        archive_loc(fake, ""),
        "still in the archive"
    );
    assert!(
        !ctx.ext
            .get::<ofm_app::jobs::JobsController>()
            .unwrap()
            .has_unfinished(),
        "no job was started"
    );

    // Copy INTO an archive: the OTHER panel shows the archive.
    ctx.execute(cmd::PANEL_GO_UP);
    pump_until("left the archive", || {
        left.location() == Location::Local(fx.left.clone()) && loaded(ctx, LEFT)
    });
    let right = ctx.host(RIGHT).active();
    right.load(archive_loc(fake, ""), ofm_app::panel::LoadOpts::default());
    pump_until("right shows the archive", || loaded(ctx, RIGHT));
    ctx.execute(cmd::PANEL_SWITCH);
    ctx.execute(cmd::PANEL_SWITCH); // back to the left panel (a local folder)
    assert_eq!(ctx.active_side(), LEFT);
    put_cursor(ctx, LEFT, "plain.txt");
    ctx.execute(cmd::FILE_COPY);
    assert_eq!(modals(ctx), 0, "copy into an archive opens no dialog");
    assert_eq!(last_message(ctx).as_deref(), Some("Archives are read-only"));
    ctx.execute(cmd::FILE_MOVE);
    assert_eq!(modals(ctx), 0);
    // Put the right panel back.
    right.load(
        Location::Local(fx.right.clone()),
        ofm_app::panel::LoadOpts::default(),
    );
    pump_until("right restored", || loaded(ctx, RIGHT));
}

/// An archive that cannot be indexed (here: the stub backend) shows a message and the panel stays.
/// A file that is not an archive does not touch the panel either (it goes to the default opener,
/// which this test does not trigger: `plain.txt` is entered with Ctrl+PgDn, which never runs it).
fn broken_archive_leaves_the_panel(fx: &Fixture) {
    let ctx = &fx.ctx;
    let left = ctx.host(LEFT).active();
    put_cursor(ctx, LEFT, "broken.zip");
    ctx.execute(cmd::PANEL_ENTER_DIR);
    let st: Rc<ArchiveState> = archive_state(ctx).unwrap();
    pump_until("error shown", || {
        modals(ctx) > 0 || st.last_error().is_some()
    });
    pump_until("error dialog", || modals(ctx) > 0);
    assert!(st.last_error().is_some());
    assert_eq!(
        left.location(),
        Location::Local(fx.left.clone()),
        "the panel stays where it was"
    );
    close_dialogs(ctx);

    // Ctrl+PgDn on a plain file does nothing at all.
    put_cursor(ctx, LEFT, "plain.txt");
    ctx.execute(cmd::PANEL_ENTER_DIR);
    settle(300);
    assert_eq!(modals(ctx), 0);
    assert_eq!(left.location(), Location::Local(fx.left.clone()));
    assert!(!st.busy());

    // Esc (panel.cancel) cancels a running open / extract-for-view.
    let token = ofm_core::CancelToken::new();
    let serial = st.begin(&token);
    assert!(st.busy() && st.is_current(serial));
    ctx.execute(cmd::PANEL_CANCEL);
    assert!(token.is_cancelled() && !st.busy());
}

/// The password dialog end to end (no real archive): a worker asks, the dialog shows, a wrong
/// answer re-asks with an error line, Esc cancels, and a cancelled job closes its dialog.
fn password_prompt(fx: &Fixture) {
    let ctx = &fx.ctx;
    let source = ctx.services.archives.passwords().clone();
    let ui = ctx.ext.get::<PasswordUi>().unwrap();
    let archive = fx.left.join("enc.zip");

    // OK with a typed password.
    let (a, s) = (archive.clone(), source.clone());
    let worker = std::thread::spawn(move || s.password(&a, false));
    pump_until("dialog open", || ui.open_count() == 1);
    assert_eq!(modals(ctx), 1);
    assert!(ui.top_error().is_none(), "no error on the first ask");
    assert!(ui.type_into_top("s3cret"));
    ctx.execute(cmd::DIALOG_OK);
    pump_until("worker answered", || worker.is_finished());
    assert_eq!(worker.join().unwrap().as_deref(), Some("s3cret"));
    settle(50);
    assert_eq!(ui.open_count(), 0);

    // Retry: the dialog shows the error line; Esc cancels (worker gets None).
    let (a, s) = (archive.clone(), source.clone());
    let worker = std::thread::spawn(move || s.password(&a, true));
    pump_until("retry dialog", || ui.open_count() == 1);
    assert!(
        ui.top_error().is_some_and(|e| e.contains("Wrong password")),
        "{:?}",
        ui.top_error()
    );
    ctx.execute(cmd::DIALOG_CANCEL);
    pump_until("worker cancelled", || worker.is_finished());
    assert_eq!(worker.join().unwrap(), None);
    assert_eq!(modals(ctx), 0);

    // Dismissing a browse prompt cancels the operation, so the backend reports `Cancelled`
    // and not "a password is required".
    let token = ofm_core::CancelToken::new();
    let (a, s, t) = (archive.clone(), source.clone(), token.clone());
    let worker = std::thread::spawn(move || {
        let _scope = ofm_app::dialogs::password::scope(&t);
        s.password(&a, false)
    });
    pump_until("browse dialog", || ui.open_count() == 1);
    ctx.execute(cmd::DIALOG_CANCEL);
    pump_until("browse worker cancelled", || worker.is_finished());
    assert_eq!(worker.join().unwrap(), None);
    assert!(token.is_cancelled());

    // A cancelled extract job (no scope: Origin::Job) closes the dialog and frees the worker.
    let (a, s) = (archive.clone(), source.clone());
    let worker = std::thread::spawn(move || s.password(&a, false));
    pump_until("job dialog", || ui.open_count() == 1);
    abort_pending(Origin::Job);
    pump_until("dialog closed by cancel", || {
        ui.open_count() == 0 && worker.is_finished()
    });
    assert_eq!(worker.join().unwrap(), None);
    assert_eq!(modals(ctx), 0);
}

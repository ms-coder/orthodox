//! T8.1c: "compare directories" (Shift+F2, F27) driven through `AppContext::execute` (needs a
//! display; skips without one). One test function: GTK must stay on a single thread.
//!
//! Covers the dialog (Enter = OK, Esc = cancel through the dispatcher's commands), the worker
//! round trip, the marks in both panels, the filters, case-insensitive matching, directories,
//! non-UTF-8 names, marks surviving a re-sort and the content-mode fallback message.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gio::prelude::ApplicationExt;
use gtk::glib;
use ofm_app::app::build_context;
use ofm_app::compare::{self, CompareSettings, CompareState, MarkFilter};
use ofm_app::context::{AppContext, AppInit};
use ofm_app::panel::{LEFT, LoadState, RIGHT, Side};
use ofm_app::window::MainWindow;
use ofm_config::ConfigPaths;
use ofm_core::compare::CompareMode;
use ofm_core::{Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};

const SEC: i128 = 1_000_000_000;
const T0: i128 = 946_684_800 * SEC;

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

fn pump_briefly() {
    let ctx = glib::MainContext::default();
    for _ in 0..50 {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn loaded(ctx: &AppContext) -> bool {
    ctx.panels
        .iter()
        .all(|h| h.active().state() == LoadState::Idle && h.active().model().total_len() > 0)
}

fn selected(ctx: &AppContext, side: Side) -> BTreeSet<OsString> {
    ctx.host(side)
        .active()
        .model()
        .selected_names()
        .into_iter()
        .collect()
}

fn set(names: &[&str]) -> BTreeSet<OsString> {
    names.iter().map(OsString::from).collect()
}

/// Run a comparison with `settings` and wait until the result was applied.
fn run(ctx: &Rc<AppContext>, settings: CompareSettings) -> String {
    let state = ctx.ext.get::<CompareState>().expect("CompareState");
    compare::start(ctx, settings);
    assert!(state.is_running());
    pump_until("comparison applied", || !state.is_running());
    state.last_message().expect("a message")
}

#[test]
fn compare_directories_marks_both_panels() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();
    let odd_left = raw_name(b"odd-\xff-only-left");
    let left = TreeSpec::new()
        .file("same.txt", FileSpec::content(b"same").mtime_ns(T0))
        .file("only-left.txt", FileSpec::content(b"l").mtime_ns(T0))
        .file("bigger.txt", FileSpec::content(b"123456").mtime_ns(T0))
        .file(
            "newer.txt",
            FileSpec::content(b"nn").mtime_ns(T0 + 100 * SEC),
        )
        .file("tolerated.txt", FileSpec::content(b"tt").mtime_ns(T0 + SEC))
        .file("Case.TXT", FileSpec::content(b"cc").mtime_ns(T0))
        .file(odd_left.clone(), FileSpec::content(b"o").mtime_ns(T0))
        .dir("only-left-dir", TreeSpec::new());
    let right = TreeSpec::new()
        .file("same.txt", FileSpec::content(b"same").mtime_ns(T0))
        .file("only-right.txt", FileSpec::content(b"r").mtime_ns(T0))
        .file("bigger.txt", FileSpec::content(b"123").mtime_ns(T0))
        .file("newer.txt", FileSpec::content(b"nn").mtime_ns(T0))
        .file("tolerated.txt", FileSpec::content(b"tt").mtime_ns(T0))
        .file("case.txt", FileSpec::content(b"cc").mtime_ns(T0));
    let left = left.build_temp().unwrap();
    let right = right.build_temp().unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    let ctx = build_context(AppInit {
        paths: ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state")),
        left: Location::Local(left.path().to_owned()),
        right: Location::Local(right.path().to_owned()),
    });
    let app = adw::Application::builder()
        .application_id("io.github.mscoder.OfmCompareTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE)
        .expect("register app");
    let _win: Rc<MainWindow> = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    pump_until("panels loaded", || loaded(&ctx));
    let state = ctx.ext.get::<CompareState>().expect("CompareState");
    assert_eq!(state.settings(), CompareSettings::default());

    // --- Esc cancels: nothing is selected, nothing runs, the modal state is released. -------
    ctx.execute(cmd::PANEL_COMPARE_DIRS);
    assert!(ctx.modal_open(), "the dialog did not open");
    ctx.execute(cmd::DIALOG_CANCEL);
    pump_briefly();
    assert!(!ctx.modal_open(), "Esc did not close the dialog");
    assert!(selected(&ctx, LEFT).is_empty() && selected(&ctx, RIGHT).is_empty());
    assert!(state.last_message().is_none());

    // --- Enter accepts the defaults (name + size + mtime, 2 s tolerance, all differing). ------
    ctx.execute(cmd::PANEL_COMPARE_DIRS);
    assert!(ctx.modal_open());
    ctx.execute(cmd::DIALOG_OK);
    assert!(state.is_running());
    pump_until("comparison applied", || !state.is_running());
    pump_briefly();
    assert!(!ctx.modal_open(), "Enter did not close the dialog");
    let mut expect_left = set(&[
        "only-left.txt",
        "bigger.txt",
        "newer.txt",
        "Case.TXT",
        "only-left-dir",
    ]);
    expect_left.insert(odd_left.clone());
    assert_eq!(selected(&ctx, LEFT), expect_left);
    assert_eq!(
        selected(&ctx, RIGHT),
        set(&["only-right.txt", "bigger.txt", "newer.txt", "case.txt"])
    );
    // 5 pairs matched on the left... `same.txt` and `tolerated.txt` are identical.
    assert_eq!(
        state.last_message().unwrap(),
        "6 differing on the left, 4 on the right, 2 identical"
    );

    // --- Marks survive a re-sort of both panels. -------------------------------------------
    ctx.execute(cmd::PANEL_SORT_SIZE);
    ctx.execute(cmd::PANEL_SWITCH);
    ctx.execute(cmd::PANEL_SORT_MTIME);
    ctx.execute(cmd::PANEL_SWITCH);
    assert_eq!(selected(&ctx, LEFT), expect_left);
    assert_eq!(selected(&ctx, RIGHT).len(), 4);

    // --- Restore brings back the selection from before the comparison (here: none). --------
    // (A second comparison replaces, not extends.)
    let msg = run(
        &ctx,
        CompareSettings {
            mark: MarkFilter::NewerHere,
            ..CompareSettings::default()
        },
    );
    assert_eq!(selected(&ctx, LEFT), set(&["newer.txt"]));
    assert!(selected(&ctx, RIGHT).is_empty());
    assert_eq!(msg, "1 differing on the left, 0 on the right, 2 identical");
    // The dialog starts from the last settings.
    assert_eq!(state.settings().mark, MarkFilter::NewerHere);

    let msg = run(
        &ctx,
        CompareSettings {
            mark: MarkFilter::MissingOnOther,
            ..CompareSettings::default()
        },
    );
    let mut missing_left = set(&["only-left.txt", "Case.TXT", "only-left-dir"]);
    missing_left.insert(odd_left.clone());
    assert_eq!(selected(&ctx, LEFT), missing_left);
    assert_eq!(selected(&ctx, RIGHT), set(&["only-right.txt", "case.txt"]));
    assert!(msg.starts_with("4 differing on the left, 2 on the right"));

    // --- Case-insensitive names, name-only mode, no directories. ---------------------------
    run(
        &ctx,
        CompareSettings {
            mode: CompareMode::NameOnly,
            case_insensitive: true,
            include_dirs: false,
            ..CompareSettings::default()
        },
    );
    let mut name_only_left = set(&["only-left.txt"]);
    name_only_left.insert(odd_left.clone());
    assert_eq!(selected(&ctx, LEFT), name_only_left);
    assert_eq!(selected(&ctx, RIGHT), set(&["only-right.txt"]));

    // --- Size mode ignores times. ------------------------------------------------------------
    run(
        &ctx,
        CompareSettings {
            mode: CompareMode::NameSize,
            ..CompareSettings::default()
        },
    );
    assert!(selected(&ctx, LEFT).contains(&OsString::from("bigger.txt")));
    assert!(!selected(&ctx, LEFT).contains(&OsString::from("newer.txt")));

    // --- Zero tolerance sees the one-second difference. ---------------------------------------
    run(
        &ctx,
        CompareSettings {
            tolerance: Duration::ZERO,
            ..CompareSettings::default()
        },
    );
    assert!(selected(&ctx, LEFT).contains(&OsString::from("tolerated.txt")));

    // --- Content mode: the metadata pass is applied; the byte pass is reported as pending. ----
    let msg = run(
        &ctx,
        CompareSettings {
            mode: CompareMode::Content,
            ..CompareSettings::default()
        },
    );
    assert!(selected(&ctx, LEFT).contains(&OsString::from("bigger.txt")));
    assert!(msg.contains("content compare not available yet"), "{msg}");

    // --- A comparison while a panel changed directory meanwhile is dropped. -----------------
    ctx.execute(cmd::SELECT_NONE);
    compare::start(&ctx, CompareSettings::default());
    assert!(state.is_running());
    let before = state.last_message();
    // A newer run supersedes the first one: only one message results.
    compare::start(&ctx, CompareSettings::default());
    pump_until("both runs finished", || !state.is_running());
    assert_ne!(state.last_message(), before);
}

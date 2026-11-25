//! T5.2a: the viewer commands driven through `AppContext::execute` (the `CommandDispatcher`
//! path), plus keymap resolution of every `viewer.*` command in the three presets (headless).
//! The window scenario needs a display (Xvfb in `scripts/check.sh`) and skips itself without one;
//! real key events are covered by `viewer_real.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dispatch::{CommandRegistry, ExecResult, WindowKind, resolve};
use ofm_app::panel::LoadState;
use ofm_app::viewer::{self, Dir, GotoTarget, SearchSpec, ViewerHost};
use ofm_app::window::MainWindow;
use ofm_config::{ConfigPaths, Keymap, Preset};
use ofm_core::{CommandId, KeyChord, KeyContext, Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec};
use ofm_viewer::{Needle, SearchOptions};

const VIEWER_COMMANDS: &[CommandId] = &[
    cmd::VIEWER_CLOSE,
    cmd::VIEWER_TOGGLE_HEX,
    cmd::VIEWER_ENCODING,
    cmd::VIEWER_SEARCH,
    cmd::VIEWER_SEARCH_NEXT,
    cmd::VIEWER_SEARCH_PREV,
    cmd::VIEWER_WRAP,
    cmd::VIEWER_GOTO,
    cmd::VIEWER_LINE_UP,
    cmd::VIEWER_LINE_DOWN,
    cmd::VIEWER_PAGE_UP,
    cmd::VIEWER_PAGE_DOWN,
    cmd::VIEWER_TOP,
    cmd::VIEWER_BOTTOM,
    cmd::VIEWER_SCROLL_LEFT,
    cmd::VIEWER_SCROLL_RIGHT,
    cmd::VIEWER_NEXT_FILE,
    cmd::VIEWER_PREV_FILE,
];

#[test]
fn every_viewer_command_has_a_handler_and_a_key_in_all_presets() {
    let mut reg = CommandRegistry::new();
    ofm_app::hooks::install_commands(&mut reg);
    assert!(reg.duplicates().is_empty(), "{:?}", reg.duplicates());
    assert!(reg.has(cmd::FILE_VIEW));
    let tmp = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(tmp.path().join("c"), tmp.path().join("s"));
    for preset in [Preset::Far, Preset::Mc, Preset::Tc] {
        let km = Keymap::load(&paths, preset, None).0;
        for c in VIEWER_COMMANDS {
            assert!(reg.has(*c), "no handler for {c}");
            assert!(
                km.chords_for(*c)
                    .iter()
                    .any(|(ctx, _)| *ctx == KeyContext::Viewer),
                "{preset:?}: {c} has no key in the viewer section"
            );
        }
        for (chord, command) in [
            ("F4", cmd::VIEWER_TOGGLE_HEX),
            ("F7", cmd::VIEWER_SEARCH),
            ("Esc", cmd::VIEWER_CLOSE),
        ] {
            let chord: KeyChord = chord.parse().unwrap();
            assert_eq!(
                km.resolve_in(KeyContext::Viewer, chord),
                Some(command),
                "{preset:?}: {chord}"
            );
        }
    }
}

fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let main = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "timed out waiting for: {what}"
        );
        main.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle(ctx: &AppContext) {
    pump_until("panels loaded", || {
        ctx.panels
            .iter()
            .all(|h| h.tabs().iter().all(|t| t.state() != LoadState::Loading))
    });
}

fn execute(ctx: &Rc<AppContext>, c: CommandId) {
    assert_eq!(ctx.execute(c), ExecResult::Handled, "{c}");
}

#[test]
fn viewer_window_scenario() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();
    let mut body = String::new();
    for i in 0..400 {
        body.push_str(&format!("line {i}\n"));
        if i == 200 {
            body.push_str("a distinctive NEEDLE line\n");
        }
    }
    let left = TreeSpec::new()
        .dir("adir", TreeSpec::new())
        .file("a.txt", FileSpec::content(body.as_bytes()))
        .file("b.txt", FileSpec::content(b"second\n"))
        .build_temp()
        .unwrap();
    let right = TreeSpec::new().build_temp().unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state"));
    let ctx = build_context(AppInit {
        paths,
        left: Location::Local(left.path().to_owned()),
        right: Location::Local(right.path().to_owned()),
    });
    let app = adw::Application::builder()
        .application_id("io.github.mscoder.OfmViewerTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE).unwrap();
    let win = MainWindow::new(&app, &ctx);
    win.present();
    ctx.start_loading();
    settle(&ctx);
    let host = ctx.ext.get::<ViewerHost>().unwrap();
    let cursor_to = |name: &str| {
        assert!(
            ctx.active_controller()
                .mutate(|m| m.set_cursor_name(name.as_ref()))
        );
    };

    // F3 on a directory opens no viewer but starts the size calculation (F19, T8.11): the
    // calculated size lands in the entry (the empty dir totals 0 bytes, never the fs size).
    cursor_to("adir");
    let fs_size = ctx
        .active_controller()
        .model()
        .cursor_entry()
        .map(|e| e.size);
    execute(&ctx, cmd::FILE_VIEW);
    assert!(host.is_empty());
    pump_until("dir size calculated", || {
        ctx.active_controller()
            .model()
            .cursor_entry()
            .is_some_and(|e| Some(e.size) != fs_size && e.size == 0)
    });
    // Viewer commands without a viewer are harmless.
    execute(&ctx, cmd::VIEWER_TOGGLE_HEX);

    // F3 on a file opens a viewer window; the status line shows encoding, mode and line count.
    cursor_to("a.txt");
    execute(&ctx, cmd::FILE_VIEW);
    assert_eq!(host.len(), 1);
    let v = viewer::current(&ctx).unwrap();
    assert!(v.path().ends_with("a.txt"));
    pump_until("first page", || v.status_text().1.contains("Line 1 "));
    let (left_text, right_text) = v.status_text();
    assert!(left_text.contains("UTF-8 (auto)"), "{left_text}");
    pump_until("line count", || v.status_text().1.contains("/ 401"));
    assert!(right_text.contains("Offset 0"), "{right_text}");

    // Scrolling moves the position; End shows the last page (100%), Home the first again.
    execute(&ctx, cmd::VIEWER_PAGE_DOWN);
    pump_until("page down", || v.top() > 0);
    execute(&ctx, cmd::VIEWER_BOTTOM);
    pump_until("bottom", || v.status_text().1.ends_with("100%"));
    execute(&ctx, cmd::VIEWER_TOP);
    pump_until("top", || {
        v.top() == 0 && v.status_text().1.contains("Line 1 ")
    });

    // Hex mode and back.
    execute(&ctx, cmd::VIEWER_TOGGLE_HEX);
    assert!(v.is_hex());
    pump_until("hex status", || {
        let t = v.status_text();
        t.0.contains("Hex") && !t.1.contains("Line")
    });
    execute(&ctx, cmd::VIEWER_TOGGLE_HEX);
    pump_until("text status", || v.status_text().1.contains("Line 1 "));
    execute(&ctx, cmd::VIEWER_WRAP);
    assert!(v.is_wrap());
    pump_until("wrap status", || v.status_text().0.contains("Wrap"));

    // Go to line 100 (1-based) and to an offset.
    v.goto(GotoTarget::Line(100));
    pump_until("goto line", || v.status_text().1.contains("Line 100 "));
    // Offset 16 is inside "line 2" (bytes 14..21): the view snaps to the start of that row.
    v.goto(GotoTarget::Offset(16));
    pump_until("goto offset", || v.top() == 14);

    // Search: the match is highlighted at its offset; next wraps around the single hit.
    let needle_at = body.find("NEEDLE").unwrap() as u64;
    let spec = SearchSpec {
        needle: Needle::Text("needle".into()),
        opts: SearchOptions {
            case_sensitive: false,
            wrap: true,
        },
    };
    v.search(spec, Dir::Forward);
    pump_until("search hit", || v.hl().is_some());
    assert_eq!(v.hl().unwrap().start, needle_at);
    pump_until("page at the hit", || {
        v.top() <= needle_at && v.top() + 200 > needle_at
    });
    v.goto(GotoTarget::Offset(0));
    pump_until("top again", || v.top() == 0);
    execute(&ctx, cmd::VIEWER_SEARCH_NEXT);
    pump_until("next search wrapped to the same hit", || {
        v.status_text().1.contains("wrapped") || v.top() > 0
    });

    // Next / previous file in the panel list.
    execute(&ctx, cmd::VIEWER_NEXT_FILE);
    assert!(v.path().ends_with("b.txt"));
    pump_until("second file", || v.status_text().1.contains("/ 1"));
    execute(&ctx, cmd::VIEWER_PREV_FILE);
    assert!(v.path().ends_with("a.txt"));
    execute(&ctx, cmd::VIEWER_PREV_FILE); // already first: stays
    assert!(v.path().ends_with("a.txt"));

    // Key resolution of a viewer window honours open dialogs (Esc closes the dialog, not the viewer).
    let esc: KeyChord = "Esc".parse().unwrap();
    assert_eq!(
        resolve(&ctx, WindowKind::Viewer, esc),
        Some(cmd::VIEWER_CLOSE)
    );
    execute(&ctx, cmd::VIEWER_ENCODING); // opens the encoding dialog over the viewer
    assert!(ctx.modal_open());
    assert_eq!(
        resolve(&ctx, WindowKind::Viewer, esc),
        Some(cmd::DIALOG_CANCEL)
    );
    execute(&ctx, cmd::DIALOG_CANCEL);
    assert!(!ctx.modal_open());
    assert_eq!(host.len(), 1);

    // Close.
    execute(&ctx, cmd::VIEWER_CLOSE);
    pump_until("viewer closed", || host.is_empty());
}

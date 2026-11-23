//! T7.3: the Find window and panelize driven in-process with a FAKE engine (needs a display; skips
//! without one). Covers streaming + throttling, cancel, window close, go to, panelize into a list
//! panel and the file-operation source mapping. The real engine is covered by `find_real.rs`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixtures are read on the test thread
)]

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dialogs::find::FindWindow;
use ofm_app::jobs::targets::Targets;
use ofm_app::panel::{LEFT, LoadState};
use ofm_app::search::run::{RunEvent, RunState, Starter};
use ofm_app::search::{self, state};
use ofm_app::window::MainWindow;
use ofm_config::ConfigPaths;
use ofm_core::{CancelToken, Entry, EntryKind, Location, cmd};
use ofm_search::{Hit, SearchEvent, SearchHandle, SearchProgress, SearchSummary, SearchWarning};
use ofm_testkit::tree::{FileSpec, TreeSpec};

fn pump_until(what: &str, mut done: impl FnMut() -> bool) {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "timed out: {what}"
        );
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct Env {
    ctx: Rc<AppContext>,
    _win: Rc<MainWindow>,
    left: tempfile_dir::Dir,
    _right: tempfile_dir::Dir,
    _cfg: tempfile_dir::Dir,
}

/// `ofm_testkit` temp dirs under a short alias.
mod tempfile_dir {
    pub type Dir = ofm_testkit::temp::TempDir;
}

fn setup(app_id: &str) -> Option<Env> {
    setup_themed(app_id, None)
}

fn setup_themed(app_id: &str, theme: Option<&str>) -> Option<Env> {
    // SAFETY: set before GTK starts and before any other thread exists (single test).
    unsafe {
        std::env::set_var("GSK_RENDERER", "cairo");
        std::env::set_var("GDK_BACKEND", "x11");
        std::env::remove_var("WAYLAND_DISPLAY");
    }
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return None;
    }
    let _ = adw::init();
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"alpha"))
        .file("b.txt", FileSpec::content(b"bravo"))
        .dir(
            "sub",
            TreeSpec::new()
                .file("c.txt", FileSpec::content(b"charlie"))
                .file("d.txt", FileSpec::content(b"delta")),
        )
        .build_temp()
        .unwrap();
    let right = TreeSpec::new()
        .file("x", FileSpec::content(b"x"))
        .build_temp()
        .unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    if let Some(t) = theme {
        std::fs::create_dir_all(cfg.path().join("config")).unwrap();
        std::fs::write(
            cfg.path().join("config/config.toml"),
            format!("[theme]\nname = \"{t}\"\n"),
        )
        .unwrap();
    }
    let ctx = build_context(AppInit {
        paths: ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state")),
        left: Location::Local(left.path().to_owned()),
        right: Location::Local(right.path().to_owned()),
    });
    let app = adw::Application::builder().application_id(app_id).build();
    app.register(gtk::gio::Cancellable::NONE)
        .expect("register app");
    let win = MainWindow::new(&app, &ctx);
    ctx.start_loading();
    pump_until("panels loaded", || {
        ctx.panels
            .iter()
            .all(|h| h.active().state() == LoadState::Idle && h.active().model().total_len() > 0)
    });
    Some(Env {
        ctx,
        _win: win,
        left,
        _right: right,
        _cfg: cfg,
    })
}

fn hit_for(path: &Path) -> Hit {
    let md = std::fs::symlink_metadata(path).unwrap();
    let mut e = Entry::new(
        path.file_name().unwrap(),
        if md.is_dir() {
            EntryKind::Dir
        } else {
            EntryKind::File
        },
    );
    e.size = md.len();
    e.real_path = Some(path.to_owned());
    Hit {
        location: Location::Local(path.to_owned()),
        entry: e,
        content: None,
    }
}

/// How the fake engine behaves.
struct Fake {
    batches: Vec<Vec<Hit>>,
    delay: Duration,
    warnings: usize,
    /// Keep going until cancelled after the batches are sent.
    hold: bool,
    ended: Arc<AtomicBool>,
}

fn starter(fake: Fake) -> Starter {
    let fake = Arc::new(std::sync::Mutex::new(Some(fake)));
    Arc::new(move |_query, cancel: CancelToken| {
        let fake = fake
            .lock()
            .unwrap()
            .take()
            .expect("fake engine started once");
        let (tx, rx) = async_channel::bounded(64);
        let token = cancel.clone();
        std::thread::spawn(move || {
            let mut sent = 0u64;
            for i in 0..fake.warnings {
                let _ = tx.send_blocking(SearchEvent::Warning(SearchWarning {
                    path: format!("/denied/{i}").into(),
                    message: "Permission denied".into(),
                }));
            }
            for batch in fake.batches {
                if token.is_cancelled() {
                    break;
                }
                sent += batch.len() as u64;
                if tx.send_blocking(SearchEvent::Found(batch)).is_err() {
                    fake.ended.store(true, Ordering::SeqCst);
                    return;
                }
                let _ = tx.send_blocking(SearchEvent::Progress(SearchProgress {
                    dirs_scanned: 1,
                    files_scanned: sent,
                    hits: sent,
                    current: Some("/fake".into()),
                }));
                if !fake.delay.is_zero() {
                    std::thread::sleep(fake.delay);
                }
            }
            while fake.hold && !token.is_cancelled() {
                if tx.is_closed() {
                    fake.ended.store(true, Ordering::SeqCst);
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send_blocking(SearchEvent::Done(SearchSummary {
                hits: sent,
                files_scanned: sent,
                dirs_scanned: 1,
                elapsed: Duration::from_millis(5),
                cancelled: token.is_cancelled(),
                truncated: false,
            }));
            fake.ended.store(true, Ordering::SeqCst);
        });
        Ok(SearchHandle::from_parts(rx, cancel))
    })
}

fn open_find(env: &Env, fake: Fake) -> Rc<FindWindow> {
    state(&env.ctx).unwrap().set_starter(starter(fake));
    env.ctx.execute(cmd::SEARCH_FIND);
    let w = state(&env.ctx)
        .unwrap()
        .window()
        .expect("find window opened");
    // Roots default to the active panel's directory.
    assert_eq!(w.resolved_roots(), [env.left.path().to_owned()]);
    w
}

fn finished(w: &FindWindow) -> bool {
    matches!(w.run().state(), RunState::Finished { .. })
}

fn three_hits(env: &Env) -> Vec<Vec<Hit>> {
    let l = env.left.path();
    vec![vec![
        hit_for(&l.join("a.txt")),
        hit_for(&l.join("sub/c.txt")),
        hit_for(&l.join("sub/d.txt")),
    ]]
}

fn alt_f7_opens_one_window_and_results_stream_in() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx1") else {
        return;
    };
    let w = open_find(
        &env,
        Fake {
            batches: three_hits(&env),
            delay: Duration::ZERO,
            warnings: 2,
            hold: false,
            ended: Arc::default(),
        },
    );
    // A second Alt+F7 reuses the window.
    env.ctx.execute(cmd::SEARCH_FIND);
    assert!(Rc::ptr_eq(&w, &state(&env.ctx).unwrap().window().unwrap()));

    w.start();
    pump_until("search finished", || finished(&w));
    assert_eq!(w.result_count(), 3);
    let stats = w.run().stats();
    assert_eq!((stats.found, stats.warnings), (3, 2));
    assert!(w.status_text().starts_with("Done:"), "{}", w.status_text());
    assert!(w.status_text().contains("3 found"));
    assert_eq!(w.run().warnings().len(), 2);
}

fn invalid_fields_are_reported_and_nothing_starts() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx2") else {
        return;
    };
    let w = open_find(
        &env,
        Fake {
            batches: vec![],
            delay: Duration::ZERO,
            warnings: 0,
            hold: false,
            ended: Arc::default(),
        },
    );
    let mut f = w.form();
    f.size_min = "lots".into();
    w.set_form(&f);
    w.start();
    assert_eq!(w.run().state(), RunState::Idle);
    f.size_min = String::new();
    f.after = "2027-02-30".into();
    w.set_form(&f);
    w.start();
    assert_eq!(w.run().state(), RunState::Idle);
}

fn updates_are_rate_limited_and_nothing_is_lost() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx3") else {
        return;
    };
    // 300k hits in batches of 256, as fast as the channel allows.
    let proto = hit_for(&env.left.path().join("a.txt"));
    let batches: Vec<Vec<Hit>> = (0..1172).map(|_| vec![proto.clone(); 256]).collect();
    let total: usize = batches.iter().map(Vec::len).sum();
    let w = open_find(
        &env,
        Fake {
            batches,
            delay: Duration::ZERO,
            warnings: 0,
            hold: false,
            ended: Arc::default(),
        },
    );
    let updates = Rc::new(std::cell::Cell::new(0u32));
    let u2 = Rc::clone(&updates);
    w.run().changed.connect(move |ev| {
        if *ev == RunEvent::Stats {
            u2.set(u2.get() + 1);
        }
    });
    let t0 = Instant::now();
    w.start();
    pump_until("search finished", || finished(&w));
    let secs = t0.elapsed().as_secs_f64();
    assert_eq!(w.result_count(), total);
    // <= 20 updates per second (+ start, the final flush and slack for timer jitter).
    let allowed = (secs * 20.0).ceil() as u32 + 5;
    assert!(
        updates.get() <= allowed,
        "{} UI updates in {secs:.2} s (allowed {allowed})",
        updates.get()
    );
}

fn stop_keeps_results_and_ends_the_engine() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx4") else {
        return;
    };
    let ended = Arc::new(AtomicBool::new(false));
    let w = open_find(
        &env,
        Fake {
            batches: three_hits(&env),
            delay: Duration::ZERO,
            warnings: 0,
            hold: true,
            ended: Arc::clone(&ended),
        },
    );
    w.start();
    pump_until("hits arrive", || w.result_count() == 3);
    assert!(w.run().is_active());
    // Esc while running = stop (the window stays open).
    w.on_cancel();
    pump_until("stopped", || finished(&w));
    assert!(matches!(
        w.run().state(),
        RunState::Finished {
            cancelled: true,
            ..
        }
    ));
    assert_eq!(w.result_count(), 3);
    assert!(w.status_text().starts_with("Stopped:"));
    pump_until("engine thread ended", || ended.load(Ordering::SeqCst));
    assert!(state(&env.ctx).unwrap().window().is_some());
    // Esc when idle = close.
    w.on_cancel();
    assert!(state(&env.ctx).unwrap().window().is_none());
}

fn closing_the_window_cancels_the_search() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx5") else {
        return;
    };
    let ended = Arc::new(AtomicBool::new(false));
    let w = open_find(
        &env,
        Fake {
            batches: three_hits(&env),
            delay: Duration::ZERO,
            warnings: 0,
            hold: true,
            ended: Arc::clone(&ended),
        },
    );
    w.start();
    pump_until("hits arrive", || w.result_count() == 3);
    w.window.close();
    assert!(state(&env.ctx).unwrap().window().is_none());
    drop(w);
    pump_until("engine thread ended after close", || {
        ended.load(Ordering::SeqCst)
    });
}

fn go_to_puts_the_cursor_on_the_file_in_the_active_panel() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx6") else {
        return;
    };
    let w = open_find(
        &env,
        Fake {
            batches: three_hits(&env),
            delay: Duration::ZERO,
            warnings: 0,
            hold: false,
            ended: Arc::default(),
        },
    );
    w.start();
    pump_until("finished", || finished(&w));
    w.select_rows(&[2]); // sub/d.txt
    let ctrl = env.ctx.host(LEFT).active();
    w.go_to_selected();
    // close_after is on by default.
    assert!(state(&env.ctx).unwrap().window().is_none());
    let want = Location::Local(env.left.path().join("sub"));
    pump_until("panel shows sub with the cursor on d.txt", || {
        ctrl.location() == want
            && ctrl.state() == LoadState::Idle
            && ctrl
                .model()
                .cursor_entry()
                .is_some_and(|e| e.name == "d.txt")
    });
}

fn panelize_shows_a_list_panel_and_file_ops_map_to_real_paths() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx7") else {
        return;
    };
    let w = open_find(
        &env,
        Fake {
            batches: three_hits(&env),
            delay: Duration::ZERO,
            warnings: 0,
            hold: false,
            ended: Arc::default(),
        },
    );
    w.start();
    pump_until("finished", || finished(&w));
    let ctrl = env.ctx.host(LEFT).active();
    let before = ctrl.location();
    w.select_rows(&[0]); // one selected row: panelize ALL
    w.panelize();
    pump_until("list panel loaded", || {
        matches!(ctrl.location(), Location::List { .. })
            && ctrl.state() == LoadState::Idle
            && ctrl.model().total_len() == 3
    });
    let Location::List { id } = ctrl.location() else {
        unreachable!()
    };
    assert_eq!(
        ctrl.services().lists.title(id).as_deref(),
        Some("Search results")
    );
    assert!(
        search::state(&env.ctx)
            .unwrap()
            .last_message()
            .unwrap()
            .starts_with("3 results")
    );
    // Rows are named relative to the search root.
    let names: Vec<String> = (0..ctrl.model().visible_len())
        .filter_map(|i| {
            ctrl.model()
                .visible(i)
                .map(|e| e.name.to_string_lossy().into_owned())
        })
        .collect();
    assert!(names.contains(&"sub/c.txt".to_owned()), "{names:?}");

    // File operations see the REAL paths of the marked rows.
    ctrl.mutate(|m| {
        m.set_cursor_name("sub/c.txt".as_ref());
        if let Some(at) = m.cursor() {
            m.toggle_select(at);
        }
    });
    let t = Targets::from_active(&env.ctx).expect("targets");
    let sources = t.sources();
    assert_eq!(
        sources,
        [Location::Local(env.left.path().join("sub/c.txt"))]
    );

    // Backspace (go up) returns to the directory the panel came from.
    env.ctx.execute(cmd::PANEL_GO_UP);
    pump_until("back at the directory", || {
        ctrl.location() == before && ctrl.state() == LoadState::Idle
    });
    // The unused list was dropped.
    assert_eq!(ctrl.services().lists.len(), 1); // still reachable through history (forward)
}

fn panelize_selected_rows_only_when_several_are_selected() {
    let Some(env) = setup("io.github.mscoder.OfmFindCtx8") else {
        return;
    };
    let w = open_find(
        &env,
        Fake {
            batches: three_hits(&env),
            delay: Duration::ZERO,
            warnings: 0,
            hold: false,
            ended: Arc::default(),
        },
    );
    w.start();
    pump_until("finished", || finished(&w));
    let ctrl = env.ctx.host(LEFT).active();
    w.select_rows(&[0, 2]);
    w.panelize();
    pump_until("2 rows", || {
        matches!(ctrl.location(), Location::List { .. })
            && ctrl.state() == LoadState::Idle
            && ctrl.model().total_len() == 2
    });
}

/// Screenshots of the Find window with results, in every theme (`target/screenshots/find-*.png`).
fn screenshots() {
    if std::process::Command::new("import")
        .arg("-version")
        .output()
        .is_err()
    {
        eprintln!("no ImageMagick: skipping screenshots");
        return;
    }
    for (i, theme) in ["far-blue", "commander-light", "adwaita"]
        .iter()
        .enumerate()
    {
        let Some(env) = setup_themed(&format!("io.github.mscoder.OfmFindShot{i}"), Some(theme))
        else {
            return;
        };
        env._win.present();
        let l = env.left.path().to_owned();
        let mut hits = Vec::new();
        for f in ["a.txt", "b.txt", "sub/c.txt", "sub/d.txt"] {
            let mut h = hit_for(&l.join(f));
            if f.starts_with("sub") {
                h.content = Some(ofm_search::ContentHit {
                    first: ofm_search::ContentMatch {
                        line: 12,
                        byte_offset: 100,
                        snippet: "let needle = find(\"needle\");".into(),
                        snippet_match: 4..10,
                    },
                    count: 2,
                });
            }
            hits.push(h);
        }
        let w = open_find(
            &env,
            Fake {
                batches: vec![hits],
                delay: Duration::ZERO,
                warnings: 1,
                hold: false,
                ended: Arc::default(),
            },
        );
        let mut f = w.form();
        f.text = "needle".into();
        f.mask = "*.txt | *.bak".into();
        w.set_form(&f);
        w.start();
        pump_until("finished", || finished(&w));
        w.select_rows(&[2]);
        // Let GTK map and paint the windows.
        let end = Instant::now() + Duration::from_millis(800);
        while Instant::now() < end {
            glib::MainContext::default().iteration(false);
            std::thread::sleep(Duration::from_millis(5));
        }
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(dir.join(format!("find-{theme}.png")))
            .status();
        w.window.close();
    }
}

/// GTK may only be initialised on one thread, so all scenarios run from one test.
#[test]
fn find_scenarios() {
    eprintln!("== alt_f7_opens_one_window_and_results_stream_in");
    alt_f7_opens_one_window_and_results_stream_in();
    eprintln!("== invalid_fields_are_reported_and_nothing_starts");
    invalid_fields_are_reported_and_nothing_starts();
    eprintln!("== updates_are_rate_limited_and_nothing_is_lost");
    updates_are_rate_limited_and_nothing_is_lost();
    eprintln!("== stop_keeps_results_and_ends_the_engine");
    stop_keeps_results_and_ends_the_engine();
    eprintln!("== closing_the_window_cancels_the_search");
    closing_the_window_cancels_the_search();
    eprintln!("== go_to_puts_the_cursor_on_the_file_in_the_active_panel");
    go_to_puts_the_cursor_on_the_file_in_the_active_panel();
    eprintln!("== panelize_shows_a_list_panel_and_file_ops_map_to_real_paths");
    panelize_shows_a_list_panel_and_file_ops_map_to_real_paths();
    eprintln!("== panelize_selected_rows_only_when_several_are_selected");
    panelize_selected_rows_only_when_several_are_selected();
    eprintln!("== screenshots");
    screenshots();
}

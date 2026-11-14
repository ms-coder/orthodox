//! Widget tests: need a display (Xvfb in `scripts/check.sh`, single-threaded) and skip themselves
//! without one. All GTK work lives in ONE test so GTK is only ever initialised on one thread.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::gdk::ModifierType;
use gtk::glib;
use gtk::prelude::*;
use gtk::subclass::prelude::*;
use ofm_config::{ConfigPaths, Theme};
use ofm_core::panel::{CursorMove, SortKey};
use ofm_core::{Entry, EntryKind, Location, cmd};

use super::*;
use crate::app::build_context;
use crate::context::{AppInit, Services};
use crate::dispatch::ExecResult;
use crate::theme::SystemColors;

fn pump(mut done: impl FnMut() -> bool) {
    let ctx = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn filled_controller(n: u32) -> Rc<PanelController> {
    let ctrl = PanelController::new(Services::new(), Location::Local("/data".into()));
    let entries: Vec<Entry> = (0..n)
        .map(|i| {
            let mut e = Entry::new(format!("file{i:06}.txt"), EntryKind::File);
            e.size = u64::from(i) * 37;
            e.mtime_ns = 1_700_000_000_000_000_000 + i128::from(i) * 1_000_000_000;
            e.mode = 0o100_644;
            e
        })
        .collect();
    {
        let mut m = ctrl.model_mut();
        m.begin_load(true, false);
        m.apply_batch(entries);
        m.finish_load();
    }
    ctrl
}

fn palette(id: &str) -> Rc<Palette> {
    let theme = Theme::builtin(id).expect("builtin theme");
    Rc::new(Palette::from_theme(
        Rc::new(theme),
        SystemColors::fallback(),
    ))
}

fn snapshot_once(list: &FileList) {
    let snap = gtk::Snapshot::new();
    WidgetImpl::snapshot(list.imp(), &snap);
}

/// The only `#[test]` that touches GTK: the harness may run every test on its own thread and GTK
/// must stay on the thread that initialised it.
#[test]
fn gtk_widget_tests() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    list_widget_end_to_end();
    commands_drive_sort_view_and_hidden();
    // Visual self-check: OFM_FILELIST_SHOTS=<dir> writes one PNG per theme and view mode.
    if let Some(dir) = std::env::var_os("OFM_FILELIST_SHOTS") {
        render_shots(std::path::Path::new(&dir));
    }
}

/// A directory-like sample with every highlight class, marks and a cursor.
fn sample_controller() -> Rc<PanelController> {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let ctrl = PanelController::new(
        Services::new(),
        Location::Local("/home/user/projects".into()),
    );
    let t0 = 1_700_000_000_000_000_000_i128;
    let mk = |name: &str, kind: EntryKind, size: u64, mode: u32, age_days: i128| {
        let mut e = Entry::new(name, kind);
        e.size = size;
        e.mode = mode;
        e.mtime_ns = t0 + age_days * 86_400 * 1_000_000_000;
        e
    };
    let file = |n: &str, size: u64, age: i128| mk(n, EntryKind::File, size, 0o100_644, age);
    let dir = |n: &str, age: i128| mk(n, EntryKind::Dir, 0, 0o040_755, age);
    let mut entries = vec![
        dir("src", 3),
        dir("docs", 9),
        dir("target", 12),
        dir(".cache", 1),
        dir(
            "a-directory-with-a-really-long-name-that-must-be-ellipsized",
            5,
        ),
        file("Cargo.toml", 1_204, 2),
        file("Cargo.lock", 48_913, 2),
        file("README.md", 5_120, 40),
        file("main.rs", 22_400, 1),
        file("lib.rs", 8_100, 1),
        file("backup-2026.tar.gz", 734_003_200, 30),
        file("photo.jpg", 2_411_724, 7),
        file("notes.txt", 311, 8),
        file(".hidden-config", 90, 100),
        file("data.bin", 12_884_901_888, 365),
        mk("run.sh", EntryKind::File, 640, 0o100_755, 4),
        mk(
            "latest",
            EntryKind::Symlink {
                target: Some(ofm_core::TargetKind::Dir),
            },
            0,
            0o120_777,
            4,
        ),
        mk(
            "link-to-file",
            EntryKind::Symlink {
                target: Some(ofm_core::TargetKind::File),
            },
            12,
            0o120_777,
            4,
        ),
        mk(
            "broken-link",
            EntryKind::Symlink { target: None },
            0,
            0o120_777,
            4,
        ),
    ];
    entries.push(Entry::new(
        OsString::from_vec(b"bad-\xff-name.txt".to_vec()),
        EntryKind::File,
    ));
    for i in 0..40 {
        entries.push(file(
            &format!("file{i:02}.dat"),
            1000 * (i + 1),
            i128::from(i),
        ));
    }
    {
        let mut m = ctrl.model_mut();
        m.begin_load(true, false);
        m.apply_batch(entries);
        m.finish_load();
        m.set_cursor(4);
        m.select_range(7, 9, true);
        m.toggle_select(11);
    }
    ctrl
}

fn render_shots(dir: &std::path::Path) {
    let custom = ofm_config::config::PanelsSection {
        custom_columns: ["name", "ext", "size", "mode"].map(String::from).to_vec(),
        ..Default::default()
    };
    configure(&custom);
    let ctrl = sample_controller();
    let list = FileList::new();
    let win = gtk::Window::new();
    win.set_default_size(820, 440);
    win.set_child(Some(&list));
    list.bind(Some(Rc::clone(&ctrl)));
    win.present();
    for theme in ofm_config::theme::BUILTIN_THEMES {
        list.set_palette(palette(theme));
        for (mode, active) in [
            (ViewMode::Brief, true),
            (ViewMode::Full, true),
            (ViewMode::Custom, true),
            (ViewMode::Brief, false),
        ] {
            list.set_active(active);
            set_view_mode(&ctrl, mode);
            pump_for(Duration::from_millis(150));
            let paintable = gtk::WidgetPaintable::new(Some(&list));
            let snap = gtk::Snapshot::new();
            paintable.snapshot(&snap, 820.0, 440.0);
            let (Some(node), Some(renderer)) = (snap.to_node(), win.renderer()) else {
                eprintln!("no render node / renderer");
                continue;
            };
            let tex = renderer.render_texture(&node, None);
            let name = format!(
                "filelist-{theme}-{}{}.png",
                mode.as_str(),
                if active { "" } else { "-inactive" }
            );
            tex.save_to_png(dir.join(&name)).expect("save png");
            eprintln!("wrote {name}");
        }
    }
    win.close();
}

fn pump_for(d: Duration) {
    let ctx = glib::MainContext::default();
    let end = Instant::now() + d;
    while Instant::now() < end {
        ctx.iteration(false);
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn list_widget_end_to_end() {
    let ctrl = filled_controller(100_000);
    let list = FileList::new();
    list.set_palette(palette("far-blue"));
    list.set_active(true);
    list.bind(Some(Rc::clone(&ctrl)));
    let win = gtk::Window::new();
    win.set_default_size(900, 600);
    win.set_child(Some(&list));
    win.present();
    pump(|| list.window().1 > 0);

    // -- geometry is reported to the controller ---------------------------------------------
    let (first, rows, cols) = list.window();
    assert_eq!(first, 0);
    assert!(rows >= 10, "rows {rows}");
    assert!(cols >= 2, "brief mode shows several columns, got {cols}");
    let g = ctrl.geometry();
    assert_eq!((g.rows_per_page, g.columns), (rows, cols));

    // -- cursor movement scrolls the window -------------------------------------------------
    ctrl.cursor_move(CursorMove::End);
    let (first, rows, cols) = list.window();
    let cursor = ctrl.model().cursor().unwrap();
    assert_eq!(cursor, 100_000, "..-row plus 100k entries");
    assert!(
        first <= cursor && cursor < first + rows * cols,
        "{first} {cursor}"
    );
    ctrl.cursor_move(CursorMove::Home);
    assert_eq!(list.window().0, 0);

    // -- mouse: click, Ctrl, Shift, double click ----------------------------------------------
    let imp = list.imp();
    let geo = imp.geo().expect("geometry");
    let rh = f64::from(geo.row_h);
    // Centre of the cell (col, row).
    let at = |col: u32, row: u32| {
        let (x, y) = geo.cell_origin(col, row);
        (f64::from(x) + 20.0, f64::from(y) + rh / 2.0)
    };
    let opened = Rc::new(std::cell::Cell::new(None));
    {
        let opened = Rc::clone(&opened);
        list.connect_open_entry(move |_, i| opened.set(Some(i)));
    }
    let activated = Rc::new(std::cell::Cell::new(0));
    {
        let activated = Rc::clone(&activated);
        list.connect_activate_panel(move |_| activated.set(activated.get() + 1));
    }
    let (x, y) = at(0, 2);
    imp.on_press(1, ModifierType::empty(), 1, x, y);
    assert_eq!(ctrl.model().cursor(), Some(2));
    assert_eq!(activated.get(), 1);
    imp.on_press(1, ModifierType::CONTROL_MASK, 1, x, y + rh);
    assert_eq!(ctrl.model().cursor(), Some(3));
    assert_eq!(ctrl.model().selected_count(), 1);
    imp.on_press(1, ModifierType::SHIFT_MASK, 1, x, y + 3.0 * rh);
    assert_eq!(ctrl.model().cursor(), Some(5));
    assert_eq!(ctrl.model().selected_count(), 3, "rows 3..=5 marked");
    imp.on_press(1, ModifierType::empty(), 2, x, y);
    assert_eq!(opened.get(), Some(2));
    // Second column (brief): x beyond one cell width.
    let (x2, y2) = at(1, 2);
    imp.on_press(1, ModifierType::empty(), 1, x2, y2);
    assert_eq!(ctrl.model().cursor(), Some(rows + 2));

    // -- header click sorts, again reverses --------------------------------------------------
    imp.on_press(1, ModifierType::empty(), 1, 30.0, 9.0);
    assert!(ctrl.model().sort_spec().reverse, "name sort toggled");

    // -- view mode commands (through the dispatcher) -----------------------------------------
    set_view_mode(&ctrl, ViewMode::Full);
    assert_eq!(list.window().2, 1);
    assert_eq!(ctrl.geometry().columns, 1);
    pump(|| true);
    snapshot_once(&list);
    set_view_mode(&ctrl, ViewMode::Custom);
    snapshot_once(&list);
    set_view_mode(&ctrl, ViewMode::Brief);
    assert!(list.window().2 >= 2);

    // -- wheel scrolling keeps the cursor where it is ----------------------------------------
    ctrl.cursor_move(CursorMove::Home);
    imp.on_scroll(0.0, 5.0);
    assert!(list.window().0 > 0);
    assert_eq!(ctrl.model().cursor(), Some(0));

    // -- 100k entries: frame time ------------------------------------------------------------
    for mode in [ViewMode::Brief, ViewMode::Full] {
        set_view_mode(&ctrl, mode);
        ctrl.cursor_move(CursorMove::Home);
        let before = list.frame_stats();
        let t = Instant::now();
        let steps = 400;
        for _ in 0..steps {
            ctrl.cursor_move(CursorMove::By(7));
            snapshot_once(&list);
        }
        let st = list.frame_stats();
        let frames = st.frames - before.frames;
        assert_eq!(frames, steps);
        let avg = (st.total_us - before.total_us) / frames;
        eprintln!(
            "{mode:?}: {frames} frames, avg {avg} us, max {} us, wall {:?}",
            st.max_us,
            t.elapsed()
        );
        assert!(
            avg < 16_000,
            "{mode:?}: average frame {avg} us is not smooth"
        );
    }
    win.close();
}

fn commands_drive_sort_view_and_hidden() {
    let dir = tempfile_dir();
    let ctx = build_context(AppInit {
        paths: ConfigPaths::with_dirs(dir.join("config"), dir.join("state")),
        left: Location::Local(dir.clone()),
        right: Location::Local(dir.clone()),
    });
    let ctrl = ctx.active_controller();
    // Fill without touching the file system.
    {
        let mut m = ctrl.model_mut();
        m.begin_load(false, false);
        m.apply_batch(vec![
            Entry::new("b", EntryKind::File),
            Entry::new(".hidden", EntryKind::File),
            Entry::new("a", EntryKind::File),
        ]);
        m.finish_load();
    }
    let hidden_before = ctrl.model().show_hidden();

    assert_eq!(ctx.execute(cmd::PANEL_SORT_SIZE), ExecResult::Handled);
    let s = ctrl.model().sort_spec();
    assert_eq!((s.key, s.reverse), (SortKey::Size, false));
    ctx.execute(cmd::PANEL_SORT_SIZE);
    assert!(ctrl.model().sort_spec().reverse, "same key twice reverses");
    ctx.execute(cmd::PANEL_SORT_EXT);
    let s = ctrl.model().sort_spec();
    assert_eq!((s.key, s.reverse), (SortKey::Ext, false));
    ctx.execute(cmd::PANEL_SORT_MTIME);
    assert_eq!(ctrl.model().sort_spec().key, SortKey::Mtime);
    ctx.execute(cmd::PANEL_SORT_NAME);
    assert_eq!(ctrl.model().sort_spec().key, SortKey::Name);
    ctx.execute(cmd::PANEL_SORT_REVERSE);
    assert!(ctrl.model().sort_spec().reverse);
    assert!(ctrl.model().sort_spec().dirs_first, "dirs-first is kept");

    ctx.execute(cmd::PANEL_TOGGLE_HIDDEN);
    assert_eq!(ctrl.model().show_hidden(), !hidden_before);
    ctx.execute(cmd::PANEL_TOGGLE_HIDDEN);
    assert_eq!(ctrl.model().show_hidden(), hidden_before);

    ctx.execute(cmd::PANEL_VIEW_FULL);
    assert_eq!(view_mode(&ctrl), ViewMode::Full);
    ctx.execute(cmd::PANEL_VIEW_CUSTOM);
    assert_eq!(view_mode(&ctrl), ViewMode::Custom);
    ctx.execute(cmd::PANEL_VIEW_BRIEF);
    assert_eq!(view_mode(&ctrl), ViewMode::Brief);
    // The other panel is untouched.
    assert_eq!(view_mode(&ctx.other_controller()), settings().default_view);
}

fn tempfile_dir() -> std::path::PathBuf {
    let t = ofm_testkit::tree::TreeSpec::new().build_temp().unwrap();
    let p = t.path().to_owned();
    // Keep the directory alive for the whole process: tests are short-lived.
    std::mem::forget(t);
    p
}

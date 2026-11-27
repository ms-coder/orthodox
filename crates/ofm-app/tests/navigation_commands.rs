//! T2.3: navigation commands (go up/root/home/open, history, tabs, quick search, filter,
//! selection, mask + hotlist dialogs, mount menu) driven through `AppContext::execute`, plus
//! keymap resolution of every T2.3 command in the far and tc presets.
//! The UI scenario needs a display (Xvfb in `scripts/check.sh`) and skips itself without one.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::glib;
use gtk::prelude::*;
use ofm_app::app::build_context;
use ofm_app::context::{AppContext, AppInit};
use ofm_app::dialogs::{hotlist, mask};
use ofm_app::dispatch::{CommandRegistry, ExecResult};
use ofm_app::navigation::modal::ModalStack;
use ofm_app::navigation::quicksearch::{self, Mode, QuickSearchState};
use ofm_app::navigation::tabs::TabBars;
use ofm_app::navigation::{mount, session};
use ofm_app::panel::{LEFT, LoadState, RIGHT};
use ofm_app::window::MainWindow;
use ofm_config::history::History;
use ofm_config::{ConfigPaths, Keymap, Preset};
use ofm_core::{CommandId, KeyChord, Location, cmd};
use ofm_testkit::tree::{FileSpec, TreeSpec};

// ---- keymap resolution (headless) ------------------------------------------------------------------

const T23_BINDINGS: &[(&str, CommandId)] = &[
    ("Backspace", cmd::PANEL_GO_UP),
    ("Ctrl+PgUp", cmd::PANEL_GO_UP),
    ("Ctrl+\\", cmd::PANEL_GO_ROOT),
    ("Ctrl+Home", cmd::PANEL_GO_HOME),
    ("Ctrl+L", cmd::PANEL_GOTO_PATH),
    ("Enter", cmd::PANEL_OPEN),
    ("Ctrl+PgDn", cmd::PANEL_ENTER_DIR),
    ("Ctrl+U", cmd::PANEL_SWAP),
    ("Alt+=", cmd::PANEL_SAME_DIR),
    ("Ctrl+R", cmd::PANEL_REFRESH),
    ("Alt+Left", cmd::PANEL_HISTORY_BACK),
    ("Alt+Right", cmd::PANEL_HISTORY_FORWARD),
    ("Alt+F12", cmd::PANEL_HISTORY_LIST),
    ("Ctrl+D", cmd::PANEL_HOTLIST),
    ("Ctrl+Shift+D", cmd::PANEL_HOTLIST_ADD),
    ("Alt+F1", cmd::PANEL_DRIVE_MENU_LEFT),
    ("Alt+F2", cmd::PANEL_DRIVE_MENU_RIGHT),
    ("Ctrl+S", cmd::PANEL_QUICKSEARCH),
    ("Ctrl+Down", cmd::PANEL_QUICKSEARCH_NEXT),
    ("Ctrl+Up", cmd::PANEL_QUICKSEARCH_PREV),
    ("Ctrl+Shift+S", cmd::PANEL_FILTER),
    ("Esc", cmd::PANEL_CANCEL),
    ("Ctrl+T", cmd::TAB_NEW),
    ("Ctrl+W", cmd::TAB_CLOSE),
    ("Ctrl+Tab", cmd::TAB_NEXT),
    ("Ctrl+Shift+Tab", cmd::TAB_PREV),
    ("Ctrl+Shift+L", cmd::TAB_LOCK),
    ("Ins", cmd::SELECT_TOGGLE),
    ("Shift+Up", cmd::SELECT_EXTEND_UP),
    ("Shift+Down", cmd::SELECT_EXTEND_DOWN),
    ("Shift+PgUp", cmd::SELECT_EXTEND_PAGE_UP),
    ("Shift+PgDn", cmd::SELECT_EXTEND_PAGE_DOWN),
    ("Shift+Home", cmd::SELECT_EXTEND_HOME),
    ("Shift+End", cmd::SELECT_EXTEND_END),
    ("Gray+", cmd::SELECT_BY_MASK),
    ("Gray-", cmd::SELECT_UNMASK),
    ("Gray*", cmd::SELECT_INVERT),
    ("Ctrl+Gray+", cmd::SELECT_ALL),
    ("Ctrl+Gray-", cmd::SELECT_NONE),
];

fn keymap(preset: Preset) -> Keymap {
    let tmp = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(tmp.path().join("c"), tmp.path().join("s"));
    Keymap::load(&paths, preset, None).0
}

/// Find a button by its visible (mnemonic-free) label, including inside an Adwaita dialog.
fn find_button(root: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    if let Some(button) = root.downcast_ref::<gtk::Button>()
        && button
            .label()
            .is_some_and(|text| text.replace('_', "") == label)
    {
        return Some(button.clone());
    }
    let mut child = root.first_child();
    while let Some(widget) = child {
        if let Some(button) = find_button(&widget, label) {
            return Some(button);
        }
        child = widget.next_sibling();
    }
    None
}

#[test]
fn t23_commands_have_handlers_and_keys_in_far_and_tc() {
    let mut reg = CommandRegistry::new();
    ofm_app::hooks::install_commands(&mut reg);
    assert!(reg.duplicates().is_empty(), "{:?}", reg.duplicates());
    for preset in [Preset::Far, Preset::Tc] {
        let km = keymap(preset);
        for (chord, command) in T23_BINDINGS {
            let chord: KeyChord = chord.parse().unwrap();
            assert_eq!(
                km.resolve_main(chord, false),
                Some(*command),
                "{preset:?}: {chord}"
            );
            assert!(reg.has(*command), "no handler for {command}");
        }
    }
    // Dialog keys reach handlers (they are registered here, not left NotImplemented).
    assert!(reg.has(cmd::DIALOG_OK) && reg.has(cmd::DIALOG_CANCEL));
    // TC: Space toggles the selection, Ctrl+M is the multi-rename tool and "restore selection"
    // (no TC equivalent) sits on Ctrl+Shift+R (M9).
    let tc = keymap(Preset::Tc);
    for (chord, command) in [
        ("Space", cmd::SELECT_TOGGLE),
        ("Ctrl+M", cmd::FILE_BATCH_RENAME),
        ("Ctrl+Shift+R", cmd::SELECT_RESTORE),
    ] {
        let chord: KeyChord = chord.parse().unwrap();
        assert_eq!(tc.resolve_main(chord, false), Some(command), "Tc: {chord}");
    }
    // Far keeps Ctrl+M = restore selection.
    let chord: KeyChord = "Ctrl+M".parse().unwrap();
    assert_eq!(
        keymap(Preset::Far).resolve_main(chord, false),
        Some(cmd::SELECT_RESTORE)
    );
}

// ---- UI scenario -----------------------------------------------------------------------------------

fn pump_until(mut done: impl FnMut() -> bool) {
    let main = glib::MainContext::default();
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        main.iteration(false);
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn settle(ctx: &AppContext) {
    pump_until(|| {
        ctx.panels
            .iter()
            .all(|h| h.tabs().iter().all(|t| t.state() != LoadState::Loading))
    });
}

fn run(ctx: &Rc<AppContext>, c: CommandId) {
    assert_eq!(ctx.execute(c), ExecResult::Handled, "{c}");
    settle(ctx);
}

fn loc(ctx: &AppContext, side: usize) -> Location {
    ctx.host(side).active().location()
}

fn cursor_name(ctx: &AppContext, side: usize) -> String {
    ctx.host(side)
        .active()
        .model()
        .cursor_entry()
        .map(|e| e.name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn visible_names(ctx: &AppContext, side: usize) -> Vec<String> {
    let c = ctx.host(side).active();
    let m = c.model();
    (0..m.visible_len())
        .filter_map(|i| m.visible(i))
        .map(|e| e.name.to_string_lossy().into_owned())
        .collect()
}

fn cursor_to(ctx: &AppContext, side: usize, name: &str) {
    assert!(
        ctx.host(side)
            .active()
            .mutate(|m| m.set_cursor_name(name.as_ref())),
        "no entry {name}"
    );
}

fn local(p: &Path) -> Location {
    Location::Local(p.to_owned())
}

#[test]
fn navigation_scenario() {
    if gtk::init().is_err() {
        eprintln!("no display: skipping");
        return;
    }
    let _ = adw::init();
    let left = TreeSpec::new()
        .dir(
            "a",
            TreeSpec::new()
                .dir("b", TreeSpec::new().file("x.txt", FileSpec::content(b"x")))
                .file("c.txt", FileSpec::content(b"c")),
        )
        .file("d.txt", FileSpec::content(b"d"))
        .file("e.rs", FileSpec::content(b"e"))
        .file("f.png", FileSpec::content(b"f"))
        .file("g.PNG", FileSpec::content(b"g"))
        .file("h.tmp", FileSpec::content(b"h"))
        .build_temp()
        .unwrap();
    let right = TreeSpec::new()
        .file("r1", FileSpec::content(b"1"))
        .build_temp()
        .unwrap();
    let cfg = TreeSpec::new().build_temp().unwrap();
    let paths = ConfigPaths::with_dirs(cfg.path().join("config"), cfg.path().join("state"));
    let ctx = build_context(AppInit {
        paths: paths.clone(),
        left: local(&left.path().join("a/b")),
        right: local(right.path()),
    });
    let app = adw::Application::builder()
        .application_id("io.github.mscoder.OfmNavTest")
        .build();
    app.register(gtk::gio::Cancellable::NONE).unwrap();
    let win = MainWindow::new(&app, &ctx);
    win.present();
    ctx.start_loading();
    settle(&ctx);

    // ---- go up: cursor lands on the directory we came from ---------------------------------------
    run(&ctx, cmd::PANEL_GO_UP);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));
    assert_eq!(cursor_name(&ctx, LEFT), "b");
    run(&ctx, cmd::PANEL_GO_UP);
    assert_eq!(loc(&ctx, LEFT), local(left.path()));
    assert_eq!(cursor_name(&ctx, LEFT), "a");

    // ---- open / enter, `..` goes up -----------------------------------------------------------------
    run(&ctx, cmd::PANEL_OPEN);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));
    cursor_to(&ctx, LEFT, "b");
    run(&ctx, cmd::PANEL_ENTER_DIR);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a/b")));
    cursor_to(&ctx, LEFT, "..");
    run(&ctx, cmd::PANEL_OPEN);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));

    // ---- history back / forward ---------------------------------------------------------------------
    run(&ctx, cmd::PANEL_HISTORY_BACK);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a/b")));
    run(&ctx, cmd::PANEL_HISTORY_BACK);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));
    run(&ctx, cmd::PANEL_HISTORY_FORWARD);
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a/b")));
    // The list dialog opens, Enter picks the highlighted (current) entry, Esc closes.
    let stack = ctx.ext.get::<ModalStack>().unwrap();
    ctx.execute(cmd::PANEL_HISTORY_LIST);
    assert!(ctx.modal_open() && stack.depth() == 1);
    ctx.execute(cmd::DIALOG_CANCEL);
    assert!(!ctx.modal_open() && stack.depth() == 0);

    // ---- root, home, refresh --------------------------------------------------------------------------
    run(&ctx, cmd::PANEL_GO_ROOT);
    assert_eq!(loc(&ctx, LEFT), local(Path::new("/")));
    run(&ctx, cmd::PANEL_GO_HOME);
    assert_eq!(loc(&ctx, LEFT), local(&glib::home_dir()));
    run(&ctx, cmd::PANEL_REFRESH);
    assert_eq!(loc(&ctx, LEFT), local(&glib::home_dir()));
    // Swap panels, then other panel -> same dir.
    let (home, right_before) = (loc(&ctx, LEFT), loc(&ctx, RIGHT));
    run(&ctx, cmd::PANEL_SWAP);
    assert_eq!(loc(&ctx, LEFT), right_before);
    assert_eq!(loc(&ctx, RIGHT), home);
    run(&ctx, cmd::PANEL_SWAP);
    assert_eq!(loc(&ctx, LEFT), home);
    run(&ctx, cmd::PANEL_SAME_DIR);
    assert_eq!(loc(&ctx, RIGHT), home);

    // Put the left panel on the fixture root for the rest.
    ctx.host(LEFT)
        .active()
        .load(local(left.path()), Default::default());
    settle(&ctx);

    // ---- tabs -------------------------------------------------------------------------------------------
    let host = ctx.host(LEFT);
    let bars = ctx.ext.get::<TabBars>().unwrap();
    let bar = bars.bar(LEFT).unwrap();
    assert!(!bar.is_visible(), "bar hidden with a single tab");
    run(&ctx, cmd::TAB_NEW);
    assert_eq!((host.tab_count(), host.active_index()), (2, 1));
    assert!(bar.is_visible());
    assert_eq!(bar.captions().len(), 2);
    run(&ctx, cmd::TAB_PREV);
    assert_eq!(host.active_index(), 0);
    run(&ctx, cmd::TAB_PREV); // wraps
    assert_eq!(host.active_index(), 1);
    run(&ctx, cmd::TAB_NEXT);
    assert_eq!(host.active_index(), 0);
    // Lock tab 0: it cannot be closed and navigation opens a new tab.
    run(&ctx, cmd::TAB_LOCK);
    assert!(host.active().locked());
    run(&ctx, cmd::TAB_CLOSE);
    assert_eq!(host.tab_count(), 2, "locked tab is not closed");
    cursor_to(&ctx, LEFT, "a");
    run(&ctx, cmd::PANEL_OPEN);
    assert_eq!(host.tab_count(), 3);
    assert_eq!(
        host.tabs()[0].location(),
        local(left.path()),
        "locked tab stayed"
    );
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));
    // Session round trip keeps tabs, lock and the active tab.
    let ps = session::capture(host);
    assert_eq!(ps.tabs.len(), 3);
    assert!(ps.tabs[0].locked);
    assert_eq!(ps.active_tab, host.active_index());
    run(&ctx, cmd::TAB_CLOSE);
    host.set_active(0);
    run(&ctx, cmd::TAB_LOCK);
    assert!(!host.active().locked());
    run(&ctx, cmd::TAB_CLOSE);
    assert_eq!(host.tab_count(), 1);
    assert!(!bar.is_visible());
    run(&ctx, cmd::TAB_CLOSE);
    assert_eq!(host.tab_count(), 1, "the last tab stays");
    assert_eq!(loc(&ctx, LEFT), local(left.path()));
    {
        // Restoring into a fresh host-sized state.
        session::restore(host, &ps);
        settle(&ctx);
        assert_eq!(host.tab_count(), 3);
        assert!(host.tabs()[0].locked());
    }
    host.set_active(0);
    run(&ctx, cmd::TAB_LOCK);
    for _ in 0..2 {
        let last = host.tab_count() - 1;
        assert!(host.close_tab(last));
    }
    assert_eq!(host.tab_count(), 1);
    assert!(!host.active().locked());

    // ---- quick search ----------------------------------------------------------------------------------
    let qs = ctx
        .ext
        .get::<QuickSearchState>()
        .unwrap()
        .bar(LEFT)
        .unwrap();
    ctx.host(LEFT).active().mutate(|m| m.set_cursor(0));
    quicksearch::start_with(&ctx, 'e');
    assert!(qs.is_open());
    assert_eq!(cursor_name(&ctx, LEFT), "e.rs");
    qs.open(Mode::Search, "png");
    assert_eq!(cursor_name(&ctx, LEFT), "f.png");
    run(&ctx, cmd::PANEL_QUICKSEARCH_NEXT);
    assert_eq!(cursor_name(&ctx, LEFT), "g.PNG");
    run(&ctx, cmd::PANEL_QUICKSEARCH_PREV);
    assert_eq!(cursor_name(&ctx, LEFT), "f.png");
    run(&ctx, cmd::PANEL_CANCEL);
    assert!(!qs.is_open());
    // Filter mode hides rows; Enter keeps the filter, Esc clears it.
    run(&ctx, cmd::PANEL_FILTER);
    assert!(qs.is_open() && qs.mode() == Mode::Filter);
    qs.open(Mode::Filter, "png");
    assert_eq!(visible_names(&ctx, LEFT), ["..", "f.png", "g.PNG"]);
    quicksearch::confirm_active(&ctx);
    assert!(!qs.is_open());
    assert_eq!(
        visible_names(&ctx, LEFT).len(),
        3,
        "filter stays after Enter"
    );
    run(&ctx, cmd::PANEL_CANCEL);
    assert!(visible_names(&ctx, LEFT).len() > 3);
    run(&ctx, cmd::PANEL_FILTER);
    qs.open(Mode::Filter, "rs");
    run(&ctx, cmd::PANEL_CANCEL);
    assert!(
        !qs.is_open() && visible_names(&ctx, LEFT).len() > 3,
        "Esc drops the filter"
    );

    // ---- selection --------------------------------------------------------------------------------------
    let sel = |ctx: &AppContext| ctx.host(LEFT).active().model().selected_names();
    ctx.host(LEFT).active().mutate(|m| m.set_cursor(0));
    run(&ctx, cmd::SELECT_ALL);
    assert_eq!(sel(&ctx).len(), 6, "all but `..`");
    run(&ctx, cmd::SELECT_NONE);
    assert!(sel(&ctx).is_empty());
    run(&ctx, cmd::SELECT_RESTORE);
    assert_eq!(sel(&ctx).len(), 6);
    run(&ctx, cmd::SELECT_NONE);
    mask::apply(&ctx, "*.{png,rs};!e.*", true, true).unwrap();
    assert_eq!(
        sel(&ctx),
        ["f.png", "g.PNG"],
        "case-insensitive, exclusions"
    );
    run(&ctx, cmd::SELECT_INVERT);
    assert_eq!(sel(&ctx), ["a", "d.txt", "e.rs", "h.tmp"]);
    run(&ctx, cmd::SELECT_NONE);
    mask::apply(&ctx, "*", true, true).unwrap();
    assert_eq!(sel(&ctx).len(), 5, "files only keeps directories out");
    mask::apply(&ctx, "*.png", false, true).unwrap();
    assert_eq!(sel(&ctx).len(), 3);
    assert!(mask::apply(&ctx, "*.{png", true, true).is_err());
    run(&ctx, cmd::SELECT_NONE);
    // toggle moves down; Shift+Down extends.
    ctx.host(LEFT).active().mutate(|m| m.set_cursor(1)); // "a"
    run(&ctx, cmd::SELECT_TOGGLE);
    assert_eq!(sel(&ctx), ["a"]);
    assert_eq!(cursor_name(&ctx, LEFT), "d.txt");
    run(&ctx, cmd::SELECT_EXTEND_DOWN);
    run(&ctx, cmd::SELECT_EXTEND_DOWN);
    assert_eq!(sel(&ctx), ["a", "d.txt", "e.rs"]);
    assert_eq!(cursor_name(&ctx, LEFT), "f.png");
    run(&ctx, cmd::SELECT_EXTEND_UP);
    assert_eq!(sel(&ctx), ["a", "d.txt", "e.rs", "f.png"]);
    run(&ctx, cmd::SELECT_NONE);
    run(&ctx, cmd::SELECT_EXTEND_END);
    assert_eq!(
        sel(&ctx).len(),
        3,
        "from the cursor to the row before the last"
    );
    run(&ctx, cmd::SELECT_NONE);

    // ---- dialogs: mask, goto, hotlist, drive menu --------------------------------------------------------
    ctx.execute(cmd::SELECT_BY_MASK);
    assert!(ctx.modal_open());
    ctx.execute(cmd::DIALOG_OK); // default = last mask ("*.png"): valid, selects, closes
    assert!(!ctx.modal_open());
    assert_eq!(sel(&ctx), ["f.png", "g.PNG"]);
    ctx.execute(cmd::SELECT_UNMASK);
    assert!(ctx.modal_open());
    ctx.execute(cmd::DIALOG_CANCEL);
    assert!(!ctx.modal_open());
    ctx.execute(cmd::PANEL_GOTO_PATH);
    assert!(ctx.modal_open());
    ctx.execute(cmd::DIALOG_OK); // entry holds the current directory: no-op navigation
    assert!(!ctx.modal_open());

    ctx.host(LEFT).active().mutate(|m| m.set_cursor(1));
    run(&ctx, cmd::PANEL_OPEN); // into "a"
    assert!(hotlist::add_current(&ctx));
    assert!(!hotlist::add_current(&ctx), "no duplicates");
    run(&ctx, cmd::PANEL_GO_ROOT);
    ctx.execute(cmd::PANEL_HOTLIST);
    assert!(ctx.modal_open());
    ctx.execute(cmd::DIALOG_OK); // picks the first entry: "a"
    settle(&ctx);
    assert!(!ctx.modal_open());
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));
    // Enter activates the focused dialog button instead of picking a row / saving and closing.
    ctx.execute(cmd::PANEL_HOTLIST);
    assert!(ctx.modal_open());
    let edit = find_button(win.window.upcast_ref(), "Edit...").unwrap();
    edit.grab_focus();
    assert_eq!(stack.focus_widget(), Some(edit.clone().upcast()));
    ctx.execute(cmd::DIALOG_OK);
    assert!(ctx.modal_open(), "Edit opened the hotlist editor");
    assert_eq!(loc(&ctx, LEFT), local(&left.path().join("a")));
    let remove = find_button(win.window.upcast_ref(), "Remove").unwrap();
    remove.grab_focus();
    ctx.execute(cmd::DIALOG_OK);
    assert!(
        ctx.modal_open(),
        "Remove acted without treating Enter as Save and Close"
    );
    ctx.execute(cmd::DIALOG_CANCEL);
    assert!(!ctx.modal_open());

    let items = mount::collect(&ctx);
    assert!(items.len() >= 3, "home, root, trash are always listed");
    ctx.execute(cmd::PANEL_DRIVE_MENU_RIGHT);
    assert!(ctx.modal_open());
    assert_eq!(ctx.active_side(), RIGHT);
    ctx.execute(cmd::DIALOG_OK); // first row: Home
    settle(&ctx);
    assert_eq!(loc(&ctx, RIGHT), local(&glib::home_dir()));

    // ---- persisted history: masks and the hotlist are written by a worker ------------------------------
    pump_until(|| {
        let (h, _) = History::load(&paths);
        h.masks.first().map(String::as_str) == Some("*.png") && h.hotlist.len() == 1
    });
    let (h, _) = History::load(&paths);
    assert!(h.masks.contains(&"*.{png,rs};!e.*".to_owned()));
    assert_eq!(h.hotlist[0].path_buf(), Some(left.path().join("a")));
}

//! T8.11 real-binary tests (pre-G3 UI gaps): the panel context menu (Shift+F10, Menu key, keys
//! inside the menu), the F9 menu bar's own keys, the Help dialog (F1), F3 on a directory = size calculation (F19) and
//! `[terminal] return_to_panels`, all through the real `ofm` under a private Xvfb with real key events (see `real_support`). Skips
//! when the harness tools are missing. Screenshots land in `target/screenshots/`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::path::Path;
use std::time::{Duration, Instant};

use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, Shot, tools_available};

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    false
}

fn trees() -> (TreeSpec, TreeSpec) {
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"hello viewer\n"))
        .file("b.txt", FileSpec::content(b"b"))
        .dir(
            "sub",
            TreeSpec::new()
                .file("one", FileSpec::content(b"1"))
                .file("two", FileSpec::content(b"22")),
        );
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    (left, right)
}

fn launch(config: &str) -> Running {
    let (l, r) = trees();
    Running::launch_with(config, &l, &r)
}

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

/// Visible `ofm` windows (the viewer/dialogs are extra toplevels).
fn window_count(s: &Running) -> usize {
    let out = s.x("xdotool", &["search", "--onlyvisible", "--class", "ofm"]);
    String::from_utf8_lossy(&out.stdout).lines().count()
}

/// Pixels that differ between two shots.
fn diff(a: &Shot, b: &Shot) -> usize {
    a.diff_pixels(b)
}

/// Shift+F10 opens the context menu at the cursor row and Esc closes it; the Menu key opens it
/// too, and Up/Down/Enter inside the menu run the focused command (F3 "View" opens the viewer).
#[test]
fn context_menu_opens_runs_and_closes_with_real_keys() {
    if !tools_available() {
        return;
    }
    let s = launch("");
    let plain = s.shot();

    // Shift+F10 on `..` (cursor home): the menu appears over the left panel.
    s.keys(&["shift+F10"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) > 2000),
        "Shift+F10 did not open the context menu"
    );
    std::thread::sleep(Duration::from_millis(400));
    save(&s, "preg3-context-menu");

    // Esc closes it: the panels are back unchanged.
    s.keys(&["Escape"]);
    let closed = wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) < 50);
    assert!(closed, "Esc did not close the context menu");

    // The Menu key opens it too; Down + Enter runs the second entry ("View") on a.txt.
    s.keys(&["Down", "Down"]); // `..` -> sub -> a.txt
    std::thread::sleep(Duration::from_millis(300));
    s.keys(&["Menu"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) > 2000),
        "the Menu key did not open the context menu"
    );
    s.keys(&["Down"]); // Open -> View
    s.keys(&["Return"]);
    assert!(
        wait_until(Duration::from_secs(10), || window_count(&s) >= 2),
        "Enter on the View row did not open the viewer"
    );
    // Esc closes the viewer; only the main window remains.
    s.keys(&["Escape"]);
    assert!(
        wait_until(Duration::from_secs(10), || window_count(&s) == 1),
        "Esc did not close the viewer"
    );
}

/// F9 menu bar: Right/Left switch menus, Esc closes it and gives the focus back (the screen is
/// as before and panel keys work), Enter runs the focused entry.
#[test]
fn menu_bar_switches_closes_and_runs_with_real_keys() {
    if !tools_available() {
        return;
    }
    let s = launch("");
    s.keys(&["Down", "Down"]); // `..` -> sub -> a.txt
    std::thread::sleep(Duration::from_millis(500));
    let plain = s.shot();
    s.keys(&["F9"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) > 2000),
        "F9 did not open the menu"
    );
    std::thread::sleep(Duration::from_millis(400));
    let files = s.shot();
    s.keys(&["Right"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&files, &s.shot()) > 2000),
        "Right did not switch to the next menu"
    );
    std::thread::sleep(Duration::from_millis(400));
    save(&s, "preg3-menubar-right");
    s.keys(&["Left"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&files, &s.shot()) < 50),
        "Left did not switch back to the first menu"
    );
    s.keys(&["Right", "Escape"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) < 50),
        "Esc did not close the menu and restore the panel"
    );
    // The panel has the keys again: Down/Up move its cursor.
    s.keys(&["Down"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) > 100),
        "Down did not reach the panel after the menu closed"
    );
    s.keys(&["Up"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) < 50),
        "Up did not move the cursor back"
    );
    // First entry of the first menu is View: it opens the viewer on a.txt.
    s.keys(&["F9"]);
    std::thread::sleep(Duration::from_millis(500));
    s.keys(&["Return"]);
    assert!(
        wait_until(Duration::from_secs(10), || window_count(&s) >= 2),
        "Enter did not run the focused menu entry"
    );
}

/// F1 opens the Help dialog (searchable command list) and Esc closes it.
#[test]
fn f1_opens_help_and_esc_closes_it() {
    if !tools_available() {
        return;
    }
    let s = launch("");
    let plain = s.shot();
    s.keys(&["F1"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) > 10000),
        "F1 did not open the Help dialog"
    );
    std::thread::sleep(Duration::from_millis(400));
    save(&s, "preg3-help");

    // Typing filters the list: a rare substring changes the dialog visibly.
    let unfiltered = s.shot();
    s.keys(&["b", "a", "t", "c", "h"]);
    assert!(
        wait_until(Duration::from_secs(5), || diff(&unfiltered, &s.shot())
            > 1000),
        "the Help search did not filter"
    );
    std::thread::sleep(Duration::from_millis(300));
    save(&s, "preg3-help-filtered");

    s.keys(&["Escape"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&plain, &s.shot()) < 50),
        "Esc did not close the Help dialog"
    );
}

/// F3 on a directory starts the size calculation (F19): no viewer opens and the folder's size
/// appears in the size column (full view).
#[test]
fn f3_on_a_directory_calculates_its_size() {
    if !tools_available() {
        return;
    }
    let s = launch("[panels]\ndefault_view = \"full\"\n");
    assert_eq!(window_count(&s), 1);
    s.keys(&["Down"]); // `..` -> sub
    std::thread::sleep(Duration::from_millis(400));
    let before = s.shot();
    s.keys(&["F3"]);
    // The size column cell of `sub` changes ("<DIR>" -> the calculated size).
    assert!(
        wait_until(Duration::from_secs(15), || diff(&before, &s.shot()) > 30),
        "F3 on a directory did not show a calculated size"
    );
    std::thread::sleep(Duration::from_millis(300));
    save(&s, "preg3-f3-dirsize");
    assert_eq!(
        window_count(&s),
        1,
        "F3 on a directory must not open the viewer"
    );
}

/// Type `sleep 2` into the command line and press Enter; returns the shot of the panels before,
/// once the terminal has replaced them.
fn run_sleep(s: &Running) -> Shot {
    // The shell must be at its first prompt before a line is fed to it.
    std::thread::sleep(Duration::from_secs(2));
    let panels = s.shot();
    s.keys(&["s", "l", "e", "e", "p", "space", "2", "Return"]);
    assert!(
        wait_until(Duration::from_secs(10), || diff(&panels, &s.shot())
            > 100_000),
        "the command did not bring up the terminal"
    );
    panels
}

/// `[terminal] return_to_panels = true`: the panels come back when the command ends; by default
/// the terminal stays up.
#[test]
fn return_to_panels_brings_the_panels_back() {
    if !tools_available() {
        return;
    }
    let s = launch("[terminal]\nreturn_to_panels = true\n");
    let panels = run_sleep(&s);
    assert!(
        wait_until(Duration::from_secs(15), || diff(&panels, &s.shot())
            < 20_000),
        "the panels did not come back after the command"
    );
    drop(s);

    let s = launch("");
    let panels = run_sleep(&s);
    std::thread::sleep(Duration::from_secs(4));
    assert!(
        diff(&panels, &s.shot()) > 100_000,
        "by default the terminal stays up"
    );
}

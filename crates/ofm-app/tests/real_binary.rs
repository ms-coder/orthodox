//! Real-binary smoke tests: launch the `ofm` executable under its own Xvfb, send real X key events
//! with `xdotool`, quit with F10 and assert on the saved `session.toml` (plus screenshots for
//! sanity). Library-level tests cannot catch bugs in `app::run()` wiring (e.g. a dropped
//! `AppContext` that silently kills every key), so this is the only test that runs the real
//! start-up path.
//!
//! Key injection: `xdotool` finds and focuses the window, but its `key F5` on the stock Xvfb/evdev
//! keymap is sent as Alt+F5 (the F-keys are CTRL+ALT-type keys and xdotool 3.2016 picks the Alt
//! level), so keys go through a 30-line Python/ctypes helper that calls XTest directly (the same
//! mechanism xdotool uses, minus the keymap guesswork).
//!
//! Skips (with a log line) when `Xvfb`, `xdotool`, `import` (ImageMagick) or `python3` is missing.
//! Needs no outer display: it starts its own Xvfb. The app runs with temp XDG dirs only.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::chunks_exact_to_as_chunks,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::path::{Path, PathBuf};
use std::time::Duration;

use ofm_config::session::PanelSession;
use real_support::{Running, tools_available};

fn cursor(p: &PanelSession) -> Option<String> {
    p.tabs[p.active_tab].cursor_name.clone()
}

fn tab_path(p: &PanelSession, i: usize) -> PathBuf {
    p.tabs[i].path_buf().expect("tab path")
}

/// Tab switches the active panel, Down moves the cursor in that panel only, quit saves it.
#[test]
fn keys_tab_and_cursor_then_quit_saves_session() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    let before = s.shot();
    assert!(before.distinct_colors() >= 6, "window looks blank");
    s.keys(&["Tab", "Down", "Down"]);
    let after = s.shot();
    assert!(
        before.diff_pixels(&after) > 50,
        "screen did not change after Tab/Down/Down: keys are not reaching the app"
    );
    let (session, s) = s.quit();
    assert_eq!(session.active_panel, 1, "Tab did not switch panel");
    assert_eq!(session.panels.len(), 2);
    // Right dir sorted: `..`, x1, x2, x3 -> two Downs = x2.
    assert_eq!(cursor(&session.panels[1]).as_deref(), Some("x2"));
    let left_cursor = cursor(&session.panels[0]);
    assert!(
        matches!(left_cursor.as_deref(), None | Some("..")),
        "left cursor moved: {left_cursor:?}"
    );
    assert_eq!(tab_path(&session.panels[1], 0), s.right);
}

/// Navigation: Down onto `sub`, Enter descends; Backspace goes up and lands back on `sub`.
#[test]
fn keys_enter_dir_and_go_up() {
    if !tools_available() {
        return;
    }
    // Directories first: `..`, sub, a.txt, b.txt.
    let s = Running::launch("");
    s.keys(&["Down", "Return"]);
    let (session, s) = s.quit();
    assert_eq!(
        tab_path(&session.panels[0], 0),
        s.left.join("sub"),
        "Enter did not descend into the directory"
    );

    let s = Running::launch("");
    s.keys(&["Down", "Return", "BackSpace"]);
    let (session, s) = s.quit();
    assert_eq!(
        tab_path(&session.panels[0], 0),
        s.left,
        "Backspace did not go up"
    );
    assert_eq!(
        cursor(&session.panels[0]).as_deref(),
        Some("sub"),
        "cursor should return to the directory we came from"
    );
}

/// Tabs: Ctrl+T twice then Ctrl+W once leaves two tabs; view-mode keys stick.
#[test]
fn keys_tabs_and_view_mode() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    s.keys(&["ctrl+t", "ctrl+t", "ctrl+w", "ctrl+2"]);
    let (session, _s) = s.quit();
    let left = &session.panels[0];
    assert_eq!(left.tabs.len(), 2, "tab.new x2 + tab.close should leave 2");
    assert_eq!(left.active_tab, 1);
    assert_eq!(left.tabs[1].view, "full", "Ctrl+2 did not select Full view");

    let s = Running::launch("[panels]\ndefault_view = \"full\"\n");
    s.keys(&["ctrl+1"]);
    let (session, _s) = s.quit();
    assert_eq!(session.panels[0].tabs[0].view, "brief");
}

/// F9 opens the menu bar popover (screen changes), Escape closes it again, quit still works.
#[test]
fn keys_f9_opens_menu_and_escape_closes() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    let closed = s.shot();
    s.keys(&["F9"]);
    std::thread::sleep(Duration::from_millis(300));
    let open = s.shot();
    assert!(
        closed.diff_pixels(&open) > 500,
        "F9 did not visibly open a menu ({} px differ)",
        closed.diff_pixels(&open)
    );
    s.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(300));
    let again = s.shot();
    assert!(
        open.diff_pixels(&again) > 500,
        "Escape did not close the menu"
    );
    let (session, _s) = s.quit();
    assert_eq!(session.panels.len(), 2, "quit after menu did not save");
}

/// Screenshot of the real binary (`target/screenshots/real-binary-smoke.png`), must not be blank.
#[test]
fn real_binary_screenshot_is_not_blank() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    assert!(s.shot().distinct_colors() >= 6);
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join("real-binary-smoke.png"));
}

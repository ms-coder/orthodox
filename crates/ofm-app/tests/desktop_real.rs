//! T8.4 real-binary tests (F31, F33): the real `ofm` under a private Xvfb with real key events
//! and real mouse drags (see `real_support`): Enter runs an executable script in the terminal,
//! Ctrl+C / Ctrl+X / Ctrl+V copy and move files between two directories, a drag from one panel to
//! the other copies (Shift: moves), and the session (tabs, cursors, active panel, window size)
//! survives quitting and starting again. Screenshots land in `target/screenshots/`.
//! Skips when the harness tools are missing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::path::Path;
use std::time::{Duration, Instant};

use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, tools_available};

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

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

const CONFIG: &str = "[terminal]\nshell = \"/bin/sh\"\nscrollback = 1000\n";

/// Enter on an executable script runs it in the terminal with the panel directory as the working
/// directory, shows its output until Enter, then the panels come back.
#[test]
fn enter_runs_an_executable_script_in_the_terminal() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .file("other.txt", FileSpec::content(b"text"))
        .file(
            "run.sh",
            FileSpec::content(b"#!/bin/sh\npwd > ran.txt\necho HELLO-FROM-SCRIPT\n").mode(0o755),
        );
    let right = TreeSpec::new().file("x", FileSpec::content(b"x"));
    let s = Running::launch_with(CONFIG, &left, &right);
    // `..`, other.txt, run.sh: the cursor goes to run.sh first, so the comparison shots below
    // show the same cursor row before and after the run.
    s.keys(&["Down", "Down"]);
    let panels = s.shot();
    s.keys(&["Return"]);
    let marker = s.left.join("ran.txt");
    assert!(
        wait_until(Duration::from_secs(10), || marker.exists()),
        "the script did not run"
    );
    // The working directory is the panel directory.
    let cwd = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(Path::new(cwd.trim()), s.left.as_path());
    // The terminal holds the screen (the panels are hidden): the frames and columns are gone.
    std::thread::sleep(Duration::from_millis(1000));
    let term = s.shot();
    save(&s, "desktop-script-terminal");
    let away = term.diff_pixels(&panels);
    assert!(away > 5_000, "the terminal did not take over ({away})");
    // Enter returns to the panels (the new `ran.txt` shows up in the list: a small difference).
    s.keys(&["Return"]);
    assert!(
        wait_until(Duration::from_secs(10), || s.shot().diff_pixels(&panels)
            < away / 2),
        "the panels did not come back"
    );
    save(&s, "desktop-script-back");
    // And the app still takes keys: Ctrl+O shows the terminal view again (the key dispatcher is
    // alive and the focus is back on the panels).
    let back = s.shot();
    s.keys(&["ctrl+o"]);
    assert!(wait_until(Duration::from_secs(5), || s
        .shot()
        .diff_pixels(&back)
        > 5_000));
    s.keys(&["ctrl+o"]);
}

/// Ctrl+C then Ctrl+V in the other panel copies; Ctrl+X then Ctrl+V moves.
#[test]
fn clipboard_keys_copy_and_move_between_panels() {
    if !tools_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    let (l, r) = (s.left.clone(), s.right.clone());
    // `..`, sub, a.txt, b.txt: cursor to a.txt.
    s.keys(&["Down", "Down", "ctrl+c", "Tab", "ctrl+v"]);
    assert!(
        wait_until(Duration::from_secs(10), || r.join("a.txt").exists()),
        "Ctrl+V did not copy"
    );
    assert_eq!(std::fs::read(r.join("a.txt")).unwrap(), b"a");
    assert!(l.join("a.txt").exists(), "copy keeps the source");

    // Back to the left panel, cursor to b.txt (the cursor is on a.txt), cut, paste right.
    s.keys(&["Tab", "Down", "ctrl+x", "Tab", "ctrl+v"]);
    assert!(
        wait_until(Duration::from_secs(10), || r.join("b.txt").exists()
            && !l.join("b.txt").exists()),
        "Ctrl+X, Ctrl+V did not move"
    );
    assert_eq!(std::fs::read(r.join("b.txt")).unwrap(), b"b");
    save(&s, "desktop-clipboard-keys");
}

/// xdotool drag from the left panel's `a.txt` row to the right panel: copy, and with Shift move.
#[test]
fn dragging_a_file_to_the_other_panel_copies_and_shift_moves() {
    if !tools_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    let (l, r) = (s.left.clone(), s.right.clone());
    // Rows (brief view): `..` y=80, sub y=98, a.txt y=117, b.txt y=135; left panel x=45.
    let drag = |from: (i32, i32), to: (i32, i32), shift: bool| {
        let (fx, fy) = (from.0.to_string(), from.1.to_string());
        let (tx, ty) = (to.0.to_string(), to.1.to_string());
        let mid = ((from.0 + to.0) / 2).to_string();
        let midy = ((from.1 + to.1) / 2).to_string();
        let steps: Vec<Vec<&str>> = {
            let mut v = vec![
                vec!["mousemove", &fx, &fy],
                vec!["mousedown", "1"],
                vec!["mousemove_relative", "--", "12", "12"],
                vec!["mousemove", &mid, &midy],
                vec!["mousemove", &tx, &ty],
            ];
            if shift {
                v.push(vec!["keydown", "shift"]);
            }
            v.push(vec!["mousemove_relative", "--", "2", "2"]);
            v.push(vec!["mouseup", "1"]);
            if shift {
                v.push(vec!["keyup", "shift"]);
            }
            v
        };
        for step in steps {
            let out = s.x("xdotool", &step);
            assert!(out.status.success(), "xdotool {step:?}");
            std::thread::sleep(Duration::from_millis(200));
        }
    };
    drag((45, 117), (900, 400), false);
    assert!(
        wait_until(Duration::from_secs(10), || r.join("a.txt").exists()),
        "drop did not copy a.txt"
    );
    assert!(l.join("a.txt").exists(), "a plain drop copies");
    save(&s, "desktop-drop-copy");
    // b.txt moves with Shift held. (After the copy the right panel got a.txt; b.txt is the row
    // below a.txt on the left.)
    drag((45, 135), (900, 400), true);
    assert!(
        wait_until(Duration::from_secs(10), || r.join("b.txt").exists()),
        "Shift drop did not transfer b.txt"
    );
    assert!(
        wait_until(Duration::from_secs(5), || !l.join("b.txt").exists()),
        "Shift drop must move (source removed)"
    );
}

/// Tabs, paths, cursor names, the active panel and the window size come back after a restart.
#[test]
fn session_survives_a_restart() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new().file("a.txt", FileSpec::content(b"a")).dir(
        "sub",
        TreeSpec::new()
            .file("s1", FileSpec::content(b"1"))
            .file("s2", FileSpec::content(b"22"))
            .file("s3", FileSpec::content(b"333")),
    );
    let right = TreeSpec::new()
        .file("x1", FileSpec::content(b"x"))
        .file("x2", FileSpec::content(b"x"))
        .file("x3", FileSpec::content(b"x"));
    let mut s = Running::launch_with("", &left, &right);
    let wid = s.wait_for_window();
    s.x("xdotool", &["windowsize", &wid, "1100", "700"]);
    std::thread::sleep(Duration::from_millis(500));
    // Left: into sub (`..`, sub, a.txt -> Down, Enter), cursor on s2, a new tab, cursor on s3.
    // Right: cursor on x2.
    s.keys(&[
        "Down", "Return", "Down", "Down", "ctrl+t", "Down", "Down", "Down", "Tab", "Down", "Down",
    ]);
    let (first, mut s) = s.quit();
    assert_eq!(first.active_panel, 1, "{first:?}");
    assert_eq!(first.panels.len(), 2);
    let sub = s.left.join("sub");
    let lt = &first.panels[0];
    assert_eq!(lt.tabs.len(), 2, "{lt:?}");
    assert_eq!(lt.active_tab, 1);
    assert_eq!(lt.tabs[0].path_buf().unwrap(), sub);
    assert_eq!(lt.tabs[1].path_buf().unwrap(), sub);
    assert_eq!(lt.tabs[0].cursor_name.as_deref(), Some("s2"));
    assert_eq!(lt.tabs[1].cursor_name.as_deref(), Some("s3"));
    assert_eq!(first.panels[1].tabs[0].cursor_name.as_deref(), Some("x2"));
    // The saved size is the content size (the window's client-side shadow is not part of it).
    assert!(
        (1080..=1100).contains(&first.window.width) && (680..=700).contains(&first.window.height),
        "{:?}",
        first.window
    );

    // Start again without directory arguments: the session is restored, so quitting right away
    // writes the same state.
    s.restart(&[]);
    // The restored listings and cursors arrive from worker threads: let them land (a loaded
    // machine needs longer than `wait_for_content` gives).
    std::thread::sleep(Duration::from_secs(2));
    save(&s, "desktop-session-restored");
    let (second, _s) = s.quit();
    assert_eq!(second.active_panel, first.active_panel);
    assert_eq!(second.panels, first.panels);
    assert_eq!(second.window, first.window);
}

//! T4.1 real-binary tests for the embedded terminal (F12, ADR-0003): Ctrl+O toggles the view,
//! typed commands reach the shell, panel navigation syncs the shell's cwd and a shell `cd`
//! moves the panel. Launches the real `ofm` under a private Xvfb and injects real key events
//! (see `real_support` and `real_binary.rs` for the harness rationale).
//!
//! Every test spawns the shell as `/bin/bash` via `[terminal]` in the config (deterministic
//! regardless of the caller's `$SHELL`) and skips when the harness tools or bash are missing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::time::{Duration, Instant};

use real_support::{Running, Shot, tools_available};

/// Deterministic bash spawn: config beats `$SHELL` (and the harness HOME is isolated).
const CONFIG: &str = "[terminal]\nshell = \"/bin/bash\"\nscrollback = 1000\n";

fn bash_available() -> bool {
    let ok = real_support::have("bash");
    if !ok {
        eprintln!("skipping: bash not installed");
    }
    ok
}

fn wait_until(timeout: Duration, mut done: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + timeout;
    while Instant::now() < end {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    false
}

/// The center of the window: file lists render text here, the empty terminal does not.
fn center(s: &Shot) -> usize {
    const X0: usize = 100;
    const Y0: usize = 300;
    const W: usize = 1400;
    const H: usize = 500;
    let mut set = std::collections::HashSet::new();
    for y in Y0..Y0 + H {
        let row = y * s.w * 3;
        for x in X0..X0 + W {
            let p = row + x * 3;
            set.insert([s.px[p], s.px[p + 1], s.px[p + 2]]);
        }
    }
    set.len()
}

/// Type text into the focused widget (the VTE) and press Enter afterwards.
fn type_line(s: &Running, text: &str) {
    let out = s.x(
        "xdotool",
        &["type", "--delay", "40", "--clearmodifiers", "--", text],
    );
    assert!(out.status.success(), "xdotool type failed");
    s.keys(&["Return"]);
}

/// Ctrl+O hides the panels (terminal full size) and pressing it again brings them back.
#[test]
fn ctrl_o_toggles_panels_and_terminal() {
    if !tools_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    let before = s.shot();
    let colors_before = center(&before);

    s.keys(&["ctrl+o"]);
    let hidden = wait_until(Duration::from_secs(10), || {
        s.shot().diff_pixels(&before) > 2000
    });
    assert!(
        hidden,
        "Ctrl+O did not change the view (terminal did not show)"
    );
    // The terminal surface is nearly uniform: far fewer colors than two file lists.
    let term_colors = center(&s.shot());
    assert!(
        term_colors < colors_before,
        "terminal should replace the file lists: {term_colors} vs {colors_before} colors"
    );

    s.keys(&["ctrl+o"]);
    let restored = wait_until(Duration::from_secs(10), || {
        // The lists are back (pixel-perfect equality is not reachable: the panel grabbed
        // the focus and now draws its focus outline).
        center(&s.shot()) + 2 >= colors_before
    });
    assert!(restored, "second Ctrl+O did not bring the panels back");
}

/// With the terminal focused, a typed command runs in the shell: the file it creates appears.
#[test]
fn typed_command_creates_file_in_panel_dir() {
    if !tools_available() || !bash_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    s.keys(&["ctrl+o"]);
    // The bash prompt (text pixels) means the shell is up and has the focus.
    let ready = wait_until(Duration::from_secs(20), || center(&s.shot()) >= 3);
    assert!(ready, "shell prompt never appeared in the terminal");

    let made = s.left.join("touched_by_terminal.txt");
    type_line(&s, "touch touched_by_terminal.txt");
    assert!(
        wait_until(Duration::from_secs(15), || made.exists()),
        "typed command did not create {made:?}"
    );
}

/// Entering a subdirectory in the panel feeds a `cd` into the idle shell (ADR-0003 sync):
/// a file created by the shell afterwards lands in the new directory.
#[test]
fn panel_navigation_syncs_shell_cwd() {
    if !tools_available() || !bash_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    // Left lists directories first: `..`, sub, a.txt, b.txt. (Enter is no good here: the
    // F-key bar button that holds the initial focus consumes it — a chord the button
    // ignores does the job.)
    s.keys(&["Down", "ctrl+Next"]);
    let sub = s.left.join("sub");

    s.keys(&["ctrl+o"]);
    let ready = wait_until(Duration::from_secs(20), || center(&s.shot()) >= 3);
    assert!(ready, "shell prompt never appeared in the terminal");

    let marker = sub.join("marker.txt");
    type_line(&s, "touch marker.txt");
    assert!(
        wait_until(Duration::from_secs(15), || marker.exists()),
        "marker did not appear in the panel's new directory {sub:?} (cd sync broken?)"
    );
    assert!(
        !s.left.join("marker.txt").exists(),
        "marker landed in the old directory: the shell was not synced"
    );
}

/// The synced `cd` carries the directory's raw bytes; VTE reports them back as committed text.
/// A name that is not UTF-8 must neither abort the app nor miss the shell.
#[test]
fn panel_navigation_into_a_non_utf8_directory_syncs_the_shell() {
    if !tools_available() || !bash_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    let odd = s.left.join(OsStr::from_bytes(b"a-\xff"));
    std::fs::create_dir(&odd).unwrap();
    std::thread::sleep(Duration::from_secs(1)); // the watcher adds it before `sub`
    s.keys(&["Down", "ctrl+Next"]);

    s.keys(&["ctrl+o"]);
    let ready = wait_until(Duration::from_secs(20), || center(&s.shot()) >= 3);
    assert!(ready, "shell prompt never appeared in the terminal");
    type_line(&s, "touch marker.txt");
    assert!(
        wait_until(Duration::from_secs(15), || odd.join("marker.txt").exists()),
        "marker did not appear in {odd:?}"
    );
}

/// A `cd` typed in the shell moves the active panel (OSC 7, ADR-0003): the session saved at
/// quit records the panel inside the subdirectory.
#[test]
fn shell_cd_moves_panel() {
    if !tools_available() || !bash_available() {
        return;
    }
    let s = Running::launch(CONFIG);
    s.keys(&["ctrl+o"]);
    let ready = wait_until(Duration::from_secs(20), || center(&s.shot()) >= 3);
    assert!(ready, "shell prompt never appeared in the terminal");

    type_line(&s, "cd sub");
    // The OSC 7 reply travels shell -> OFM and navigates the (hidden) panel; give it time,
    // then assert on the session saved at quit.
    std::thread::sleep(Duration::from_secs(2));

    let (session, s) = s.quit();
    let panel = &session.panels[0];
    let path = panel.tabs[panel.active_tab].path_buf().expect("tab path");
    assert_eq!(
        path,
        s.left.join("sub"),
        "typing `cd sub` in the shell did not move the panel (OSC 7 not wired?)"
    );
}

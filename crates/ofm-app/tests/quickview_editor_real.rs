//! T5.2b real-binary tests: quick view (F22, Ctrl+Q) and the external editor (F23, F4/Shift+F4)
//! through the real `ofm` under a private Xvfb with real key events (see `real_support`).
//!
//! Editors are tiny shell scripts that write the path they were given into a marker file;
//! they are chosen through `$EDITOR` (GUI path, launched detached) and through `[editor]
//! terminal = true` (runs inside the embedded terminal). Skips when the harness tools are
//! missing. Screenshots land in `target/screenshots/` for a visual check.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::ffi::OsStr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ofm_testkit::temp::TempDir;
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

/// `<dir>/editor.sh` writing its first argument to `<dir>/marker`.
fn marker_editor(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join("marker");
    let script = dir.join("editor.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

fn trees() -> (TreeSpec, TreeSpec) {
    let text: String = (1..=60)
        .map(|i| format!("quick view sample line number {i}\n"))
        .collect();
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(text.as_bytes()))
        .file("b.bin", FileSpec::content(&[0, 1, 2, 3, 0, 255, 0, 7]))
        .dir(
            "sub",
            TreeSpec::new()
                .file("one", FileSpec::content(b"1"))
                .file("two", FileSpec::content(b"22")),
        );
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    (left, right)
}

fn launch(config: &str, env: &[(&str, &OsStr)]) -> Running {
    let (l, r) = trees();
    Running::launch_env(config, &std::env::temp_dir(), &l, &r, env)
}

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

/// Number of pixels that differ in the right half of the window.
fn right_half_diff(a: &Shot, b: &Shot) -> usize {
    let half = a.w / 2;
    let mut n = 0;
    for y in 0..a.h {
        for x in half..a.w {
            let p = (y * a.w + x) * 3;
            if a.px[p..p + 3] != b.px[p..p + 3] {
                n += 1;
            }
        }
    }
    n
}

/// The preview pane takes its colours from the theme: it draws in all three built-in themes
/// (screenshots `quickview-<theme>-{text,dir}.png`).
#[test]
fn quick_view_draws_in_every_theme() {
    if !tools_available() {
        return;
    }
    for theme in ["far-blue", "commander-light", "adwaita"] {
        let s = launch(&format!("[theme]\nname = \"{theme}\"\n"), &[]);
        let plain = s.shot();
        s.keys(&["Down", "Down", "ctrl+q"]);
        assert!(
            wait_until(Duration::from_secs(10), || right_half_diff(
                &plain,
                &s.shot()
            ) > 2000),
            "{theme}: no preview drawn"
        );
        std::thread::sleep(Duration::from_millis(400));
        save(&s, &format!("quickview-{theme}-text"));
        s.keys(&["Up", "Up"]);
        std::thread::sleep(Duration::from_millis(500));
        save(&s, &format!("quickview-{theme}-dir"));
    }
}

/// Ctrl+Q turns the right panel into a preview that follows the cursor and Ctrl+Q restores it.
#[test]
fn quick_view_follows_the_cursor_and_restores_the_panel() {
    if !tools_available() {
        return;
    }
    let s = launch("", &[]);
    let plain = s.shot();
    s.keys(&["Down", "Down"]); // `..` -> sub -> a.txt
    s.keys(&["ctrl+q"]);
    assert!(
        wait_until(Duration::from_secs(10), || right_half_diff(
            &plain,
            &s.shot()
        ) > 2000),
        "Ctrl+Q did not draw a preview in the other panel"
    );
    std::thread::sleep(Duration::from_millis(400));
    let text = s.shot();
    save(&s, "quickview-text");

    // The cursor moves to the binary file: the preview changes by itself.
    s.keys(&["Down"]);
    assert!(
        wait_until(Duration::from_secs(10), || right_half_diff(
            &text,
            &s.shot()
        ) > 1000),
        "the preview did not follow the cursor"
    );
    std::thread::sleep(Duration::from_millis(300));
    save(&s, "quickview-binary");

    // Up twice: the directory summary.
    s.keys(&["Up", "Up"]);
    std::thread::sleep(Duration::from_millis(600));
    save(&s, "quickview-dir");

    // Back to the original cursor row, then Ctrl+Q: the other panel is as before.
    s.keys(&["Down", "Down"]);
    s.keys(&["Up", "Up"]);
    s.keys(&["Home"]);
    s.keys(&["ctrl+q"]);
    s.keys(&["Down"]); // no-op settle
    let restored = wait_until(Duration::from_secs(10), || {
        right_half_diff(&plain, &s.shot()) == 0
    });
    assert!(
        restored,
        "the other panel was not restored by the second Ctrl+Q"
    );
}

/// F4 with `$EDITOR` set to a script runs it detached with the file under the cursor.
#[test]
fn f4_runs_the_editor_from_the_environment() {
    if !tools_available() {
        return;
    }
    let tools = TempDir::new().unwrap();
    let (script, marker) = marker_editor(tools.path());
    let s = launch(
        "",
        &[("EDITOR", script.as_os_str()), ("VISUAL", OsStr::new(""))],
    );
    s.keys(&["Down", "Down"]); // a.txt
    s.keys(&["F4"]);
    assert!(
        wait_until(Duration::from_secs(15), || marker.exists()
            && std::fs::read(&marker).is_ok_and(|b| !b.is_empty())),
        "F4 did not start the editor"
    );
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        s.left.join("a.txt").as_os_str().as_encoded_bytes()
    );
}

/// Shift+F4 asks for a name, creates the empty file and opens it in the editor.
#[test]
fn shift_f4_creates_and_edits_a_new_file() {
    if !tools_available() {
        return;
    }
    let tools = TempDir::new().unwrap();
    let (script, marker) = marker_editor(tools.path());
    let s = launch(
        "",
        &[("EDITOR", script.as_os_str()), ("VISUAL", OsStr::new(""))],
    );
    s.keys(&["shift+F4"]);
    save(&s, "newfile-dialog");
    s.keys(&["n", "e", "w", "period", "t", "x", "t", "Return"]);
    let made = s.left.join("new.txt");
    assert!(
        wait_until(Duration::from_secs(15), || marker.exists()
            && std::fs::read(&marker).is_ok_and(|b| !b.is_empty())),
        "Shift+F4 did not start the editor"
    );
    assert!(made.exists(), "the new file was not created");
    assert_eq!(std::fs::metadata(&made).unwrap().len(), 0);
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        made.as_os_str().as_encoded_bytes()
    );
}

/// A terminal editor (`[editor] terminal = true`) runs inside the embedded terminal and the
/// panels come back when it exits.
#[test]
fn terminal_editor_runs_in_the_embedded_terminal() {
    if !tools_available() || !real_support::have("bash") {
        return;
    }
    let tools = TempDir::new().unwrap();
    let (script, marker) = marker_editor(tools.path());
    let config = format!(
        "[terminal]\nshell = \"/bin/bash\"\n[editor]\ncommand = \"{} %f\"\nterminal = true\n",
        script.display()
    );
    let s = launch(&config, &[]);
    // Wait until the shell is at its prompt (it must be idle for F4 to type into it).
    s.keys(&["ctrl+o"]);
    std::thread::sleep(Duration::from_millis(2500));
    s.keys(&["ctrl+o"]);
    let panels = s.shot();
    s.keys(&["Down", "Down"]); // a.txt
    s.keys(&["F4"]);
    assert!(
        wait_until(Duration::from_secs(20), || marker.exists()
            && std::fs::read(&marker).is_ok_and(|b| !b.is_empty())),
        "the terminal editor did not run"
    );
    assert_eq!(
        std::fs::read(&marker).unwrap(),
        s.left.join("a.txt").as_os_str().as_encoded_bytes()
    );
    // The editor exited: the shell is idle again and the panels are back (cursor moved by
    // two rows, so only check that the terminal is not the full-size view any more).
    let back = wait_until(Duration::from_secs(10), || {
        let now = s.shot();
        now.distinct_colors() >= panels.distinct_colors().saturating_sub(4)
    });
    save(&s, "editor-terminal-after");
    assert!(
        back,
        "the panels did not come back after the terminal editor exited"
    );
}

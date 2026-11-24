//! T8.3c real-binary tests: batch rename (F20) through the real `ofm` under a private Xvfb with
//! real key events (see `real_support`): Ctrl+Shift+M, a scripted editor swaps two names, Enter in
//! the preview applies, the names on disk are checked, Ctrl+Alt+Z undoes it. Once with a detached
//! GUI editor (the "Waiting for your editor" dialog, Enter = Apply) and once with a terminal
//! editor (`[editor] terminal = true`: the flow continues when the editor exits).
//! Skips when the harness tools are missing. Screenshots land in `target/screenshots/`.
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

/// `<dir>/editor.sh`: swaps the lines `a.txt` and `b.txt` of the file it is given, then writes
/// `<dir>/marker`.
fn swap_editor(dir: &Path) -> (PathBuf, PathBuf) {
    let marker = dir.join("marker");
    let script = dir.join("editor.sh");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nsed -i 's/^a\\.txt$/TMP/; s/^b\\.txt$/a.txt/; s/^TMP$/b.txt/' \"$1\"\necho done > '{}'\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, marker)
}

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

fn text(dir: &Path, name: &str) -> String {
    std::fs::read_to_string(dir.join(name)).unwrap_or_default()
}

fn launch(config: &str, env: &[(&str, &OsStr)]) -> Running {
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("b.txt", FileSpec::content(b"b"))
        .dir(
            "sub",
            TreeSpec::new().file("inner", FileSpec::content(b"i")),
        );
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    Running::launch_env(config, &std::env::temp_dir(), &left, &right, env)
}

/// Select a.txt and b.txt (`..`, sub, a.txt, b.txt: two Downs, then Insert twice).
fn select_both(s: &Running) {
    s.keys(&["Down", "Down", "Insert", "Insert"]);
}

/// The common part after the editor has been started: apply the preview with Enter, check the
/// swap on disk, undo with Ctrl+Alt+Z and check the original names are back.
fn apply_and_undo(s: &Running, shot: &str) {
    // Preview is up: Enter applies.
    std::thread::sleep(Duration::from_millis(800));
    save(s, &format!("{shot}-preview"));
    s.keys(&["Return"]);
    assert!(
        wait_until(Duration::from_secs(15), || text(&s.left, "a.txt") == "b"
            && text(&s.left, "b.txt") == "a"),
        "the names were not swapped on disk: a.txt={:?} b.txt={:?}",
        text(&s.left, "a.txt"),
        text(&s.left, "b.txt")
    );
    // No temp name is left behind.
    let mut names: Vec<String> = std::fs::read_dir(&s.left)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["a.txt", "b.txt", "sub"]);
    // The undo log is under the state directory.
    let last = s.paths.state_dir.join("batchrename/last.toml");
    assert!(
        wait_until(Duration::from_secs(10), || last.exists()),
        "no undo log at {}",
        last.display()
    );

    // Ctrl+Alt+Z: reverse preview, Enter undoes.
    s.keys(&["ctrl+alt+z"]);
    std::thread::sleep(Duration::from_millis(800));
    save(s, &format!("{shot}-undo-preview"));
    s.keys(&["Return"]);
    assert!(
        wait_until(Duration::from_secs(15), || text(&s.left, "a.txt") == "a"
            && text(&s.left, "b.txt") == "b"),
        "undo did not restore the names: a.txt={:?} b.txt={:?}",
        text(&s.left, "a.txt"),
        text(&s.left, "b.txt")
    );
    assert!(
        wait_until(Duration::from_secs(10), || !last.exists()),
        "the undo log must be removed after a complete undo"
    );
}

/// A detached editor (`$EDITOR` script): the waiting dialog appears, Enter applies the edit.
#[test]
fn ctrl_shift_m_swaps_names_with_a_gui_editor_and_undo_restores_them() {
    if !tools_available() {
        return;
    }
    let tools = TempDir::new().unwrap();
    let (script, marker) = swap_editor(tools.path());
    let s = launch(
        "",
        &[("EDITOR", script.as_os_str()), ("VISUAL", OsStr::new(""))],
    );
    select_both(&s);
    s.keys(&["ctrl+shift+m"]);
    assert!(
        wait_until(Duration::from_secs(15), || marker.exists()),
        "Ctrl+Shift+M did not start the editor"
    );
    std::thread::sleep(Duration::from_millis(500));
    save(&s, "batchrename-real-waiting");
    s.keys(&["Return"]); // Apply in the waiting dialog
    apply_and_undo(&s, "batchrename-real-gui");
}

/// A terminal editor runs in the embedded terminal; when it exits the preview comes up by itself.
#[test]
fn ctrl_shift_m_with_a_terminal_editor_continues_when_the_editor_exits() {
    if !tools_available() || !real_support::have("bash") {
        return;
    }
    let tools = TempDir::new().unwrap();
    let (script, marker) = swap_editor(tools.path());
    let config = format!(
        "[terminal]\nshell = \"/bin/bash\"\n[editor]\ncommand = \"{} %f\"\nterminal = true\n",
        script.display()
    );
    let s = launch(&config, &[]);
    select_both(&s);
    s.keys(&["ctrl+shift+m"]);
    assert!(
        wait_until(Duration::from_secs(20), || marker.exists()),
        "the terminal editor did not run"
    );
    apply_and_undo(&s, "batchrename-real-terminal");
}

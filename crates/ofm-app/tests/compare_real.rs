//! T8.1c real-binary tests: "compare directories" (Shift+F2, F27) through the real `ofm` under a
//! private Xvfb with real key events (see `real_support`). The selection is observed through its
//! effect: F5 copies exactly the selected files to the other panel, so the file system tells
//! which entries the comparison marked (a wrongly marked file that exists on the other side would
//! raise the conflict dialog and stall the copy). Skips when the harness tools are missing.
//! Screenshots land in `target/screenshots/` (`compare-dialog-*.png`) for a visual check.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::collections::BTreeSet;
use std::path::Path;
use std::time::{Duration, Instant};

use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, tools_available};

fn names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect()
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

fn wait_names(what: &str, dir: &Path, want: &BTreeSet<String>) {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if &names(dir) == want {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    panic!("{what}: {:?} != {want:?}", names(dir));
}

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

/// Shift+F2 + Enter with the default options marks what is missing or different on each side;
/// F5 then copies the left marks (and, after Tab, the right ones) and nothing else.
#[test]
fn shift_f2_enter_marks_the_differences_in_both_panels() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .file("common", FileSpec::content(b"same"))
        .file("only-l1", FileSpec::content(b"1"))
        .file("only-l2", FileSpec::content(b"2"));
    let right = TreeSpec::new()
        .file("common", FileSpec::content(b"same"))
        .file("only-r1", FileSpec::content(b"r"));
    let s = Running::launch_with("", &left, &right);

    s.keys(&["shift+F2"]);
    save(&s, "compare-dialog-open");
    s.keys_paced(&["Return"], 800);
    save(&s, "compare-marked");

    // Left marks: only-l1, only-l2 -> copied to the right.
    s.keys(&["F5", "Return"]);
    wait_names(
        "left marks copied right",
        &s.right,
        &set(&["common", "only-r1", "only-l1", "only-l2"]),
    );
    // Right marks (still only-r1): Tab, F5 copies it to the left.
    s.keys(&["Tab", "F5", "Return"]);
    wait_names(
        "right mark copied left",
        &s.left,
        &set(&["common", "only-l1", "only-l2", "only-r1"]),
    );
}

/// Esc leaves the dialog without marking anything and without wedging the keymap; the option
/// mnemonics work (Alt+M = "only missing on the other side"), so the differing file is not marked
/// and the copy never meets a conflict.
#[test]
fn escape_cancels_and_mnemonics_choose_the_marks() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .file("common", FileSpec::content(b"same"))
        .file("differs", FileSpec::content(b"longer content"))
        .file("only-l", FileSpec::content(b"1"));
    let right = TreeSpec::new()
        .file("common", FileSpec::content(b"same"))
        .file("differs", FileSpec::content(b"x"));
    let s = Running::launch_with("", &left, &right);

    s.keys(&["shift+F2", "Escape"]);
    // The dialog is gone: reopen it, choose "only missing on the other side", accept.
    s.keys(&["shift+F2"]);
    s.keys(&["alt+m"]);
    save(&s, "compare-dialog-missing");
    s.keys_paced(&["Return"], 800);
    s.keys(&["F5", "Return"]);
    wait_names(
        "only the missing file copied",
        &s.right,
        &set(&["common", "differs", "only-l"]),
    );
    assert_eq!(std::fs::read(s.right.join("differs")).unwrap(), b"x");
}

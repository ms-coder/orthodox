//! End-to-end file-operation tests: the real `ofm` binary under its own Xvfb, driven with real X
//! key events (F5/F6/F7/F8/Shift+F6/Ctrl+J/F10 and the dialogs' own keys), asserting on the FILE
//! SYSTEM afterwards (never on pixels, except "something opened" sanity checks). The engine is the
//! real one (`ofm-jobs`), so these tests prove the UI wiring, the dialogs and the engine together.
//!
//! Harness: `tests/real_support/mod.rs` (shared with `real_binary.rs`). Skips with a log line when
//! Xvfb/xdotool/import/python3 are missing. Every test starts its own Xvfb and app with temp
//! XDG/HOME dirs, so nothing touches the user's files.
//!
//! Slow tests (large data, ~1 GB of disk traffic) are `#[ignore]`d unless `OFM_E2E_SLOW=1` is set:
//! `OFM_E2E_SLOW=1 cargo test -p ofm-app --test real_binary_fileops -- --include-ignored`
//! (they use a directory under `target/` = a real disk, because `/tmp` is a tmpfs on Ubuntu and a
//! 100 MB copy there finishes faster than a key press can cancel it).
//!
//! Keys used (far preset): Ins marks and moves down, Ctrl+Num+ selects all, F5 copy, F6 move,
//! Shift+F6 rename, F7 mkdir, F8 delete (to trash), Shift+F8 delete permanently, Ctrl+J jobs view
//! (Del = cancel job, Ctrl+J again closes it; Alt+P = Pause button), F10 quit. Dialogs: Enter =
//! `dialog.ok` (also in a text entry), F2 = `dialog.queue`, Ctrl+A = `dialog.apply_all`. Conflict
//! dialog: Enter = Skip, Alt+S Skip, Alt+O Overwrite, Alt+W overwrite if newer, Alt+A apply to
//! all. A few tests deliberately confirm with the buttons' mnemonics (Alt+M, Alt+R, Alt+C) so the
//! native GTK path stays covered too.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test code runs on the test thread, not the GTK thread
)]

mod real_support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use ofm_testkit::compare::{Compare, assert_trees_equal, compare_trees};
use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, tools_available};

const NS: i128 = 1_000_000_000;
/// Source mtime in the conflict tests (2020-09-13).
const MTIME_SRC: i128 = 1_600_000_000 * NS;
const MTIME_OLDER: i128 = 1_500_000_000 * NS;
const MTIME_NEWER: i128 = 1_700_000_000 * NS;

// ---- helpers ----------------------------------------------------------------------------------

/// Poll `f` every 50 ms until it returns true; panics with `what` after `secs`.
fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if f() {
            return;
        }
        assert!(Instant::now() < end, "timed out after {secs}s: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Every `*.ofm-part-*` temp name below `dir`.
fn parts_in(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return found;
    };
    for e in rd.flatten() {
        let p = e.path();
        if e.file_name().to_string_lossy().contains(".ofm-part-") {
            found.push(p.clone());
        }
        if e.file_type().is_ok_and(|t| t.is_dir()) {
            found.extend(parts_in(&p));
        }
    }
    found
}

fn assert_no_parts(dirs: &[&Path]) {
    for d in dirs {
        let p = parts_in(d);
        assert!(p.is_empty(), "leftover temp files: {p:?}");
    }
}

/// Wait until `dir` equals the reference tree built from `spec` (content, mode, mtime).
fn wait_tree_equals(what: &str, dir: &Path, spec: &TreeSpec, cmp: fn() -> Compare) {
    let reference = spec.build_temp().unwrap();
    wait_until(what, 25, || {
        compare_trees(reference.path(), dir, cmp()).is_empty()
    });
    assert_trees_equal(reference.path(), dir, cmp());
}

fn cmp_content_mtime() -> Compare {
    Compare {
        xattrs: false,
        ..Compare::all()
    }
}

/// Types plain text into the focused entry (letters, digits, `/ . _ - space`).
fn type_text(s: &Running, text: &str) {
    for c in text.chars() {
        let name = match c {
            '/' => "slash".to_owned(),
            '.' => "period".to_owned(),
            '_' => "underscore".to_owned(),
            '-' => "minus".to_owned(),
            ' ' => "space".to_owned(),
            c if c.is_ascii_lowercase() || c.is_ascii_digit() => c.to_string(),
            c => panic!("type_text: unsupported character {c:?}"),
        };
        s.keys_paced(&[&name], 40);
    }
}

fn set_mtime(path: &Path, ns: i128) {
    let t = SystemTime::UNIX_EPOCH + Duration::from_secs(u64::try_from(ns / NS).unwrap());
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(t)
        .unwrap();
}

/// A tree with a few files, a nested directory and a relative symlink.
fn sample_tree() -> TreeSpec {
    let pattern = |seed: u8, n: usize| -> Vec<u8> { (0..n).map(|i| (i as u8) ^ seed).collect() };
    TreeSpec::new()
        .dir(
            "d",
            TreeSpec::new()
                .file("n1", FileSpec::content(b"nested one"))
                .dir(
                    "deep",
                    TreeSpec::new().file("deeper.bin", FileSpec::content(&pattern(7, 70_000))),
                )
                .dir("empty", TreeSpec::new())
                .symlink("ln", "n1"),
        )
        .file("f1", FileSpec::content(&pattern(1, 10_000)))
        .file("f2", FileSpec::content(b"two"))
        .file("f3", FileSpec::content(&pattern(3, 300_000)))
        .file("g", FileSpec::content(b"not marked"))
}

/// `sample_tree` without `g` (what the marked set contains).
fn sample_marked() -> TreeSpec {
    let pattern = |seed: u8, n: usize| -> Vec<u8> { (0..n).map(|i| (i as u8) ^ seed).collect() };
    TreeSpec::new()
        .dir(
            "d",
            TreeSpec::new()
                .file("n1", FileSpec::content(b"nested one"))
                .dir(
                    "deep",
                    TreeSpec::new().file("deeper.bin", FileSpec::content(&pattern(7, 70_000))),
                )
                .dir("empty", TreeSpec::new())
                .symlink("ln", "n1"),
        )
        .file("f1", FileSpec::content(&pattern(1, 10_000)))
        .file("f2", FileSpec::content(b"two"))
        .file("f3", FileSpec::content(&pattern(3, 300_000)))
}

/// Cursor starts on `..`; Down lands on `d` (dirs first), then Ins x4 marks d, f1, f2, f3 and
/// leaves the cursor on `g`.
fn mark_sample(s: &Running) {
    s.keys(&["Down", "Insert", "Insert", "Insert", "Insert"]);
}

// ---- 1. copy ----------------------------------------------------------------------------------

#[test]
fn f5_copies_marked_files_and_directory() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &sample_tree(), &TreeSpec::new());
    mark_sample(&s);
    s.keys(&["F5", "Return"]);
    wait_tree_equals(
        "copy to land",
        &s.right,
        &sample_marked(),
        cmp_content_mtime,
    );
    // Sources untouched.
    assert_trees_equal(
        sample_tree().build_temp().unwrap().path(),
        &s.left,
        cmp_content_mtime(),
    );
    assert_no_parts(&[&s.left, &s.right]);
}

// ---- 2. move ----------------------------------------------------------------------------------

#[test]
fn f6_moves_marked_files_same_device() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &sample_tree(), &TreeSpec::new());
    mark_sample(&s);
    s.keys(&["F6", "alt+m"]);
    wait_tree_equals(
        "move to land",
        &s.right,
        &sample_marked(),
        cmp_content_mtime,
    );
    let only_g = TreeSpec::new().file("g", FileSpec::content(b"not marked"));
    wait_tree_equals("sources to vanish", &s.left, &only_g, cmp_content_mtime);
    assert_no_parts(&[&s.left, &s.right]);
}

// ---- 3. conflicts -----------------------------------------------------------------------------

fn conflict_src() -> TreeSpec {
    TreeSpec::new()
        .file("c1", FileSpec::content(b"SRC-c1").mtime_ns(MTIME_SRC))
        .file("c2", FileSpec::content(b"SRC-c2").mtime_ns(MTIME_SRC))
        .file("c3", FileSpec::content(b"SRC-c3").mtime_ns(MTIME_SRC))
}

fn read(p: &Path) -> String {
    fs::read_to_string(p).unwrap_or_else(|e| format!("<{e}>"))
}

#[test]
fn f5_conflict_answers_skip_overwrite_and_if_newer() {
    if !tools_available() {
        return;
    }
    let dest = TreeSpec::new()
        .file("c1", FileSpec::content(b"old-c1").mtime_ns(MTIME_OLDER))
        .file("c2", FileSpec::content(b"new-c2").mtime_ns(MTIME_NEWER))
        .file("c3", FileSpec::content(b"old-c3").mtime_ns(MTIME_OLDER));
    let s = Running::launch_with("", &conflict_src(), &dest);
    let d = |n: &str| s.right.join(n);

    // Run A, one answer per conflict (asked in name order): c1 Skip (Alt+S), c2 Overwrite
    // (Alt+O), c3 Skip (Alt+S). Only c2 may change.
    s.keys_paced(
        &["ctrl+KP_Add", "F5", "Return", "alt+s", "alt+o", "alt+s"],
        600,
    );
    wait_until("c2 overwritten", 20, || read(&d("c2")) == "SRC-c2");
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(read(&d("c1")), "old-c1", "Skip changed c1");
    assert_eq!(read(&d("c3")), "old-c3", "Skip changed c3");
    assert_no_parts(&[&s.right]);

    // Run B: Overwrite, answered three times (one answer per dialog, mnemonic confirm).
    s.keys_paced(
        &["ctrl+KP_Add", "F5", "alt+c", "alt+o", "alt+o", "alt+o"],
        600,
    );
    wait_until("all three overwritten", 20, || {
        ["c1", "c2", "c3"]
            .iter()
            .all(|n| read(&d(n)) == format!("SRC-{n}"))
    });
    assert_no_parts(&[&s.right]);

    // Run C: overwrite if newer. Reset: c1/c3 stale (older than source), c2 newer than source.
    for (n, body, t) in [
        ("c1", "stale1", MTIME_OLDER),
        ("c2", "keep-2", MTIME_NEWER),
        ("c3", "stale3", MTIME_OLDER),
    ] {
        fs::write(d(n), body).unwrap();
        set_mtime(&d(n), t);
    }
    std::thread::sleep(Duration::from_millis(500)); // let the panel watcher settle
    s.keys_paced(
        &["ctrl+KP_Add", "F5", "Return", "alt+w", "alt+w", "alt+w"],
        600,
    );
    wait_until("stale files replaced", 20, || {
        read(&d("c1")) == "SRC-c1" && read(&d("c3")) == "SRC-c3"
    });
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        read(&d("c2")),
        "keep-2",
        "overwrite-if-newer replaced a newer file"
    );
    assert_no_parts(&[&s.right]);
}

// ---- 4. mkdir ---------------------------------------------------------------------------------

#[test]
fn f7_creates_nested_directories() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &TreeSpec::new(), &TreeSpec::new());
    s.keys(&["F7"]);
    type_text(&s, "a/b/c");
    s.keys(&["Return"]);
    wait_until("a/b/c created", 15, || s.left.join("a/b/c").is_dir());
}

// ---- 5. permanent delete, symlinks must not be followed ---------------------------------------

#[test]
fn shift_f8_deletes_tree_without_following_symlinks() {
    if !tools_available() {
        return;
    }
    let outside = TreeSpec::new()
        .file("keep.txt", FileSpec::content(b"precious"))
        .dir("inner", TreeSpec::new().file("x", FileSpec::content(b"x")))
        .build_temp()
        .unwrap();
    let out = outside.path();
    let left = TreeSpec::new()
        .dir(
            "victim",
            TreeSpec::new()
                .file("own.txt", FileSpec::content(b"mine"))
                .dir("sub", TreeSpec::new().file("y", FileSpec::content(b"y")))
                .symlink("to_dir", out)
                .symlink("to_file", out.join("keep.txt"))
                .symlink("dangling", "/nonexistent/ofm-e2e"),
        )
        .file("survivor", FileSpec::content(b"s"));
    let s = Running::launch_with("", &left, &TreeSpec::new());
    // Permanent delete of a folder defaults to the Cancel button, so confirm with the Delete
    // button's mnemonic (Alt+D) instead of Enter.
    s.keys(&["Down", "shift+F8", "alt+d"]);
    wait_until("victim to disappear", 20, || {
        fs::symlink_metadata(s.left.join("victim")).is_err()
    });
    assert_eq!(read(&out.join("keep.txt")), "precious");
    assert_eq!(read(&out.join("inner/x")), "x");
    assert_eq!(
        read(&s.left.join("survivor")),
        "s",
        "cursor/selection hit the wrong item"
    );
}

// ---- 6. trash ---------------------------------------------------------------------------------

#[test]
fn f8_moves_to_trash_with_trashinfo() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .dir("tdir", TreeSpec::new().file("in", FileSpec::content(b"i")))
        .file("t1.txt", FileSpec::content(b"trash me"));
    let s = Running::launch_with("", &left, &TreeSpec::new());
    let trash = s.data_home.join("Trash");
    s.keys(&["Down", "Insert", "Insert", "F8", "Return"]);
    wait_until("items in the trash", 20, || {
        trash.join("files/t1.txt").exists() && trash.join("files/tdir").exists()
    });
    assert!(!s.left.join("t1.txt").exists());
    assert!(!s.left.join("tdir").exists());
    assert_eq!(read(&trash.join("files/t1.txt")), "trash me");
    for (name, orig) in [
        ("t1.txt", s.left.join("t1.txt")),
        ("tdir", s.left.join("tdir")),
    ] {
        let info = read(&trash.join(format!("info/{name}.trashinfo")));
        assert!(info.starts_with("[Trash Info]"), "bad trashinfo: {info:?}");
        let path_line = info
            .lines()
            .find_map(|l| l.strip_prefix("Path="))
            .unwrap_or_else(|| panic!("no Path= in {info:?}"));
        assert!(
            path_line.ends_with(&orig.to_string_lossy().into_owned()),
            "Path= {path_line:?} does not name {orig:?}"
        );
        assert!(
            info.contains("DeletionDate="),
            "no DeletionDate in {info:?}"
        );
    }
}

// ---- 7. rename --------------------------------------------------------------------------------

#[test]
fn shift_f6_renames_in_place() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .file("old.txt", FileSpec::content(b"payload"))
        .file("other.txt", FileSpec::content(b"o"));
    let s = Running::launch_with("", &left, &TreeSpec::new());
    // Only the stem is selected when the dialog opens, so typing keeps the extension.
    s.keys(&["Down", "shift+F6"]);
    type_text(&s, "renamed");
    s.keys(&["Return"]);
    wait_until("rename", 15, || s.left.join("renamed.txt").exists());
    assert_eq!(read(&s.left.join("renamed.txt")), "payload");
    assert!(!s.left.join("old.txt").exists());
    assert_eq!(read(&s.left.join("other.txt")), "o");
}

// ---- 10. cursor after copy --------------------------------------------------------------------

#[test]
fn cursor_lands_on_the_new_item_after_copy() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new()
        .dir("d1", TreeSpec::new())
        .file("f1.txt", FileSpec::content(b"copy me"))
        .file("zz.txt", FileSpec::content(b"z"));
    let right = TreeSpec::new()
        .file("aaa", FileSpec::content(b"a"))
        .file("bbb", FileSpec::content(b"b"));
    let s = Running::launch_with("", &left, &right);
    s.keys(&["Down", "Down", "F5", "Return"]); // cursor: d1, f1.txt
    wait_until("copy", 20, || s.right.join("f1.txt").exists());
    std::thread::sleep(Duration::from_millis(1200)); // watcher + focus_name
    let (session, s) = s.quit();
    let cursor = |i: usize| {
        let p = &session.panels[i];
        p.tabs[p.active_tab].cursor_name.clone()
    };
    assert_eq!(
        cursor(1).as_deref(),
        Some("f1.txt"),
        "destination panel cursor"
    );
    assert_eq!(
        cursor(0).as_deref(),
        Some("f1.txt"),
        "source panel cursor moved"
    );
    assert_eq!(read(&s.right.join("f1.txt")), "copy me");
}

// ---- 8/9. cancel, jobs view, quit with a running job (slow: real disk) -------------------------

/// `OFM_E2E_SLOW=1` enables the tests that move hundreds of MB.
fn slow_enabled() -> bool {
    if std::env::var_os("OFM_E2E_SLOW").is_none() {
        eprintln!("skipping: set OFM_E2E_SLOW=1 to run the large-copy tests");
        return false;
    }
    true
}

const BIG_FILES: u64 = 8;
const BIG_MB: u64 = 128;

fn big_tree() -> TreeSpec {
    let mut t = TreeSpec::new();
    for i in 0..BIG_FILES {
        t = t.file(format!("big{i}"), FileSpec::size(BIG_MB << 20));
    }
    t
}

/// A scratch dir on the same disk as `target/` (not tmpfs).
fn disk_base() -> TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e-scratch");
    fs::create_dir_all(&base).unwrap();
    TempDir::in_dir(&base).unwrap()
}

/// Select all, F5, tick "Sync to disk" (Alt+S) so the copy takes a while, Enter.
fn start_big_copy(s: &Running) {
    s.keys(&["ctrl+KP_Add", "F5", "alt+s", "Return"]);
}

#[test]
#[ignore = "slow: ~1 GB on a real disk; set OFM_E2E_SLOW=1 and pass --include-ignored"]
fn cancel_mid_copy_leaves_no_partial_files() {
    if !tools_available() || !slow_enabled() {
        return;
    }
    let base = disk_base();
    let s = Running::launch_in("", base.path(), &big_tree(), &TreeSpec::new());
    start_big_copy(&s);
    // Open the jobs view and cancel the (selected) job with the "Cancel job" button's mnemonic as
    // fast as the keys can go (Del is covered by `del_in_jobs_view_cancels_the_job`).
    s.burst_paced(&["ctrl+j", "alt+j"], 80);
    // The job has stopped when the directory listing (names and sizes) stays the same for 2 s.
    let snapshot = || -> Vec<(String, u64)> {
        let mut v: Vec<_> = fs::read_dir(&s.right)
            .unwrap()
            .flatten()
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    e.metadata().map_or(0, |m| m.len()),
                )
            })
            .collect();
        v.sort();
        v
    };
    let (mut last, mut since) = (snapshot(), Instant::now());
    let end = Instant::now() + Duration::from_secs(40);
    while since.elapsed() < Duration::from_secs(2) {
        assert!(Instant::now() < end, "copy neither finished nor stopped");
        std::thread::sleep(Duration::from_millis(100));
        let now = snapshot();
        if now != last {
            (last, since) = (now, Instant::now());
        }
    }
    assert_no_parts(&[&s.right]);
    let mut missing = 0;
    for i in 0..BIG_FILES {
        let (src, dst) = (
            s.left.join(format!("big{i}")),
            s.right.join(format!("big{i}")),
        );
        match fs::metadata(&dst) {
            Err(_) => missing += 1,
            Ok(m) => {
                assert_eq!(
                    m.len(),
                    BIG_MB << 20,
                    "partial file under final name: {dst:?}"
                );
                assert!(
                    fs::read(&src).unwrap() == fs::read(&dst).unwrap(),
                    "corrupt copy {dst:?}"
                );
            }
        }
        assert!(src.exists(), "source vanished: {src:?}");
    }
    assert!(
        missing > 0,
        "cancel arrived after the copy had finished: the test did not exercise cancellation"
    );
}

#[test]
#[ignore = "slow: ~1 GB on a real disk; set OFM_E2E_SLOW=1 and pass --include-ignored"]
fn jobs_view_toggles_and_quit_with_running_job_prompts() {
    if !tools_available() || !slow_enabled() {
        return;
    }
    let base = disk_base();
    let mut s = Running::launch_in("", base.path(), &big_tree(), &TreeSpec::new());
    start_big_copy(&s);
    let running = |s: &Running| !(0..BIG_FILES).all(|i| s.right.join(format!("big{i}")).exists());
    // Pause the job (Alt+P in the jobs view) so it is still unfinished when we quit.
    let before = s.shot();
    s.burst_paced(&["ctrl+j", "alt+p"], 80);
    std::thread::sleep(Duration::from_millis(300));
    let open = s.shot();
    assert!(
        before.diff_pixels(&open) > 500,
        "Ctrl+J did not open the jobs view"
    );
    s.keys(&["ctrl+j"]);
    std::thread::sleep(Duration::from_millis(300));
    let closed = s.shot();
    assert!(
        open.diff_pixels(&closed) > 500,
        "Ctrl+J did not close the jobs view"
    );

    // F10 with a job running asks; the app must still be alive. Esc keeps it running.
    assert!(
        running(&s),
        "copy finished before the quit prompt (pause did not hold it)"
    );
    s.keys(&["F10"]);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        s.app.try_wait().unwrap().is_none(),
        "F10 quit without asking"
    );
    let prompt = s.shot();
    assert!(closed.diff_pixels(&prompt) > 500, "no quit prompt appeared");
    s.keys(&["Escape"]);
    assert!(
        s.app.try_wait().unwrap().is_none(),
        "Esc on the prompt quit the app"
    );

    // Answer it: Enter = quit and cancel the jobs. Nothing partial may remain.
    s.keys(&["F10", "Return"]);
    wait_until("ofm to exit", 20, || s.app.try_wait().unwrap().is_some());
    assert_no_parts(&[&s.right]);
    for i in 0..BIG_FILES {
        if let Ok(m) = fs::metadata(s.right.join(format!("big{i}"))) {
            assert_eq!(m.len(), BIG_MB << 20, "partial file after quit-and-cancel");
        }
    }
}

// ---- dialog keys through the keymap ----------------------------------------------------------
//
// Formerly pinned bugs: libadwaita stops key events at an `adw::Dialog`, so the main window's key
// controller never saw them and `dialog.ok` (Enter in a text entry), `dialog.queue` (F2),
// `dialog.apply_all` (Ctrl+A), `jobs.cancel` (Del) and `app.jobs` (Ctrl+J) were dead while a
// dialog was open. Fixed by attaching the dispatcher to every presented dialog
// (`navigation::modal::present`). Plus two dialog details: unique Alt+A mnemonic in the conflict
// dialog and the stem-only selection in the rename dialog.

#[test]
fn enter_in_copy_dialog_entry_submits() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &sample_tree(), &TreeSpec::new());
    mark_sample(&s);
    s.keys(&["F5", "Return"]);
    wait_tree_equals(
        "copy to land",
        &s.right,
        &sample_marked(),
        cmp_content_mtime,
    );
}

#[test]
fn enter_in_mkdir_dialog_entry_submits() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &TreeSpec::new(), &TreeSpec::new());
    s.keys(&["F7"]);
    type_text(&s, "a");
    s.keys(&["Return"]);
    wait_until("a created", 10, || s.left.join("a").is_dir());
}

#[test]
fn f2_in_copy_dialog_queues() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &sample_tree(), &TreeSpec::new());
    mark_sample(&s);
    s.keys(&["F5", "F2"]);
    wait_tree_equals(
        "queued copy to land",
        &s.right,
        &sample_marked(),
        cmp_content_mtime,
    );
}

/// Ctrl+A (`dialog.apply_all`) in the conflict dialog: one answer for all remaining conflicts.
#[test]
fn ctrl_a_in_conflict_dialog_applies_to_all() {
    if !tools_available() {
        return;
    }
    let dest = TreeSpec::new()
        .file("c1", FileSpec::content(b"old-c1").mtime_ns(MTIME_OLDER))
        .file("c2", FileSpec::content(b"old-c2").mtime_ns(MTIME_OLDER))
        .file("c3", FileSpec::content(b"old-c3").mtime_ns(MTIME_OLDER));
    let s = Running::launch_with("", &conflict_src(), &dest);
    s.keys_paced(&["ctrl+KP_Add", "F5", "alt+c", "ctrl+a", "alt+o"], 600);
    wait_until("all overwritten with one answer", 15, || {
        ["c1", "c2", "c3"]
            .iter()
            .all(|n| read(&s.right.join(n)) == format!("SRC-{n}"))
    });
}

#[test]
fn rename_dialog_preselects_only_the_stem() {
    if !tools_available() {
        return;
    }
    let left = TreeSpec::new().file("old.txt", FileSpec::content(b"payload"));
    let s = Running::launch_with("", &left, &TreeSpec::new());
    s.keys(&["Down", "shift+F6"]);
    type_text(&s, "renamed");
    s.keys(&["alt+r"]);
    wait_until("rename keeps the extension", 10, || {
        s.left.join("renamed.txt").exists()
    });
}

#[test]
fn conflict_dialog_alt_a_applies_to_all() {
    if !tools_available() {
        return;
    }
    let dest = TreeSpec::new()
        .file("c1", FileSpec::content(b"old-c1").mtime_ns(MTIME_OLDER))
        .file("c2", FileSpec::content(b"old-c2").mtime_ns(MTIME_OLDER))
        .file("c3", FileSpec::content(b"old-c3").mtime_ns(MTIME_OLDER));
    let s = Running::launch_with("", &conflict_src(), &dest);
    // Alt+A should tick "apply to all", then one Alt+O answers all three conflicts.
    s.keys_paced(&["ctrl+KP_Add", "F5", "alt+c", "alt+a", "alt+o"], 600);
    wait_until("all overwritten with one answer", 15, || {
        ["c1", "c2", "c3"]
            .iter()
            .all(|n| read(&s.right.join(n)) == format!("SRC-{n}"))
    });
}

#[test]
#[ignore = "slow: ~1 GB on a real disk; set OFM_E2E_SLOW=1 and pass --include-ignored"]
fn ctrl_j_toggles_the_jobs_view() {
    if !tools_available() || !slow_enabled() {
        return;
    }
    let base = disk_base();
    let s = Running::launch_in("", base.path(), &big_tree(), &TreeSpec::new());
    let before = s.shot();
    s.keys(&["ctrl+j"]);
    std::thread::sleep(Duration::from_millis(300));
    let open = s.shot();
    assert!(
        before.diff_pixels(&open) > 500,
        "Ctrl+J did not open the jobs view"
    );
    s.keys(&["ctrl+j"]);
    std::thread::sleep(Duration::from_millis(300));
    let closed = s.shot();
    assert!(
        open.diff_pixels(&closed) > 500,
        "Ctrl+J did not close the jobs view"
    );
}

#[test]
#[ignore = "slow: ~1 GB on a real disk; set OFM_E2E_SLOW=1 and pass --include-ignored"]
fn del_in_jobs_view_cancels_the_job() {
    if !tools_available() || !slow_enabled() {
        return;
    }
    let base = disk_base();
    let s = Running::launch_in("", base.path(), &big_tree(), &TreeSpec::new());
    start_big_copy(&s);
    s.burst(&["ctrl+j", "Delete"]);
    std::thread::sleep(Duration::from_secs(6));
    let done = (0..BIG_FILES).all(|i| s.right.join(format!("big{i}")).exists());
    assert!(!done, "Del did not cancel the running job");
}

/// Regression (G2): the jobs view selected its OLDEST row, so with finished jobs above the running
/// one, Cancel / Pause / Del acted on a finished job and the copy ran on. A quick mkdir job
/// finishes first; then the big copy starts; Del in the jobs view must stop the copy.
#[test]
#[ignore = "slow: ~1 GB on a real disk; set OFM_E2E_SLOW=1 and pass --include-ignored"]
fn del_cancels_the_running_job_when_finished_jobs_are_listed_above_it() {
    if !tools_available() || !slow_enabled() {
        return;
    }
    let base = disk_base();
    let s = Running::launch_in("", base.path(), &big_tree(), &TreeSpec::new());
    // Job 1: F7 mkdir "x" in the left panel (finishes at once and stays in the jobs list).
    s.keys_paced(&["F7", "x", "Return"], 400);
    std::thread::sleep(Duration::from_millis(1500));
    // Job 2: the big copy of all marked files.
    start_big_copy(&s);
    s.burst(&["ctrl+j", "Delete"]);
    std::thread::sleep(Duration::from_secs(6));
    let done = (0..BIG_FILES).all(|i| s.right.join(format!("big{i}")).exists());
    assert!(
        !done,
        "Del hit a finished job instead of the running copy (selection must follow the running job)"
    );
}

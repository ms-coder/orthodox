//! T7.3 real-binary tests: Find files (Alt+F7) and "Panelize command" in the real `ofm` under a
//! private Xvfb with REAL key events (see `real_support`). The find tests need the real search
//! engine (T7.1); they skip with a message while `ofm-search` is still the stub.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixture reads on the test thread
)]

mod real_support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ofm_core::CancelToken;
use ofm_search::{SearchError, SearchHandle, SearchQuery};
use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, tools_available};

fn wait_until(what: &str, secs: u64, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

fn engine_available() -> bool {
    let probe = SearchHandle::start(
        SearchQuery {
            roots: vec![std::env::temp_dir()],
            ..SearchQuery::default()
        },
        CancelToken::new(),
    );
    match probe {
        Err(SearchError::NotImplemented(_)) => {
            eprintln!("skipping: the search engine (T7.1) is not merged yet");
            false
        }
        _ => true,
    }
}

fn ofm_windows(s: &Running) -> Vec<String> {
    let out = s.x("xdotool", &["search", "--onlyvisible", "--class", "ofm"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Press Alt+F7 and focus the new window (a real window manager would; bare Xvfb has none).
fn open_find(s: &Running) {
    let before = ofm_windows(s);
    s.keys(&["alt+F7"]);
    wait_until("the Find window", 15, || {
        ofm_windows(s).len() > before.len()
    });
    let wins = ofm_windows(s);
    let w = wins.iter().find(|w| !before.contains(w)).unwrap().clone();
    s.x("xdotool", &["windowfocus", &w]);
    std::thread::sleep(Duration::from_millis(500));
}

fn focus_main(s: &Running) {
    let wins = ofm_windows(s);
    s.x("xdotool", &["windowfocus", &wins[0]]);
    std::thread::sleep(Duration::from_millis(300));
}

/// Types text: letters, digits, `/ . _ - space` and `*` (Shift+8).
fn type_text(s: &Running, text: &str) {
    for c in text.chars() {
        let name = match c {
            '/' => "slash".to_owned(),
            '.' => "period".to_owned(),
            '_' => "underscore".to_owned(),
            '-' => "minus".to_owned(),
            ' ' => "space".to_owned(),
            '*' => "shift+8".to_owned(),
            c if c.is_ascii_lowercase() || c.is_ascii_digit() => c.to_string(),
            c => panic!("type_text: unsupported character {c:?}"),
        };
        s.keys_paced(&[&name], 40);
    }
}

fn trees() -> (TreeSpec, TreeSpec) {
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"alpha needle\n"))
        .file("b.txt", FileSpec::content(b"bravo\n"))
        .file("c.log", FileSpec::content(b"log\n"))
        .dir(
            "sub",
            TreeSpec::new().file(
                "inner.txt",
                FileSpec::content(b"first\nsecond needle here\n"),
            ),
        );
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    (left, right)
}

fn launch() -> Running {
    let (l, r) = trees();
    Running::launch_with("", &l, &r)
}

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

fn names_in(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn tab_path(session: &ofm_config::session::Session, side: usize) -> PathBuf {
    let p = &session.panels[side];
    p.tabs[p.active_tab].path_buf().expect("tab path")
}

// ---- panelize command (T7.2 runner) ------------------------------------------------------------

/// Ctrl+Alt+P, type `ls`, Enter: the output becomes a list panel; F5 copies the real file under
/// the cursor; Backspace returns to the directory.
#[test]
fn panelize_command_shows_output_and_file_ops_work_on_real_files() {
    if !tools_available() {
        return;
    }
    let s = launch();
    s.keys(&["ctrl+alt+p"]);
    std::thread::sleep(Duration::from_millis(500));
    type_text(&s, "ls");
    s.keys(&["Return"]);
    std::thread::sleep(Duration::from_millis(1200));
    save(&s, "find-panelize-command");
    // Left panel = list of `ls` output (a.txt, b.txt, c.log, sub). Move to the first row.
    s.keys(&["Home"]);
    let right_before = names_in(&s.right);
    s.keys(&["F5"]);
    std::thread::sleep(Duration::from_millis(600));
    s.keys(&["Return"]); // confirm the copy dialog
    wait_until(
        "a file was copied into the right panel's directory",
        15,
        || names_in(&s.right).len() > right_before.len(),
    );
    let copied: Vec<String> = names_in(&s.right)
        .into_iter()
        .filter(|n| !right_before.contains(n))
        .collect();
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert!(
        ["a.txt", "b.txt", "c.log", "sub"].contains(&copied[0].as_str()),
        "{copied:?}"
    );
    // The source is untouched (copy, not move).
    assert_eq!(names_in(&s.left), ["a.txt", "b.txt", "c.log", "sub"]);

    // Backspace leaves the list; the session then records the original directory.
    s.keys(&["BackSpace"]);
    std::thread::sleep(Duration::from_millis(500));
    let (session, _s) = s.quit();
    assert_eq!(tab_path(&session, 0), _s.left);
}

// ---- find (needs the real engine) --------------------------------------------------------------

/// Alt+F7, mask `*.txt`, Enter: results appear; Alt+U (results), Enter = go to: the active panel
/// shows the directory of the first hit.
#[test]
fn alt_f7_search_then_enter_goes_to_the_result() {
    if !tools_available() || !engine_available() {
        return;
    }
    let s = launch();
    open_find(&s);
    type_text(&s, "*.txt");
    s.keys(&["Return"]);
    std::thread::sleep(Duration::from_millis(1500));
    save(&s, "find-results");
    s.keys(&["alt+u", "Return"]); // results list, go to the first hit
    wait_until("the Find window closed after go to", 10, || {
        ofm_windows(&s).len() == 1
    });
    focus_main(&s);
    let (session, s) = s.quit();
    // Wherever the first hit lives, the panel shows its directory (left dir or left/sub).
    let shown = tab_path(&session, 0);
    assert!(shown == s.left || shown == s.left.join("sub"), "{shown:?}");
}

/// F10 in the results = panelize all into the active panel; F5 there copies a real file.
#[test]
fn f10_panelizes_results_and_f5_copies_a_real_file() {
    if !tools_available() || !engine_available() {
        return;
    }
    let s = launch();
    open_find(&s);
    type_text(&s, "*.txt");
    s.keys(&["Return"]);
    std::thread::sleep(Duration::from_millis(1500));
    s.keys(&["alt+u", "F10"]);
    wait_until("the Find window closed after panelize", 10, || {
        ofm_windows(&s).len() == 1
    });
    focus_main(&s);
    std::thread::sleep(Duration::from_millis(800));
    save(&s, "find-panelized");
    let right_before = names_in(&s.right);
    s.keys(&["Home", "F5"]);
    std::thread::sleep(Duration::from_millis(600));
    s.keys(&["Return"]);
    wait_until("a result file was copied", 15, || {
        names_in(&s.right).len() > right_before.len()
    });
    let copied: Vec<String> = names_in(&s.right)
        .into_iter()
        .filter(|n| !right_before.contains(n))
        .collect();
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert!(
        ["a.txt", "b.txt", "inner.txt"].contains(&copied[0].as_str()),
        "{copied:?}"
    );
}

fn thread_count(pid: u32) -> usize {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .count()
}

/// Esc stops a running search (the window stays), a second Esc closes it, and no search threads
/// are left behind.
#[test]
fn esc_stops_a_long_search_then_closes_and_leaves_no_threads() {
    if !tools_available() || !engine_available() {
        return;
    }
    let s = launch();
    let pid = s.app.id();
    std::thread::sleep(Duration::from_secs(1));
    let baseline = thread_count(pid);
    open_find(&s);
    // Search the whole file system for something that does not exist: runs for a good while.
    s.keys(&["alt+i"]);
    type_text(&s, "/");
    s.keys(&["alt+m"]);
    type_text(&s, "zzz_no_such_name_*");
    s.keys(&["Return"]);
    std::thread::sleep(Duration::from_millis(1500));
    assert!(
        thread_count(pid) > baseline,
        "the engine should be running worker threads"
    );
    s.keys(&["Escape"]); // stop
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        ofm_windows(&s).len(),
        2,
        "first Esc must only stop the search"
    );
    s.keys(&["Escape"]); // close
    wait_until("the Find window closed", 10, || ofm_windows(&s).len() == 1);
    wait_until("search threads gone", 10, || {
        thread_count(pid) <= baseline + 1
    });
}

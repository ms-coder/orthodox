//! Real-binary tests of the F3 viewer (F21, T5.2a): the real `ofm` under a private Xvfb, real key
//! events through XTest (the lesson of M3: synthetic `ctx.execute` tests miss key-path bugs).
//!
//! The viewer is a second top-level window; what happens inside is read from screenshots: the
//! Far Blue theme paints search matches in pure yellow (`[colors.viewer] match_bg = #FFFF00`),
//! and a different screen after F4 / PgDn / Alt+F8 proves that the key reached the command.
//! Skips (with a log line) when Xvfb/xdotool/import/python3 are missing.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::time::{Duration, Instant};

use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, Shot, tools_available};

/// Pixels close to the Far Blue match colour `#FFFF00` (the renderer may be off by a step).
fn yellow(s: &Shot) -> usize {
    // ImageMagick may hand out 16-bit samples (big endian): look at the high byte of each.
    let bpp = s.px.len() / (s.w * s.h);
    let step = bpp / 3;
    s.px.chunks_exact(bpp)
        .filter(|p| p[0] > 235 && p[step] > 235 && p[2 * step] < 40)
        .count()
}

/// Window ids of the app (the main window plus any viewer), oldest first.
fn ofm_windows(s: &Running) -> Vec<String> {
    let out = s.x("xdotool", &["search", "--onlyvisible", "--class", "ofm"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(15);
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

/// F3 on the first file of the left panel; returns once the viewer window is mapped and focused
/// (a real window manager would focus it; the bare Xvfb has none).
fn open_viewer(s: &Running) -> String {
    let before = ofm_windows(s);
    s.keys(&["Down", "F3"]);
    wait_until("the viewer window", || ofm_windows(s).len() > before.len());
    let wins = ofm_windows(s);
    let viewer = wins
        .iter()
        .find(|w| !before.contains(w))
        .expect("new window")
        .clone();
    s.x("xdotool", &["windowfocus", &viewer]);
    std::thread::sleep(Duration::from_millis(900)); // first page from the worker
    viewer
}

fn text_fixture() -> TreeSpec {
    let mut body = String::from("Viewer fixture\n==============\n");
    for i in 1..=300 {
        body.push_str(&format!(
            "line {i} fn example_{i}(x: u32) -> u32 {{ x + {i} }}\n"
        ));
        if i == 150 {
            body.push_str("here is the unique NEEDLE in the haystack\n");
        }
        if i == 250 {
            body.push_str("a second Needle near the end\n");
        }
    }
    TreeSpec::new()
        .file("a.txt", FileSpec::content(body.as_bytes()))
        .file("b.txt", FileSpec::content(b"second\nfile\n"))
}

fn right_fixture() -> TreeSpec {
    TreeSpec::new().file("x1", FileSpec::content(b"x"))
}

/// The whole F21 story: open with F3, scroll with real keys, F4 hex and back, F2 wrap, F7 search
/// (dialog keys: type, Enter, Esc), next match, go to, close with Esc.
#[test]
fn f3_opens_f4_hex_f7_search_and_esc_closes() {
    if !tools_available() {
        return;
    }
    let s = Running::launch_with("", &text_fixture(), &right_fixture());
    let main_only = ofm_windows(&s);
    assert_eq!(main_only.len(), 1);
    let viewer = open_viewer(&s);
    let text = s.shot();
    assert!(text.distinct_colors() >= 6, "viewer looks blank");
    let base_yellow = yellow(&text);

    // Scrolling: Down / PgDn / End change the screen, Home returns to the first page.
    s.keys(&["Page_Down"]);
    let paged = s.shot();
    assert!(text.diff_pixels(&paged) > 500, "PgDn did not scroll");
    s.keys(&["Home"]);
    let home = s.shot();
    assert!(
        text.diff_pixels(&home) < 50,
        "Home did not return to the first page ({} px differ)",
        text.diff_pixels(&home)
    );

    // F4: hex view, F4 again: back to the same text screen.
    s.keys(&["F4"]);
    let hex = s.shot();
    assert!(text.diff_pixels(&hex) > 2000, "F4 did not switch to hex");
    s.keys(&["F4"]);
    let back = s.shot();
    assert!(
        text.diff_pixels(&back) < 50,
        "F4 twice did not restore the text view ({} px differ)",
        text.diff_pixels(&back)
    );

    // F2 toggles wrapping (long lines are cut at the window width by default).
    s.keys(&["F2", "F2"]);

    // F7: the dialog takes keys (typing goes into the entry), Esc cancels it and keeps the viewer.
    s.keys(&["F7"]);
    let dialog = s.shot();
    assert!(
        back.diff_pixels(&dialog) > 2000,
        "F7 did not open the dialog"
    );
    s.keys(&["n", "e", "e", "d", "l", "e"]);
    s.keys(&["Escape"]);
    let after_cancel = s.shot();
    assert!(
        back.diff_pixels(&after_cancel) < 50,
        "Esc in the search dialog did not just close the dialog"
    );
    assert_eq!(
        ofm_windows(&s).len(),
        2,
        "Esc in the dialog closed the viewer"
    );

    // F7, type, Enter: jumps to the match (yellow highlight appears).
    s.keys(&["F7", "n", "e", "e", "d", "l", "e", "Return"]);
    std::thread::sleep(Duration::from_millis(600));
    let found = s.shot();
    let y1 = yellow(&found);
    assert!(
        y1 > base_yellow + 30,
        "no highlighted match after search ({base_yellow} -> {y1} yellow px)"
    );
    assert!(
        back.diff_pixels(&found) > 2000,
        "search did not move the view"
    );

    // Shift+F7: next match (case-insensitive: "Needle" near the end), still highlighted.
    s.keys(&["shift+F7"]);
    std::thread::sleep(Duration::from_millis(600));
    let next = s.shot();
    assert!(
        yellow(&next) > base_yellow + 30,
        "no highlight on the next match"
    );
    assert!(
        found.diff_pixels(&next) > 1000,
        "Shift+F7 did not move to the next match"
    );

    // Alt+F8: go to byte offset 0x1000, Enter.
    s.keys(&["alt+F8", "0", "x", "1", "0", "0", "0", "Return"]);
    std::thread::sleep(Duration::from_millis(600));
    let gone = s.shot();
    assert!(
        next.diff_pixels(&gone) > 1000,
        "Alt+F8 go to did not move the view"
    );

    // Esc closes the viewer; the main window is still there and alive.
    s.keys(&["Escape"]);
    wait_until("the viewer to close", || ofm_windows(&s) == main_only);
    assert!(!ofm_windows(&s).contains(&viewer));
    s.keys(&["Down"]);
    let (_session, _s) = s.quit(); // F10 in the main window still works
}

/// F8 opens the encoding list; keys select with Down / Enter and the dialog closes. Gray+ steps
/// to the next file of the panel.
#[test]
fn f8_encoding_dialog_and_next_file() {
    if !tools_available() {
        return;
    }
    // windows-1251 bytes: "Привет" twice, then plain text in the second file.
    let cp1251 = b"\xCF\xF0\xE8\xE2\xE5\xF2 \xEC\xE8\xF0\n\xCF\xF0\xE8\xE2\xE5\xF2\n".to_vec();
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(&cp1251))
        .file("b.txt", FileSpec::content(b"second file\nwith two lines\n"));
    let s = Running::launch_with("", &left, &right_fixture());
    let main_only = ofm_windows(&s);
    open_viewer(&s);
    let auto = s.shot();
    // F8, Down (to UTF-8), Enter: the Cyrillic bytes now decode as replacement characters.
    s.keys(&["F8", "Down", "Return"]);
    std::thread::sleep(Duration::from_millis(500));
    let forced = s.shot();
    assert!(
        auto.diff_pixels(&forced) > 100,
        "forcing UTF-8 did not change the text ({} px differ)",
        auto.diff_pixels(&forced)
    );
    assert_eq!(
        ofm_windows(&s).len(),
        2,
        "the encoding dialog closed the viewer"
    );
    // The next file in the panel list.
    s.keys(&["KP_Add"]);
    std::thread::sleep(Duration::from_millis(600));
    let second = s.shot();
    assert!(
        forced.diff_pixels(&second) > 100,
        "Gray+ did not open the next file"
    );
    s.keys(&["Escape"]);
    wait_until("the viewer to close", || ofm_windows(&s) == main_only);
}

/// A 10 GB sparse file opens at once: the window is up and painted within a couple of seconds,
/// scrolling to the end and hex mode work, and Esc closes it (the line counter is cancelled).
#[test]
fn sparse_ten_gigabyte_file_opens_instantly() {
    if !tools_available() {
        return;
    }
    let base = std::env::temp_dir();
    let left = TreeSpec::new().file("big.bin", FileSpec::sparse(10 * 1024 * 1024 * 1024));
    let s = Running::launch_in("", &base, &left, &right_fixture());
    let main_only = ofm_windows(&s);
    let t0 = Instant::now();
    s.keys_paced(&["Down", "F3"], 100);
    wait_until("the viewer window", || ofm_windows(&s).len() > 1);
    let viewer = ofm_windows(&s).pop().expect("viewer");
    s.x("xdotool", &["windowfocus", &viewer]);
    wait_until("painted viewer content", || s.shot().distinct_colors() >= 6);
    assert!(
        t0.elapsed() < Duration::from_secs(6),
        "10 GB file took {:?} to show",
        t0.elapsed()
    );
    s.keys(&["End"]);
    let end = s.shot();
    s.keys(&["F4"]);
    let hex = s.shot();
    assert!(end.diff_pixels(&hex) > 500, "hex mode did not show");
    s.keys(&["Escape"]);
    wait_until("the viewer to close", || ofm_windows(&s) == main_only);
}

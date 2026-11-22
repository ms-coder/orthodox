//! T8.2b real-binary test: the user menu (F28, F2) through the real `ofm` under a private Xvfb with
//! real key events: F2, Up/Down + Enter, hotkey letters, Esc, a terminal entry, and the trust
//! prompt of a local `.ofm-menu.toml` (untrusted = not shown/run; trusted; changed = asked
//! again). Skips when the harness tools are missing. Screenshots go to `target/screenshots/`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::path::Path;
use std::time::{Duration, Instant};

use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, tools_available};

const GLOBAL_MENU: &str = r#"
version = 1
[[item]]
key = "M"
title = "Marker"
command = "printf %%s %f > marker.out"
[[item]]
key = "T"
title = "Terminal marker"
command = "printf term > term.out"
terminal = true
"#;

const LOCAL_MENU: &str = r#"
version = 1
[[item]]
key = "L"
title = "Local entry"
command = "printf local > local.out"
"#;

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

fn launch() -> Running {
    let left = TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("b.txt", FileSpec::content(b"b"));
    let right = TreeSpec::new().file("x1", FileSpec::content(b"x"));
    let s = Running::launch_with("", &left, &right);
    std::fs::write(s.paths.menu_file(), GLOBAL_MENU).unwrap();
    s
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// F2, Down/Up, Enter, hotkeys and Esc through real key events.
#[test]
fn user_menu_keys_run_entries() {
    if !tools_available() {
        return;
    }
    let s = launch();
    let marker = s.left.join("marker.out");
    let term = s.left.join("term.out");
    s.keys(&["Down"]); // `..` -> a.txt
    let plain = s.shot();

    // Esc closes without running anything.
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(700));
    assert!(
        s.shot().diff_pixels(&plain) > 2000,
        "F2 did not open the menu"
    );
    save(&s, "usermenu-real-menu");
    s.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!marker.exists());

    // Down, Up, Enter: the first entry runs in the panel directory with the cursor file.
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(700));
    s.keys(&["Down", "Up", "Return"]);
    assert!(
        wait_until(Duration::from_secs(10), || marker.exists()),
        "Enter did not run the first entry"
    );
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(read(&marker), "a.txt");

    // A hotkey letter runs a terminal entry (own terminal view, exact argv).
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(700));
    s.keys(&["t"]);
    assert!(
        wait_until(Duration::from_secs(15), || term.exists()),
        "hotkey T did not run the terminal entry"
    );
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(read(&term), "term");
    // The panels are back and usable: the menu opens again.
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(700));
    assert!(s.shot().diff_pixels(&plain) > 2000, "menu did not reopen");
    s.keys(&["Escape"]);
}

/// A local menu is not offered or run until trusted; changed content is asked again.
#[test]
fn local_menu_needs_trust() {
    if !tools_available() {
        return;
    }
    let s = launch();
    let local_out = s.left.join("local.out");
    let local_file = s.left.join(".ofm-menu.toml");
    std::fs::write(&local_file, LOCAL_MENU).unwrap();
    std::thread::sleep(Duration::from_millis(500));

    // Untrusted: the prompt comes first, the entry's hotkey does nothing.
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(800));
    s.keys(&["l"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!local_out.exists(), "untrusted local entry ran");
    s.keys(&["Return"]); // focus starts on "Show contents"
    std::thread::sleep(Duration::from_millis(500));
    save(&s, "usermenu-real-trust");
    // Tab -> Ignore, Tab -> Trust, Enter.
    s.keys(&["Tab", "Tab", "Return"]);
    std::thread::sleep(Duration::from_millis(900));
    save(&s, "usermenu-real-after-trust");
    s.keys(&["l"]);
    assert!(
        wait_until(Duration::from_secs(10), || local_out.exists()),
        "trusted local entry did not run"
    );
    assert_eq!(read(&local_out), "local");
    assert!(s.paths.state_dir.join("menu-trust.toml").exists());

    // Same content: no prompt, the menu opens and the hotkey runs straight away.
    std::fs::remove_file(&local_out).unwrap();
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(800));
    s.keys(&["l"]);
    assert!(
        wait_until(Duration::from_secs(10), || local_out.exists()),
        "trusted entry asked again"
    );

    // Changed content: untrusted again (prompt; Esc = Ignore; the entry is not offered).
    std::fs::remove_file(&local_out).unwrap();
    std::fs::write(&local_file, format!("{LOCAL_MENU}# edited\n")).unwrap();
    s.keys(&["F2"]);
    std::thread::sleep(Duration::from_millis(800));
    s.keys(&["l"]);
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        !local_out.exists(),
        "changed menu ran without a new trust decision"
    );
    s.keys(&["Escape"]); // Ignore -> menu without the local entry
    std::thread::sleep(Duration::from_millis(800));
    s.keys(&["l"]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(!local_out.exists(), "ignored local entry ran");
    s.keys(&["Escape"]);
}

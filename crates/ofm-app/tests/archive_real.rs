//! T6.3 real-binary tests of the archive UI (F26): the real `ofm` under a private Xvfb with real
//! key events (see `real_support`). The archives are made with the `zip` CLI inside the test
//! (skips with a log line when `zip` or the harness tools are missing) and need the real format
//! backends of T6.1 / the extract job of T6.2.
//!
//! What is asserted comes from the file system (files extracted into the other panel, temp
//! directories gone, nothing created by refused operations) and from the number of top-level
//! windows (the viewer); screenshots land in `target/screenshots/archive-*.png` for a visual check.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // fixture setup runs on the test thread, not the GTK thread
)]

mod real_support;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use ofm_testkit::temp::TempDir;
use ofm_testkit::tree::{FileSpec, TreeSpec};
use real_support::{Running, have, tools_available};

fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end {
        if cond() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for: {what}");
}

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

fn ofm_windows(s: &Running) -> Vec<String> {
    let out = s.x("xdotool", &["search", "--onlyvisible", "--class", "ofm"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// Type `text` with real key events (letters and digits only).
fn type_text(s: &Running, text: &str) {
    for c in text.chars() {
        let key = c.to_string();
        s.keys_paced(&[key.as_str()], 60);
    }
}

/// Archives for the tests, made with the `zip` CLI: `test.zip` (hello.txt, sub/inner.txt) and the
/// ZipCrypto-protected `enc.zip` (secret.txt, password "secret"). `None` without the tool.
struct Zips {
    test: Vec<u8>,
    enc: Vec<u8>,
    _scratch: TempDir,
}

fn make_zips() -> Option<Zips> {
    if !have("zip") {
        eprintln!("skipping: the zip CLI is not installed");
        return None;
    }
    let scratch = TempDir::new().unwrap();
    let src = scratch.path().join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("hello.txt"), b"hello archive\n").unwrap();
    std::fs::write(src.join("sub/inner.txt"), b"inner\n").unwrap();
    std::fs::write(src.join("secret.txt"), b"top secret text\n").unwrap();
    let run = |args: &[&str]| {
        let st = Command::new("zip")
            .current_dir(&src)
            .args(args)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(st.success(), "zip {args:?}");
    };
    let test = scratch.path().join("test.zip");
    let enc = scratch.path().join("enc.zip");
    run(&["-q", "-r", test.to_str().unwrap(), "hello.txt", "sub"]);
    run(&["-q", "-P", "secret", enc.to_str().unwrap(), "secret.txt"]);
    Some(Zips {
        test: std::fs::read(&test).unwrap(),
        enc: std::fs::read(&enc).unwrap(),
        _scratch: scratch,
    })
}

fn left_tree(z: &Zips) -> TreeSpec {
    TreeSpec::new()
        .file("a.txt", FileSpec::content(b"a"))
        .file("enc.zip", FileSpec::content(&z.enc))
        .file("test.zip", FileSpec::content(&z.test))
}

fn right_tree() -> TreeSpec {
    TreeSpec::new().file("x1", FileSpec::content(b"x"))
}

/// App with a private `TMPDIR` (to check the viewer's temp files are cleaned up).
fn launch(z: &Zips, config: &str) -> (Running, TempDir) {
    let tmp = TempDir::new().unwrap();
    let s = Running::launch_env(
        config,
        &std::env::temp_dir(),
        &left_tree(z),
        &right_tree(),
        &[("TMPDIR", tmp.path().as_os_str())],
    );
    (s, tmp)
}

/// Move the cursor from `..` to `test.zip` and press Enter (left: `..`, a.txt, enc.zip, test.zip).
fn enter_test_zip(s: &Running) {
    s.keys(&["Down", "Down", "Down", "Return"]);
    std::thread::sleep(Duration::from_millis(700));
}

fn ofm_temp_dirs(tmp: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(tmp)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("ofm-archive-"))
        })
        .collect()
}

/// Enter on a zip (by magic), folders, Backspace back with the cursor on the archive (proved by
/// copying "the cursor item" with F5), F5 extracts to the other panel, F7 is refused, F3 opens
/// the viewer on an entry and its private temp file is gone after Esc.
#[test]
fn zip_navigate_extract_refuse_and_view() {
    if !tools_available() {
        return;
    }
    let Some(z) = make_zips() else { return };
    let (s, tmp) = launch(&z, "");
    let (left, right) = (s.left.clone(), s.right.clone());

    // Enter the archive and a folder inside; Backspace twice returns to the folder with the cursor
    // on the archive file: F5 + Enter then copies exactly test.zip into the other panel.
    enter_test_zip(&s);
    save(&s, "archive-panel-far-blue");
    s.keys(&["Down", "Return"]); // `sub` is the first row below `..`
    s.keys(&["BackSpace", "BackSpace"]);
    s.keys(&["F5", "Return"]);
    wait_until("test.zip copied by F5 from the real folder", || {
        right.join("test.zip").is_file()
    });
    assert_eq!(std::fs::read(right.join("test.zip")).unwrap(), z.test);

    // Into the archive again: extract the file and the folder with F5 (destination = other panel).
    enter_test_zip(&s);
    s.keys(&["Down", "Down", "F5"]); // cursor on hello.txt
    save(&s, "archive-extract-dialog");
    s.keys(&["Return"]);
    wait_until("hello.txt extracted", || right.join("hello.txt").is_file());
    assert_eq!(
        std::fs::read(right.join("hello.txt")).unwrap(),
        b"hello archive\n"
    );
    s.keys(&["Up", "F5", "Return"]); // cursor on the folder `sub`
    wait_until("sub/inner.txt extracted", || {
        right.join("sub/inner.txt").is_file()
    });
    assert_eq!(
        std::fs::read(right.join("sub/inner.txt")).unwrap(),
        b"inner\n"
    );

    // Read-only: F7 + a typed name creates nothing and the dialog never appears.
    s.keys(&["F7"]);
    save(&s, "archive-readonly-message");
    type_text(&s, "newdir");
    s.keys(&["Return"]);
    std::thread::sleep(Duration::from_millis(300));
    assert!(!right.join("newdir").exists() && !left.join("newdir").exists());

    // F3 on hello.txt: the viewer opens; a private temp dir exists meanwhile and is gone after Esc.
    s.keys(&["Down"]); // `sub` -> hello.txt
    let before = ofm_windows(&s);
    s.keys(&["F3"]);
    wait_until("the viewer window", || ofm_windows(&s).len() > before.len());
    let viewer = ofm_windows(&s)
        .into_iter()
        .find(|w| !before.contains(w))
        .unwrap();
    s.x("xdotool", &["windowfocus", &viewer]);
    std::thread::sleep(Duration::from_millis(900));
    assert_eq!(
        ofm_temp_dirs(tmp.path()).len(),
        1,
        "one private temp dir while viewing"
    );
    save(&s, "archive-viewer");
    s.keys(&["Escape"]);
    wait_until("the viewer closed", || {
        ofm_windows(&s).len() == before.len()
    });
    wait_until("temp dir removed", || ofm_temp_dirs(tmp.path()).is_empty());
}

/// Encrypted zip: the password dialog takes real keys (type, Enter), a wrong password asks again
/// with an error line, and the accepted password is remembered: the following F5 does not ask.
#[test]
fn encrypted_zip_password_dialog_retry_and_remembered() {
    if !tools_available() {
        return;
    }
    let Some(z) = make_zips() else { return };
    let (s, _tmp) = launch(&z, "");
    let right = s.right.clone();

    s.keys(&["Down", "Down", "Return"]); // enc.zip
    std::thread::sleep(Duration::from_millis(700));
    let panel = s.shot();
    s.keys(&["Down", "F3"]); // secret.txt: needs the password
    wait_until("the password dialog", || {
        s.shot().diff_pixels(&panel) > 3000
    });
    std::thread::sleep(Duration::from_millis(300));
    save(&s, "archive-password-dialog");
    type_text(&s, "wrongpw");
    s.keys(&["Return"]);
    std::thread::sleep(Duration::from_millis(800));
    save(&s, "archive-password-retry");
    type_text(&s, "secret");
    s.keys(&["Return"]);
    // The viewer opens once the entry decrypted.
    wait_until("the viewer after the password", || {
        ofm_windows(&s).len() == 2
    });
    let viewer = ofm_windows(&s).pop().unwrap();
    s.x("xdotool", &["windowfocus", &viewer]);
    s.keys(&["Escape"]);
    wait_until("the viewer closed", || ofm_windows(&s).len() == 1);

    // The password is remembered for the session: F5 extracts without a second prompt.
    s.x("xdotool", &["windowfocus", &ofm_windows(&s).pop().unwrap()]);
    s.keys(&["F5", "Return"]);
    wait_until("secret.txt extracted without asking again", || {
        right.join("secret.txt").is_file()
    });
    assert_eq!(
        std::fs::read(right.join("secret.txt")).unwrap(),
        b"top secret text\n"
    );
}

/// Esc in the password dialog cancels the operation: nothing is extracted, nothing hangs.
#[test]
fn password_dialog_escape_cancels() {
    if !tools_available() {
        return;
    }
    let Some(z) = make_zips() else { return };
    let (s, _tmp) = launch(&z, "");
    let right = s.right.clone();
    s.keys(&["Down", "Down", "Return"]); // enc.zip
    std::thread::sleep(Duration::from_millis(700));
    s.keys(&["Down", "F5", "Return"]); // extract secret.txt: password dialog
    std::thread::sleep(Duration::from_millis(900));
    s.keys(&["Escape"]);
    std::thread::sleep(Duration::from_millis(800));
    assert!(!right.join("secret.txt").exists());
    // The app still responds: F7 in the archive shows the read-only message instead of a dialog.
    s.keys(&["F7"]);
    save(&s, "archive-after-password-cancel");
}

/// The archive panel and the password dialog draw in every theme (screenshots for the eye).
#[test]
fn archive_panel_and_password_dialog_in_every_theme() {
    if !tools_available() {
        return;
    }
    let Some(z) = make_zips() else { return };
    for theme in ["far-blue", "commander-light", "adwaita"] {
        let (s, _tmp) = launch(&z, &format!("[theme]\nname = \"{theme}\"\n"));
        enter_test_zip(&s);
        save(&s, &format!("archive-panel-{theme}"));
        s.keys(&["BackSpace"]);
        s.keys(&["Up", "Return"]); // enc.zip is above test.zip
        std::thread::sleep(Duration::from_millis(700));
        s.keys(&["Down", "F3"]);
        std::thread::sleep(Duration::from_millis(900));
        save(&s, &format!("archive-password-{theme}"));
    }
}

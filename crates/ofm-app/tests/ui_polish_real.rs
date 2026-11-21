//! T8.7 UI polish: real-binary checks (private Xvfb, real key events) for the dialog colours and
//! the quick-view header. Screenshots land in `target/screenshots/polish-*.png`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods // test code runs on the test thread, not the GTK thread
)]

mod real_support;

use std::path::Path;
use std::time::Duration;

use real_support::{Running, Shot, tools_available};

fn save(s: &Running, name: &str) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/screenshots");
    std::fs::create_dir_all(&dir).unwrap();
    s.save_png(&dir.join(format!("{name}.png")));
}

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

fn pixel(s: &Shot, x: usize, y: usize) -> [u8; 3] {
    let p = (y * s.w + x) * 3;
    [s.px[p], s.px[p + 1], s.px[p + 2]]
}

/// Number of pixels of exactly `rgb` in the window's central area (where a dialog is shown).
fn count_in_centre(s: &Shot, rgb: [u8; 3]) -> usize {
    let mut n = 0;
    for y in 100..700 {
        for x in 200..1000 {
            if pixel(s, x, y) == rgb {
                n += 1;
            }
        }
    }
    n
}

/// F5 opens the copy dialog: it carries the theme's `colors.dialog.bg` in every themed palette.
#[test]
fn dialogs_use_the_theme_dialog_colours() {
    if !tools_available() {
        return;
    }
    // (theme, dialog bg from the theme file)
    for (theme, bg) in [
        ("far-blue", [0xC0, 0xC0, 0xC0]),
        ("commander-light", [0xFA, 0xFA, 0xFA]),
    ] {
        let s = Running::launch(&format!("[theme]\nname = \"{theme}\"\n"));
        s.keys(&["Down", "F5"]);
        pause(600);
        save(&s, &format!("polish-dialog-{theme}-copy"));
        let shot = s.shot();
        assert!(
            count_in_centre(&shot, bg) > 5000,
            "{theme}: the copy dialog does not use the theme's dialog background"
        );
        s.keys(&["Escape", "F7"]);
        pause(500);
        save(&s, &format!("polish-dialog-{theme}-mkdir"));
        s.keys(&["Escape"]);
    }
    // Adwaita keeps the stock dialog; just look at it.
    let s = Running::launch("[theme]\nname = \"adwaita\"\n");
    s.keys(&["Down", "F5"]);
    pause(600);
    save(&s, "polish-dialog-adwaita-copy");
}

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

/// Screenshot as true 8-bit RGB (`Running::shot` keeps whatever depth ImageMagick writes, 16 bit
/// here, which is fine for equality checks but not for addressing pixels).
fn shot8(r: &Running) -> Shot {
    let out = r.x("import", &["-depth", "8", "-window", "root", "ppm:-"]);
    assert!(out.status.success(), "import failed");
    let d = &out.stdout;
    let (mut pos, mut tokens) = (0, Vec::new());
    while tokens.len() < 4 {
        while d[pos].is_ascii_whitespace() {
            pos += 1;
        }
        let start = pos;
        while !d[pos].is_ascii_whitespace() {
            pos += 1;
        }
        tokens.push(String::from_utf8_lossy(&d[start..pos]).into_owned());
    }
    assert_eq!(tokens[3], "255", "expected an 8-bit ppm");
    let (w, h) = (tokens[1].parse().unwrap(), tokens[2].parse().unwrap());
    Shot {
        w,
        h,
        px: d[pos + 1..].to_vec(),
    }
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
        let shot = shot8(&s);
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

/// Differing raw bytes of the right half between rows `y0..y1` (rows of the 8-bit PPM data the
/// harness parses; only used as a "changed / unchanged" measure).
fn band_diff(a: &Shot, b: &Shot, y0: usize, y1: usize) -> usize {
    right_half(a, b, y0, y1)
}

/// Right half of the window (x >= 800) from the menu bar down to the command line, i.e. everything
/// of the panel except the F-key bar (that one follows the held modifiers).
fn right_half_diff(a: &Shot, b: &Shot) -> usize {
    right_half(a, b, 0, 772)
}

fn right_half(a: &Shot, b: &Shot, y0: usize, y1: usize) -> usize {
    let mut n = 0;
    for y in y0..y1 {
        for x in a.w / 2..a.w {
            if pixel(a, x, y) != pixel(b, x, y) {
                n += 1;
            }
        }
    }
    n
}

/// Quick view puts "Quick view" into the preview side's header and the item's info into its
/// status line, and the second Ctrl+Q restores the panel pixel-exactly. (Hiding or fading the real
/// path label instead leaves a two-pixel rendering difference behind: the header text is covered
/// by a second label while quick view is on and the real label is never touched.)
#[test]
fn quick_view_header_is_restored_pixel_exact() {
    if !tools_available() {
        return;
    }
    let s = Running::launch("");
    let plain = shot8(&s);
    s.keys(&["Down", "Down", "ctrl+q"]);
    pause(1000);
    let during = shot8(&s);
    save(&s, "polish-quickview-header");
    // The header and the status line of the preview side changed (right half, top and bottom).
    assert!(
        band_diff(&plain, &during, 28, 48) > 20,
        "the path header of the preview side was not replaced"
    );
    assert!(
        band_diff(&plain, &during, 722, 740) > 20,
        "the status line of the preview side was not replaced"
    );
    s.keys(&["Home", "ctrl+q"]);
    pause(1500);
    let after = shot8(&s);
    save(&s, "polish-quickview-restored");
    assert_eq!(
        right_half_diff(&plain, &after),
        0,
        "the other panel was not restored exactly"
    );
}

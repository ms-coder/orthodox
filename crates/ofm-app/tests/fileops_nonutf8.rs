//! T3.6c: non-UTF-8 names and folders through the file-operation dialogs (invariant 4: paths are
//! bytes). The dialog fields SHOW lossy text, but what they submit must keep the original bytes:
//! an untouched destination field names the original (possibly non-UTF-8) folder, and an untouched
//! single-item name field keeps the item's exact name. Driven through `AppContext::execute` with
//! the REAL engine (no `capture`): after Enter (`dialog.ok`) the FILE SYSTEM is checked for the
//! exact byte names. Needs a display (Xvfb in `check.sh`); skips itself without one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod fileops_support;

use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;

use fileops_support::*;
use gtk::prelude::*;
use ofm_core::cmd;
use ofm_testkit::tree::{FileSpec, TreeSpec, raw_name};

/// `caf\xe9.txt` — Latin-1 é, invalid UTF-8; shown as `caf\u{fffd}.txt`.
const RAW_FILE: &[u8] = b"caf\xe9.txt";
/// `ba\x80d.dat` — a stray continuation byte, invalid UTF-8.
const RAW_FILE2: &[u8] = b"ba\x80d.dat";
/// `ziel\xfc\xfe` — a non-UTF-8 destination folder ("target").
const RAW_DIR: &[u8] = b"ziel\xfc\xfe";

/// Directory entries of `dir` as raw names, sorted.
fn entry_names(dir: &std::path::Path) -> Vec<OsString> {
    let mut names: Vec<OsString> = std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.sort();
    names
}

#[test]
fn non_utf8_names_survive_the_dialogs() {
    if !init() {
        return;
    }
    let left = TreeSpec::new()
        .file(raw_name(RAW_FILE), FileSpec::content(b"coffee"))
        .file(raw_name(RAW_FILE2), FileSpec::content(b"move me"));
    let right = TreeSpec::new().dir(raw_name(RAW_DIR), TreeSpec::new());
    let fx = Fixture::new(
        "io.github.mscoder.OfmFileopsNonUtf8",
        left,
        right,
        theme_from_env().as_deref(),
    );
    let ctx = &fx.ctx;
    let lp = fx.left.path().to_owned();
    let dest_dir = fx.right.path().join(raw_name(RAW_DIR));
    let cursor_on = |side: usize, name: &[u8]| {
        assert!(
            ctx.host(side)
                .active()
                .mutate(|m| m.set_cursor_name(std::ffi::OsStr::from_bytes(name))),
            "no entry {:?} in panel {side}",
            String::from_utf8_lossy(name)
        );
    };

    // Park the right panel INSIDE the non-UTF-8 folder: its path is the copy dialog's default
    // destination, shown lossy.
    fx.run(cmd::PANEL_SWITCH);
    cursor_on(1, RAW_DIR);
    fx.run(cmd::PANEL_ENTER_DIR);
    settle(ctx);
    fx.run(cmd::PANEL_SWITCH);
    assert_eq!(
        ctx.other_controller().location().local_path(),
        Some(dest_dir.as_path())
    );

    // ---- F5 copy: untouched lossy destination and name keep the original bytes ---------------------
    cursor_on(0, RAW_FILE);
    fx.run(cmd::FILE_COPY);
    assert_eq!(fx.modal_depth(), 1);
    let shown_dest = dest_dir.to_string_lossy().into_owned();
    assert!(shown_dest.contains('\u{fffd}'));
    assert_eq!(
        fx.entry("ofm-transfer-dest").text().as_str(),
        shown_dest.as_str()
    );
    let shown_name = raw_name(RAW_FILE).to_string_lossy().into_owned();
    assert!(shown_name.contains('\u{fffd}'));
    assert_eq!(fx.entry("ofm-transfer-mask").text().as_str(), shown_name);
    fx.shot("nonutf8-copy-dialog");
    fx.run(cmd::DIALOG_OK); // Enter submits exactly what the fields' origins are
    assert_eq!(fx.modal_depth(), 0);
    let copied = dest_dir.join(raw_name(RAW_FILE));
    pump_until("copy lands under the exact byte name", || copied.is_file());
    assert_eq!(std::fs::read(&copied).unwrap(), b"coffee");
    assert_eq!(
        entry_names(&dest_dir),
        vec![raw_name(RAW_FILE)],
        "no lossy (U+FFFD) twin may be created"
    );
    assert!(
        lp.join(raw_name(RAW_FILE)).is_file(),
        "the source is untouched"
    );

    // ---- Shift+F6 rename with the untouched lossy name: refused as "unchanged" ---------------------
    cursor_on(0, RAW_FILE2);
    fx.run(cmd::FILE_RENAME);
    fx.run(cmd::DIALOG_OK);
    assert_eq!(
        fx.modal_depth(),
        1,
        "the dialog refuses to rename to U+FFFD"
    );
    assert!(fx.find("ofm-transfer-error").is_visible());
    fx.run(cmd::DIALOG_CANCEL);
    assert_eq!(fx.modal_depth(), 0);

    // ---- F6 move: same untouched-fields rule, and the source disappears ----------------------------
    fx.run(cmd::FILE_MOVE);
    fx.run(cmd::DIALOG_OK);
    let moved = dest_dir.join(raw_name(RAW_FILE2));
    pump_until("move lands under the exact byte name", || {
        moved.is_file() && !lp.join(raw_name(RAW_FILE2)).exists()
    });
    assert_eq!(std::fs::read(&moved).unwrap(), b"move me");
    let mut expected = vec![raw_name(RAW_FILE), raw_name(RAW_FILE2)];
    expected.sort();
    assert_eq!(entry_names(&dest_dir), expected);

    // ---- Shift+F6 with an EDITED name is a real rename ---------------------------------------------
    cursor_on(0, RAW_FILE);
    fx.run(cmd::FILE_RENAME);
    fx.entry("ofm-transfer-mask").set_text("kaffee.txt");
    fx.run(cmd::DIALOG_OK);
    pump_until("rename lands", || {
        lp.join("kaffee.txt").is_file() && !lp.join(raw_name(RAW_FILE)).exists()
    });
    assert_eq!(std::fs::read(lp.join("kaffee.txt")).unwrap(), b"coffee");

    // ---- F7 mkdir inside the non-UTF-8 folder (parent from bytes, not from text) -------------------
    fx.run(cmd::PANEL_SWITCH);
    fx.run(cmd::FILE_MKDIR);
    fx.entry("ofm-mkdir-name").set_text("newdir");
    fx.run(cmd::DIALOG_OK);
    pump_until("directory created in the non-UTF-8 folder", || {
        dest_dir.join("newdir").is_dir()
    });

    assert_eq!(fx.modal_depth(), 0);
    assert!(!ctx.modal_open());
}

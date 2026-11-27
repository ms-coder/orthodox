//! The `--help`, `--version` and `--dump-keys` flags must work without a display (T10.1):
//! the real binary is started with `DISPLAY`/`WAYLAND_DISPLAY` removed and must print and
//! exit before touching GTK.
#![allow(clippy::unwrap_used)]

use std::process::Command;

fn ofm(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_ofm"))
        .args(args)
        // No display at all: even the default of the parent session must not leak in.
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("GDK_BACKEND")
        .output()
        .unwrap()
}

#[test]
fn version_prints_without_a_display() {
    let out = ofm(&["--version"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert!(stdout.starts_with("ofm "), "{stdout}");
    assert!(stdout.trim().ends_with(env!("CARGO_PKG_VERSION")), "{stdout}");
}

#[test]
fn help_prints_usage_without_a_display() {
    for flag in ["--help", "-h"] {
        let out = ofm(&[flag]);
        assert!(out.status.success(), "{flag}: {out:?}");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.contains("Usage: ofm"), "{stdout}");
        assert!(stdout.contains("--dump-keys"), "{stdout}");
    }
}

#[test]
fn dump_keys_prints_markdown_without_a_display() {
    for preset in ["far", "tc", "mc"] {
        let out = ofm(&["--dump-keys", preset]);
        assert!(out.status.success(), "{preset}: {out:?}");
        let stdout = String::from_utf8(out.stdout).unwrap();
        assert!(stdout.starts_with("# OFM key reference"), "{stdout}");
        assert!(stdout.contains("## panels"), "{stdout}");
        assert!(stdout.contains("`file.copy`"), "{stdout}");
        // A document without a single chord row is useless.
        assert!(stdout.lines().any(|l| l.starts_with("| F")), "{stdout}");
    }
}

#[test]
fn bad_flags_exit_with_code_2_and_stderr() {
    let out = ofm(&["--dump-keys", "norton"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("unknown preset"), "{stderr}");

    let out = ofm(&["--dump-keys"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");

    let out = ofm(&["--no-such-flag"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}

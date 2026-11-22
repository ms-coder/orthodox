//! T8.5 adversarial real-shell tests for `ofm_core::menu_expand` (F28).
//!
//! Complements `menu_expand_real_shells.rs` (which covers the classic injection names) with the
//! remaining hostile name classes: option-looking names (`-rf`, `--`), glob/bracket patterns,
//! history expansion, brace expansion, names that are only quotes or end in a backslash, leading
//! and trailing whitespace, a 4 KiB name, and the empty name — run through the *real* bash, zsh
//! and fish, asserting the shell receives exactly the original bytes and never executes part of
//! a name. Control-byte names must be refused before any shell runs. Shells that are not
//! installed are skipped politely.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ofm_core::cmdline::QuoteStyle;
use ofm_core::menu_expand::{ExpandCtx, ExpandError, expand};

fn have(shell: &str) -> bool {
    Command::new(shell)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn style_of(shell: &str) -> QuoteStyle {
    if shell == "fish" {
        QuoteStyle::Fish
    } else {
        QuoteStyle::Posix
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("ofm-adv-menu-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn ctx<'a>(cursor: Option<&'a OsStr>, selection: &'a [OsString], dir: &'a Path) -> ExpandCtx<'a> {
    ExpandCtx {
        cursor,
        selection,
        dir,
        other_dir: dir,
        other_cursor: cursor,
    }
}

fn run_shell(shell: &str, script: &[u8], cwd: &Path) -> std::process::Output {
    Command::new(shell)
        .arg("-c")
        .arg(OsStr::from_bytes(script))
        .current_dir(cwd)
        .output()
        .unwrap()
}

/// Adversarial names without control bytes (those must be refused pre-shell).
fn hostile_names() -> Vec<OsString> {
    let mut v: Vec<OsString> = [
        "-rf",        // looks like an option
        "--",         // option terminator
        "-",          // lone dash
        "--help=x",   // long option with value
        "?",          // glob
        "[a-b]",      // glob class
        "*.*",        // glob
        "#comment",   // shell comment start
        "!hist",      // history expansion (interactive bash)
        "a{b,c}d",    // brace expansion
        "$(())",      // arithmetic
        "x=y",        // assignment-looking
        "'''",        // only quotes
        "''",         // empty-string-looking
        "'",          // lone quote
        "end\\",      // trailing backslash
        "\\",         // lone backslash
        " lead",      // leading space
        "trail ",     // trailing space
        "\ttab-lead", // leading tab is a CONTROL byte — excluded below
        "~",          // tilde expansion
        "~/etc",      // tilde path
        "=cmd",       // zsh: =command expansion
        "100%",       // percent (menu escape char) as data
        "%f",         // placeholder-looking
        "a%nb",       // placeholder-looking
        "%{input:x}", // placeholder-looking
    ]
    .iter()
    .map(OsString::from)
    .collect();
    v.retain(|n| !ofm_core::cmdline::has_control_bytes(n.as_bytes()));
    // 4 KiB name and a non-UTF-8 name.
    v.push(OsString::from("B".repeat(4096)));
    v.push(OsStringExt::from_vec(b"\xf1\xf2'\\ ".to_vec()));
    v
}

const WORD_TEMPLATE: &str = "printf '%%s\\n' %s";

#[test]
fn hostile_names_round_trip_through_real_shells() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("hostile-{shell}"));
        let names = hostile_names();
        let c = ctx(Some(OsStr::new("cursor-'file'")), &names, &sc.0);
        let script = expand(WORD_TEMPLATE, &c, style_of(shell))
            .unwrap()
            .finish()
            .unwrap();
        let out = run_shell(shell, &script, &sc.0);
        assert!(
            out.status.success(),
            "{shell}: {script:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let mut want = Vec::new();
        for n in &names {
            want.extend_from_slice(n.as_bytes());
            want.push(b'\n');
        }
        assert_eq!(
            out.stdout,
            want,
            "{shell}: selection expanded wrongly (script {:?})",
            String::from_utf8_lossy(&script)
        );
        // Nothing inside a name was executed.
        assert!(
            !sc.0.join("pwned").exists() && !Path::new("/tmp/pwned").exists(),
            "{shell}: executed part of a substituted name"
        );
    }
}

#[test]
fn hostile_cursor_and_stem_round_trip() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("stem-{shell}"));
        for name in hostile_names() {
            for template in ["printf '%%s\\n' %f", "printf '%%s\\n' %n"] {
                let want_name: OsString = if template.contains("%n") {
                    Path::new(&name)
                        .file_stem()
                        .unwrap_or(name.as_os_str())
                        .to_owned()
                } else {
                    name.clone()
                };
                let c = ctx(Some(name.as_os_str()), &[], &sc.0);
                let script = expand(template, &c, style_of(shell))
                    .unwrap()
                    .finish()
                    .unwrap();
                let out = run_shell(shell, &script, &sc.0);
                assert!(
                    out.status.success(),
                    "{shell}: {script:?}: {}",
                    String::from_utf8_lossy(&out.stderr)
                );
                assert_eq!(
                    out.stdout,
                    [want_name.as_bytes(), b"\n"].concat(),
                    "{shell}: {template:?} for {name:?}"
                );
            }
        }
        assert!(
            !sc.0.join("pwned").exists(),
            "{shell}: executed part of a substituted name"
        );
    }
}

/// Control bytes (tab, CR, LF, ESC, DEL, NUL) must make `expand` refuse — the shell must never
/// see the bytes, and (stronger) the test asserts the refusal without spawning a shell at all.
#[test]
fn control_byte_names_are_refused_before_any_shell_runs() {
    for bytes in [
        &b"a\tb"[..], // tab
        b"a\rb",      // CR
        b"a\nb",      // LF
        b"a\x1bb",    // ESC
        b"a\x7fb",    // DEL
        b"a\0b",      // NUL
        b"\x01",      // bare SOH
    ] {
        let name: OsString = OsStringExt::from_vec(bytes.to_vec());
        let sel = [name.clone()];
        let c = ctx(Some(&name), &sel, Path::new("/tmp"));
        for template in ["echo %f", "echo %s", "echo %n"] {
            let err = expand(template, &c, style_of("bash")).unwrap_err();
            assert!(
                matches!(err, ExpandError::UnsafeName { .. }),
                "{template:?} name {bytes:?}: {err:?}"
            );
        }
    }
}

/// The empty name round-trips as an empty shell word in every real shell.
#[test]
fn empty_name_round_trips() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("empty-{shell}"));
        let empty = OsString::new();
        let other = OsString::from("x");
        let sel = [empty.clone(), other];
        let c = ctx(None, &sel, &sc.0);
        let script = expand(WORD_TEMPLATE, &c, style_of(shell))
            .unwrap()
            .finish()
            .unwrap();
        let out = run_shell(shell, &script, &sc.0);
        assert_eq!(out.stdout, b"\nx\n", "{shell}: script {script:?}");
    }
}

/// A 4 KiB name passes through the real shell byte-exact (guards against quoting length bugs).
#[test]
fn four_kib_name_round_trips() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("big-{shell}"));
        let big = OsString::from(format!("'\\{}", "y".repeat(4090)));
        let c = ctx(Some(big.as_os_str()), &[], &sc.0);
        let script = expand("printf '%%s\\n' %f", &c, style_of(shell))
            .unwrap()
            .finish()
            .unwrap();
        let out = run_shell(shell, &script, &sc.0);
        assert!(
            out.status.success(),
            "{shell}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(out.stdout, [big.as_bytes(), b"\n"].concat(), "{shell}");
    }
}

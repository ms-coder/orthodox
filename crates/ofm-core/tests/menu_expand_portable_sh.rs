//! T8.10 regression: user-menu commands run through `/bin/sh -c` (dash on Ubuntu), which does
//! not understand bash's `$'…'`. A non-UTF-8 name such as `\xff';touch pwned;#` quoted as
//! `$'\xff\';touch pwned;#'` made dash run `touch pwned`. [`QuoteStyle::PortableSh`] must hand
//! every POSIX shell (the real `/bin/sh`, `dash`, `bash --posix`) exactly the original bytes.
//!
//! The reference "un-quoter" is a real `sh -c 'printf %s\000 …'` run, not a hand-written parser.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ofm_core::cmdline::{QuoteStyle, shell_quote_for};
use ofm_core::menu_expand::{ExpandCtx, ExpandError, expand};

/// (program, leading args) of every POSIX-ish shell present on this machine.
fn shells() -> Vec<(&'static str, Vec<&'static str>)> {
    let candidates: [(&str, Vec<&str>); 3] = [
        ("/bin/sh", vec![]),
        ("dash", vec![]),
        ("bash", vec!["--posix"]),
    ];
    candidates
        .into_iter()
        .filter(|(prog, args)| {
            Command::new(prog)
                .args(args)
                .arg("-c")
                .arg("true")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .collect()
}

struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Scratch {
        let p = std::env::temp_dir().join(format!("ofm-portable-{tag}-{}", std::process::id()));
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

fn run(shell: &(&str, Vec<&str>), script: &[u8], cwd: &Path) -> std::process::Output {
    Command::new(shell.0)
        .args(&shell.1)
        .arg("-c")
        .arg(OsStr::from_bytes(script))
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn nasty() -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = [
        "plain",
        "",
        "it's",
        "\\'",
        "\\'; touch pwned; #",
        "'; touch pwned; #",
        "$(touch pwned)",
        "`touch pwned`",
        "$HOME",
        "*",
        "-rf",
        "київ",
    ]
    .iter()
    .map(|s| s.as_bytes().to_vec())
    .collect();
    v.push(b"\xff';touch pwned;#".to_vec()); // the reviewer's reproduction
    v.push(b"\xff\\';touch pwned;#".to_vec());
    v.push(b"\xff$(touch pwned)\xfe".to_vec());
    v.push(b"x\xff`touch pwned`".to_vec());
    v.push(b"\xc3".to_vec()); // truncated UTF-8
    v.push(b"a\xe2\x82".to_vec());
    v
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

/// NUL-terminated `printf` of every argument: unambiguous for any byte but NUL.
const ARGV_TEMPLATE: &str = "printf '%%s\\000' %f %F";

fn split_nul(out: &[u8]) -> Vec<Vec<u8>> {
    let mut v: Vec<Vec<u8>> = out.split(|&b| b == 0).map(<[u8]>::to_vec).collect();
    assert_eq!(v.pop(), Some(Vec::new()), "output must end with NUL");
    v
}

/// The reviewer's reproduction, pinned: this is what the *old* `QuoteStyle::Posix` text form
/// produced for the name `\xff';touch pwned;#` (`$'…'` with `'` written `\'`). dash does not
/// know ANSI-C quoting and runs `touch pwned`. Kept so the attack stays documented next to the
/// fix; the tests above prove the new form never produces this.
#[test]
fn old_ansi_c_quoting_was_injectable_under_dash() {
    let old = "printf '%s\\000' $'\\xff\\';touch pwned;#'";
    let sc = Scratch::new("before");
    for shell in shells() {
        let _ = run(&shell, old.as_bytes(), &sc.0);
    }
    assert!(
        sc.0.join("pwned").exists(),
        "expected the old quoting to be injectable (documentation of the attack)"
    );
}

#[test]
fn nasty_names_reach_every_posix_shell_unchanged() {
    for shell in shells() {
        let sc = Scratch::new("names");
        for name in nasty() {
            let name = OsString::from_vec(name);
            let c = ctx(Some(name.as_os_str()), &[], &sc.0);
            let script = expand(ARGV_TEMPLATE, &c, QuoteStyle::PortableSh)
                .unwrap()
                .finish()
                .unwrap();
            assert!(
                !script.windows(2).any(|w| w == b"$'"),
                "ANSI-C quoting in {script:?}"
            );
            let out = run(&shell, &script, &sc.0);
            assert!(
                out.status.success(),
                "{shell:?}: {script:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert_eq!(
                split_nul(&out.stdout),
                vec![name.as_bytes().to_vec(), name.as_bytes().to_vec()],
                "{shell:?}: {script:?}"
            );
            assert!(
                !sc.0.join("pwned").exists(),
                "{shell:?}: injection by {name:?}"
            );
        }
    }
}

#[test]
fn selection_and_dirs_are_quoted_the_same_way() {
    for shell in shells() {
        let sc = Scratch::new("sel");
        let names: Vec<OsString> = nasty().into_iter().map(OsString::from_vec).collect();
        let dir = PathBuf::from(OsString::from_vec(b"/tmp/\xff';touch pwned;#/d".to_vec()));
        let c = ctx(Some(OsStr::new("cursor")), &names, &dir);
        let script = expand("printf '%%s\\000' %s %d %D", &c, QuoteStyle::PortableSh)
            .unwrap()
            .finish()
            .unwrap();
        let out = run(&shell, &script, &sc.0);
        assert!(out.status.success(), "{shell:?}");
        let mut want: Vec<Vec<u8>> = names.iter().map(|n| n.as_bytes().to_vec()).collect();
        want.push(dir.as_os_str().as_bytes().to_vec());
        want.push(dir.as_os_str().as_bytes().to_vec());
        assert_eq!(split_nul(&out.stdout), want, "{shell:?}");
        assert!(!sc.0.join("pwned").exists());
    }
}

#[test]
fn control_bytes_are_still_refused() {
    let sc = Scratch::new("ctl");
    for bad in [&b"a\nb"[..], b"\x15x", b"\xff\n", b"tab\t", b"del\x7f"] {
        let name = OsString::from_vec(bad.to_vec());
        let c = ctx(Some(name.as_os_str()), &[], &sc.0);
        assert!(matches!(
            expand(ARGV_TEMPLATE, &c, QuoteStyle::PortableSh),
            Err(ExpandError::UnsafeName { .. })
        ));
    }
}

/// The typed-text (String) form used by the command line when the terminal's shell is not
/// bash/zsh/fish: printable, no `$'`, and exact under every POSIX shell for names without
/// control bytes. Control bytes go through `$(printf …)` runs, which cannot be made
/// newline-exact inline in POSIX sh (command substitution strips trailing newlines of a run) —
/// callers must refuse control-byte names with `has_control_bytes` first, exactly as the
/// command-line inserters already do; `trailing_newline_gap_is_documented` pins the loss.
#[test]
fn portable_typed_text_round_trips_through_real_shells() {
    for shell in shells() {
        let sc = Scratch::new("typed");
        for name in nasty() {
            let quoted = shell_quote_for(QuoteStyle::PortableSh, OsStr::from_bytes(&name));
            assert!(
                quoted
                    .bytes()
                    .all(|b| (0x20..0x7f).contains(&b) || b >= 0x80),
                "not printable: {quoted:?}"
            );
            assert!(!quoted.contains("$'"), "{quoted:?}");
            let script = format!("printf '%s\\000' {quoted}");
            let out = run(&shell, script.as_bytes(), &sc.0);
            assert_eq!(split_nul(&out.stdout), vec![name], "{shell:?}: {script:?}");
            assert!(!sc.0.join("pwned").exists());
        }
    }
}

/// The documented gap: command substitution strips trailing newlines of a `printf` run, so a
/// run that is or ends in newlines is not exact in the typed-text form. Pinned so a change is
/// noticed (the exec path and the command-line inserters refuse control bytes before quoting).
#[test]
fn trailing_newline_gap_is_documented() {
    let q = shell_quote_for(QuoteStyle::PortableSh, OsStr::from_bytes(b"x\n"));
    assert_eq!(q, "'x'\"$(printf '\\012')\"");
    // A run of only newlines collapses to nothing; runs must not end in a newline.
    let q = shell_quote_for(QuoteStyle::PortableSh, OsStr::from_bytes(b"a\nb"));
    assert_eq!(q, "'a'\"$(printf '\\012')\"'b'");
}

mod proptests {
    use super::*;
    use proptest::prelude::*;

    fn name_strategy() -> BoxedStrategy<Vec<u8>> {
        prop_oneof![
            2 => Just(b"\xff';touch pwned;#".to_vec()),
            1 => Just(b"\\';touch pwned;#".to_vec()),
            8 => proptest::collection::vec(any::<u8>(), 0..24).prop_map(|v| {
                v.into_iter()
                    .map(|b| if b < 0x20 || b == 0x7f { b'x' } else { b })
                    .collect()
            }),
        ]
        .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(48))]

        /// Any name, expanded and run by the real sh/dash/bash --posix, comes back byte-exact
        /// and runs nothing extra.
        #[test]
        fn any_name_round_trips_through_real_sh(name in name_strategy()) {
            let sc = Scratch::new("prop");
            let name = OsString::from_vec(name);
            let c = ctx(Some(name.as_os_str()), &[], &sc.0);
            let script = expand(ARGV_TEMPLATE, &c, QuoteStyle::PortableSh)
                .unwrap()
                .finish()
                .unwrap();
            for shell in shells() {
                let out = run(&shell, &script, &sc.0);
                prop_assert!(out.status.success(), "{shell:?}: {script:?}");
                prop_assert_eq!(
                    split_nul(&out.stdout),
                    vec![name.as_bytes().to_vec(), name.as_bytes().to_vec()]
                );
                prop_assert!(!sc.0.join("pwned").exists());
            }
        }
    }
}

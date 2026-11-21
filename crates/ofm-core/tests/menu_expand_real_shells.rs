//! Round-trips the user-menu placeholder expansion (F28) through the *real* bash, zsh and
//! fish (each test skips when the shell is not installed): a menu template expanded by OFM
//! must hand the shell exactly the original names — quoted per shell family, never executed
//! (`$(touch pwned)`, the fish `\'; touch pwned; #` injection, …) — and the `%{input:…}`
//! answers must land verbatim. Proptests then fuzz the same property over random names.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use ofm_core::cmdline::QuoteStyle;
use ofm_core::menu_expand::{ExpandCtx, expand};

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
        let p = std::env::temp_dir().join(format!("ofm-menu-{tag}-{}", std::process::id()));
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

/// Nasty names *without* control bytes (those are refused before any shell sees them).
fn nasty_names() -> Vec<OsString> {
    let v: Vec<OsString> = [
        "",
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
        "--help",
        "київ",
        "emoji-😀",
        "}",
        "%f", // a name that looks like a placeholder must not be re-expanded
        "%{input:x}",
    ]
    .iter()
    .map(OsString::from)
    .collect();
    let mut v = v;
    v.push(OsString::from_vec(vec![b'x', 0xff, b'y']));
    v
}

fn ctx_for<'a>(
    cursor: Option<&'a OsStr>,
    selection: &'a [OsString],
    dir: &'a Path,
    other_dir: &'a Path,
) -> ExpandCtx<'a> {
    ExpandCtx {
        cursor,
        selection,
        dir,
        other_dir,
        other_cursor: Some(OsStr::new("other")),
    }
}

fn ctx_full<'a>(
    cursor: Option<&'a OsStr>,
    selection: &'a [OsString],
    dir: &'a Path,
    other_dir: &'a Path,
) -> ExpandCtx<'a> {
    ExpandCtx {
        cursor,
        selection,
        dir,
        other_dir,
        other_cursor: cursor,
    }
}

/// `printf '%s\n' <quoted names>` prints every name followed by a newline; the `%%` keeps
/// the template's own printf format from being read as a menu placeholder.
const WORD_TEMPLATE: &str = "printf '%%s\\n' %s";

fn run_shell(shell: &str, script: &[u8], cwd: &Path) -> std::process::Output {
    Command::new(shell)
        .arg("-c")
        .arg(OsStr::from_bytes(script))
        .current_dir(cwd)
        .output()
        .unwrap()
}

fn no_pwned(sc: &Scratch, shell: &str) {
    assert!(
        !sc.0.join("pwned").exists(),
        "{shell}: executed part of a substituted name"
    );
}

#[test]
fn selection_round_trips_through_real_shells() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("sel-{shell}"));
        for names in [vec![OsString::from("solo")], nasty_names()] {
            let c = ctx_for(Some(OsStr::new("cursor")), &names, &sc.0, &sc.0);
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
                out.stdout, want,
                "{shell}: selection expanded wrongly for {names:?}"
            );
            no_pwned(&sc, shell);
        }
    }
}

#[test]
fn every_name_placeholder_round_trips() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("names-{shell}"));
        let style = style_of(shell);
        for name in nasty_names() {
            for template in [
                "printf '%%s\\n' %f",
                "printf '%%s\\n' %n",
                "printf '%%s\\n' %F",
            ] {
                // %n strips the last extension, so compare against the stem for that one.
                let want_name: OsString = if template.contains("%n") {
                    Path::new(&name)
                        .file_stem()
                        .unwrap_or(name.as_os_str())
                        .to_owned()
                } else {
                    name.clone()
                };
                let c = ctx_full(Some(name.as_os_str()), &[], &sc.0, &sc.0);
                let script = expand(template, &c, style).unwrap().finish().unwrap();
                let out = run_shell(shell, &script, &sc.0);
                assert_eq!(
                    out.stdout,
                    [want_name.as_bytes(), b"\n"].concat(),
                    "{shell}: {template:?} for {name:?}"
                );
            }
            // The directory placeholders quote whole paths the same way.
            for template in ["printf '%%s\\n' %d", "printf '%%s\\n' %D"] {
                let c = ctx_for(Some(name.as_os_str()), &[], Path::new("/tmp"), &sc.0);
                let script = expand(template, &c, style).unwrap().finish().unwrap();
                let out = run_shell(shell, &script, &sc.0);
                let want_path = if template.contains("%d") {
                    String::from("/tmp")
                } else {
                    sc.0.to_string_lossy().into_owned()
                };
                assert_eq!(
                    out.stdout,
                    format!("{want_path}\n").as_bytes(),
                    "{shell}: {template:?}"
                );
            }
            no_pwned(&sc, shell);
        }
    }
}

#[test]
fn empty_selection_falls_back_to_the_cursor_file() {
    if !have("bash") {
        eprintln!("skipping bash: not installed");
        return;
    }
    let sc = Scratch::new("fallback");
    let name = OsString::from("only-'file'");
    let c = ctx_for(Some(name.as_os_str()), &[], &sc.0, &sc.0);
    let script = expand(WORD_TEMPLATE, &c, QuoteStyle::Posix)
        .unwrap()
        .finish()
        .unwrap();
    let out = run_shell("bash", &script, &sc.0);
    assert_eq!(out.stdout, b"only-'file'\n");
}

#[test]
fn input_answers_land_verbatim() {
    for shell in ["bash", "zsh", "fish"] {
        if !have(shell) {
            eprintln!("skipping {shell}: not installed");
            continue;
        }
        let sc = Scratch::new(&format!("input-{shell}"));
        let c = ctx_for(None, &[], &sc.0, &sc.0);

        // Unquoted in the template: the answer is the user's typing, so a one-word answer
        // arrives as exactly that word.
        let pending = expand("printf '%%s\\n' %{input:Archive name}", &c, style_of(shell)).unwrap();
        assert_eq!(pending.prompts(), ["Archive name"]);
        let script = pending.fill_inputs(&["café-42".to_string()]).unwrap();
        let out = run_shell(shell, &script, &sc.0);
        assert!(
            out.status.success(),
            "{shell}: {script:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout,
            "café-42\n".as_bytes(),
            "{shell}: one-word answer"
        );

        // Quoted by the template author: multi-word answers stay one shell word.
        let pending = expand("printf '%%s\\n' '%{input:Title}'", &c, style_of(shell)).unwrap();
        assert_eq!(pending.prompts(), ["Title"]);
        let script = pending
            .fill_inputs(&["my archive \"quoted\" $notvar".to_string()])
            .unwrap();
        let out = run_shell(shell, &script, &sc.0);
        assert!(
            out.status.success(),
            "{shell}: {script:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            out.stdout, b"my archive \"quoted\" $notvar\n",
            "{shell}: answer not substituted verbatim"
        );
    }
}

mod proptests {
    use super::*;
    use ofm_core::cmdline::shell_quote_for;
    use proptest::prelude::*;

    /// Random names with control bytes mapped away (they are refused — see the property
    /// below); may be non-UTF-8, may contain any shell metacharacter byte.
    fn name_strategy() -> BoxedStrategy<OsString> {
        prop_oneof![
            2 => Just(OsString::from("\\'; touch pwned; #")),
            2 => Just(OsString::from("'; touch pwned; #")),
            1 => Just(OsString::from("$(touch pwned)")),
            1 => Just(OsString::from("-rf")),
            1 => Just(OsString::from("київ")),
            8 => proptest::collection::vec(any::<u8>(), 0..24).prop_map(|v| {
                OsString::from_vec(
                    v.into_iter()
                        .map(|b| if b < 0x20 || b == 0x7f { b'x' } else { b })
                        .collect(),
                )
            }),
        ]
        .boxed()
    }

    /// Random template literals without `%` (the property adds the placeholder itself).
    fn literal_strategy() -> BoxedStrategy<String> {
        proptest::collection::vec(any::<u8>(), 0..20)
            .prop_map(|v| {
                String::from_utf8_lossy(
                    &v.into_iter()
                        .map(|b| if b == b'%' { b'x' } else { b })
                        .collect::<Vec<u8>>(),
                )
                .into_owned()
            })
            .boxed()
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(64))]

        /// The no-injection core property: a %f name expanded into `printf '%s\n'` and run by
        /// the real bash prints exactly the original bytes.
        #[test]
        fn cursor_name_round_trips_through_bash(name in name_strategy()) {
            if !have("bash") {
                // Rare (CI without bash); reject the case so it is neither pass nor fail.
                return Ok(());
            }
            let sc = Scratch::new("prop");
            let c = ctx_for(Some(name.as_os_str()), &[], &sc.0, &sc.0);
            let script = expand("printf '%%s\\n' %f", &c, QuoteStyle::Posix)
                .unwrap()
                .finish()
                .unwrap();
            let out = run_shell("bash", &script, &sc.0);
            prop_assert!(out.status.success(), "{:?}", String::from_utf8_lossy(&out.stderr));
            let want = [name.as_bytes(), b"\n"].concat();
            prop_assert_eq!(out.stdout, want, "script {:?}", script);
            prop_assert!(!sc.0.join("pwned").exists());
        }

        /// Whole selections round-trip: %s is every name as its own quoted word.
        #[test]
        fn selection_round_trips_through_bash(
            names in prop::collection::vec(name_strategy(), 0..6),
        ) {
            if !have("bash") {
                return Ok(());
            }
            let sc = Scratch::new("prop-sel");
            let cursor = OsString::from("cursor-fallback");
            let c = ctx_for(Some(cursor.as_os_str()), &names, &sc.0, &sc.0);
            let script = expand(WORD_TEMPLATE, &c, QuoteStyle::Posix)
                .unwrap()
                .finish()
                .unwrap();
            let out = run_shell("bash", &script, &sc.0);
            prop_assert!(out.status.success());
            let effective: Vec<&OsString> = if names.is_empty() { vec![&cursor] } else { names.iter().collect() };
            let mut want = Vec::new();
            for n in effective {
                want.extend_from_slice(n.as_bytes());
                want.push(b'\n');
            }
            prop_assert_eq!(out.stdout, want, "script {:?}", script);
        }

        /// Pure composition property (no shell): expansion is exactly the literal text with
        /// each name replaced by its `shell_quote_for` form — for both shell families.
        #[test]
        fn expansion_is_literals_plus_quoted_names(
            left in literal_strategy(),
            right in literal_strategy(),
            name in name_strategy(),
            style in prop::sample::select(vec![QuoteStyle::Posix, QuoteStyle::Fish]),
        ) {
            let template = format!("{left} %f {right}");
            let c = ctx_for(Some(name.as_os_str()), &[], Path::new("/d"), Path::new("/D"));
            let got = expand(&template, &c, style).unwrap().finish().unwrap();
            let want: Vec<u8> = format!("{left} {} {right}", shell_quote_for(style, name.as_os_str()))
                .into_bytes();
            prop_assert_eq!(got, want);
        }

        /// Names with control bytes are always refused, for every placeholder and style.
        #[test]
        fn control_byte_names_are_refused(
            bytes in prop::collection::vec(0u8..=0x20, 1..8),
            style in prop::sample::select(vec![QuoteStyle::Posix, QuoteStyle::Fish]),
        ) {
            // Force at least one true control byte (0x20 itself is a plain space).
            let mut bytes = bytes;
            bytes[0] = 0x15;
            let name = OsString::from_vec(bytes.clone());
            let sel = [name.clone()];
            let c = ctx_full(Some(name.as_os_str()), &sel, Path::new("/d"), Path::new("/D"));
            for template in ["echo %f", "echo %n", "echo %s", "echo %F"] {
                // Braces stay out of `prop_assert!` (it treats them as format placeholders).
                let refused =
                    matches!(
                        expand(template, &c, style),
                        Err(ofm_core::menu_expand::ExpandError::UnsafeName { .. })
                    );
                prop_assert!(refused, "{template}");
            }
        }
    }
}

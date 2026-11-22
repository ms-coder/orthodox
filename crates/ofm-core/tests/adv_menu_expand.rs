//! T8.5 adversarial tests for `ofm_core::menu_expand` (F28 placeholder expansion, pure part).
//!
//! Extends the module unit tests and `menu_expand_real_shells.rs` with grammar edge cases:
//! nested/unbalanced braces inside `%{input:…}`, prompt text that itself contains placeholder
//! syntax, `%`-runs, multi-byte unknown placeholders, extreme selection sizes (0/1/10000) and
//! extreme names (4 KiB, dotfiles, names that are only quotes), plus a full control-byte refusal
//! matrix (tab, CR, LF, ESC, DEL, NUL).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use ofm_core::cmdline::{QuoteStyle, shell_quote_for};
use ofm_core::menu_expand::{ExpandCtx, ExpandError, expand, validate_template};

fn ctx<'a>(cursor: Option<&'a OsStr>, selection: &'a [OsString], dir: &'a Path) -> ExpandCtx<'a> {
    ExpandCtx {
        cursor,
        selection,
        dir,
        other_dir: Path::new("/other"),
        other_cursor: Some(OsStr::new("other.txt")),
    }
}

fn posix(template: &str, c: &ExpandCtx<'_>) -> Vec<u8> {
    expand(template, c, QuoteStyle::Posix)
        .unwrap()
        .finish()
        .unwrap()
}

// --- Grammar: braces and percents ----------------------------------------------------------

/// `%{input:…}` ends at the FIRST `}`: braces inside the prompt are prompt text, and the rest of
/// the template continues as literals.
#[test]
fn braces_inside_input_prompt_are_prompt_text() {
    let c = ctx(None, &[], Path::new("/d"));
    let e = expand("%{input:a{b}c}", &c, QuoteStyle::Posix).unwrap();
    assert_eq!(
        e.prompts(),
        ["a{b"],
        "prompt ends at the first closing brace"
    );
    assert_eq!(e.fill_inputs(&["X".into()]).unwrap(), b"Xc}");

    // A `}` right after the empty prompt: prompt "" and a literal "}" behind it.
    let e = expand("%{input:}}", &c, QuoteStyle::Posix).unwrap();
    assert_eq!(e.prompts(), [""]);
    assert_eq!(e.fill_inputs(&["Y".into()]).unwrap(), b"Y}");
}

/// Prompt text that looks like placeholder syntax must not be re-scanned: `%f`, `%%` and
/// `%{input:x}` inside a prompt stay literal prompt text. The prompt still ends at the FIRST
/// `}` — here that is the inner brace of `%{input:nested}`.
#[test]
fn prompt_text_containing_placeholder_syntax_is_not_rescanned() {
    let c = ctx(None, &[], Path::new("/d"));
    let template = "%{input:100% %f %% %{input:nested}}";
    let e = expand(template, &c, QuoteStyle::Posix).unwrap();
    assert_eq!(e.prompts(), ["100% %f %% %{input:nested"],);
    assert_eq!(e.fill_inputs(&["answer".into()]).unwrap(), b"answer}");
}

/// `%{` without any closing brace is an unterminated-placeholder error naming the `%` position,
/// even at the end of the template and even when an `%{input:` prefix is present.
#[test]
fn unterminated_brace_variants() {
    let c = ctx(None, &[], Path::new("/d"));
    for (template, position) in [
        ("%{", 0),
        ("%{input:x", 0),
        ("echo %{input:x", 5),
        ("%{input:", 0),
    ] {
        assert_eq!(
            expand(template, &c, QuoteStyle::Posix),
            Err(ExpandError::UnterminatedPlaceholder { position }),
            "{template:?}"
        );
        assert!(validate_template(template).is_err());
    }
}

/// Percent-runs scan left to right in pairs: `%%%f` is an escaped `%` followed by the cursor
/// placeholder; `%%%%f` is two escaped percents and a literal `f`; a trailing lone `%` after an
/// escape is a stray percent.
#[test]
fn percent_runs() {
    let c = ctx(Some(OsStr::new("n")), &[], Path::new("/d"));
    assert_eq!(posix("%%%f", &c), b"%'n'");
    assert_eq!(posix("%%%%f", &c), b"%%f");
    assert_eq!(posix("a%%b", &c), b"a%b");
    assert_eq!(
        expand("%%%", &c, QuoteStyle::Posix),
        Err(ExpandError::StrayPercent { position: 2 })
    );
}

/// A multi-byte unknown placeholder reports the exact byte position of the `%` and the decoded
/// character, and `validate_template` agrees with `expand`.
#[test]
fn multibyte_unknown_placeholder_positions() {
    let c = ctx(None, &[], Path::new("/d"));
    for (template, position, found) in [("echo %ф", 5, "ф"), ("%€", 0, "€"), ("ab%中cd", 2, "中")]
    {
        assert_eq!(
            expand(template, &c, QuoteStyle::Posix),
            Err(ExpandError::UnknownPlaceholder {
                position,
                found: found.into()
            }),
            "{template:?}"
        );
        assert!(validate_template(template).is_err(), "{template:?}");
    }
}

/// `%%` immediately before `{` does not start an input placeholder: `%{` after an escape is a
/// literal `%{`... it is not — the scanner treats `%` + `%` as an escape, so `%%{input:x}` is a
/// literal `%` followed by `{input:x}`. Pin that reading.
#[test]
fn escaped_percent_before_brace_is_literal() {
    let c = ctx(None, &[], Path::new("/d"));
    let e = expand("%%{input:x}", &c, QuoteStyle::Posix).unwrap();
    assert!(e.prompts().is_empty(), "no prompt: {e:?}");
    assert_eq!(e.finish().unwrap(), b"%{input:x}");
}

// --- Sizes ---------------------------------------------------------------------------------

/// %s with 0 effective names (no selection, no cursor) is MissingCursor, with 1 name it falls
/// back to the cursor, and with 10000 names every one lands as its own quoted word — exact
/// composition with `shell_quote_for`, no shell needed.
#[test]
fn selection_size_extremes() {
    let c0 = ctx(None, &[], Path::new("/d"));
    assert_eq!(
        expand("echo %s", &c0, QuoteStyle::Posix),
        Err(ExpandError::MissingCursor { placeholder: 's' })
    );

    let c1 = ctx(Some(OsStr::new("solo")), &[], Path::new("/d"));
    assert_eq!(posix("echo %s", &c1), b"echo 'solo'");

    let many: Vec<OsString> = (0..10_000)
        .map(|i| format!("n{i}"))
        .map(OsString::from)
        .collect();
    let c = ctx(Some(OsStr::new("cursor")), &many, Path::new("/d"));
    let got = posix("tar -cf out.tar %s", &c);
    let want = format!(
        "tar -cf out.tar {}",
        many.iter()
            .map(|n| shell_quote_for(QuoteStyle::Posix, n))
            .collect::<Vec<_>>()
            .join(" ")
    );
    assert_eq!(got, want.into_bytes());
}

/// A 4 KiB single-byte name expands and quotes exactly (composition property), for both shell
/// families.
#[test]
fn four_kib_name_expands_exactly() {
    let big = OsString::from("x".repeat(4096));
    let c = ctx(Some(&big), &[], Path::new("/d"));
    for style in [QuoteStyle::Posix, QuoteStyle::Fish] {
        let got = expand("echo %f", &c, style).unwrap().finish().unwrap();
        let want = format!("echo {}", shell_quote_for(style, &big)).into_bytes();
        assert_eq!(got, want);
    }
    // And as part of a selection.
    let sel = [big.clone()];
    let c = ctx(None, &sel, Path::new("/d"));
    let got = posix("echo %s", &c);
    assert_eq!(
        got,
        format!("echo {}", shell_quote_for(QuoteStyle::Posix, &big)).into_bytes()
    );
}

// --- Names ---------------------------------------------------------------------------------

/// `%n` stems for degenerate dotfile names follow `Path::file_stem` semantics exactly (the
/// documented contract) and never panic.
#[test]
fn stem_of_degenerate_dot_names() {
    for name in [".", "..", "...", ".x", "..x", "x.", "..x..", "...", "a."] {
        let want_stem = Path::new(name)
            .file_stem()
            .unwrap_or_else(|| OsStr::new(name));
        let c = ctx(Some(OsStr::new(name)), &[], Path::new("/d"));
        let got = posix("echo %n", &c);
        let want = format!("echo '{}'", want_stem.to_string_lossy());
        assert_eq!(got, want.as_bytes(), "name {name:?}");
    }
}

/// An empty name is a legal selection entry and expands to an empty quoted word. (`%%` is the
/// literal percent escape — the template grammar does not know about shell quotes.)
#[test]
fn empty_name_expands_to_empty_word() {
    let sel = [OsString::new(), OsString::from("b")];
    let c = ctx(None, &sel, Path::new("/d"));
    assert_eq!(posix("printf '%%s' %s", &c), b"printf '%s' '' 'b'");
}

/// Names consisting only of quotes/backslashes — the quoting escape-identity cases.
#[test]
fn quote_only_names_compose_exactly() {
    for name in ["'", "''", "'''", "\\", "\\\\", "'\\", "\\'", "a'b'c"] {
        let os_name = OsString::from(name);
        let c = ctx(Some(&os_name), &[], Path::new("/d"));
        for style in [QuoteStyle::Posix, QuoteStyle::Fish] {
            let got = expand("echo %f", &c, style).unwrap().finish().unwrap();
            let want = format!("echo {}", shell_quote_for(style, &os_name)).into_bytes();
            assert_eq!(got, want, "{name:?} {style:?}");
        }
    }
}

/// Names starting with `-`, `~`, `$`, backticks, `*`, `?` are data (quoting handles them). This
/// is the pure side; the real-shell proof lives in adv_menu_expand_shells.rs.
#[test]
fn shell_metachar_leading_names_expand_as_data() {
    for name in [
        "-rf", "--", "~", "~/x", "$HOME", "`id`", "*", "?", "[a-b]", "#c", "!b",
    ] {
        let os_name = OsString::from(name);
        let c = ctx(Some(&os_name), &[], Path::new("/d"));
        let got = posix("echo %f", &c);
        let want = format!("echo {}", shell_quote_for(QuoteStyle::Posix, &os_name)).into_bytes();
        assert_eq!(got, want, "{name:?}");
        // Single quotes are always enough: the expansion must contain the name verbatim.
        let text = String::from_utf8(got).unwrap();
        assert!(
            text.contains(&format!("'{name}'")) || text.contains(name),
            "{name:?}"
        );
    }
}

// --- Control-byte refusal matrix -----------------------------------------------------------

/// Every control byte class (tab, CR, LF, ESC, DEL, NUL) is refused for every name placeholder
/// and for directories — the expansion must error, not emit bytes for the terminal to eat.
#[cfg(unix)]
#[test]
fn control_byte_matrix_refused_everywhere() {
    let offenders: Vec<Vec<u8>> = vec![
        b"a\tb".to_vec(),
        b"a\rb".to_vec(),
        b"a\nb".to_vec(),
        b"a\x1bb".to_vec(),
        b"a\x7fb".to_vec(),
        b"a\0b".to_vec(),
        vec![0x15], // bare NAK
    ];
    for bytes in offenders {
        let name: OsString = OsStringExt::from_vec(bytes.clone());
        let sel = [name.clone()];
        let c = ExpandCtx {
            cursor: Some(&name),
            selection: &sel,
            dir: Path::new("/d"),
            other_dir: Path::new("/d"),
            other_cursor: Some(&name),
        };
        for template in ["echo %f", "echo %F", "echo %n", "echo %s"] {
            let err = expand(template, &c, QuoteStyle::Posix).unwrap_err();
            assert!(
                matches!(err, ExpandError::UnsafeName { .. }),
                "{template:?} name {bytes:?}: {err:?}"
            );
        }
        let bad_dir: std::path::PathBuf =
            std::path::PathBuf::from(OsString::from_vec([b"/d".to_vec(), bytes.clone()].concat()));
        let c2 = ExpandCtx {
            cursor: None,
            selection: &[],
            dir: bad_dir.as_path(),
            other_dir: bad_dir.as_path(),
            other_cursor: None,
        };
        for template in ["echo %d", "echo %D"] {
            assert!(
                matches!(
                    expand(template, &c2, QuoteStyle::Posix),
                    Err(ExpandError::UnsafeName { .. })
                ),
                "{template:?} dir {bytes:?}"
            );
        }
    }
}

/// Non-control high bytes and non-UTF-8 bytes are NOT refused — they are quoted (Posix uses
/// `$'…'`, fish escapes outside the quotes) so the original bytes survive exactly.
#[cfg(unix)]
#[test]
fn non_utf8_high_bytes_are_quoted_not_refused() {
    for bytes in [
        vec![b'x', 0xff, b'y'],
        vec![0x80, 0x81],
        b"caf\xe9".to_vec(),
    ] {
        let name: OsString = OsStringExt::from_vec(bytes);
        let c = ctx(Some(&name), &[], Path::new("/d"));
        let got = posix("echo %f", &c);
        let want = format!("echo {}", shell_quote_for(QuoteStyle::Posix, &name)).into_bytes();
        assert_eq!(got, want, "name {name:?}");
    }
}

// --- Input answers -------------------------------------------------------------------------

/// Several identical prompts are asked several times and filled in order; an answer containing
/// control bytes is refused by index.
#[test]
fn repeated_prompts_fill_in_order_and_validate_each_answer() {
    let c = ctx(None, &[], Path::new("/d"));
    let e = expand("%{input:P} %{input:P} %{input:P}", &c, QuoteStyle::Posix).unwrap();
    assert_eq!(e.prompts(), ["P", "P", "P"]);
    assert_eq!(
        e.fill_inputs(&["1".into(), "2".into(), "3".into()])
            .unwrap(),
        b"1 2 3"
    );
    let e = expand("%{input:P} %{input:P}", &c, QuoteStyle::Posix).unwrap();
    assert_eq!(
        e.fill_inputs(&["ok".into(), "bad\x1b".into()]),
        Err(ExpandError::UnsafeAnswer { index: 1 })
    );
}

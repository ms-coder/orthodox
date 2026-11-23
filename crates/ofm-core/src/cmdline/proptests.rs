//! Property tests for the M4 cmdline helpers (PLAN T4.3 covers the UI side): quoting must
//! round-trip any byte string through a reference POSIX un-quoter, OSC 7 URIs must round-trip
//! any path bytes, and completion must stay inside the candidate set.

use std::ffi::OsString;
use std::path::PathBuf;

use proptest::prelude::*;

use super::quote::{
    QuoteStyle, cd_command_for, fish_quote_bytes, has_control_bytes, shell_quote,
    shell_quote_bytes, shell_quote_for,
};

fn arb_bytes() -> impl Strategy<Value = Vec<u8>> {
    proptest::collection::vec(any::<u8>(), 0..64)
}

fn os(bytes: &[u8]) -> OsString {
    #[cfg(unix)]
    {
        std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
    }
    #[cfg(not(unix))]
    {
        OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Reference un-quoter for the two forms [`shell_quote`] produces, written independently of the
/// quoter: concatenated `'…'` runs where `'` is the `'\''` idiom (POSIX), and `$'…'` with
/// `\\`, `\'` and `\xHH` escapes (ANSI-C). `None` = not something our quoter could emit.
fn reference_unquote(quoted: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    // The word is either entirely single-quoted runs or one ANSI-C string.
    if quoted.first() == Some(&b'$') {
        let body = quoted.get(1..)?;
        if body.first() != Some(&b'\'') {
            return None;
        }
        let mut j = 1;
        while j < body.len() {
            match body[j] {
                b'\'' => {
                    return (j + 1 == body.len()).then_some(out);
                }
                b'\\' => match *body.get(j + 1)? {
                    b'\\' => {
                        out.push(b'\\');
                        j += 2;
                    }
                    b'\'' => {
                        out.push(b'\'');
                        j += 2;
                    }
                    b'x' => {
                        let h = body.get(j + 2..j + 4)?;
                        let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
                        out.push(hex(h[0])? * 16 + hex(h[1])?);
                        j += 4;
                    }
                    _ => return None,
                },
                b => {
                    out.push(b);
                    j += 1;
                }
            }
        }
        return None; // unterminated $'
    }
    // Single-quote runs: 'lit' '\'' 'lit' … 'lit'
    if quoted.first() != Some(&b'\'') {
        return None;
    }
    let mut i = 1;
    loop {
        // literal run until the next quote
        while let Some(&b) = quoted.get(i) {
            if b == b'\'' {
                break;
            }
            out.push(b);
            i += 1;
        }
        // at a closing quote: either `'\''` (an escaped quote, continue) or the end of the word
        if quoted.get(i + 1..i + 3) == Some(b"\\'") {
            out.push(b'\'');
            i += 4; // skip the whole '\'' idiom (its last quote re-opens the next literal run)
            continue;
        }
        if quoted.get(i) == Some(&b'\'') && quoted.get(i + 1).is_none() {
            return Some(out);
        }
        return None;
    }
}

/// Reference un-quoter for the fish forms: `'…'` runs (`\\` and `\'` escapes) concatenated with
/// bare `\xNN` escapes.
fn fish_reference_unquote(quoted: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < quoted.len() {
        match quoted[i] {
            b'\'' => {
                i += 1;
                loop {
                    match *quoted.get(i)? {
                        b'\'' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            let n = *quoted.get(i + 1)?;
                            if n != b'\\' && n != b'\'' {
                                return None;
                            }
                            out.push(n);
                            i += 2;
                        }
                        b => {
                            out.push(b);
                            i += 1;
                        }
                    }
                }
            }
            b'\\' => {
                if quoted.get(i + 1) != Some(&b'x') {
                    return None;
                }
                let h = quoted.get(i + 2..i + 4)?;
                let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
                out.push(hex(h[0])? * 16 + hex(h[1])?);
                i += 4;
            }
            _ => return None,
        }
    }
    if quoted.is_empty() { None } else { Some(out) }
}

proptest! {
    #[test]
    fn shell_quote_bytes_roundtrips_any_bytes(bytes in arb_bytes()) {
        let quoted = shell_quote_bytes(&bytes);
        prop_assert_eq!(reference_unquote(&quoted), Some(bytes.clone()));
    }

    #[test]
    fn shell_quote_roundtrips_any_bytes(bytes in arb_bytes()) {
        let name = os(&bytes);
        let quoted = shell_quote(&name);
        // The String form is ASCII whenever the name is not valid UTF-8 (ANSI-C escapes).
        prop_assert!(quoted.is_ascii() || name.to_str().is_some());
        prop_assert_eq!(reference_unquote(quoted.as_bytes()), Some(bytes));
    }

    #[test]
    fn shell_quote_is_always_printable(bytes in arb_bytes()) {
        let name = os(&bytes);
        for style in [QuoteStyle::Posix, QuoteStyle::Fish] {
            let q = shell_quote_for(style, &name);
            prop_assert!(!has_control_bytes(q.as_bytes()), "{style:?}: {q:?}");
        }
    }

    #[test]
    fn fish_quote_roundtrips_any_bytes(bytes in arb_bytes()) {
        let quoted = fish_quote_bytes(&bytes);
        prop_assert_eq!(fish_reference_unquote(&quoted), Some(bytes));
    }

    #[test]
    fn fish_shell_quote_roundtrips_any_bytes(bytes in arb_bytes()) {
        let quoted = shell_quote_for(QuoteStyle::Fish, &os(&bytes));
        prop_assert_eq!(fish_reference_unquote(quoted.as_bytes()), Some(bytes));
    }

    #[test]
    fn cd_command_roundtrips_through_unquote(path_bytes in arb_bytes()) {
        // NUL is the one byte a path can never contain.
        let path_bytes: Vec<u8> = path_bytes.into_iter().filter(|&b| b != 0).collect();
        let path = PathBuf::from(os(&path_bytes));
        for style in [QuoteStyle::Posix, QuoteStyle::Fish] {
            let cmd = cd_command_for(style, &path);
            if has_control_bytes(&path_bytes) {
                prop_assert_eq!(cmd, None);
                continue;
            }
            let cmd = cmd.unwrap_or_default();
            let rest = cmd.strip_prefix(b" cd -- ").unwrap_or(cmd.as_slice());
            let quoted = rest.strip_suffix(b"\n").unwrap_or(rest);
            let got = match style {
                QuoteStyle::Posix => reference_unquote(quoted),
                QuoteStyle::Fish => fish_reference_unquote(quoted),
                QuoteStyle::PortableSh => unreachable!("not in the style list above"),
            };
            prop_assert_eq!(got, Some(path_bytes.clone()));
        }
    }

    #[test]
    fn osc7_roundtrips_any_path_bytes(path_bytes in arb_bytes()) {
        // OSC 7 carries absolute paths without NUL.
        let mut path: Vec<u8> = path_bytes.into_iter().filter(|&b| b != 0).collect();
        path.insert(0, b'/');
        let path = PathBuf::from(os(&path));
        let uri = super::osc7_encode("box", &path);
        prop_assert_eq!(
            super::osc7_parse(uri.as_bytes()),
            Some(("box".to_owned(), path))
        );
    }

    #[test]
    fn complete_in_matches_exactly_the_prefix_candidates(
        raw_names in proptest::collection::vec(arb_bytes(), 0..16),
        idx in any::<usize>(),
    ) {
        let names: Vec<OsString> = raw_names.iter().map(|n| os(n)).collect();
        let prefix = names
            .get(idx % names.len().max(1))
            .map(|n| {
                let bytes = n.as_encoded_bytes();
                os(&bytes[..bytes.len() / 2])
            })
            .unwrap_or_default();
        let c = super::complete_in(&names, &prefix);
        for m in &c.matches {
            prop_assert!(m.as_encoded_bytes().starts_with(prefix.as_encoded_bytes()));
        }
        let n_match = names
            .iter()
            .filter(|n| n.as_encoded_bytes().starts_with(prefix.as_encoded_bytes()))
            .count();
        prop_assert_eq!(c.matches.len(), n_match);
        prop_assert_eq!(c.unique, n_match == 1);
    }
}

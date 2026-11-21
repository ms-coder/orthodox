//! User-menu placeholder expansion (F28): turns a menu entry's command template into
//! shell-ready bytes with every substituted name individually quoted.
//!
//! Placeholders:
//! - `%f`  name of the file under the cursor (active panel)
//! - `%F`  name of the file under the cursor in the other panel
//! - `%n`  name under the cursor without its extension (`Path::file_stem` semantics, so
//!   `report.pdf` → `report`, `.bashrc` stays `.bashrc`, `a.tar.gz` → `a.tar`)
//! - `%s`  the selection, space separated; falls back to the cursor file when nothing is
//!   selected
//! - `%d`  directory of the active panel, `%D` directory of the other panel
//! - `%{input:Prompt}`  filled in a second step ([`Expanded::fill_inputs`]) so the UI can ask
//!   first; one prompt per occurrence, in order
//! - `%%`  a literal `%`
//!
//! Safety rules (the M4 quoting lessons, see [`crate::cmdline::quote`]):
//! - every substituted name goes through [`shell_quote_for`] for the target shell family, so a
//!   name can never break out of its word — raw names are never concatenated into the command;
//! - names and directories containing control bytes are refused with
//!   [`ExpandError::UnsafeName`]: the terminal's line editor acts on those even inside quotes;
//! - unknown placeholders are an error, never passed through silently;
//! - answers to `%{input:…}` are inserted literally (the user typed them, like a command line),
//!   except that control bytes are refused there as well.

use std::ffi::{OsStr, OsString};
use std::path::Path;

use thiserror::Error;

use crate::cmdline::{QuoteStyle, bytes_of, has_control_bytes, shell_quote_for};

/// Everything [`expand`] needs to know about the two panels. All names are plain file names
/// (no directory component); the directories are full paths.
#[derive(Debug, Clone, Copy)]
pub struct ExpandCtx<'a> {
    /// Name under the cursor in the active panel (`None` in an empty listing).
    pub cursor: Option<&'a OsStr>,
    /// Selected names in the active panel; empty means `%s` falls back to [`Self::cursor`].
    pub selection: &'a [OsString],
    /// Directory shown by the active panel.
    pub dir: &'a Path,
    /// Directory shown by the other panel.
    pub other_dir: &'a Path,
    /// Name under the cursor in the other panel.
    pub other_cursor: Option<&'a OsStr>,
}

/// Why a template could not be expanded.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ExpandError {
    /// A `%…` that is not one of the F28 placeholders.
    #[error("unknown placeholder '%{}' at byte {position}", found.escape_default())]
    UnknownPlaceholder {
        /// Byte offset of the `%`.
        position: usize,
        /// What followed the `%` (truncated).
        found: String,
    },
    /// A `%` at the very end of the template.
    #[error("stray '%' at byte {position}")]
    StrayPercent {
        /// Byte offset of the `%`.
        position: usize,
    },
    /// A `%{…` without the closing `}`.
    #[error("unterminated '%{{' placeholder at byte {position} (missing '}}')")]
    UnterminatedPlaceholder {
        /// Byte offset of the `%`.
        position: usize,
    },
    /// A name (or directory) to substitute holds a control byte; the line editor of a
    /// terminal would act on it even inside quotes.
    #[error("cannot expand %{placeholder}: name {name:?} contains control bytes")]
    UnsafeName {
        /// The placeholder the name was substituted for.
        placeholder: char,
        /// The offending name, lossy.
        name: String,
    },
    /// The placeholder needs a file under the cursor but the panel listing is empty.
    #[error("no file under the cursor for %{placeholder}")]
    MissingCursor {
        /// The placeholder that needed a cursor file.
        placeholder: char,
    },
    /// [`Expanded::fill_inputs`] got a different number of answers than prompts.
    #[error("expected {expected} answer(s), got {got}")]
    WrongAnswerCount {
        /// Prompts pending.
        expected: usize,
        /// Answers supplied.
        got: usize,
    },
    /// An answer holds a control byte.
    #[error("answer {index} contains control bytes")]
    UnsafeAnswer {
        /// Index of the offending answer.
        index: usize,
    },
}

/// A template after [`expand`]: literal, shell-ready byte segments plus the pending
/// `%{input:…}` prompts. Finish it with [`Expanded::fill_inputs`] (or
/// [`Expanded::finish`] when nothing was asked).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    segments: Vec<Segment>,
    prompts: Vec<String>,
}

/// One piece of a half-expanded template.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    /// Literal command bytes; substituted names are already quoted inside.
    Bytes(Vec<u8>),
    /// The `idx`-th pending prompt.
    Input(usize),
}

/// Expand `template` for `ctx`, quoting substituted names for `style`. Does not run anything
/// and does no I/O; [`Expanded::fill_inputs`] completes the `%{input:…}` prompts.
pub fn expand(
    template: &str,
    ctx: &ExpandCtx<'_>,
    style: QuoteStyle,
) -> Result<Expanded, ExpandError> {
    let pieces = scan(template)?;
    let mut segments = Vec::with_capacity(pieces.len());
    let mut prompts = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    for piece in pieces {
        match piece {
            Piece::Literal(bytes) => cur.extend_from_slice(bytes),
            Piece::Input(prompt) => {
                flush(&mut cur, &mut segments);
                segments.push(Segment::Input(prompts.len()));
                prompts.push(prompt.to_owned());
            }
            Piece::Name(kind) => {
                flush(&mut cur, &mut segments);
                match kind {
                    NameKind::Cursor => push_quoted(&mut cur, style, need_cursor(ctx, 'f')?, 'f')?,
                    NameKind::OtherCursor => {
                        push_quoted(&mut cur, style, need_cursor(ctx, 'F')?, 'F')?
                    }
                    NameKind::Stem => {
                        let name = need_cursor(ctx, 'n')?;
                        let stem = Path::new(name).file_stem().unwrap_or(name);
                        push_quoted(&mut cur, style, stem, 'n')?;
                    }
                    NameKind::Selection => {
                        if ctx.selection.is_empty() {
                            push_quoted(&mut cur, style, need_cursor(ctx, 's')?, 's')?;
                        } else {
                            for (i, name) in ctx.selection.iter().enumerate() {
                                if i > 0 {
                                    cur.push(b' ');
                                }
                                push_quoted(&mut cur, style, name, 's')?;
                            }
                        }
                    }
                    NameKind::Dir => push_quoted(&mut cur, style, ctx.dir.as_os_str(), 'd')?,
                    NameKind::OtherDir => {
                        push_quoted(&mut cur, style, ctx.other_dir.as_os_str(), 'D')?
                    }
                }
            }
        }
    }
    flush(&mut cur, &mut segments);
    Ok(Expanded { segments, prompts })
}

/// Syntax-check a template without panel context (used by ofm-config to warn at load time).
/// Accepts any name-bearing placeholder; only the placeholder grammar is checked.
pub fn validate_template(template: &str) -> Result<(), ExpandError> {
    scan(template).map(|_| ())
}

impl Expanded {
    /// The prompts the UI must ask, one per `%{input:…}` occurrence, in order.
    pub fn prompts(&self) -> &[String] {
        &self.prompts
    }

    /// Finish without inputs: the command bytes, or an error when prompts are still pending.
    pub fn finish(self) -> Result<Vec<u8>, ExpandError> {
        if self.prompts.is_empty() {
            Ok(self.concatenate(&[]))
        } else {
            Err(ExpandError::WrongAnswerCount {
                expected: self.prompts.len(),
                got: 0,
            })
        }
    }

    /// Fill every `%{input:…}` with its answer (literal bytes, exactly what the user typed;
    /// control bytes are refused) and return the finished command bytes.
    pub fn fill_inputs(self, answers: &[String]) -> Result<Vec<u8>, ExpandError> {
        if answers.len() != self.prompts.len() {
            return Err(ExpandError::WrongAnswerCount {
                expected: self.prompts.len(),
                got: answers.len(),
            });
        }
        for (i, answer) in answers.iter().enumerate() {
            if has_control_bytes(answer.as_bytes()) {
                return Err(ExpandError::UnsafeAnswer { index: i });
            }
        }
        Ok(self.concatenate(answers))
    }

    /// `answers` must already be validated (length and control bytes).
    fn concatenate(self, answers: &[String]) -> Vec<u8> {
        let mut out = Vec::new();
        for segment in self.segments {
            match segment {
                Segment::Bytes(bytes) => out.extend_from_slice(&bytes),
                Segment::Input(i) => out.extend_from_slice(answers[i].as_bytes()),
            }
        }
        out
    }
}

/// A syntactic piece of a template, from the single grammar walk in [`scan`].
enum Piece<'t> {
    /// Literal template bytes (may span several `%%-escaped` percents).
    Literal(&'t [u8]),
    /// A name-bearing placeholder.
    Name(NameKind),
    /// `%{input:Prompt}` with its prompt text.
    Input(&'t str),
}

/// Which name a placeholder stands for.
#[derive(Debug, Clone, Copy)]
enum NameKind {
    Cursor,
    OtherCursor,
    Stem,
    Selection,
    Dir,
    OtherDir,
}

/// The one place the placeholder grammar is parsed: splits `template` into literal runs and
/// placeholders, validating syntax. `expand` renders the pieces, `validate_template` only
/// checks them, so config-time warnings and run-time errors can never drift apart.
fn scan(template: &str) -> Result<Vec<Piece<'_>>, ExpandError> {
    let bytes = template.as_bytes();
    let mut pieces = Vec::new();
    let mut literal_start: Option<usize> = None;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'%' {
            if literal_start.is_none() {
                literal_start = Some(i);
            }
            i += 1;
            continue;
        }
        if let Some(start) = literal_start.take() {
            pieces.push(Piece::Literal(&bytes[start..i]));
        }
        match bytes.get(i + 1).copied() {
            // `%%` keeps the second percent as the start of the next literal run.
            Some(b'%') => {
                literal_start = Some(i + 1);
                i += 2;
            }
            Some(b'f') => fill_name(&mut pieces, NameKind::Cursor, &mut i),
            Some(b'F') => fill_name(&mut pieces, NameKind::OtherCursor, &mut i),
            Some(b'n') => fill_name(&mut pieces, NameKind::Stem, &mut i),
            Some(b's') => fill_name(&mut pieces, NameKind::Selection, &mut i),
            Some(b'd') => fill_name(&mut pieces, NameKind::Dir, &mut i),
            Some(b'D') => fill_name(&mut pieces, NameKind::OtherDir, &mut i),
            Some(b'{') => {
                // bytes[i+1] is '{' (ASCII), so i+2 is a char boundary.
                let rest = &template[i + 2..];
                let Some(end) = rest.find('}') else {
                    return Err(ExpandError::UnterminatedPlaceholder { position: i });
                };
                let inner = &rest[..end];
                if let Some(prompt) = inner.strip_prefix("input:") {
                    pieces.push(Piece::Input(prompt));
                    i += 2 + end + 1;
                } else {
                    let mut found = String::from("{");
                    found.extend(inner.chars().take(24));
                    if inner.len() > 24 {
                        found.push('…');
                    }
                    return Err(ExpandError::UnknownPlaceholder { position: i, found });
                }
            }
            // End of template, or an unknown single-character/multi-byte placeholder.
            None => return Err(ExpandError::StrayPercent { position: i }),
            Some(_) => {
                let found: String = template[i + 1..].chars().take(1).collect();
                return Err(ExpandError::UnknownPlaceholder { position: i, found });
            }
        }
    }
    if let Some(start) = literal_start {
        pieces.push(Piece::Literal(&bytes[start..]));
    }
    Ok(pieces)
}

/// Push a single-character name placeholder and advance `i` past it.
fn fill_name(pieces: &mut Vec<Piece<'_>>, kind: NameKind, i: &mut usize) {
    pieces.push(Piece::Name(kind));
    *i += 2;
}

/// Close the open literal run, if any.
fn flush(cur: &mut Vec<u8>, segments: &mut Vec<Segment>) {
    if !cur.is_empty() {
        segments.push(Segment::Bytes(std::mem::take(cur)));
    }
}

/// The name a cursor placeholder refers to, or [`ExpandError::MissingCursor`].
fn need_cursor<'a>(ctx: &'a ExpandCtx<'_>, placeholder: char) -> Result<&'a OsStr, ExpandError> {
    let name = if placeholder == 'F' {
        ctx.other_cursor
    } else {
        ctx.cursor
    };
    name.ok_or(ExpandError::MissingCursor { placeholder })
}

/// Append `name` to `out` as one quoted, shell-safe word — refusing control bytes.
fn push_quoted(
    out: &mut Vec<u8>,
    style: QuoteStyle,
    name: &OsStr,
    placeholder: char,
) -> Result<(), ExpandError> {
    if has_control_bytes(&bytes_of(name)) {
        return Err(ExpandError::UnsafeName {
            placeholder,
            name: name.to_string_lossy().into_owned(),
        });
    }
    // shell_quote_for output is always printable text, so the bytes below are exact.
    out.extend_from_slice(shell_quote_for(style, name).as_bytes());
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn os(bytes: &[u8]) -> OsString {
        std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
    }

    fn ctx<'a>(cursor: Option<&'a OsStr>, selection: &'a [OsString]) -> ExpandCtx<'a> {
        ExpandCtx {
            cursor,
            selection,
            dir: Path::new("/tmp/left dir"),
            other_dir: Path::new("/tmp/right"),
            other_cursor: Some(OsStr::new("other.txt")),
        }
    }

    fn expanded_bytes(template: &str, c: &ExpandCtx<'_>, style: QuoteStyle) -> Vec<u8> {
        expand(template, c, style).unwrap().finish().unwrap()
    }

    #[test]
    fn literals_pass_through() {
        let c = ctx(None, &[]);
        assert_eq!(expanded_bytes("du -sh", &c, QuoteStyle::Posix), b"du -sh");
        assert_eq!(expanded_bytes("", &c, QuoteStyle::Posix), b"");
        // Unicode and newlines in the template itself are the author's business.
        assert_eq!(
            expanded_bytes("echo київ", &c, QuoteStyle::Posix),
            "echo київ".as_bytes()
        );
    }

    #[test]
    fn percent_percent_is_literal() {
        let c = ctx(None, &[]);
        assert_eq!(expanded_bytes("100%%", &c, QuoteStyle::Posix), b"100%");
        assert_eq!(expanded_bytes("%%f", &c, QuoteStyle::Posix), b"%f");
        assert_eq!(expanded_bytes("%%%%", &c, QuoteStyle::Posix), b"%%");
    }

    #[test]
    fn name_placeholders_posix() {
        let c = ctx(Some(OsStr::new("it's a file.txt")), &[]);
        assert_eq!(
            expanded_bytes("echo %f", &c, QuoteStyle::Posix),
            b"echo 'it'\\''s a file.txt'"
        );
        assert_eq!(
            expanded_bytes("echo %F", &c, QuoteStyle::Posix),
            b"echo 'other.txt'"
        );
        assert_eq!(
            expanded_bytes("echo %n", &c, QuoteStyle::Posix),
            b"echo 'it'\\''s a file'"
        );
        assert_eq!(
            expanded_bytes("echo %d", &c, QuoteStyle::Posix),
            b"echo '/tmp/left dir'"
        );
        assert_eq!(
            expanded_bytes("echo %D", &c, QuoteStyle::Posix),
            b"echo '/tmp/right'"
        );
    }

    #[test]
    fn fish_style_quotes_differently() {
        let c = ctx(Some(OsStr::new("back\\slash'quote")), &[]);
        assert_eq!(
            expanded_bytes("echo %f", &c, QuoteStyle::Fish),
            b"echo 'back\\\\slash\\'quote'"
        );
    }

    #[test]
    #[cfg(unix)]
    fn non_utf8_names_are_quoted_printable() {
        let name = os(b"\xffx");
        let c = ctx(Some(&name), &[]);
        assert_eq!(
            expanded_bytes("echo %f", &c, QuoteStyle::Posix),
            b"echo $'\\xffx'"
        );
        assert_eq!(
            expanded_bytes("echo %f", &c, QuoteStyle::Fish),
            b"echo \\xff'x'"
        );
    }

    #[test]
    fn selection_is_space_separated_quoted_names() {
        let sel = vec![
            OsString::from("a b"),
            OsString::from("$(touch pwned)"),
            OsString::from("c"),
        ];
        let c = ctx(Some(OsStr::new("cursor")), &sel);
        assert_eq!(
            expanded_bytes("tar -czf out.tgz %s", &c, QuoteStyle::Posix),
            b"tar -czf out.tgz 'a b' '$(touch pwned)' 'c'"
        );
    }

    #[test]
    fn empty_selection_falls_back_to_cursor() {
        let c = ctx(Some(OsStr::new("solo")), &[]);
        assert_eq!(
            expanded_bytes("echo %s", &c, QuoteStyle::Posix),
            b"echo 'solo'"
        );
    }

    #[test]
    fn stem_semantics() {
        let cases = [
            ("report.pdf", "report"),
            ("a.tar.gz", "a.tar"),
            (".bashrc", ".bashrc"),
            ("noext", "noext"),
            ("trailing.", "trailing"),
            ("..dots..", "..dots."),
        ];
        for (name, want) in cases {
            let c = ctx(Some(OsStr::new(name)), &[]);
            let got = expanded_bytes("echo %n", &c, QuoteStyle::Posix);
            assert_eq!(got, format!("echo '{want}'").as_bytes(), "{name}");
        }
    }

    #[test]
    #[cfg(unix)]
    fn control_bytes_are_refused_for_every_name_placeholder() {
        let bad = os(b"a\x15b\n");
        let sel = [bad.clone()];
        let c = ExpandCtx {
            cursor: Some(&bad),
            selection: &sel,
            dir: Path::new("/tmp/left dir"),
            other_dir: Path::new("/tmp/right"),
            other_cursor: Some(&bad),
        };
        for template in ["echo %f", "echo %F", "echo %n", "echo %s"] {
            let err = expand(template, &c, QuoteStyle::Posix).unwrap_err();
            assert!(
                matches!(err, ExpandError::UnsafeName { .. }),
                "{template}: {err:?}"
            );
        }
        // Directories too.
        for template in ["echo %d", "echo %D"] {
            let bad_dir = std::path::PathBuf::from(os(b"/x/y\x1b"));
            let c2 = ExpandCtx {
                cursor: None,
                selection: &[],
                dir: bad_dir.as_path(),
                other_dir: bad_dir.as_path(),
                other_cursor: None,
            };
            let err = expand(template, &c2, QuoteStyle::Posix).unwrap_err();
            assert!(
                matches!(err, ExpandError::UnsafeName { .. }),
                "{template}: {err:?}"
            );
        }
    }

    #[test]
    fn missing_cursor_is_an_error_not_empty() {
        let c = ctx(None, &[]);
        for (template, placeholder) in [("echo %f", 'f'), ("echo %n", 'n'), ("echo %s", 's')] {
            assert_eq!(
                expand(template, &c, QuoteStyle::Posix),
                Err(ExpandError::MissingCursor { placeholder }),
                "{template}"
            );
        }
        let mut c2 = ctx(None, &[]);
        c2.other_cursor = None;
        assert_eq!(
            expand("echo %F", &c2, QuoteStyle::Posix),
            Err(ExpandError::MissingCursor { placeholder: 'F' })
        );
    }

    #[test]
    fn unknown_and_broken_placeholders() {
        let c = ctx(None, &[]);
        assert_eq!(
            expand("echo %x", &c, QuoteStyle::Posix),
            Err(ExpandError::UnknownPlaceholder {
                position: 5,
                found: "x".into()
            })
        );
        assert_eq!(
            expand("echo %", &c, QuoteStyle::Posix),
            Err(ExpandError::StrayPercent { position: 5 })
        );
        assert_eq!(
            expand("%{input:never closed", &c, QuoteStyle::Posix),
            Err(ExpandError::UnterminatedPlaceholder { position: 0 })
        );
        assert_eq!(
            expand("%{zip:file}", &c, QuoteStyle::Posix),
            Err(ExpandError::UnknownPlaceholder {
                position: 0,
                found: "{zip:file".into()
            })
        );
        // Multi-byte unknown placeholder.
        assert_eq!(
            expand("echo %ф", &c, QuoteStyle::Posix),
            Err(ExpandError::UnknownPlaceholder {
                position: 5,
                found: "ф".into()
            })
        );
    }

    #[test]
    fn prompts_are_collected_then_filled_in_order() {
        let c = ctx(None, &[]);
        let e = expand(
            "f %{input:Archive name}.tar.gz %{input:Second} end",
            &c,
            QuoteStyle::Posix,
        )
        .unwrap();
        assert_eq!(e.prompts(), ["Archive name", "Second"]);
        assert_eq!(
            expand("no prompt", &c, QuoteStyle::Posix)
                .unwrap()
                .prompts(),
            Vec::<String>::new()
        );
        let done = e
            .fill_inputs(&["my archive".to_string(), "two".to_string()])
            .unwrap();
        assert_eq!(done, b"f my archive.tar.gz two end");
        // Empty prompt text is allowed.
        assert_eq!(
            expand("%{input:}", &c, QuoteStyle::Posix)
                .unwrap()
                .fill_inputs(&["x".to_string()])
                .unwrap(),
            b"x"
        );
    }

    #[test]
    fn fill_inputs_validates_count_and_answers() {
        let c = ctx(None, &[]);
        let e = expand("a %{input:P1} b %{input:P2}", &c, QuoteStyle::Posix).unwrap();
        assert_eq!(
            e.clone().fill_inputs(&["one".into()]),
            Err(ExpandError::WrongAnswerCount {
                expected: 2,
                got: 1
            })
        );
        assert_eq!(
            e.clone().fill_inputs(&[]),
            Err(ExpandError::WrongAnswerCount {
                expected: 2,
                got: 0
            })
        );
        assert_eq!(
            e.clone().finish(),
            Err(ExpandError::WrongAnswerCount {
                expected: 2,
                got: 0
            })
        );
        assert_eq!(
            e.fill_inputs(&["ok".into(), "bad\nanswer".into()]),
            Err(ExpandError::UnsafeAnswer { index: 1 })
        );
    }

    #[test]
    fn mixed_template_expands_end_to_end() {
        let sel = vec![OsString::from("one"), OsString::from("two")];
        let c = ctx(Some(OsStr::new("file.tar.gz")), &sel);
        let e = expand(
            "tar -czf %{input:Name}.tgz %s && cd -- %d && echo %n 100%%",
            &c,
            QuoteStyle::Posix,
        )
        .unwrap();
        assert_eq!(e.prompts(), ["Name"]);
        assert_eq!(
            e.fill_inputs(&["out".to_string()]).unwrap(),
            b"tar -czf out.tgz 'one' 'two' && cd -- '/tmp/left dir' && echo 'file.tar' 100%"
        );
    }

    #[test]
    fn validate_template_checks_grammar_only() {
        let c = ctx(None, &[]);
        assert!(validate_template("anything %f %s %d %D %F %n %% %{input:Hi}").is_ok());
        assert!(validate_template("%q").is_err());
        assert!(validate_template("trailing %").is_err());
        assert!(validate_template("%{input:x").is_err());
        // validate_template accepts name placeholders without a context…
        assert!(validate_template("%f").is_ok());
        // …while expand reports the missing cursor.
        assert!(expand("%f", &c, QuoteStyle::Posix).is_err());
    }
}

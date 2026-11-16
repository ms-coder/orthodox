//! Parsing of typed `cd` commands (pure, no I/O): OFM executes a *plain* `cd` itself by
//! navigating the panel instead of the shell (ADR-0003); anything it cannot resolve exactly is
//! left to the shell, whose OSC 7 reply then moves the panel.

use std::path::{Component, Path, PathBuf};

/// What a command line's first word and operand say about a `cd` (see [`parse_cd`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CdTarget {
    /// Not a plain `cd` (another command, `cd` with options/variables, or more than one operand):
    /// the shell must run it; OSC 7 will move the panel afterwards.
    NotCd,
    /// `cd` or `cd ~`.
    Home,
    /// `cd -`: the previous directory, which only the shell knows (`$OLDPWD`).
    Previous,
    /// `cd ~/x`: `x` is the path relative to the home directory.
    HomeRelative(PathBuf),
    /// `cd x`: `x` relative to the panel's directory.
    Relative(PathBuf),
    /// `cd /x`.
    Absolute(PathBuf),
}

/// Classify `line` (the command-line text) as a `cd` command. Recognises `cd`, `cd -`, `cd ~`,
/// `cd ~/x`, `cd path` (absolute or relative, unquoted, single- or double-quoted with backslash
/// escapes) and an optional `--` separator; ignores surrounding whitespace. Anything else —
/// options like `-P`, variables, globs, extra operands, other commands — is [`CdTarget::NotCd`]:
/// better one command too many in the shell than a wrong panel navigation.
pub fn parse_cd(line: &str) -> CdTarget {
    let Some(tokens) = tokenize(line) else {
        return CdTarget::NotCd;
    };
    let mut it = tokens.into_iter();
    if it.next().as_deref() != Some("cd") {
        return CdTarget::NotCd;
    }
    let mut operand = it.next();
    if operand.as_deref() == Some("--") {
        operand = it.next();
    }
    let Some(op) = operand else {
        return CdTarget::Home;
    };
    if it.next().is_some() {
        return CdTarget::NotCd; // two operands: not a plain cd
    }
    match op.as_str() {
        _ if op.contains(['$', '*', '?', '`', '<', '>', ';', '&', '|']) => CdTarget::NotCd,
        "-" => CdTarget::Previous,
        "~" => CdTarget::Home,
        _ if op.starts_with("~/") => CdTarget::HomeRelative(PathBuf::from(&op[2..])),
        _ if op.starts_with('/') => CdTarget::Absolute(PathBuf::from(op)),
        _ if op.starts_with('-') => CdTarget::NotCd, // cd option (e.g. -P): let the shell do it
        _ => CdTarget::Relative(PathBuf::from(op)),
    }
}

/// Resolve [`parse_cd`] against `cwd` (the active panel's directory) and `home`, with lexical
/// cleanup of `.`/`..` (no file system access, no symlink resolution — the shell's OSC 7 reply
/// is authoritative). `None` for [`CdTarget::NotCd`] and [`CdTarget::Previous`] (only the shell
/// knows `$OLDPWD`): those are sent to the shell instead.
pub fn parse_cd_target(line: &str, cwd: &Path, home: &Path) -> Option<PathBuf> {
    match parse_cd(line) {
        CdTarget::NotCd | CdTarget::Previous => None,
        CdTarget::Home => Some(home.to_path_buf()),
        CdTarget::HomeRelative(p) => Some(normalize(&home.join(p))),
        CdTarget::Relative(p) => Some(normalize(&cwd.join(p))),
        CdTarget::Absolute(p) => Some(normalize(&p)),
    }
}

/// Lexical path cleanup: drop `.` components, apply `..` by popping (a leading `..` on a
/// relative path stays; an absolute path is clamped at `/`), keep the result non-empty.
pub(crate) fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    let mut absolute = false;
    for comp in path.components() {
        match comp {
            Component::RootDir => {
                absolute = true;
                out.push("/");
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() && !absolute {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() && absolute {
        out.push("/");
    }
    out
}

/// Split `line` into shell words (single quotes literal, double quotes with `\" \\ \$ \``
/// escapes, unquoted with any-backslash escape). `None` when the line is empty or a quote is
/// unterminated (unparseable => [`CdTarget::NotCd`]).
fn tokenize(line: &str) -> Option<Vec<String>> {
    let bytes = line.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }
        let mut tok = String::new();
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
            match bytes[i] {
                b'\'' => {
                    let start = i + 1;
                    let end = bytes[start..]
                        .iter()
                        .position(|&b| b == b'\'')
                        .map(|p| start + p)?;
                    tok.push_str(&line[start..end]);
                    i = end + 1;
                }
                b'"' => {
                    i += 1;
                    while i < bytes.len() && bytes[i] != b'"' {
                        if bytes[i] == b'\\'
                            && matches!(bytes.get(i + 1), Some(b'"' | b'\\' | b'$' | b'`'))
                        {
                            i += 1; // keep the escaped char, drop the backslash
                        }
                        tok.push(line[i..].chars().next()?);
                        i += line[i..].chars().next()?.len_utf8();
                    }
                    if i >= bytes.len() {
                        return None; // unterminated "
                    }
                    i += 1;
                }
                b'\\' if bytes.get(i + 1).is_some() => {
                    i += 1;
                    tok.push(line[i..].chars().next()?);
                    i += line[i..].chars().next()?.len_utf8();
                }
                _ => {
                    tok.push(line[i..].chars().next()?);
                    i += line[i..].chars().next()?.len_utf8();
                }
            }
        }
        tokens.push(tok);
    }
    (!tokens.is_empty()).then_some(tokens)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn target(line: &str) -> Option<PathBuf> {
        parse_cd_target(line, Path::new("/data/work"), Path::new("/home/u"))
    }

    #[test]
    fn plain_cd_forms() {
        assert_eq!(parse_cd(""), CdTarget::NotCd);
        assert_eq!(parse_cd("ls -l"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd"), CdTarget::Home);
        assert_eq!(parse_cd("  cd  "), CdTarget::Home);
        assert_eq!(parse_cd("cd ~"), CdTarget::Home);
        assert_eq!(parse_cd("cd -"), CdTarget::Previous);
        assert_eq!(parse_cd("cd --"), CdTarget::Home);
        assert_eq!(
            parse_cd("cd ~/src"),
            CdTarget::HomeRelative(PathBuf::from("src"))
        );
        assert_eq!(
            parse_cd("cd /tmp"),
            CdTarget::Absolute(PathBuf::from("/tmp"))
        );
        assert_eq!(parse_cd("cd x/y"), CdTarget::Relative(PathBuf::from("x/y")));
        // Options, variables, globs and extra operands go to the shell.
        assert_eq!(parse_cd("cd -P /tmp"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd $HOME"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd ~/a$b"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd a b"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd x; rm -rf /"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd x && make"), CdTarget::NotCd);
        assert_eq!(parse_cd("cdx"), CdTarget::NotCd);
        assert_eq!(parse_cd("echo cd"), CdTarget::NotCd);
        assert_eq!(parse_cd("cd 'unterminated"), CdTarget::NotCd);
    }

    #[test]
    fn quoted_operands() {
        assert_eq!(
            parse_cd("cd 'my dir'"),
            CdTarget::Relative(PathBuf::from("my dir"))
        );
        assert_eq!(
            parse_cd("cd \"it's x\""),
            CdTarget::Relative(PathBuf::from("it's x"))
        );
        assert_eq!(
            parse_cd(r"cd /tmp/my\ dir"),
            CdTarget::Absolute(PathBuf::from("/tmp/my dir"))
        );
        assert_eq!(
            parse_cd("cd -- 'x y'"),
            CdTarget::Relative(PathBuf::from("x y"))
        );
    }

    #[test]
    fn resolves_against_cwd_and_home_with_lexical_cleanup() {
        assert_eq!(target("cd"), Some(PathBuf::from("/home/u")));
        assert_eq!(target("cd ~"), Some(PathBuf::from("/home/u")));
        assert_eq!(target("cd -"), None);
        assert_eq!(target("cd ~/a"), Some(PathBuf::from("/home/u/a")));
        assert_eq!(target("cd ../lib"), Some(PathBuf::from("/data/lib")));
        assert_eq!(target("cd ./x/../y"), Some(PathBuf::from("/data/work/y")));
        assert_eq!(target("cd /a/../.."), Some(PathBuf::from("/")));
        assert_eq!(target("cd x"), Some(PathBuf::from("/data/work/x")));
        assert_eq!(target("ls"), None);
    }

    #[test]
    fn normalize_keeps_leading_parentdots_on_relative_paths() {
        assert_eq!(normalize(Path::new("../a")), PathBuf::from("../a"));
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));
        assert_eq!(normalize(Path::new("./a/./b")), PathBuf::from("a/b"));
        assert_eq!(normalize(Path::new("/..")), PathBuf::from("/"));
    }
}

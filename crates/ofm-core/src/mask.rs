//! File masks (owned by T1.1).
//!
//! Syntax: patterns separated by `;`: `*.txt;*.md`, brace groups `*.{jpg,png}`,
//! `?` and `*` wildcards, `[abc]` classes, leading `!` on a pattern = exclude (`*;!*.tmp`).
//! A name matches if any include pattern matches and no exclude pattern matches (an empty include
//! list means "everything"). Matching is on the file name bytes, never on a lossy `String`.
//!
//! Semantics details (v1.0): segments are trimmed of ASCII whitespace; `?` matches one character
//! (one byte for non-UTF-8 names); `[...]` classes are byte-oriented with `a-z` ranges and
//! `[!...]` negation; case-insensitive folding is ASCII-only. There is no escape syntax, so a
//! literal `*`, `?`, `[`, `{` or `;` cannot be matched.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStringExt;

/// Maximum number of patterns a single brace group expansion may produce.
const MAX_EXPANSIONS: usize = 64;

/// Mask parse failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MaskError {
    /// Malformed pattern (unbalanced brace/bracket, empty).
    #[error("invalid mask {mask:?}: {reason}")]
    Invalid {
        /// The mask text.
        mask: String,
        /// Reason.
        reason: String,
    },
}

fn invalid(mask: &str, reason: impl Into<String>) -> MaskError {
    MaskError::Invalid {
        mask: mask.to_owned(),
        reason: reason.into(),
    }
}

/// Options for [`Mask::parse`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MaskOpts {
    /// Match case-insensitively.
    pub case_insensitive: bool,
}

/// A compiled selection/filter mask.
#[derive(Debug, Clone)]
pub struct Mask {
    source: String,
    case_insensitive: bool,
    /// Expanded include patterns (empty = match everything).
    includes: Vec<Vec<u8>>,
    /// Expanded exclude patterns.
    excludes: Vec<Vec<u8>>,
}

impl Mask {
    /// Compile a mask expression.
    pub fn parse(mask: &str, opts: MaskOpts) -> Result<Mask, MaskError> {
        let mut includes: Vec<Vec<u8>> = Vec::new();
        let mut excludes: Vec<Vec<u8>> = Vec::new();
        for segment in mask.split(';') {
            let segment = segment.trim_ascii();
            if segment.is_empty() {
                continue;
            }
            let (exclude, pattern) = match segment.strip_prefix('!') {
                Some(rest) => (true, rest),
                None => (false, segment),
            };
            if pattern.is_empty() {
                return Err(invalid(mask, "empty pattern after '!'"));
            }
            let mut expanded: Vec<Vec<u8>> = Vec::new();
            let mut budget = MAX_EXPANSIONS;
            expand_braces(pattern.as_bytes(), &mut expanded, &mut budget)
                .map_err(|reason| invalid(mask, reason))?;
            for pat in expanded {
                validate_classes(&pat).map_err(|reason| invalid(mask, reason))?;
                if exclude {
                    excludes.push(pat);
                } else {
                    includes.push(pat);
                }
            }
        }
        Ok(Mask {
            source: mask.to_owned(),
            case_insensitive: opts.case_insensitive,
            includes,
            excludes,
        })
    }

    /// True if `name` matches.
    pub fn is_match(&self, name: &OsStr) -> bool {
        let name = name.as_encoded_bytes();
        let included = self.includes.is_empty()
            || self
                .includes
                .iter()
                .any(|p| glob_match(p, name, self.case_insensitive));
        included
            && !self
                .excludes
                .iter()
                .any(|p| glob_match(p, name, self.case_insensitive))
    }

    /// The original text.
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Expand the first brace group in `pat` into `out` (recursively; plain patterns are copied).
/// Returns `Err(reason)` on unbalanced braces, empty groups or expansion overflow.
fn expand_braces(pat: &[u8], out: &mut Vec<Vec<u8>>, budget: &mut usize) -> Result<(), String> {
    // Reject `}` without a matching `{` anywhere (braces have no literal meaning).
    let mut depth: i32 = 0;
    for &c in pat {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth < 0 {
                    return Err("unbalanced '}'".to_owned());
                }
            }
            _ => {}
        }
    }
    let Some(open) = pat.iter().position(|&c| c == b'{') else {
        out.push(pat.to_vec());
        return Ok(());
    };
    let Some(close) = matching_close(pat, open) else {
        return Err("unbalanced '{'".to_owned());
    };
    let (prefix, body, suffix) = (&pat[..open], &pat[open + 1..close], &pat[close + 1..]);
    let mut combined: Vec<u8> = Vec::new();
    for alt in split_alternatives(body) {
        if alt.is_empty() {
            return Err("empty '{}' alternative".to_owned());
        }
        if *budget == 0 {
            return Err(format!("more than {MAX_EXPANSIONS} brace combinations"));
        }
        *budget -= 1;
        combined.clear();
        combined.extend_from_slice(prefix);
        combined.extend_from_slice(alt);
        combined.extend_from_slice(suffix);
        expand_braces(&combined, out, budget)?;
    }
    Ok(())
}

/// Index of the `}` matching the `{` at `open`, or `None`.
fn matching_close(pat: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, &c) in pat.iter().enumerate().skip(open) {
        match c {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Split a brace body on top-level `,` (commas inside nested groups stay).
fn split_alternatives(body: &[u8]) -> Vec<&[u8]> {
    let mut alts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, &c) in body.iter().enumerate() {
        match c {
            b'{' => depth += 1,
            b'}' => depth -= 1,
            b',' if depth == 0 => {
                alts.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    alts.push(&body[start..]);
    alts
}

/// Reject `[` without a closing `]` (an empty class `[]` also never closes, by fnmatch rules).
fn validate_classes(pat: &[u8]) -> Result<(), String> {
    let mut i = 0usize;
    while i < pat.len() {
        match pat[i] {
            b'[' => match class_end(pat, i) {
                Some(end) => i = end,
                None => return Err("unbalanced '['".to_owned()),
            },
            _ => i += 1,
        }
    }
    Ok(())
}

/// One past the `]` that closes the class starting at `start`, or `None` if unterminated.
/// A `]` as the first (or second, after `!`/`^`) member is a literal member.
fn class_end(pat: &[u8], start: usize) -> Option<usize> {
    let mut i = start + 1;
    if matches!(pat.get(i), Some(b'!') | Some(b'^')) {
        i += 1;
    }
    // A `]` here is a literal member.
    if pat.get(i) == Some(&b']') {
        i += 1;
    }
    while i < pat.len() {
        if pat[i] == b']' {
            return Some(i + 1);
        }
        i += 1;
    }
    None
}

/// Does the byte at `text[t]` match the class starting at `pat[p]` (`p` points at `[`)?
/// Returns `(index_after_class, matched)`, or `None` if the class is unterminated.
fn class_match(pat: &[u8], p: usize, byte: u8, ci: bool) -> Option<(usize, bool)> {
    let end = class_end(pat, p)?;
    let close = end - 1; // index of the closing `]`
    let mut i = p + 1;
    let negate = matches!(pat.get(i), Some(b'!') | Some(b'^'));
    if negate {
        i += 1;
    }
    let mut member = false;
    // A `]` right after `[` (or `[!`) is a literal member, not the class end.
    if pat.get(i) == Some(&b']') {
        if byte_eq(byte, b']', ci) {
            member = true;
        }
        i += 1;
    }
    while i < close {
        let lo = pat[i];
        if pat.get(i + 1) == Some(&b'-') && i + 2 < close {
            let hi = pat[i + 2];
            if in_range(byte, lo, hi, ci) {
                member = true;
            }
            i += 3;
        } else {
            if byte_eq(byte, lo, ci) {
                member = true;
            }
            i += 1;
        }
    }
    Some((end, member != negate))
}

fn byte_eq(a: u8, b: u8, ci: bool) -> bool {
    if ci {
        a.eq_ignore_ascii_case(&b)
    } else {
        a == b
    }
}

fn in_range(byte: u8, lo: u8, hi: u8, ci: bool) -> bool {
    if ci {
        // Check the range against both cases of the byte (range endpoints are not canonicalized).
        let b = byte.to_ascii_lowercase();
        let b_up = byte.to_ascii_uppercase();
        return (lo <= b && b <= hi) || (lo <= b_up && b_up <= hi);
    }
    lo <= byte && byte <= hi
}

/// Length of the UTF-8 character starting at `t` (1 for non-UTF-8 bytes).
fn char_len(text: &[u8], t: usize) -> usize {
    let mut n = 1;
    while t + n < text.len() && text[t + n] & 0b1100_0000 == 0b1000_0000 {
        n += 1;
    }
    n
}

/// Wildcard match with `*` (any run), `?` (one character) and `[...]` classes.
/// Iterative with single-star backtracking (the classic O(n·m) algorithm).
fn glob_match(pat: &[u8], text: &[u8], ci: bool) -> bool {
    let mut p = 0usize;
    let mut t = 0usize;
    let mut star: Option<usize> = None; // position of the last '*' in `pat`
    let mut star_t = 0usize; // text position when that '*' was reached
    while t < text.len() {
        if p < pat.len() {
            match pat[p] {
                b'*' => {
                    star = Some(p);
                    star_t = t;
                    p += 1;
                    continue;
                }
                b'?' => {
                    p += 1;
                    t += char_len(text, t);
                    continue;
                }
                b'[' => match class_match(pat, p, text[t], ci) {
                    Some((end, member)) => {
                        if member {
                            p = end;
                            t += char_len(text, t);
                            continue;
                        }
                    }
                    None => {
                        // Unterminated class: '[' as a literal byte.
                        if byte_eq(pat[p], text[t], ci) {
                            p += 1;
                            t += 1;
                            continue;
                        }
                    }
                },
                c => {
                    if byte_eq(c, text[t], ci) {
                        p += 1;
                        t += 1;
                        continue;
                    }
                }
            }
        }
        // Mismatch: backtrack into the last `*`, letting it swallow one more character.
        if let Some(s) = star {
            p = s + 1;
            star_t += char_len(text, star_t);
            t = star_t;
        } else {
            return false;
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// Rename mask for copy/move dialogs: `*.txt` -> `*.bak`, i.e. source pattern + target pattern.
/// Reserved `[N]`/`[C]` placeholders are v1.1.
#[derive(Debug, Clone)]
pub struct RenameMask {
    target: String,
    /// Compiled target pattern bytes.
    pat: Vec<u8>,
    /// `*` yields the source stem (name without extension) when the target contains a dot,
    /// otherwise the whole source name.
    star_is_stem: bool,
}

impl RenameMask {
    /// Parse the destination-name pattern (e.g. `*.bak`, `backup_*`, `*`).
    /// Only literals, `*` and `?` are allowed (no braces or classes).
    pub fn parse(target: &str) -> Result<RenameMask, MaskError> {
        if target.is_empty() {
            return Err(invalid(target, "empty rename mask"));
        }
        if target
            .bytes()
            .any(|c| matches!(c, b'{' | b'}' | b'[' | b']'))
        {
            return Err(invalid(
                target,
                "braces and classes are not supported in rename masks",
            ));
        }
        Ok(RenameMask {
            target: target.to_owned(),
            pat: target.as_bytes().to_vec(),
            star_is_stem: target.contains('.'),
        })
    }

    /// Apply to a source name: each `*` is replaced by the source stem (or whole name without a dot
    /// in the pattern), so `a.txt` + `*.bak` = `a.bak`; `?` copies the char at the same position.
    pub fn apply(&self, source_name: &OsStr) -> OsString {
        let s = source_name.as_encoded_bytes();
        let stem_len = if self.star_is_stem {
            // Name up to the last dot; a leading dot (`.bashrc`) means "no extension".
            match s.iter().rposition(|&c| c == b'.') {
                Some(p) if p > 0 => p,
                _ => s.len(),
            }
        } else {
            s.len()
        };
        let mut out: Vec<u8> = Vec::with_capacity(self.pat.len() + stem_len);
        for (k, &c) in self.pat.iter().enumerate() {
            match c {
                b'*' => out.extend_from_slice(&s[..stem_len]),
                b'?' => {
                    // The source byte at the same position as the `?` in the pattern, if any.
                    if k < s.len() {
                        out.push(s[k]);
                    }
                }
                _ => out.push(c),
            }
        }
        OsString::from_vec(out)
    }

    /// The original target text.
    pub fn target(&self) -> &str {
        &self.target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unwrap_mask(parsed: Result<Mask, MaskError>) -> Mask {
        match parsed {
            Ok(mask) => mask,
            Err(e) => panic!("mask should parse: {e}"),
        }
    }
    fn m(mask: &str) -> Mask {
        unwrap_mask(Mask::parse(mask, MaskOpts::default()))
    }
    fn mci(mask: &str) -> Mask {
        unwrap_mask(Mask::parse(
            mask,
            MaskOpts {
                case_insensitive: true,
            },
        ))
    }
    fn matches(mask: &str, name: &str) -> bool {
        m(mask).is_match(OsStr::new(name))
    }

    #[test]
    fn mask_basics() {
        assert!(matches("*", "anything"));
        assert!(matches("*.txt", "a.txt"));
        assert!(!matches("*.txt", "a.txt.bak"));
        assert!(matches("*.txt;*.md", "x.md"));
        assert!(!matches("*.txt", "x.md"));
        assert!(matches("a?c", "abc"));
        assert!(!matches("a?c", "ac"));
        assert!(!matches("a?c", "abbc")); // `?` is exactly one character
        assert!(matches("[abc]*", "b.txt"));
        assert!(!matches("[abc]*", "d.txt"));
        assert!(matches("[a-c]*", "b1"));
        assert!(!matches("[a-c]*", "d1"));
        assert!(matches("[!a-c]*", "d1"));
        assert!(!matches("[!a-c]*", "b1"));
        // Empty include list = everything.
        assert!(matches("!*.tmp", "keep.txt"));
        assert!(!matches("!*.tmp", "x.tmp"));
        assert!(!matches("*;!*.tmp", "x.tmp"));
        assert!(matches("*;!*.tmp", "x.txt"));
        // Empty mask matches everything.
        assert!(matches("", "whatever"));
        // Trailing separators and spaces are tolerated.
        assert!(matches("*.txt;", "a.txt"));
        assert!(matches(" *.txt ; *.md ", "b.md"));
        // `?` matches exactly one UTF-8 character (two bytes here), `*` the rest.
        assert!(matches("f??ü.txt", "füüü.txt"));
        assert!(!matches("f?ü.txt", "füüü.txt")); // first `?` only covers one character
        assert!(!matches("f?le*", "füüü.txt"));
    }

    #[test]
    fn mask_braces() {
        let mask = mci("*.{jpg,png}");
        assert!(mask.is_match(OsStr::new("photo.JPG")));
        assert!(mask.is_match(OsStr::new("photo.png")));
        assert!(!mask.is_match(OsStr::new("photo.gif")));
        // Nested groups and combinations.
        let mask = m("{a,b}{1,2}");
        assert!(mask.is_match(OsStr::new("a1")));
        assert!(mask.is_match(OsStr::new("b2")));
        assert!(!mask.is_match(OsStr::new("a3")));
        // Exclude via braces.
        let mask = m("*;!{*.tmp,*.log}");
        assert!(mask.is_match(OsStr::new("x.txt")));
        assert!(!mask.is_match(OsStr::new("x.log")));
    }

    #[test]
    fn mask_case() {
        assert!(!m("*.txt").is_match(OsStr::new("A.TXT")));
        assert!(mci("*.txt").is_match(OsStr::new("A.TXT")));
        assert!(mci("[A-Z]*").is_match(OsStr::new("g.txt")));
        assert!(!m("[A-Z]*").is_match(OsStr::new("g.txt")));
    }

    #[test]
    fn mask_parse_errors() {
        for bad in ["{", "}", "a{b", "a}b", "{}", "{a,}", "[", "a[b", "!"] {
            assert!(Mask::parse(bad, MaskOpts::default()).is_err(), "{bad:?}");
        }
        // `[]` is an unterminated class (the `]` is a literal member), `[]]` is the `]` class.
        assert!(Mask::parse("[]", MaskOpts::default()).is_err());
        assert!(m("[]]*").is_match(OsStr::new("]x")));
        // Expansion overflow.
        let big = "{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}{a,b}";
        assert!(Mask::parse(big, MaskOpts::default()).is_err()); // 128 > 64
    }

    #[test]
    fn mask_non_utf8_names() {
        use std::os::unix::ffi::OsStrExt;
        let raw = OsStr::from_bytes(b"caf\xe9.txt"); // invalid UTF-8
        assert!(m("*.txt").is_match(raw));
        assert!(m("caf?*").is_match(raw));
        assert!(!m("café*").is_match(raw));
    }

    #[test]
    fn rename_mask() {
        let r = |t: &str| match RenameMask::parse(t) {
            Ok(m) => m,
            Err(e) => panic!("rename mask should parse: {e}"),
        };
        assert_eq!(
            r("*.bak").apply(OsStr::new("a.txt")),
            OsString::from("a.bak")
        );
        assert_eq!(
            r("*.bak").apply(OsStr::new("a.b.txt")),
            OsString::from("a.b.bak")
        );
        assert_eq!(
            r("*.bak").apply(OsStr::new(".bashrc")),
            OsString::from(".bashrc.bak")
        );
        // No dot in the pattern: `*` = whole name.
        assert_eq!(
            r("backup_*").apply(OsStr::new("a.txt")),
            OsString::from("backup_a.txt")
        );
        assert_eq!(r("*").apply(OsStr::new("a.txt")), OsString::from("a.txt"));
        assert_eq!(r("*").apply(OsStr::new("noext")), OsString::from("noext"));
        // `?` copies the source char at the same position.
        assert_eq!(r("?x").apply(OsStr::new("abcd")), OsString::from("ax"));
        assert_eq!(r("??").apply(OsStr::new("a")), OsString::from("a"));
        // Multiple `*` (no dot in the pattern: each `*` = whole name).
        assert_eq!(
            r("*_*").apply(OsStr::new("a.txt")),
            OsString::from("a.txt_a.txt")
        );
        assert!(RenameMask::parse("").is_err());
        assert!(RenameMask::parse("{a}").is_err());
        assert!(RenameMask::parse("[a]").is_err());
    }
}

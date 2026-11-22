//! File clipboard formats (F31), pure and unit-tested: `file://` URIs of byte paths, the
//! `text/uri-list` body and GNOME Files' `x-special/gnome-copied-files` (a first line `copy` or
//! `cut`, then one URI per line). OFM publishes both and understands both, so copy/cut/paste
//! interoperates with GNOME Files in both directions.
//!
//! Paths are bytes (CLAUDE.md invariant 4): URIs are built and parsed byte-wise, so a non-UTF-8
//! name survives a clipboard round trip unchanged.

use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

/// MIME type GNOME Files (and Nautilus-derived managers) use for copy/cut.
pub const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";

/// What a paste does with the files.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipOp {
    /// Ctrl+C: paste copies.
    Copy,
    /// Ctrl+X: paste moves.
    Cut,
}

impl ClipOp {
    fn word(self) -> &'static str {
        match self {
            ClipOp::Copy => "copy",
            ClipOp::Cut => "cut",
        }
    }
}

/// Files on the clipboard with the intended operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipFiles {
    /// Copy or cut.
    pub op: ClipOp,
    /// Absolute local paths.
    pub paths: Vec<PathBuf>,
}

/// Bytes kept literally in a path URI (RFC 3986 unreserved plus the sub-delimiters and `:@/`
/// that GLib's `g_filename_to_uri` also leaves alone).
fn is_literal(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"-._~!$&'()*+,;=:@/".contains(&b)
}

/// `file:///path` for an absolute local path (non-literal bytes are percent-encoded).
pub fn path_to_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if is_literal(b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Percent-decode to bytes; a malformed escape (`%` not followed by two hex digits) is kept
/// literally, like most URI parsers do.
fn unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%'
            && let (Some(h), Some(l)) = (
                s.get(i + 1).copied().and_then(hex),
                s.get(i + 2).copied().and_then(hex),
            )
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(s[i]);
        i += 1;
    }
    out
}

/// The local path a `file:` URI names. `None` for other schemes, remote hosts, relative paths
/// and fragments/queries-only garbage. The host must be empty or `localhost`.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.trim().strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let (host, path) = rest.split_at(slash);
    if !host.is_empty() && !host.eq_ignore_ascii_case("localhost") {
        return None;
    }
    // A `?query` or `#fragment` is not part of a file name in `file:` URIs from file managers
    // (a literal `#` or `?` is always escaped as %23 / %3F by them).
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let bytes = unescape(path.as_bytes());
    if bytes.contains(&0) {
        return None;
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

/// Body of `x-special/gnome-copied-files`: `copy\nfile:///a\nfile:///b` (no trailing newline,
/// exactly what GNOME Files writes).
pub fn encode_gnome(op: ClipOp, paths: &[PathBuf]) -> Vec<u8> {
    let mut s = String::from(op.word());
    for p in paths {
        s.push('\n');
        s.push_str(&path_to_uri(p));
    }
    s.into_bytes()
}

/// Parse `x-special/gnome-copied-files`. A first line that is neither `copy` nor `cut` is taken as
/// the first URI (some producers omit the verb: copy). Lines that are not local `file:` URIs
/// are dropped; `None` when no path remains.
pub fn decode_gnome(data: &[u8]) -> Option<ClipFiles> {
    let text = String::from_utf8_lossy(data);
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    let first = lines.next()?;
    let (op, mut uris): (ClipOp, Vec<&str>) = match first {
        "copy" => (ClipOp::Copy, Vec::new()),
        "cut" => (ClipOp::Cut, Vec::new()),
        other => (ClipOp::Copy, vec![other]),
    };
    uris.extend(lines);
    let paths: Vec<PathBuf> = uris.into_iter().filter_map(uri_to_path).collect();
    (!paths.is_empty()).then_some(ClipFiles { op, paths })
}

/// Body of `text/uri-list` (RFC 2483: CRLF line ends).
pub fn encode_uri_list(paths: &[PathBuf]) -> Vec<u8> {
    let mut s = String::new();
    for p in paths {
        s.push_str(&path_to_uri(p));
        s.push_str("\r\n");
    }
    s.into_bytes()
}

/// Parse `text/uri-list`: one URI per line, `#` lines are comments, non-local entries are skipped.
pub fn decode_uri_list(data: &[u8]) -> Vec<PathBuf> {
    String::from_utf8_lossy(data)
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(uri_to_path)
        .collect()
}

/// Text for "copy path" (`file.copy_path`): one path per line, as bytes-lossy text.
pub fn paths_as_text(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Drop duplicate paths, keeping the first occurrence and the original order. A clipboard
/// payload that lists the same URI twice must not make the paste job's target-name planning
/// reject the whole job as a source/target overlap.
pub fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = std::collections::HashSet::with_capacity(paths.len());
    paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    fn raw(b: &[u8]) -> PathBuf {
        PathBuf::from(OsString::from_vec(b.to_vec()))
    }

    #[test]
    fn uri_encodes_specials_and_keeps_slashes() {
        assert_eq!(path_to_uri(&p("/a/b c.txt")), "file:///a/b%20c.txt");
        assert_eq!(path_to_uri(&p("/x/50%#?.txt")), "file:///x/50%25%23%3F.txt");
        assert_eq!(path_to_uri(&p("/ü")), "file:///%C3%BC");
        assert_eq!(path_to_uri(&p("/it's (1).txt")), "file:///it's%20(1).txt");
    }

    #[test]
    fn uri_round_trips_non_utf8_bytes() {
        let path = raw(b"/tmp/\xff\xfe odd\nname%20");
        let uri = path_to_uri(&path);
        assert!(uri.is_ascii());
        assert!(!uri.contains('\n'));
        assert_eq!(uri_to_path(&uri), Some(path));
    }

    #[test]
    fn uri_parse_rules() {
        assert_eq!(uri_to_path("file:///a%20b"), Some(p("/a b")));
        assert_eq!(uri_to_path("file://localhost/a"), Some(p("/a")));
        assert_eq!(uri_to_path("FILE://x/a"), None); // scheme is case-sensitive here, host is remote
        assert_eq!(uri_to_path("file://otherhost/a"), None);
        assert_eq!(uri_to_path("http://x/a"), None);
        assert_eq!(uri_to_path("file:relative"), None);
        assert_eq!(uri_to_path("file:///a%00b"), None);
        assert_eq!(uri_to_path("file:///a#frag"), Some(p("/a")));
        // Malformed escapes stay literal.
        assert_eq!(uri_to_path("file:///100%"), Some(p("/100%")));
        assert_eq!(uri_to_path("file:///a%zz"), Some(p("/a%zz")));
        assert_eq!(uri_to_path("  file:///a  "), Some(p("/a")));
    }

    #[test]
    fn gnome_format_matches_gnome_files() {
        let paths = vec![p("/home/u/a b"), p("/home/u/c")];
        assert_eq!(
            encode_gnome(ClipOp::Copy, &paths),
            b"copy\nfile:///home/u/a%20b\nfile:///home/u/c".to_vec()
        );
        assert_eq!(
            encode_gnome(ClipOp::Cut, &paths[..1]),
            b"cut\nfile:///home/u/a%20b".to_vec()
        );
    }

    #[test]
    fn gnome_round_trip_with_odd_names() {
        let paths = vec![raw(b"/d/\xe9\xff"), p("/d/sp ace"), p("/d/ünï")];
        for op in [ClipOp::Copy, ClipOp::Cut] {
            let got = decode_gnome(&encode_gnome(op, &paths)).unwrap();
            assert_eq!(got.op, op);
            assert_eq!(got.paths, paths);
        }
    }

    #[test]
    fn gnome_decode_tolerates_producers() {
        // Trailing newline / CRLF (other implementations).
        let got = decode_gnome(b"cut\r\nfile:///a\r\nfile:///b\r\n").unwrap();
        assert_eq!(got.op, ClipOp::Cut);
        assert_eq!(got.paths, vec![p("/a"), p("/b")]);
        // Missing verb: the first line is already a URI, copy.
        let got = decode_gnome(b"file:///a\nfile:///b").unwrap();
        assert_eq!(got.op, ClipOp::Copy);
        assert_eq!(got.paths.len(), 2);
        // Remote and garbage entries are dropped; nothing left = None.
        let got = decode_gnome(b"copy\nsmb://h/x\nfile:///ok\nnonsense").unwrap();
        assert_eq!(got.paths, vec![p("/ok")]);
        assert_eq!(decode_gnome(b"copy\nsmb://h/x"), None);
        assert_eq!(decode_gnome(b""), None);
        assert_eq!(decode_gnome(b"cut"), None);
    }

    #[test]
    fn uri_list_round_trip_and_comments() {
        let paths = vec![raw(b"/x/\xff"), p("/y z")];
        assert_eq!(decode_uri_list(&encode_uri_list(&paths)), paths);
        let list = b"# comment\r\nfile:///a\r\n\r\nhttp://nope/\r\nfile:///b%20c\n";
        assert_eq!(decode_uri_list(list), vec![p("/a"), p("/b c")]);
    }

    #[test]
    fn copy_path_text_is_one_path_per_line() {
        assert_eq!(paths_as_text(&[p("/a"), p("/b c")]), "/a\n/b c");
        assert_eq!(paths_as_text(&[]), "");
    }

    #[test]
    fn dedupe_keeps_first_occurrence_and_order() {
        let d = vec![p("/a"), p("/b"), p("/a"), p("/c"), p("/b"), p("/a")];
        assert_eq!(dedupe_paths(d), vec![p("/a"), p("/b"), p("/c")]);
        // Non-UTF-8 duplicates dedupe by exact bytes.
        let d = vec![raw(b"/x/\xff"), raw(b"/x/\xfe"), raw(b"/x/\xff")];
        assert_eq!(dedupe_paths(d), vec![raw(b"/x/\xff"), raw(b"/x/\xfe")]);
        assert!(dedupe_paths(Vec::new()).is_empty());
    }
}

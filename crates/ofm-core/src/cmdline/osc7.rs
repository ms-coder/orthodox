//! OSC 7 working-directory URIs: `file://host/percent-encoded/path` (the protocol GNOME
//! terminals use; emitted by the rc snippets injected by the terminal module, ADR-0003).

use std::path::PathBuf;

use super::{bytes_of, os_from_bytes};

/// Whether `b` is an RFC 3986 "unreserved" character (kept literal in a URI).
fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

/// The URI for `path` on `host`: `file://HOST/percent-encoded-path`. Every byte of the path that
/// is not unreserved or `/` becomes `%XX` (uppercase hex), so any byte — including ESC/BEL, which
/// would break the OSC sequence itself, and non-UTF-8 bytes — survives; the host keeps its
/// unreserved bytes and percent-encodes the rest. A leading `/` of `path` is not doubled.
pub fn osc7_encode(host: &str, path: &std::path::Path) -> String {
    let mut out = String::from("file://");
    for &b in host.as_bytes() {
        if unreserved(b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    let path_bytes = bytes_of(path.as_os_str());
    if !path_bytes.starts_with(b"/") {
        out.push('/');
    }
    for &b in path_bytes.iter() {
        if unreserved(b) || b == b'/' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Percent-decode `bytes`: `%XX` (any case) becomes that byte, anything else — including a
/// malformed escape — passes through literally. Never fails.
fn percent_decode(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| bytes.get(i + 1..i + 3))
            .flatten()
            .and_then(|h| {
                let hi = (h[0] as char).to_digit(16);
                let lo = (h[1] as char).to_digit(16);
                match (hi, lo) {
                    (Some(hi), Some(lo)) => Some((hi * 16 + lo) as u8),
                    _ => None,
                }
            });
        match hex {
            Some(b) => {
                out.push(b);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    out
}

/// Parse an OSC 7 payload (`file://host/path`, the part after `OSC 7;`). Returns
/// `Some((host, path))` only for the `file` scheme — anything else (`ssh:`, `http:`, garbage)
/// is rejected. The host is percent-decoded into a lossy `String` (hosts are ASCII in practice);
/// the path is decoded byte-exactly into a [`PathBuf`], so non-UTF-8 paths survive. Everything
/// after the host is path (no query/fragment handling); an empty path means `/`.
pub fn osc7_parse(uri: &[u8]) -> Option<(String, PathBuf)> {
    let rest = uri.strip_prefix(b"file://")?;
    let (host, path) = match rest.iter().position(|&b| b == b'/') {
        Some(slash) => (&rest[..slash], &rest[slash..]),
        None => (rest, &b"/"[..]),
    };
    let host = String::from_utf8_lossy(&percent_decode(host)).into_owned();
    let mut path_bytes = percent_decode(path);
    if path_bytes.is_empty() {
        path_bytes.push(b'/');
    }
    Some((host, PathBuf::from(os_from_bytes(&path_bytes))))
}

/// [`osc7_parse`] for a `&str` URI that discards the host (the old name kept for callers that
/// only want the path, e.g. the VTE `current-directory-uri` signal).
pub fn osc7_to_path(uri: &str) -> Option<PathBuf> {
    osc7_parse(uri.as_bytes()).map(|(_, path)| path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::path::Path;

    use super::*;

    #[test]
    fn encode_unreserved_and_slash_stay_literal() {
        assert_eq!(osc7_encode("box", Path::new("/tmp")), "file://box/tmp");
        assert_eq!(
            osc7_encode("box", Path::new("/a/b-c.d_e~f")),
            "file://box/a/b-c.d_e~f"
        );
    }

    #[test]
    fn encode_escapes_space_quote_and_specials() {
        assert_eq!(
            osc7_encode("box", Path::new("/my dir/x'y")),
            "file://box/my%20dir/x%27y"
        );
        // ESC and BEL would terminate the OSC sequence itself; UTF-8 bytes are percent-encoded.
        assert_eq!(
            osc7_encode("box", Path::new("/a\u{1b}b\u{7f}")),
            "file://box/a%1Bb%7F"
        );
        assert_eq!(
            osc7_encode("box", Path::new("/київ")),
            "file://box/%D0%BA%D0%B8%D1%97%D0%B2"
        );
        // A relative path (caller error) still yields a rooted URI.
        assert_eq!(osc7_encode("box", Path::new("rel")), "file://box/rel");
    }

    #[test]
    fn encode_percent_encodes_host_specials() {
        assert_eq!(
            osc7_encode("odd host", Path::new("/")),
            "file://odd%20host/"
        );
    }

    #[test]
    fn parse_roundtrip_and_rejects() {
        assert_eq!(
            osc7_parse(b"file://box/tmp"),
            Some(("box".to_owned(), PathBuf::from("/tmp")))
        );
        assert_eq!(
            osc7_parse(b"file://box"),
            Some(("box".to_owned(), PathBuf::from("/")))
        );
        assert_eq!(
            osc7_to_path("file://box/a%20b"),
            Some(PathBuf::from("/a b"))
        );
        // Non-file schemes are rejected.
        assert_eq!(osc7_parse(b"ssh://box/tmp"), None);
        assert_eq!(osc7_parse(b"http://box/"), None);
        assert_eq!(osc7_parse(b"file:/no-host"), None);
        // Lowercase hex accepted, malformed escapes stay literal.
        assert_eq!(osc7_to_path("file://h/%2f"), Some(PathBuf::from("/")));
        assert_eq!(osc7_to_path("file://h/%zz"), Some(PathBuf::from("/%zz")));
        // Everything after the host is path: '?' and '#' are not query/fragment separators.
        assert_eq!(
            osc7_to_path("file://h/a?b#c"),
            Some(PathBuf::from("/a?b#c"))
        );
    }

    #[test]
    #[cfg(unix)]
    fn parse_non_utf8_path_bytes() {
        let uri = format!("file://h/{}", "%FF%FE");
        let (_, p) = osc7_parse(uri.as_bytes()).unwrap();
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(p.as_os_str().as_bytes(), b"/\xff\xfe");
    }

    #[test]
    fn encode_parse_roundtrip_simple() {
        for p in ["/", "/tmp", "/a b/c'd", "/\u{1b}x"] {
            let uri = osc7_encode("box", Path::new(p));
            assert_eq!(osc7_to_path(&uri).as_deref(), Some(Path::new(p)));
        }
    }
}

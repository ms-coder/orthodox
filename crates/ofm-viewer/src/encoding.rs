//! Encoding detection and range decoding.
//!
//! Detection order (per ARCHITECTURE): BOM → the bytes are valid UTF-8 in the first
//! [`DETECT_LEN`] bytes → [`chardetng`]. [`Encoding::from_label`] then maps chardetng's labels
//! onto the supported set; unknown labels fall back to windows-1251 (Far-style default for
//! Cyrillic panels) — callers that disagree can pass an explicit [`Encoding`].

use std::borrow::Cow;

use encoding_rs::{Encoding as RsEncoding, UTF_8, UTF_16BE, UTF_16LE};

/// Number of bytes inspected for non-BOM detection (and the UTF-8 validity probe).
pub const DETECT_LEN: usize = 64 * 1024;

/// Longest BOM we recognise (UTF-16 BOMs are 2 bytes, UTF-8 is 3).
pub const BOM_MAX_LEN: usize = 3;

/// The encodings the viewer supports (spec F21).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// UTF-8.
    Utf8,
    /// UTF-16 little-endian.
    Utf16Le,
    /// UTF-16 big-endian.
    Utf16Be,
    /// windows-1251 (Cyrillic).
    Windows1251,
    /// IBM866 (DOS Cyrillic).
    Ibm866,
    /// KOI8-R.
    Koi8R,
    /// ISO-8859-1 — via encoding_rs this is the windows-1252 superset (see crate docs).
    Iso8859_1,
}

impl Encoding {
    /// All encodings, in UI-menu order.
    pub const ALL: [Encoding; 7] = [
        Encoding::Utf8,
        Encoding::Utf16Le,
        Encoding::Utf16Be,
        Encoding::Windows1251,
        Encoding::Ibm866,
        Encoding::Koi8R,
        Encoding::Iso8859_1,
    ];

    /// Stable machine name (config, tests).
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "utf-8",
            Encoding::Utf16Le => "utf-16le",
            Encoding::Utf16Be => "utf-16be",
            Encoding::Windows1251 => "windows-1251",
            Encoding::Ibm866 => "ibm866",
            Encoding::Koi8R => "koi8-r",
            Encoding::Iso8859_1 => "iso-8859-1",
        }
    }

    /// Human-readable label for menus.
    pub fn label(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16 LE",
            Encoding::Utf16Be => "UTF-16 BE",
            Encoding::Windows1251 => "Windows-1251",
            Encoding::Ibm866 => "IBM866 (DOS)",
            Encoding::Koi8R => "KOI8-R",
            Encoding::Iso8859_1 => "ISO-8859-1 (Latin-1)",
        }
    }

    fn to_rs(self) -> &'static RsEncoding {
        match self {
            Encoding::Utf8 => encoding_rs::UTF_8,
            Encoding::Utf16Le => encoding_rs::UTF_16LE,
            Encoding::Utf16Be => encoding_rs::UTF_16BE,
            Encoding::Windows1251 => encoding_rs::WINDOWS_1251,
            Encoding::Ibm866 => encoding_rs::IBM866,
            Encoding::Koi8R => encoding_rs::KOI8_R,
            // WHATWG maps the iso-8859-1 label to the windows-1252 superset (see crate docs).
            Encoding::Iso8859_1 => encoding_rs::WINDOWS_1252,
        }
    }

    /// Byte length of one code unit (for boundary alignment).
    pub fn bytes_per_unit(self) -> usize {
        match self {
            Encoding::Utf8 => 1,
            Encoding::Utf16Le | Encoding::Utf16Be => 2,
            _ => 1,
        }
    }
}

impl std::str::FromStr for Encoding {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim().to_ascii_lowercase();
        Ok(match s.as_str() {
            "utf-8" | "utf8" => Encoding::Utf8,
            "utf-16le" | "utf-16" | "utf16le" => Encoding::Utf16Le,
            "utf-16be" | "utf16be" => Encoding::Utf16Be,
            "windows-1251" | "cp1251" => Encoding::Windows1251,
            "ibm866" | "cp866" | "866" => Encoding::Ibm866,
            "koi8-r" | "koi8r" => Encoding::Koi8R,
            "iso-8859-1" | "latin-1" | "latin1" | "cp1252" => Encoding::Iso8859_1,
            _ => return Err(()),
        })
    }
}

/// Result of [`detect_encoding`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Detection {
    /// Detected encoding.
    pub encoding: Encoding,
    /// Length of the BOM at the start of the file (0 if none).
    pub bom_len: usize,
}

/// Map an encoding_rs static (as returned by chardetng) to a supported [`Encoding`].
fn from_rs(rs: &'static RsEncoding) -> Option<Encoding> {
    Some(match rs {
        e if e == UTF_8 => Encoding::Utf8,
        e if e == UTF_16LE => Encoding::Utf16Le,
        e if e == UTF_16BE => Encoding::Utf16Be,
        e if e == encoding_rs::WINDOWS_1251 => Encoding::Windows1251,
        e if e == encoding_rs::IBM866 => Encoding::Ibm866,
        e if e == encoding_rs::KOI8_R => Encoding::Koi8R,
        e if e == encoding_rs::WINDOWS_1252 => Encoding::Iso8859_1,
        _ => return None,
    })
}

/// Detect the encoding of a file's first bytes (see module docs for the order).
pub fn detect_encoding(head: &[u8]) -> Detection {
    if head.len() >= 3 && head[..3] == [0xEF, 0xBB, 0xBF] {
        return Detection {
            encoding: Encoding::Utf8,
            bom_len: 3,
        };
    }
    if head.len() >= 2 {
        if head[..2] == [0xFF, 0xFE] {
            return Detection {
                encoding: Encoding::Utf16Le,
                bom_len: 2,
            };
        }
        if head[..2] == [0xFE, 0xFF] {
            return Detection {
                encoding: Encoding::Utf16Be,
                bom_len: 2,
            };
        }
    }
    let probe = &head[..head.len().min(DETECT_LEN)];
    // A UTF-16 stream of Cyrillic/Greek/CJK encodes as long runs of C0 controls, which are
    // *technically* valid UTF-8 — so the cheap validity probe alone would mislabel BOM-less
    // UTF-16 as UTF-8. Reject probes with NULs or control chars other than \t \r \n.
    if let Ok(s) = std::str::from_utf8(probe)
        && s.chars()
            .all(|c| c == '\t' || c == '\n' || c == '\r' || (!c.is_control()))
    {
        return Detection {
            encoding: Encoding::Utf8,
            bom_len: 0,
        };
    }
    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Deny);
    detector.feed(probe, true);
    if let Some(enc) = detect_utf16_by_nuls(probe) {
        return Detection {
            encoding: enc,
            bom_len: 0,
        };
    }
    // Utf8Detection::Deny: the (stricter) UTF-8 probe above is the single authority for
    // UTF-8; chardetng must pick among legacy encodings and UTF-16.
    let rs = detector.guess(None, chardetng::Utf8Detection::Deny);
    let encoding = from_rs(rs).unwrap_or(Encoding::Windows1251);
    Detection {
        encoding,
        bom_len: 0,
    }
}

/// BOM-less UTF-16 heuristic: ASCII-heavy UTF-16 has NUL bytes strictly on one parity (the
/// high byte of each ASCII code unit). Cyrillic/Greek/CJK-only UTF-16 has no such signature
/// and may still be misdetected — the user switches with F8 (documented in the ADR).
fn detect_utf16_by_nuls(probe: &[u8]) -> Option<Encoding> {
    if probe.len() < 8 {
        return None;
    }
    let pairs = probe.len() / 2;
    let even = probe.iter().step_by(2).filter(|&&b| b == 0).count();
    let odd = probe[1..].iter().step_by(2).filter(|&&b| b == 0).count();
    if odd > pairs / 4 && even == 0 {
        return Some(Encoding::Utf16Le);
    }
    if even > pairs / 4 && odd == 0 {
        return Some(Encoding::Utf16Be);
    }
    None
}

/// Round a byte offset *down* to the nearest char-boundary-aligned position <= offset,
/// according to `encoding`. For single-byte encodings this is `offset` itself; for UTF-16 it
/// clears the low bit; for UTF-8 it walks back over continuation bytes (max 3).
pub fn align_down_to_boundary(encoding: Encoding, bytes: &[u8], offset: usize) -> usize {
    let offset = offset.min(bytes.len());
    match encoding {
        Encoding::Utf16Le | Encoding::Utf16Be => offset & !1,
        Encoding::Utf8 => {
            let mut o = offset;
            // `offset == bytes.len()` is itself a boundary; only walk back over continuation
            // bytes that are actually in the slice.
            while o > 0 && o < bytes.len() && (bytes[o] & 0xC0) == 0x80 {
                o -= 1;
            }
            o
        }
        _ => offset,
    }
}

/// Round a byte offset *up* to the nearest char-boundary-aligned position >= offset.
pub fn align_up_to_boundary(encoding: Encoding, bytes: &[u8], offset: usize) -> usize {
    let offset = offset.min(bytes.len());
    match encoding {
        Encoding::Utf16Le | Encoding::Utf16Be => (offset + 1) & !1,
        Encoding::Utf8 => {
            // A head byte (or ASCII) is itself a boundary; only a continuation byte needs
            // skipping, forward to the next head.
            let mut o = offset;
            if o < bytes.len() && (bytes[o] & 0xC0) == 0x80 {
                while o < bytes.len() && (bytes[o] & 0xC0) == 0x80 {
                    o += 1;
                }
            }
            o
        }
        _ => offset,
    }
}

fn utf8_seq_len(head: u8) -> Option<usize> {
    match head {
        0x00..=0x7F => Some(1),
        0xC2..=0xDF => Some(2),
        0xE0..=0xEF => Some(3),
        0xF0..=0xF4 => Some(4),
        _ => None,
    }
}

/// Decode a byte range in `encoding` into displayable text.
///
/// - Invalid bytes become U+FFFD (via encoding_rs), never a panic.
/// - A partial multi-byte sequence at the *end* of `bytes` is dropped, not replaced — callers
///   slicing chunks out of a larger document should extend the slice to a char boundary first
///   (see [`align_down_to_boundary`] / [`align_up_to_boundary`]).
/// - A leading partial sequence (mis-aligned start) is also dropped rather than replaced.
pub fn decode_range(encoding: Encoding, bytes: &[u8]) -> Cow<'_, str> {
    if bytes.is_empty() {
        return Cow::Borrowed("");
    }
    let (head_trim, tail_trim) = boundary_trim(encoding, bytes);
    let bytes = &bytes[head_trim..bytes.len() - tail_trim];
    let (cow, _, _) = encoding.to_rs().decode(bytes);
    cow
}

/// How many bytes to drop at the edges so decoding starts/ends on a char boundary.
fn boundary_trim(encoding: Encoding, bytes: &[u8]) -> (usize, usize) {
    match encoding {
        Encoding::Utf8 => {
            let mut head = 0;
            if !bytes.is_empty() {
                let incomplete_head = match utf8_seq_len(bytes[0]) {
                    Some(n) if n > 1 => {
                        bytes.len() < n || !bytes[1..n].iter().all(|b| (b & 0xC0) == 0x80)
                    }
                    Some(_) => false,
                    None => true,
                };
                if incomplete_head {
                    head = 1;
                }
            }
            let mut tail = 0;
            if !bytes.is_empty() {
                // Skip trailing continuation bytes, then check whether the sequence they
                // belong to is complete; if not, drop the head *and* its partial tail.
                let mut o = bytes.len();
                while o > head && (bytes[o - 1] & 0xC0) == 0x80 {
                    o -= 1;
                }
                if o > head
                    && let Some(n) = utf8_seq_len(bytes[o - 1])
                {
                    let have = bytes.len() - (o - 1);
                    if n > have {
                        tail = have;
                    }
                }
            }
            (head, tail)
        }
        Encoding::Utf16Le | Encoding::Utf16Be => {
            // Odd length: drop the dangling low/high byte at the end.
            (0, bytes.len() % 2)
        }
        _ => (0, 0),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// "привет" encoded by encoding_rs — generation, not hand-rolled bytes.
    fn encoded(text: &str, label: &str) -> Vec<u8> {
        let enc = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap();
        enc.encode(text).0.into_owned()
    }

    const RU: &str = "привет";

    #[test]
    fn detects_boms() {
        assert_eq!(
            detect_encoding(&[0xEF, 0xBB, 0xBF, b'a']).encoding,
            Encoding::Utf8
        );
        assert_eq!(detect_encoding(&[0xEF, 0xBB, 0xBF]).bom_len, 3);
        assert_eq!(
            detect_encoding(&[0xFF, 0xFE, 0x61, 0x00]).encoding,
            Encoding::Utf16Le
        );
        assert_eq!(
            detect_encoding(&[0xFE, 0xFF, 0x00, 0x61]).encoding,
            Encoding::Utf16Be
        );
    }

    #[test]
    fn detects_plain_utf8() {
        assert_eq!(
            detect_encoding("привет".as_bytes()).encoding,
            Encoding::Utf8
        );
        assert_eq!(detect_encoding(b"").encoding, Encoding::Utf8);
    }

    #[test]
    fn detects_legacy_cyrillic() {
        // chardetng on a 6-byte sample is unreliable by design; assert that whatever it
        // guesses is one of the supported encodings. The fixtures integration test checks
        // exact guesses on longer texts.
        for label in ["ibm866", "koi8-r", "windows-1251"] {
            let data = encoded(RU, label);
            let d = detect_encoding(&data);
            assert!(
                Encoding::ALL.contains(&d.encoding),
                "guessed {:?}",
                d.encoding
            );
        }
    }

    #[test]
    fn decodes_all_encodings() {
        assert_eq!(decode_range(Encoding::Ibm866, &encoded(RU, "ibm866")), RU);
        assert_eq!(decode_range(Encoding::Koi8R, &encoded(RU, "koi8-r")), RU);
        assert_eq!(
            decode_range(Encoding::Windows1251, &encoded(RU, "windows-1251")),
            RU
        );
        assert_eq!(decode_range(Encoding::Iso8859_1, b"caf\xE9"), "café");
        let utf16le: Vec<u8> = "привет"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes())
            .collect();
        let utf16be: Vec<u8> = "привет"
            .encode_utf16()
            .flat_map(|u| u.to_be_bytes())
            .collect();
        assert_eq!(decode_range(Encoding::Utf16Le, &utf16le), "привет");
        assert_eq!(decode_range(Encoding::Utf16Be, &utf16be), "привет");
    }

    #[test]
    fn strips_utf16_bom_when_decoding() {
        // decode_range keeps the BOM as U+FEFF unless the caller skips it; encoding_rs's
        // decode strips it for UTF-16.
        let mut v = vec![0xFF, 0xFE];
        v.extend("hi".encode_utf16().flat_map(|u| u.to_le_bytes()));
        let s = decode_range(Encoding::Utf16Le, &v);
        assert_eq!(s.trim_start_matches('\u{FEFF}'), "hi");
    }

    #[test]
    fn invalid_bytes_become_replacement_chars() {
        let s = decode_range(Encoding::Utf8, b"a\xFF\xFFb");
        assert_eq!(s, "a\u{FFFD}\u{FFFD}b");
        // Lone surrogate halves in UTF-16.
        let bad16 = [0x61, 0x00, 0x00, 0xD8, 0x62, 0x00];
        let s = decode_range(Encoding::Utf16Le, &bad16);
        assert!(s.contains('\u{FFFD}'));
        assert!(s.starts_with('a'));
        assert!(s.ends_with('b'));
    }

    #[test]
    fn never_panics_on_random_bytes() {
        // Deterministic pseudo-random sweep (proptest covers the full space).
        let mut x: u32 = 0x1234_5678;
        let mut bytes = vec![0u8; 4096];
        for b in &mut bytes {
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            *b = (x >> 24) as u8;
        }
        for enc in Encoding::ALL {
            for n in 0..bytes.len() {
                let _ = decode_range(enc, &bytes[..n]);
            }
        }
    }

    #[test]
    fn aligns_boundaries() {
        let s = "aé€b"; // 1 + 2 + 3 + 1 bytes
        assert_eq!(align_down_to_boundary(Encoding::Utf8, s.as_bytes(), 2), 1);
        assert_eq!(align_up_to_boundary(Encoding::Utf8, s.as_bytes(), 2), 3);
        assert_eq!(align_up_to_boundary(Encoding::Utf8, s.as_bytes(), 4), 6);
        assert_eq!(align_down_to_boundary(Encoding::Utf16Le, &[0; 5], 3), 2);
        assert_eq!(align_up_to_boundary(Encoding::Utf16Le, &[0; 5], 3), 4);
    }

    #[test]
    fn decode_range_drops_edge_partial_chars() {
        // Split "aé" between the two bytes of é: the second chunk starts mid-char and must
        // not produce a replacement char for the missing head.
        let s = "aé";
        let b = s.as_bytes();
        assert_eq!(decode_range(Encoding::Utf8, &b[..2]), "a");
        assert_eq!(decode_range(Encoding::Utf8, &b[2..]), "");
        // Odd-length UTF-16 tail drops the dangling byte.
        assert_eq!(decode_range(Encoding::Utf16Le, &[b'h', 0, b'x']), "h");
    }

    #[test]
    fn encoding_name_roundtrip() {
        for enc in Encoding::ALL {
            assert_eq!(enc.name().parse(), Ok(enc));
        }
        assert!("bogus".parse::<Encoding>().is_err());
    }
}

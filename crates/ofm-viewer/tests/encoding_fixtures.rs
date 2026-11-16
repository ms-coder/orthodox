//! Encoding fixtures (generated here, not committed): CP866 / KOI8-R / CP1251 Russian text,
//! UTF-16LE/BE with and without BOM, Latin-1, invalid UTF-8, binary data — detection +
//! decode round-trips.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use ofm_viewer::{Document, Encoding, decode_range, detect_encoding};

const RU: &str = "Привет, мир! Это тестовый файл viewer-а.";

fn encode_utf16(s: &str, le: bool) -> Vec<u8> {
    s.encode_utf16()
        .flat_map(|u| {
            if le {
                u.to_le_bytes().to_vec()
            } else {
                u.to_be_bytes().to_vec()
            }
        })
        .collect()
}

fn open_doc(data: &[u8]) -> (tempfile::TempDir, Document) {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("fix.txt");
    std::fs::write(&p, data).unwrap();
    (dir, Document::open(&p).unwrap())
}

#[test]
fn utf16le_be_with_and_without_bom() {
    for (le, enc) in [(true, Encoding::Utf16Le), (false, Encoding::Utf16Be)] {
        let mut bom = if le {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        let body = encode_utf16(RU, le);
        bom.extend_from_slice(&body);
        let (_d, doc) = open_doc(&bom);
        let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
        let det = detect_encoding(&head);
        assert_eq!(
            det.encoding,
            enc,
            "BOM'd UTF-16{}",
            if le { "LE" } else { "BE" }
        );
        assert_eq!(det.bom_len, 2);
        let bytes = doc
            .read_range(det.bom_len as u64, usize::try_from(doc.len()).unwrap())
            .unwrap();
        assert_eq!(decode_range(det.encoding, &bytes), RU);

        // Without BOM: chardetng must guess the UTF-16 encoding from the zero-byte pattern.
        let (_d, doc) = open_doc(&body);
        let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
        let det = detect_encoding(&head);
        assert_eq!(
            det.encoding,
            enc,
            "BOM-less UTF-16{}",
            if le { "LE" } else { "BE" }
        );
        let bytes = doc
            .read_range(det.bom_len as u64, usize::try_from(doc.len()).unwrap())
            .unwrap();
        assert_eq!(decode_range(det.encoding, &bytes), RU);
    }
}

#[test]
fn legacy_russian_encodings_roundtrip() {
    let cases: [(Encoding, &str); 3] = [
        (Encoding::Ibm866, "ibm866"),
        (Encoding::Koi8R, "koi8-r"),
        (Encoding::Windows1251, "windows-1251"),
    ];
    for (enc, label) in cases {
        let rs = encoding_rs::Encoding::for_label(label.as_bytes()).unwrap();
        let (encoded, _, _) = rs.encode(RU);
        let (_d, doc) = open_doc(&encoded);
        let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
        let det = detect_encoding(&head);
        eprintln!("{label}: chardetng guessed {:?}", det.encoding);
        // windows-1251 is by far the most common Cyrillic web encoding and is detected
        // reliably; for the rarer DOS/KOI8 encodings chardetng may misguess a short sample
        // (the user fixes it with F8 — spec F21). Decoding must always be exact when the
        // caller passes the right encoding.
        if enc == Encoding::Windows1251 {
            assert_eq!(det.encoding, enc, "{label}");
        }
        assert_eq!(det.bom_len, 0);
        let bytes = doc
            .read_range(0, usize::try_from(doc.len()).unwrap())
            .unwrap();
        assert_eq!(decode_range(enc, &bytes), RU);
    }
}

#[test]
fn latin1_fixture() {
    // Note: no bytes from the ambiguous 0x80-0x9F range — that zone is where single-byte
    // guesses legitimately disagree (windows-1252 says "—", others a control char).
    let text = "café, naïve, résumé, jalapeño (100%)";
    let encoded: Vec<u8> = text.chars().map(|c| c as u32 as u8).collect();
    let (_d, doc) = open_doc(&encoded);
    let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
    let det = detect_encoding(&head);
    // High-half bytes → not UTF-8; chardetng may pick any single-byte encoding, but the file
    // must decode byte-exactly through windows-1252 (encoding_rs's ISO-8859-1).
    assert_ne!(det.encoding, Encoding::Utf8);
    let bytes = doc
        .read_range(0, usize::try_from(doc.len()).unwrap())
        .unwrap();
    assert_eq!(decode_range(Encoding::Iso8859_1, &bytes), text);
}

#[test]
fn invalid_utf8_fixture() {
    let data = b"valid prefix \xF0\x28\x8C\x28 invalid \xC0\xC1 tail";
    let (_d, doc) = open_doc(data);
    let head = doc.read_range(0, ofm_viewer::DETECT_LEN).unwrap();
    let det = detect_encoding(&head);
    // Invalid UTF-8 → falls through to chardetng; whatever it picks must never panic and must
    // decode deterministically.
    let bytes = doc
        .read_range(0, usize::try_from(doc.len()).unwrap())
        .unwrap();
    let s = decode_range(det.encoding, &bytes);
    assert!(s.starts_with("valid prefix"));
    assert!(s.ends_with("tail"));
}

#[test]
fn binary_data_decodes_without_panic() {
    let data: Vec<u8> = (0u32..256).map(|b| b as u8).collect();
    let (_d, doc) = open_doc(&data);
    for enc in Encoding::ALL {
        let bytes = doc
            .read_range(0, usize::try_from(doc.len()).unwrap())
            .unwrap();
        let _ = decode_range(enc, &bytes);
    }
}

//! Helpers for the unit tests of the viewer modules (test builds only).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

use ofm_testkit::temp::TempDir;
use ofm_viewer::Document;

/// A document with `data` as content; keep the returned directory alive while using it.
pub fn doc_with(data: &[u8]) -> (TempDir, Document) {
    let dir = TempDir::new().unwrap();
    let p = dir.path().join("f");
    std::fs::write(&p, data).unwrap();
    let d = Document::open(&p).unwrap();
    (dir, d)
}

/// `s` encoded as UTF-16.
pub fn utf16(s: &str, be: bool) -> Vec<u8> {
    s.encode_utf16()
        .flat_map(|u| if be { u.to_be_bytes() } else { u.to_le_bytes() })
        .collect()
}

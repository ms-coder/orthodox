//! Hex view: 16 bytes per row — offset, hex bytes, and a text column.
//!
//! The text column renders printable ASCII by default; pass any `byte -> char` mapping (e.g.
//! from the current [`crate::Encoding`]) for other views. Rows are produced on demand from a
//! byte slice or straight out of a [`Document`], so a 10 GB file renders one screen at a time.

use crate::document::{Document, DocumentError};
use thiserror::Error;

/// Bytes shown per hex row.
pub const BYTES_PER_ROW: usize = 16;

/// One row of the hex view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HexRow {
    /// Absolute byte offset of the row's first byte.
    pub offset: u64,
    /// Up to [`BYTES_PER_ROW`] bytes (fewer on the final short row).
    pub bytes: Vec<u8>,
    /// Text column: one char per byte, `'.'` for non-printables (per `to_text` mapping).
    pub text: String,
}

/// Errors from document-backed hex reads.
#[derive(Debug, Error)]
pub enum HexError {
    /// Underlying document read failed.
    #[error("read failed: {0}")]
    Read(#[from] DocumentError),
}

/// Build rows `[from_row, from_row + count)` from an in-memory byte slice (cheap: slices and
/// maps — no allocation beyond the rows themselves). `base` is the absolute offset of `data[0]`.
pub fn hex_rows(data: &[u8], base: u64, from_row: usize, count: usize) -> Vec<HexRow> {
    hex_rows_with(data, base, from_row, count, ascii_text)
}

/// Like [`hex_rows`] with a custom byte→text mapping.
pub fn hex_rows_with<F: Fn(u8) -> char>(
    data: &[u8],
    base: u64,
    from_row: usize,
    count: usize,
    to_text: F,
) -> Vec<HexRow> {
    let start = from_row.saturating_mul(BYTES_PER_ROW);
    if start >= data.len() || count == 0 {
        return Vec::new();
    }
    let end = (start + count * BYTES_PER_ROW).min(data.len());
    data[start..end]
        .chunks(BYTES_PER_ROW)
        .enumerate()
        .map(|(i, chunk)| HexRow {
            offset: base + (start + i * BYTES_PER_ROW) as u64,
            bytes: chunk.to_vec(),
            text: chunk.iter().map(|&b| to_text(b)).collect(),
        })
        .collect()
}

/// Build rows `[from_row, from_row + count)` reading from a [`Document`] (one bounded read —
/// cheap for the mmap fast path).
pub fn hex_rows_from(doc: &Document, from_row: u64, count: usize) -> Result<Vec<HexRow>, HexError> {
    hex_rows_from_with(doc, from_row, count, ascii_text).map_err(HexError::Read)
}

/// Like [`hex_rows_from`] with a custom byte→text mapping.
pub fn hex_rows_from_with<F: Fn(u8) -> char>(
    doc: &Document,
    from_row: u64,
    count: usize,
    to_text: F,
) -> Result<Vec<HexRow>, DocumentError> {
    if count == 0 {
        return Ok(Vec::new());
    }
    let offset = from_row.saturating_mul(BYTES_PER_ROW as u64);
    if offset >= doc.len() {
        return Ok(Vec::new());
    }
    let data = doc.read_range(offset, count.saturating_mul(BYTES_PER_ROW))?;
    Ok(data
        .chunks(BYTES_PER_ROW)
        .enumerate()
        .map(|(i, chunk)| HexRow {
            offset: offset + (i * BYTES_PER_ROW) as u64,
            bytes: chunk.to_vec(),
            text: chunk.iter().map(|&b| to_text(b)).collect(),
        })
        .collect())
}

/// Default text-column mapping: printable ASCII as-is, anything else `'.'`.
pub fn ascii_text(b: u8) -> char {
    if b.is_ascii_graphic() || b == b' ' {
        b as char
    } else {
        '.'
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_sixteen_bytes_with_offset_and_text() {
        let data: Vec<u8> = (0u8..40).collect();
        let rows = hex_rows(&data, 1000, 1, 2);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].offset, 1016);
        assert_eq!(rows[0].bytes.len(), 16);
        assert_eq!(rows[0].bytes[0], 16);
        assert_eq!(rows[1].offset, 1032);
        assert_eq!(rows[1].bytes.len(), 8); // short final row
        assert_eq!(rows[0].text.len(), 16);
    }

    #[test]
    fn text_column_replaces_non_printables() {
        let data = [b'a', 0x00, 0xFF, b' ', b'~'];
        let rows = hex_rows(&data, 0, 0, 1);
        assert_eq!(rows[0].text, "a.. ~");
    }

    #[test]
    fn out_of_range_rows_are_empty() {
        let data = [0u8; 8];
        assert!(hex_rows(&data, 0, 1, 5).is_empty());
        assert!(hex_rows(&data, 0, 0, 0).is_empty());
    }

    #[test]
    fn document_rows_match_slice_rows() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("h.bin");
        let data: Vec<u8> = (0u8..=255).collect();
        std::fs::write(&p, &data).unwrap();
        let doc = Document::open(&p).unwrap();
        let a = hex_rows_from(&doc, 3, 2).unwrap();
        let b = hex_rows(&data, 0, 3, 2);
        assert_eq!(a, b);
        assert!(hex_rows_from(&doc, 1000, 1).unwrap().is_empty());
    }
}

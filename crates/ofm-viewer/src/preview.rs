//! Quick-view previews (spec F22 backend).
//!
//! [`PreviewProvider`] is the extension point: the UI asks each registered provider
//! `can_preview`, then calls `preview` on the first that accepts. Implementations must be
//! cheap, cancellable, and must not block the UI thread — run them on a worker.

use std::path::Path;

use ofm_core::CancelToken;
use thiserror::Error;

use crate::document::{Document, DocumentError};
use crate::encoding::{Encoding, decode_range, detect_encoding};

/// What a preview produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Text preview: first `lines.len()` lines, decoded, plus total lines if already known.
    Text {
        /// Decoded preview lines (may be fewer than requested near EOF).
        lines: Vec<String>,
        /// Encoding used for decoding.
        encoding: Encoding,
        /// Total line count if the index finished within the preview budget.
        total_lines: Option<usize>,
    },
    /// An image preview rendered from this path (by the UI; the backend only tags it).
    Image {
        /// Path of the image file.
        path: std::path::PathBuf,
    },
    /// A directory summary (entry counts by kind; filled in by ofm-vfs callers in v1.0).
    DirSummary,
    /// Nothing to preview.
    None,
}

/// Preview failures.
#[derive(Debug, Error)]
pub enum PreviewError {
    /// The document could not be opened.
    #[error("open failed: {0}")]
    Open(#[from] DocumentError),
    /// Cancelled via [`CancelToken`].
    #[error("preview cancelled")]
    Cancelled,
    /// The file is empty.
    #[error("empty file")]
    Empty,
}

/// Extension point for quick-view content providers (F22).
pub trait PreviewProvider: Send + Sync {
    /// True if this provider can preview `path` (cheap: extension/stat checks only).
    fn can_preview(&self, path: &Path) -> bool;
    /// Produce the preview. Runs on a worker; must respect `cancel` between chunks.
    fn preview(&self, path: &Path, cancel: &CancelToken) -> Result<Preview, PreviewError>;
}

/// Built-in text preview: the first `max_lines` of a document, decoded with detected encoding.
#[derive(Debug, Clone)]
pub struct TextPreviewProvider {
    /// How many lines to decode at most.
    pub max_lines: usize,
    /// Cap on bytes scanned to gather those lines (bounds work on huge files).
    pub max_bytes: usize,
}

impl Default for TextPreviewProvider {
    fn default() -> Self {
        Self {
            max_lines: 200,
            max_bytes: 1024 * 1024,
        }
    }
}

impl PreviewProvider for TextPreviewProvider {
    fn can_preview(&self, path: &Path) -> bool {
        // Cheap heuristic: any regular file that exists. Decoding failures still yield a
        // (replacement-char) text preview, which is the desired fallback for unknown data.
        path.is_file()
    }

    fn preview(&self, path: &Path, cancel: &CancelToken) -> Result<Preview, PreviewError> {
        if cancel.is_cancelled() {
            return Err(PreviewError::Cancelled);
        }
        let doc = Document::open(path)?;
        if doc.is_empty() {
            return Err(PreviewError::Empty);
        }
        let head = doc.read_range(0, crate::encoding::DETECT_LEN)?;
        let detection = detect_encoding(&head);
        let encoding = detection.encoding;

        // Decode up to max_bytes from just past the BOM.
        let want = self.max_bytes;
        let bytes = doc.read_range(detection.bom_len as u64, want)?;
        let text = decode_range(encoding, &bytes);

        // Count lines with the viewer's line rules (LF, CRLF, lone CR) so quick-view line
        // counts agree with F3; str::lines() would miss lone-CR breaks. Line ranges fall on
        // ASCII terminator bytes (or the text edges), so they are always char boundaries.
        let mut index = crate::line_index::LineIndex::new();
        index.index_chunk(text.as_bytes());
        index.finish();
        let fully_read = detection.bom_len as u64 + bytes.len() as u64 >= doc.len();
        let all_count = index.line_count();
        let total_lines = (fully_read && all_count <= self.max_lines).then_some(all_count);
        let lines: Vec<String> = (0..index.line_count().min(self.max_lines))
            .filter_map(|n| {
                index
                    .line_range(n)
                    .and_then(|r| text.get(r.start as usize..r.end as usize))
                    .map(str::to_owned)
            })
            .collect();
        Ok(Preview::Text {
            lines,
            encoding,
            total_lines,
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn text_preview_first_lines() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.txt");
        std::fs::write(&p, "a\nb\nc\n").unwrap();
        let provider = TextPreviewProvider {
            max_lines: 2,
            ..Default::default()
        };
        let prev = provider.preview(&p, &CancelToken::new()).unwrap();
        match prev {
            Preview::Text {
                lines,
                encoding,
                total_lines,
            } => {
                assert_eq!(lines, vec!["a", "b"]);
                assert_eq!(encoding, Encoding::Utf8);
                assert!(total_lines.is_none()); // truncated at max_lines
            }
            other => panic!("unexpected preview: {other:?}"),
        }
    }

    #[test]
    fn text_preview_full_when_short() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.txt");
        std::fs::write(&p, "only\n").unwrap();
        let provider = TextPreviewProvider::default();
        let prev = provider.preview(&p, &CancelToken::new()).unwrap();
        match prev {
            Preview::Text {
                lines, total_lines, ..
            } => {
                assert_eq!(lines, vec!["only"]);
                assert_eq!(total_lines, Some(1));
            }
            other => panic!("unexpected preview: {other:?}"),
        }
    }

    #[test]
    fn empty_file_is_none_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("e");
        std::fs::write(&p, b"").unwrap();
        let provider = TextPreviewProvider::default();
        assert!(matches!(
            provider.preview(&p, &CancelToken::new()),
            Err(PreviewError::Empty)
        ));
    }

    #[test]
    fn can_preview_any_regular_file() {
        let provider = TextPreviewProvider::default();
        assert!(
            provider.can_preview(Path::new("/etc/hostname"))
                || !Path::new("/etc/hostname").exists()
        );
        assert!(!provider.can_preview(Path::new("/definitely/not/here")));
    }
}

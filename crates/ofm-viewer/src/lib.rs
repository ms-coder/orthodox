//! ofm-viewer: the F3/F4 viewer backend (spec F21). No GTK, no glib/gio — pure Rust, runs on a
//! worker thread, fully unit-testable. The GTK viewer UI (T5.2) is written against this API.
//!
//! File ownership:
//! - T5.0/T5.1: this crate (API + implementation), `docs/adr/0007-viewer-backend.md`
//! - T5.2: ofm-app only — do not change this API without an ADR.
//!
//! # Overview
//!
//! A viewer session looks like this:
//!
//! 1. [`Document::open`] maps (or buffers) a file. It never reads data at open time, so a 10 GB
//!    sparse file opens instantly. All heavy calls take `&self`, are `Send + Sync`, and accept an
//!    [`ofm_core::CancelToken`].
//! 2. [`detect_encoding`] picks UTF-8 / UTF-16LE/BE / windows-1251 / IBM866 / KOI8-R / ISO-8859-1
//!    (BOM → UTF-8 validity of the first 64 KiB → chardetng).
//! 3. [`index_document`] builds a [`LineIndex`] lazily in 4 MiB chunks on a worker; the UI can
//!    render the first screen before indexing finishes (progress via atomics).
//! 4. [`decode_range`] turns a byte range into displayable `str` (replacement chars for invalid
//!    bytes, never a panic, never a split multi-byte char at a chunk edge).
//! 5. [`wrap_line`] / [`slice_columns`] handle wrapped and horizontally scrolled layouts using
//!    display columns ([`unicode_width`] semantics: wide = 2, combining = 0, tabs = fixed stops).
//! 6. [`hex_rows`] renders 16-bytes-per-row hex views.
//! 7. [`Needle`] + [`find_next`] / [`find_prev`] search text (encoded needle), hex bytes, or
//!    regex over the raw bytes, forward/backward, with chunk overlap and optional wrap.
//! 8. [`PreviewProvider`] lets F22 quick-view plug in more preview kinds;
//!    [`TextPreviewProvider`] is the built-in text implementation.
//!
//! # Line-ending and encoding choices (documented limitations)
//!
//! - Line breaks: `\n`, `\r\n`, and lone `\r` each end a line (Midnight Commander semantics).
//! - `encoding_rs`' `ISO_8859_1` is the Web-standard windows-1252 superset; bytes 0x80–0x9F
//!   therefore decode to printable punctuation, not C1 controls.
//! - Case-insensitive text search folds ASCII only (upper/lower/title byte variants).
//! - Regex search runs [`regex::bytes`] on raw bytes; build the pattern from the needle encoded
//!   in the current encoding for non-UTF-8 files.
//! - If a mapped file is truncated by another process, reads beyond the new end can SIGBUS. This
//!   crate guards every access with a current file-size check and falls back to `pread`, which
//!   short-reads instead of faulting; a true TOCTOU race on the last byte of a shrinking file
//!   can still fault — reopen the document in that (rare) case.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod document;
pub mod encoding;
pub mod hex;
pub mod line_index;
pub mod preview;
pub mod search;
pub mod wrap;

pub use document::{Document, DocumentError, SourceKind};
pub use encoding::{
    BOM_MAX_LEN, DETECT_LEN, Detection, Encoding, align_down_to_boundary, align_up_to_boundary,
    decode_range, detect_encoding,
};
pub use hex::{BYTES_PER_ROW, HexRow, hex_rows, hex_rows_from};
pub use line_index::{CHUNK_SIZE, LineIndex, index_document};
pub use preview::{Preview, PreviewError, PreviewProvider, TextPreviewProvider};
pub use search::{
    Match, Needle, PreparedNeedle, SearchError, SearchOptions, find_next, find_prev, goto_line,
    goto_offset,
};
pub use wrap::{DEFAULT_TAB_WIDTH, Segment, slice_columns, wrap_line};

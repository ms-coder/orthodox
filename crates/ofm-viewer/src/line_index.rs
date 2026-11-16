//! Lazy, incremental line index.
//!
//! A [`LineIndex`] is built by feeding raw chunks ([`LineIndex::index_chunk`]); the UI drives it
//! from a worker in [`CHUNK_SIZE`] pieces so the first screen renders before indexing finishes.
//! Progress is observable through the atomics the caller passes to [`index_document`].
//!
//! Line breaks: `\n`, `\r\n`, and lone `\r` each terminate a line (Midnight Commander
//! semantics). A file whose last line has no terminator still counts that line. A trailing
//! terminator does **not** create a phantom empty final line.

use std::sync::atomic::{AtomicU64, Ordering};

use ofm_core::CancelToken;
use thiserror::Error;

/// Chunk size the worker feeds at a time (4 MiB).
pub const CHUNK_SIZE: usize = 4 * 1024 * 1024;

/// Indexing was cancelled via [`CancelToken`].
#[derive(Debug, Clone, Copy, Error)]
#[error("line indexing cancelled")]
pub struct IndexCancelled;

/// Incremental index of line start offsets.
#[derive(Debug, Default, Clone)]
pub struct LineIndex {
    /// Byte offset where each line starts. `starts[0] == 0` for non-empty input.
    starts: Vec<u64>,
    /// Content end (terminator excluded) of each *terminated* line, in order; `ends[n]`
    /// pairs with `starts[n]`.
    ends: Vec<u64>,
    /// Bytes fed so far.
    indexed_len: u64,
    /// True when the last fed byte was `\r` and the next chunk may start with `\n`.
    pending_cr: bool,
    /// True once `starts` has been seeded with the first line.
    started: bool,
    /// True after [`finish`](Self::finish).
    complete: bool,
}

impl LineIndex {
    /// Empty index; feed chunks with [`index_chunk`](Self::index_chunk).
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes indexed so far.
    pub fn indexed_len(&self) -> u64 {
        self.indexed_len
    }

    /// True once [`finish`](Self::finish) was called; totals are then exact.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// Number of lines known so far — exact iff [`is_complete`](Self::is_complete).
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Byte offset where line `n` starts, if known.
    pub fn line_start(&self, n: usize) -> Option<u64> {
        self.starts.get(n).copied()
    }

    /// Byte range of line `n`'s *content* (terminator excluded), if known.
    ///
    /// For the last, still-unterminated line the end is the indexed length so far.
    pub fn line_range(&self, n: usize) -> Option<std::ops::Range<u64>> {
        let start = self.line_start(n)?;
        let end = if n < self.ends.len() {
            self.ends[n]
        } else {
            self.indexed_len
        };
        Some(start..end.max(start))
    }

    /// Index of the line containing byte `offset` (the line whose range includes `offset`;
    /// an `offset` exactly at a line start belongs to that line). `offset` past the end clamps
    /// to the last line. Returns 0 for an empty index.
    pub fn line_containing(&self, offset: u64) -> usize {
        match self.starts.binary_search(&offset) {
            Ok(n) => n,
            Err(0) => 0,
            Err(n) => n - 1,
        }
    }

    /// Feed the next chunk of file bytes, in order. Chunks must be contiguous; use
    /// [`CHUNK_SIZE`]-sized reads on the worker.
    pub fn index_chunk(&mut self, chunk: &[u8]) {
        let base = self.indexed_len;
        let mut i = 0usize;

        if !self.started {
            self.started = true;
            if !chunk.is_empty() {
                self.starts.push(base);
            }
        }

        // Resolve a `\r` that ended the previous chunk: if this chunk starts with `\n` the
        // pair is one break and the new line starts after it; otherwise the `\r` was a lone
        // break and the new line starts here. The content end was already recorded with the
        // `\r` itself (it is the same for `\r` and `\r\n`).
        if self.pending_cr
            && let Some(&first) = chunk.first()
        {
            self.pending_cr = false;
            if first == b'\n' {
                i = 1;
            }
            self.starts.push(base + i as u64);
        }

        while i < chunk.len() {
            let Some(rel) = memchr::memchr2(b'\r', b'\n', &chunk[i..]) else {
                break;
            };
            let p = i + rel;
            if chunk[p] == b'\r' && p + 1 == chunk.len() {
                // May pair with a leading `\n` in the next chunk: record the content end now
                // but defer the next line's start until the chunk boundary resolves.
                self.ends.push(base + p as u64);
                self.pending_cr = true;
                break;
            }
            let next = p + 1 + usize::from(chunk[p] == b'\r' && chunk[p + 1] == b'\n');
            self.ends.push(base + p as u64);
            self.starts.push(base + next as u64);
            i = next;
        }
        self.indexed_len = base + chunk.len() as u64;
    }

    /// Mark the input as fully fed. Trims a phantom final start that sits exactly at EOF
    /// (produced by a trailing terminator).
    pub fn finish(&mut self) {
        if self.starts.last() == Some(&self.indexed_len) {
            self.starts.pop();
        }
        self.complete = true;
    }
}

/// Build a complete [`LineIndex`] for `doc` on the caller's worker thread.
///
/// Reads in [`CHUNK_SIZE`] chunks (safe on truncated files — see [`crate::document`]) and
/// checks `cancel` between chunks. Publishes bytes-indexed progress to `indexed_bytes` so the
/// UI can draw a progress indicator.
pub fn index_document(
    doc: &crate::document::Document,
    cancel: &CancelToken,
    indexed_bytes: &AtomicU64,
) -> Result<LineIndex, IndexCancelled> {
    let mut index = LineIndex::new();
    let mut buf = vec![0u8; CHUNK_SIZE];
    let mut off = 0u64;
    loop {
        if cancel.is_cancelled() {
            return Err(IndexCancelled);
        }
        let n = doc.read_at(off, &mut buf).map_err(|_| IndexCancelled)?; // treat read errors as a stopped index; caller reopens
        if n == 0 {
            break;
        }
        index.index_chunk(&buf[..n]);
        off += n as u64;
        indexed_bytes.store(off, Ordering::Relaxed);
    }
    index.finish();
    Ok(index)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Naive reference implementation: split on \n, \r\n, lone \r; no phantom trailing line.
    fn naive_lines(data: &[u8]) -> Vec<(u64, u64)> {
        let mut out = Vec::new();
        let mut start = 0usize;
        let mut i = 0usize;
        while i < data.len() {
            if data[i] == b'\n' {
                out.push((start as u64, i as u64));
                i += 1;
                start = i;
            } else if data[i] == b'\r' {
                let end = i;
                if i + 1 < data.len() && data[i + 1] == b'\n' {
                    i += 1;
                }
                out.push((start as u64, end as u64));
                i += 1;
                start = i;
            } else {
                i += 1;
            }
        }
        if start < data.len() {
            out.push((start as u64, data.len() as u64));
        }
        out
    }

    fn build(data: &[u8], chunk: usize) -> LineIndex {
        let mut idx = LineIndex::new();
        for c in data.chunks(chunk.max(1)) {
            idx.index_chunk(c);
        }
        idx.finish();
        idx
    }

    #[test]
    fn empty_file_has_no_lines() {
        let idx = build(b"", 4);
        assert!(idx.is_complete());
        assert_eq!(idx.line_count(), 0);
        assert_eq!(idx.line_start(0), None);
    }

    #[test]
    fn basic_lf() {
        let idx = build(b"ab\ncd\n", 1024);
        assert_eq!(idx.line_count(), 2);
        assert_eq!(idx.line_range(0), Some(0..2));
        assert_eq!(idx.line_range(1), Some(3..5));
        assert_eq!(idx.line_containing(0), 0);
        assert_eq!(idx.line_containing(3), 1);
    }

    #[test]
    fn no_trailing_newline_counts_last_line() {
        let idx = build(b"ab\ncd", 1024);
        assert_eq!(idx.line_count(), 2);
        assert_eq!(idx.line_range(1), Some(3..5));
    }

    #[test]
    fn crlf_and_lone_cr() {
        let data = b"a\r\nb\rc\n";
        let idx = build(data, 1024);
        assert_eq!(idx.line_count(), 3);
        assert_eq!(idx.line_range(0), Some(0..1));
        assert_eq!(idx.line_range(1), Some(3..4));
        assert_eq!(idx.line_range(2), Some(5..6));
    }

    #[test]
    fn cr_split_across_chunks() {
        // The \r\n pair is split by the chunk boundary; must count as one break.
        let data = b"line1\r\nline2\r\nend";
        for chunk in 1..=3 {
            let idx = build(data, chunk);
            assert_eq!(idx.line_count(), 3, "chunk={chunk}");
            assert_eq!(idx.line_range(0), Some(0..5), "chunk={chunk}");
            assert_eq!(idx.line_range(1), Some(7..12), "chunk={chunk}");
            assert_eq!(idx.line_range(2), Some(14..17), "chunk={chunk}");
        }
    }

    #[test]
    fn matches_naive_split_with_tiny_chunks() {
        let data = b"\n\r\n\rabc\ndef\r\n\nxyz\r\r\nlast";
        for chunk in 1..=7 {
            let idx = build(data, chunk);
            let want = naive_lines(data);
            assert_eq!(idx.line_count(), want.len(), "chunk={chunk}");
            for (n, (s, e)) in want.iter().enumerate() {
                assert_eq!(idx.line_range(n), Some(*s..*e), "chunk={chunk} line={n}");
            }
        }
    }

    #[test]
    fn line_count_so_far_is_incremental() {
        let mut idx = LineIndex::new();
        assert_eq!(idx.line_count(), 0);
        idx.index_chunk(b"one\ntw");
        assert_eq!(idx.line_count(), 2); // "one" + started "tw"
        assert!(!idx.is_complete());
        idx.index_chunk(b"o\nthree\n");
        assert_eq!(idx.line_count(), 4);
        idx.index_chunk(b"tail");
        idx.finish();
        assert_eq!(idx.line_count(), 4);
        assert_eq!(idx.line_range(1), Some(4..7));
        assert_eq!(idx.line_range(3), Some(14..18));
    }

    #[test]
    fn progress_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.txt");
        let line = b"0123456789abcdef\n";
        let mut data = Vec::new();
        for _ in 0..(CHUNK_SIZE / line.len() + 2) {
            data.extend_from_slice(line);
        }
        std::fs::write(&p, &data).unwrap();
        let doc = crate::document::Document::open(&p).unwrap();
        let progress = AtomicU64::new(0);
        let cancel = CancelToken::new();
        cancel.cancel();
        let err = index_document(&doc, &cancel, &progress).unwrap_err();
        assert_eq!(err.to_string(), "line indexing cancelled");
        // Not cancelled: completes and reports full progress.
        let cancel = CancelToken::new();
        let idx = index_document(&doc, &cancel, &progress).unwrap();
        assert_eq!(progress.load(Ordering::Relaxed), data.len() as u64);
        assert!(idx.is_complete());
        assert_eq!(idx.line_count(), data.len() / line.len());
    }
}

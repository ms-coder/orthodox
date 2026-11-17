# ADR-0007: Viewer backend (ofm-viewer) API

Status: accepted · 2026-11-16 · frozen for T5.2 (GTK viewer UI builds on it unchanged)

## Context

F21 (F3 viewer) and F22 (quick view) need a UI-agnostic backend: instant open of 10 GB files,
lazy line indexing, encoding switch, wrap/hex modes, search, previews. The GTK UI (T5.2) is
written by someone else against this API, so it must be small, documented, and hard to misuse
from the GTK main thread.

## Decision

- `ofm-viewer` has **no gtk/glib/gio**. It reuses `ofm_core::CancelToken`; every heavy call
  takes `&self` + `&CancelToken`, is `Send + Sync`, and is safe to run on a worker thread.
- `Document`: **no mmap** — every read is `pread` on the open fd, so a file truncated by
  another process short-reads instead of SIGBUS, with no size-check TOCTOU window at all
  (T5.5a; the original mmap fast path could SIGBUS between the stat and the page-in and
  has been removed). Open uses `O_NONBLOCK` plus an fstat regular-file check, so a path
  swapped for a FIFO/socket/device after a directory listing fails open with a clear error
  instead of blocking the thread forever; size-0 regular files (`/proc`) fall back to a
  buffered read capped at 64 MiB. A 10 GB sparse file still opens in microseconds (no data
  is read at open) and the first screen reads in < 100 ms.
- `LineIndex`: caller-fed 4 MiB chunks; `\n`, `\r\n`, and lone `\r` all break lines (MC
  semantics); no phantom line after a trailing newline; `index_document` publishes progress
  through a caller-owned `AtomicU64`. Byte-oriented, ASCII-compatible encodings only —
  raw UTF-16 miscounts (CRLF = two breaks, offsets mid-unit); the GTK UI does its own
  encoding-aware scanning and does not use it in v1.0.
- Encodings via `encoding_rs` (UTF-8, UTF-16LE/BE, windows-1251, IBM866, KOI8-R,
  ISO-8859-1=windows-1252 superset). Detection: BOM → valid UTF-8 in first 64 KiB (rejected
  if it is NUL/control-heavy — Cyrillic UTF-16LE bytes are "valid UTF-8" controls) → NUL-parity
  heuristic for BOM-less UTF-16 (ASCII-heavy files only) → chardetng (unknown → windows-1251).
  `decode_range` drops edge-partial sequences instead of emitting replacement chars, and
  alignment helpers keep chunk cuts on char boundaries. Short KOI8-R/CP866 samples may still
  misdetect (chardetng favours windows-1251); F8 switches manually.
- Search: needle encoded in the current encoding, `memchr::memmem` over 1 MiB windows with a
  needle-length overlap; case-insensitive text = up to 4 ASCII byte variants (as-is/lower/
  Capitalized/UPPER); regex = `regex::bytes` on raw bytes (boundary-spanning matches longer
  than 64 KiB may be missed). Forward/backward, optional wrap, results as byte offsets. In
  UTF-16LE/BE modes every match (text, hex, regex) must start on an even byte offset; hits at
  odd offsets are unit-straddling false positives and are skipped (T5.5a).
- Wrap/hex: `unicode-width` columns (wide=2, combining=0, tab stops configurable); hex rows
  are 16 bytes: offset, hex, ASCII text column.
- `PreviewProvider` trait (`can_preview`/`preview`) + `TextPreviewProvider` (first N lines);
  `Preview` enum: `Text | Image{path} | DirSummary | None`.

## Consequences

- The UI never blocks: open/index/search run on workers; partial line counts are displayable.
- Non-UTF-8 needles work because search operates on encoded bytes, not `str`.
- v1.1 can add Unicode case folding and full-Unicode regex over decoded text behind the same
  `PreparedNeedle` API.

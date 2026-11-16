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
- `Document`: `memmap2` for regular files (open reads nothing; first screen = page faults);
  buffered fallback capped at 64 MiB for pipes/`/proc`. Every read re-checks the current file
  size and falls back to `pread` so a concurrent truncation short-reads instead of SIGBUS
  (TOCTOU on the very last byte is documented in the crate docs).
- `LineIndex`: caller-fed 4 MiB chunks; `\n`, `\r\n`, and lone `\r` all break lines (MC
  semantics); no phantom line after a trailing newline; `index_document` publishes progress
  through a caller-owned `AtomicU64`.
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
  than 64 KiB may be missed). Forward/backward, optional wrap, results as byte offsets.
- Wrap/hex: `unicode-width` columns (wide=2, combining=0, tab stops configurable); hex rows
  are 16 bytes: offset, hex, ASCII text column.
- `PreviewProvider` trait (`can_preview`/`preview`) + `TextPreviewProvider` (first N lines);
  `Preview` enum: `Text | Image{path} | DirSummary | None`.

## Consequences

- The UI never blocks: open/index/search run on workers; partial line counts are displayable.
- Non-UTF-8 needles work because search operates on encoded bytes, not `str`.
- v1.1 can add Unicode case folding and full-Unicode regex over decoded text behind the same
  `PreparedNeedle` API.

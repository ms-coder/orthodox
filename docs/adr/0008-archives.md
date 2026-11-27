# ADR-0008: Archives (read-only browsing and extraction, F26)

Status: accepted · 2026-11-22 · frozen by T6.0 for T6.1 (provider), T6.2 (extract job), T6.3 (UI)

## Context

F26: Enter zip, tar, tar.gz/bz2/xz/zst and 7z as folders (read-only); copy out = extract; password
prompt for encrypted zip/7z; corrupt archives show an error, never crash. Archives are untrusted
input: zip-slip, symlink tricks, decompression bombs, panics in decoders. Three tasks build in
parallel (provider, extract job, UI), so the seams must be fixed first.

## Decision

**Layering.** `ofm-vfs::archive`: `open_archive(path, &dyn PasswordProvider, &ArchiveLimits, &CancelToken)`
detects the format by magic bytes (`detect_magic`, never the extension) and returns a
`Box<dyn ArchiveReader>` from one of `backend/{zip,tar,sevenz}.rs`. `ArchiveReader` exposes
`index() -> &Arc<ArchiveIndex>` and `visit(ids, cancel, f)`; `read_entry` is a convenience on top.
`ArchiveVfs` adapts an index to the `Vfs` trait for panels. The extract job (`ofm-jobs`) uses
`visit` directly and writes through its own `Fs`; `ofm-jobs` therefore depends on `ofm-vfs`.

**Crates** (pure Rust or vendored C only; verified to build on Rust 1.100 / Ubuntu 24.04 with just
`build-essential`, no extra `-dev` package):

| Format | Crate | Why |
|---|---|---|
| zip | `zip` 9, `default-features = false`, features `aes-crypto deflate deflate64 bzip2 zstd xz lzma` | ZipCrypto + AES-128/192/256, zip64, central-directory random access, actively maintained |
| tar | `tar` 0.4 | de-facto standard; GNU/pax/ustar, raw byte paths |
| gz | `flate2` (default pure-Rust backend) | |
| bz2 | `bzip2` 0.6 (default = pure-Rust `libbz2-rs-sys`) | no system libbz2 needed |
| xz | `lzma-rust2` `XzReader` (pure Rust; already a dependency of `zip` and `sevenz-rust2`) | avoids a second LZMA implementation and a system `liblzma` link |
| zst | `zstd` 0.14 (vendored C via `zstd-sys`) | fast, window limit honoured |
| 7z | `sevenz-rust2` 0.25, features `aes256 bzip2 ppmd zstd deflate` | pure Rust, AES-256, **encrypted headers**, `for_each_entries` matches `visit`, memory-limit constructor |

T6.1 adds them to `crates/ofm-vfs/Cargo.toml`. If one fails the "no extra system package" rule on
the CI image, fall back to the pure-Rust alternative of the same row and report it.

**Index.** `IndexBuilder::push(RawEntry)` per stored entry, then `finish()`: a flat `Vec<ArchiveEntry>`
(id = archive order; path, kind, size, packed size, mtime, mode, uid/gid, link target, encrypted,
synthetic, safety) plus `list_dir(dir)`, `lookup(path)`, `select(paths)` (entries and everything below,
archive order) and `to_entry()` for panel rows. Missing parent dirs are synthesized; for a duplicated
path the last entry wins (tar semantics); a symlink to a directory inside the archive can be entered.
Metadata only is kept (about 200 bytes + path per entry).

**Path scheme.** `Location::Archive { archive, inner }`: `archive` is the absolute local archive file
(no nesting in v1.0), `inner` a normalized relative path (empty = root). The parent of the root is the
directory containing the archive file (Backspace returns there, cursor on the archive). Capabilities:
`read` only; modifying calls use `VfsError::ReadOnly` (`ArchiveVfs::read_only_error`). `open_read`
returns a seekable in-memory buffer up to `MAX_OPEN_READ_BYTES` (64 MiB); larger entries (F3) are
extracted to a temp file by a job.

**Zip-slip policy** (decided at index time, `path.rs`): an entry is `Unsafe` if its path is absolute,
has any `..` component (even `a/../b`), contains NUL; if a symlink/hardlink target is absolute, empty
or climbs above the archive root (lexically); if an earlier symlink entry is a prefix of its path; or
if a parent component is a non-directory entry. Unsafe entries stay in `entries()` (for "N unsafe
entries skipped") but are invisible to `list_dir`/`lookup`/`select` and `visit` refuses them
(`ArchiveError::Unsafe`). Backslashes are ordinary name characters (zip backends convert them for
FAT/NTFS-made entries before `push`). Extraction adds a second line of defence (below).

**Password model.** `PasswordProvider::password(archive, retry) -> Option<String>` is a blocking
callback on a worker; the app implements it with a `Responder` and polls the job's `CancelToken`.
`None` on the initial request = user cancelled / no password = `NeedsPassword`. After a password was
rejected, `None` means the provider has no further attempt and the result stays `WrongPassword`.
`PasswordSource` wraps a provider with a per-archive in-memory cache, filled by `accepted()` once a
backend has verified a password (CRC/MAC); the panel and extract jobs share one source, so the user is
asked once. A wrong password triggers `retry = true` up to `MAX_PASSWORD_ATTEMPTS` (3), then
`WrongPassword`. One password per archive; never persisted.
Searches use `NoPassword` and skip encrypted archives with a warning.

**Streaming and memory.** Entry data is never loaded whole: `visit` hands a `&mut dyn Read` per entry,
in archive order, so tar.* and solid 7z are read in **one** pass (random access per entry would
decompress the archive once per file). Only zip seeks. Index building for tar.* is one sequential pass.
Every library call is wrapped in `catch_unwind` (decoder panic = `Corrupt`, invariant 7).

**Limits** (`ArchiveLimits`, tests shrink them): `max_entries` 500 000 and `max_depth` 256 are hard
(`LimitExceeded`); `max_codec_memory` 256 MiB caps LZMA/xz dictionaries and zstd windows
(`LimitExceeded(CodecMemory)`); a stream producing more bytes than its declared size is stopped
(`ExceedsDeclaredSize`: lying headers); soft warnings in `IndexWarnings` for the UI/job to show before
extracting: declared total over `warn_total_bytes` (64 GiB) and entries over `max_ratio` 1000:1 (when
>= 1 MiB). Disk-full is handled like in copy (ENOSPC question).

**Extraction** (`JobSpec::Extract { archive, entries, dest_dir, opts: ExtractOpts }`, additive change
to ADR-0006; `ExtractOpts { on_conflict, password, limits, sync }`; outcome = `Outcome::Ok(Report)`).
Same protocol as ADR-0004, in `ofm-jobs/src/extract.rs`: open, `select`, one `visit` pass; files go to
`.<name>.ofm-part-<rand>` (O_EXCL, 0600), size verified, mode `& 0o777` (never setuid/setgid/sticky;
uid/gid are never applied, even as root) and mtime restored, published with `renameat2(NOREPLACE)`;
conflicts through `ctx.resolve_conflict`; cancel/failure unlinks the temp; directories 0700 then final
mode/mtime post-order. Symlinks only if the index says safe; no component below `dest_dir` is followed
through a symlink (an existing one is a `TypeMismatch` conflict). Hardlinks become hard links to the
extracted target, else a copy of its data. Devices/fifos/sockets are skipped with a warning. xattrs
are not restored in v1.0.

## Alternatives considered

- `libarchive` bindings: one API for everything, but a system `-dev` dependency, C parsing of hostile
  input, and weaker password/encrypted-header control.
- Spawning `7z`/`unzip`/`tar`: no streaming progress, password via argv/stdin, per-distro differences.
- A per-entry `open(id) -> Read` API: simpler, but quadratic on solid archives.
- Rejecting archives that contain any unsafe entry: safer-looking, but a single odd entry would make
  a whole backup unreadable; flagging and skipping keeps the rest usable.

## Consequences

- T6.1/T6.2/T6.3 only touch their own files (`archive/**`, `extract.rs`, `panel/archive.rs`).
- `ofm-jobs` gained an `ofm-vfs` dependency and `JobError::Archive`; `ops/extract.rs` moved to
  `extract.rs`.
- Mid-archive corruption surfaces during extraction (a `Corrupt` error for that entry through the
  normal retry/skip/abort question), not at open, for formats without a central directory.
- Archive creation, nesting and write-back are v1.1.

## T6.2 implementation notes (extract job, 2026-11-22)

`ofm-jobs/src/extract.rs` (+ `extract/{safe,dirs,write}.rs`) implements the protocol above. Where it
refines or tightens the text, the code wins and is listed here (tests: `crates/ofm-jobs/tests/extract_*.rs`):

- **Seam for tests.** `extract::extract_from_reader(ctx, reader, entries, dest, opts)` is `run_extract`
  on an already opened `ArchiveReader`; the job tests drive it with an in-memory reader
  (`tests/extract_support/`) and with real archives (`extract_real.rs`, skipped while a backend is a stub).
- **The index flags are not trusted.** Every visited entry is re-validated from its raw bytes
  (relative, no `..`, no `.`/empty component, no NUL, name <= NAME_MAX); an entry that is `Unsafe`, was
  not selected, or fails the check is skipped with a warning (the warning's path is the archive, the
  hostile name only appears in the message). Every parent directory below the destination is created by
  the job or `lstat`ed through `copy::resolve_target`; a symlink or file in the way is a `TypeMismatch`
  conflict (skip / rename new / rename existing, never overwrite, never followed). A skipped directory
  is remembered, so its subtree is skipped with one question.
- **Symlinks** are stricter than the index: the target must be relative and may use `..` only as a
  leading run that stays inside the destination (`a/../b` is refused): without a `..` after a name no
  later link can climb out "through" a link that points at an ancestor.
- **Hard links** are created only to a file this job extracted (checked by `(dev, ino)` again before
  linking), otherwise skipped with a warning (stricter than "else a copy of its data"). Only where the
  file system has no hard links (`EPERM`/`EXDEV`/...) the link's stream (= its target's data) is written
  as a copy.
- **Selection.** Each selected item lands directly in the destination (`a/b/c` with selection `a/b`
  becomes `b/c`); with nested selections the outermost wins; empty = everything.
- **Limits are enforced on bytes actually written** (hard failure, not a warning): more than the
  declared size of the entry, ratio > `max_ratio` per entry (zip, when `packed_size` is known) or over
  the whole archive file (solid formats), total > `warn_total_bytes`. A declared total over
  `warn_total_bytes` fails before anything is written; the UI lifts the limit in `opts.limits` after the
  user confirmed. Free space is checked once (`statvfs` on the destination, warning only; `Fs` has no
  such call).
- **Error policy.** A corrupt/truncated entry is a warning (its temp file is removed) and the job
  continues; NeedsPassword/WrongPassword/limits/archive I/O, a failing destination (ENOSPC, EIO, EROFS,
  EDQUOT, EMFILE/ENFILE) and cancel stop it. The stream is read once, so a destination write error cannot
  be retried: it fails the job instead of asking. Other non-fatal errors on one entry (EACCES, ...) are
  a warning; 50 in a row stop the job. Directory mode/mtime are applied at the end, deepest first, also
  after failure or cancel (not after a panic: directories then keep mode 0700).
- **Not done in v1.0:** ACL/xattr inheritance handling (`copy/meta.rs` does it for copy), restoring
  ownership, extracting onto a case-insensitive file system beyond what `copy::Published` covers.
- **Anchored writes (FIX-M6M7 #5).** After the destination exists, every call below it goes through
  `fs::anchored::AnchoredFs` (ADR-0004 "Anchored access"): replacing a checked parent with a symlink
  during the job cannot redirect anything. Directory metadata is applied to the directory that was
  filled while its handle is cached, else to what is found there without following a symlink.

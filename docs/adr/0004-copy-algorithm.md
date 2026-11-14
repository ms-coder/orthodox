# ADR-0004: Copy/move/delete algorithm and the `Fs` seam

Status: accepted · 2026-11-14

## Context

Invariant 3: no data loss. A failed or cancelled copy must never leave a truncated file under the final
name; a move must never delete a source before the destination is complete and synced. The engine
must be testable with injected faults, and must be fast on modern file systems (F17).

## Decision

All file system calls in `ofm-jobs` go through `trait Fs` (`RealFs` + a test `FaultyFs` driven by
`ofm_testkit::FaultPlan`: ENOSPC, EIO, EACCES, short writes at the N-th call).

Per-file copy:

1. Open source `O_RDONLY|O_NOFOLLOW|O_CLOEXEC`, `fstat`.
2. Create `dest_dir/.<name>.ofm-part-<random>` with `O_WRONLY|O_CREAT|O_EXCL`, mode 0600.
3. Data: `ioctl(FICLONE)` -> else `copy_file_range` loop -> else read/write with a 1 MiB buffer. Sparse
   sources (`blocks*512 < size`) are copied with `SEEK_DATA`/`SEEK_HOLE`, trailing hole via `set_len`.
   `CancelToken` is checked every 1 MiB. Never set `FS_NOCOW` or attributes that defeat Btrfs compression.
4. Metadata on the open temp file: `fchmod`, `futimens` (ns), xattrs (`user.*` always; `security.*`/
   `trusted.*` only when permitted; failures are warnings), `fchown` only as root.
5. Cross-device move, or `opts.sync`: `fsync` the file, then `fsync` the directory after the rename.
6. Publish: `renameat2(RENAME_NOREPLACE)`; if the user chose overwrite, plain `rename` (atomic
   replace). An existing destination is never truncated in place.
7. On error/cancel: unlink the temp file. The final name is never half-written. Leftover
   `*.ofm-part-*` from a crash are ignored by listings of the job and may be cleaned on next run.

Directories: create with mode 0700 while copying, recurse, set final mode and times post-order.
Symlinks: recreated (`readlink` + `symlink`) unless `follow_symlinks`. FIFOs/devices/sockets: skipped
with a warning. Hardlinks inside the set: copied as separate files (documented).

Move: same `st_dev` -> `renameat2(RENAME_NOREPLACE)` per top-level item. Otherwise copy (with fsync),
verify size and mtime, then delete each source item only after its copy succeeded; partial failures
leave sources intact.

Delete: trash via the `trash` crate (freedesktop incl. per-mount `.Trash-$UID`); if unavailable on
that mount, ask (`Question::TrashUnavailable`) before deleting permanently. Permanent delete is
depth-first with `unlink`/`rmdir`, never follows symlinks, per-entry errors offer retry/skip/abort.

Jobs have two phases: plan (count files/bytes, detect dest-inside-source and same file by dev+ino)
then execute. Conflicts are asked lazily via `JobEvent::Ask`; "apply to all" is remembered per job.
Worker panics are caught and reported as `Outcome::Failed`.

## Alternatives considered

- `std::fs::copy`: truncates/overwrites in place, no cancel, no sparse/reflink control.
- Rename-based overwrite only after full copy but without a temp name in the same dir: cannot be atomic across dirs.
- `rsync`/`cp` subprocesses: no progress/conflict control, platform dependent.

## Consequences

- Extra directory entries (`.ofm-part-*`) briefly visible; file watchers may report them.
- `Fs` is a wide trait (about 25 methods); `FaultyFs` must delegate faithfully.
- Torture suite in `ofm-jobs` is mandatory for every change here (ARCHITECTURE §5).

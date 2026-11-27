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

## T3.6 review fixes (2026-11-15)

Findings of the T3.5 review, fixed in T3.6a (regression tests: `crates/ofm-jobs/tests/t36a_*.rs`):

- **Own output is never overwritten (B1).** Two sources with the same destination name (rename
  mask `*.bak` on `a.txt` + `a.md`, same name from two folders of a list panel) fail the job at plan
  time with `JobError::Invalid`. Collisions only the destination creates (case-insensitive FAT/exFAT:
  `IMG.JPG` vs `img.jpg`, also `D/` vs `d/`) are caught per item: the job records (dev, ino) of
  everything it published (files, symlinks, created dirs, same-device renames); a conflict with one
  of them is asked as `ConflictKind::SameFile` (no overwrite answer, not even a silent overwrite
  policy; "Rename existing" acts like "Rename new"; the dialog defaults to "Rename new"). A move
  unlinks a source only after its *own* destination is complete, so a refused or renamed collision
  keeps that source.
- **"Rename, never replace" without `RENAME_NOREPLACE` (M3).** NFS and many FUSE file systems
  (ntfs-3g, sshfs, gvfs) answer `EINVAL`/`ENOSYS`/`EOPNOTSUPP`. Then `link(from, to)` + `unlink(from)`
  is used (atomic, never replaces; `EEXIST` is a conflict as before; a failing `unlink` undoes the
  link). For directories, and where hard links are unavailable too (FAT: `EPERM`), the last resort is
  "`lstat` says `to` is absent, then plain `rename`". **Known race:** a name another program creates
  in the microseconds between that `lstat` and the `rename` would be replaced (for a directory only
  an empty one). Accepted: it needs a concurrent writer picking exactly that name on a file system
  that offers no atomic alternative; the job itself never creates a name it did not check.
- **Checked close (M4).** The temp file is closed with `FsFile::close` (close(2) result checked)
  before it is published: NFS/FUSE may report a deferred write error only there, and a cross-device
  move must see it before it unlinks the source.
- **Overwrite is always durable (M5).** When the user chose overwrite, the new file is fsynced
  before `rename_replace` and the directory after it, whatever `sync` says.
- **Smaller fixes.** "Rename existing" refuses (warning + skip) when the existing directory contains
  the source (m3). Sources are opened with `O_NONBLOCK` (cleared after `fstat`), FIFOs/sockets/devices
  are refused, so a FIFO swapped in after `lstat` cannot block the worker (m5). FICLONE /
  `copy_file_range` fall back to the next tier also on `EPERM`, `ETXTBSY` and on `EIO` before the
  tier copied a byte, like coreutils (m6).
- Still open (M9): openat/unlinkat-based delete/chmod (m1), cleanup of stale `.ofm-part-*` (m7).

## Anchored access (FIX-M6M7 #5, 2026-11-27)

A checked parent directory could be replaced with a symlink before the next path-based call, which
then wrote outside the destination. Additive change to the seam: `Fs::pin(path, follow) -> FsPin`
(an `O_PATH` handle, `O_NOFOLLOW` unless `follow`; default `Unsupported`; faultable as
`FaultOp::Pin`). `fs/anchored.rs` builds `AnchoredFs` on it: an `Fs` that opens every directory below
one root component by component without following symlinks and performs each call on
`/proc/self/fd/<handle>/<name>`. The root itself is opened once, following symlinks. Handles are an
LRU of 32; the parent of the item in progress is held. Only extraction uses it (ADR-0008); copy and
move are unchanged. Delete/chmod (m1) can reuse it. Needs `/proc` (checked: the job fails otherwise).

Checks that go with it: final directory mode/mtime are applied only if the directory still has the
`(dev, ino)` recorded when the job created it (else a warning; on vfat, whose inode numbers are not
stable, a false mismatch skips metadata and nothing else). A hard link is published only if the new
link has the identity of the file the job extracted. Stale handles are dropped ignoring case (vfat).

Remaining windows. None of them follows a symlink below the pinned root.

- **Destination path** (needs write access in an ANCESTOR of the destination): the path is resolved
  by name, following symlinks, until it is pinned. That includes a final component that
  `ensure_dir` has just created: replaced by a symlink before the pin, it is followed.
- **Needs write access inside the destination:**
  - a directory swapped between its `mkdir` or merge probe and its first pin is adopted (a real
    directory only; a symlink is refused). Swapped before the identity is recorded after `mkdir`,
    it also receives the final mode/mtime;
  - a pinned directory that is moved elsewhere still receives the output and its metadata;
  - a handle dropped from the cache is reopened by name, so another real directory put there
    meanwhile receives later files. This covers the final chmod/mtime too, which the identity check
    skips unless the substitute reuses the old inode number;
  - a name inside a pinned directory can be replaced between a check and the call relying on it:
    between the conflict probe and publish (still no-replace unless the user chose overwrite), and
    the temp name between its check and the rename. The hard link source may be replaced at any
    time, also while a question is open; that is detected on the link itself before publishing.

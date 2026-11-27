# M6/M7 security review (Kimi k3.1-pro, read-only, 2026-11-25)

Verdict BLOCK. (R)=reasoned from code, not reproduced. Lead verified #2, #4, #7 in code (confirmed).

**BLOCK** — no archive-only zip-slip succeeded, but resource-limit bypasses and F24/F26 correctness failures require fixes. Findings are **(R)**; custom `/tmp` PoCs were blocked by session permissions.

1. **High (R) — F3 can fill `/tmp`.** `crates/ofm-app/src/panel/archive_extract.rs:315-335` streams without `ArchiveLimits`; `view_file` passes `limit=None` at `:431-436`. Viewing a highly compressed multi-GB entry writes until ENOSPC.
2. **High (R) — separator-free panelize output grows memory forever.** `crates/ofm-core/src/panelize.rs:356-370`: once `discarding=true`, chunks without a delimiter no longer clear `carry`. `while :; do printf x; done` defeats the 4096-byte cap; the stdout channel is also unbounded (`:185`).
3. **High (R) — ordinary encrypted 7z never prompts.** `crates/ofm-vfs/src/archive/backend/sevenz.rs:148-175` retries only header-opening errors. `7z a -psecret` without encrypted headers opens with an empty password, then streaming fails at `:306-340` without consulting the provider.
4. **High (R) — normal archive searches miss entries.** `crates/ofm-search/src/engine.rs:361-380` requires the outer archive to pass name/size/date filters before opening it. A `*.txt` search never opens `data.zip`.
5. **High (R) — checked destination parents remain symlink-raceable.** `crates/ofm-jobs/src/extract/dirs.rs:48-50` caches pathnames; `extract/write.rs:233-235` and `fs/real.rs:248-252` later use full-path `openat(CWD, …)`. A local attacker replacing checked `dest/d` with a symlink redirects temp creation and rename outside. This is an external race, not archive-only traversal.
6. **Medium (R) — codec limits are applied after an uncapped probe.** `crates/ofm-vfs/src/archive/format.rs:117-140` constructs xz/zstd decoders without `ArchiveLimits` or `catch_panic`; detection precedes the capped tar constructor. A large-dictionary container can exceed the configured cap before the expected `LimitExceeded`.
7. **Medium (R) — F3/F4 reject archive search hits, contrary to F24.** `crates/ofm-app/src/search/rows.rs:130-135` requires `real_path`; `search/actions.rs:44-47` and `:73-76` therefore reject every `Location::Archive` hit.
8. **Medium (R) — entry limits do not bound indexing memory.** Zip collects all metadata before enforcing `max_entries` (`backend/zip.rs:75-117`); synthetic directories bypass the cap (`archive/index.rs:279-293`). A 2M-entry zip is fully collected before the 500k error; 500k deep paths can synthesize far more entries.
9. **Medium (R) — extraction selection is quadratic.** `crates/ofm-vfs/src/archive/index.rs:422-430` scans every selection for every entry. Selecting 100k flat entries means roughly 10¹⁰ prefix checks; the UI supplies every selected name at `panel/archive_extract.rs:64-68`.
10. **Medium (R) — a 7z root entry shifts IDs.** `backend/sevenz.rs:114-121` omits `./` from index IDs, but visitation uses the original file position at `:321`. Root + two equal-size files can associate streams with the wrong names.
11. **Low (R) — 7z symlinks become regular files.** `backend/sevenz.rs:203-209` masks off file-type bits before `sevenz_kind` tests them at `:186-200`; a symlink is extracted as a file containing its target text.

**Checked and fine**
- **(V)** 132 existing tests passed across extraction, search, panelize, list mapping, and own-output collision suites; two performance tests were ignored. The 7z real-archive test self-skips because no 7z tool is installed.
- Zip-slip normalization, duplicate last-wins selection, archive-created symlink-prefix blocking, special-file skipping, mode masking, temp+rename publication, conflict handling, cancellation, and ENOSPC cleanup are sound in the tested non-racing flow.
- F3 temp directories are unique `0700`, files use `create_new`/`0600`, and entry names cannot traverse the temp path.
- Password dialogs poll cancellation, closing drops the responder, retries are bounded, and passwords are not logged or persisted.
- Content files use `O_NONBLOCK` plus `fstat`; regex matching is linear; symlink loops and result-channel back-pressure are handled.
- Archive write commands/drops are refused; panelized operations map to real paths; same-name self-overwrite protection tests pass.

## Re-review of part 1 (merge 6d59cac; Sonnet reviewer, 2026-11-27): BLOCK
#2 and #10 fixed. #4 fixed with a side effect. #3 not fully fixed. #11 detection fixed but causes a regression. (V) = reproduced, (R) = reasoned.

- R1 HIGH (V, #3): wrong password never re-prompts. `ofm-vfs/src/archive/backend/sevenz.rs:380,395` sets `delivered=true` before `f` reads, and a wrong password only fails on read: one typo = one prompt, then Io/Checksum error (with COPY coder the visitor first gets garbage). (R) A cached PasswordSource entry that no longer fits is never forgotten. Fix: verify the password (drain first needed encrypted block, CRC ok) with bounded re-prompts before calling `f`.
- R2 HIGH (V, regression from #11): `sevenz.rs:91-111` symlink target pre-pass runs with the empty password; a plain-header encrypted 7z with one symlink fails `open()` with NeedsPassword (provider never asked), cannot be listed. Fix: only decode unencrypted blocks, or leave target None on password errors.
- R3 MEDIUM (V, #3): mixed plain + encrypted blocks: `sevenz.rs:395` no prompt after the plain entry sets `delivered`; extract-all never prompts. Same fix as R1; also stop after the last wanted entry.
- R4 MEDIUM (R): `sevenz.rs:95-109` pre-pass has no cancel check (large solid 7z with a symlink decompresses fully, uncancellable). Check cancel, stop after last symlink.
- R5 MEDIUM-LOW (R, #4): `ofm-search/src/engine.rs:374`, `archives.rs:45` every file is probed: unreadable non-matching files emit "archive skipped". Stay silent on probe I/O errors when the file does not pass filters.
- R6 LOW (R, #11): `sevenz.rs:102` with encrypted headers, symlink targets are never read even with the right password -> skipped as unsafe.
- R7 LOW (R, older): `ofm-search/src/archives.rs:90-96` content query still reports matching directory entries inside archives.
- Test gaps: wrong-then-right password, mixed archive, end-to-end symlink 7z; encrypted fixture lists coders [LZMA2, AES] but real 7-Zip order appears to be [AES, LZMA2]; `passes_filters` guard, size/mtime filters inside archives, stdout channel bound.
- Nit: `reopen_for_streaming` parses headers outside `catch_panic` (`sevenz.rs:209`).

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

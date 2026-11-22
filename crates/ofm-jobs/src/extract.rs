//! Extract archive entries (M6, T6.2; ADR-0008). Frozen request types: `spec::ExtractOpts` and
//! `JobSpec::Extract`; the outcome is the ordinary `Outcome::Ok(Report)` (`Report::created` = the
//! top-level destination paths, warnings = skipped unsafe/special entries).
//!
//! Protocol (same as copy, ADR-0004; read ADR-0008 "Extraction"):
//! 1. open the archive with `ofm_vfs::archive::open_archive` (password via `opts.password`),
//!    `index.select(entries)` -> ids; unsafe entries are never selected (warn with their count);
//! 2. one sequential `ArchiveReader::visit` pass in archive order; every entry is written through
//!    `ctx.fs()`: files to `.<name>.ofm-part-<random>` (O_EXCL, 0600), stream copied with
//!    `ctx.checkpoint()` every 1 MiB, size verified against the index, mode (`& 0o777`, never
//!    setuid/setgid/sticky, never uid/gid) and mtime set, then `renameat2(NOREPLACE)`; a conflict
//!    asks via `ctx.resolve_conflict` exactly like copy; failure/cancel unlinks the temp file;
//! 3. directories are created 0700 and get their final mode/mtime after the pass (post-order);
//! 4. symlinks are recreated only if the index says safe; no path component below `dest_dir` is
//!    ever followed through a symlink (an existing symlink in the way is a `TypeMismatch`
//!    conflict); devices/fifos/sockets are skipped with a warning.

use std::path::{Path, PathBuf};

use crate::ctx::JobCtx;
use crate::error::JobError;
use crate::spec::ExtractOpts;

/// Extract `entries` of `archive` into `dest_dir`. T6.2: stub.
pub fn run_extract(
    _ctx: &mut JobCtx,
    _archive: &Path,
    _entries: &[PathBuf],
    _dest_dir: &Path,
    _opts: &ExtractOpts,
) -> Result<(), JobError> {
    Err(JobError::Unsupported("archive extraction (T6.2)".into()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ctx::ScriptedSink;
    use crate::event::Outcome;
    use crate::fs::RealFs;
    use crate::runner::run_sync;
    use crate::spec::JobSpec;

    #[test]
    fn stub_fails_cleanly_with_a_typed_error() {
        let (sink, _) = ScriptedSink::refusing();
        let spec = JobSpec::Extract {
            archive: "/a.zip".into(),
            entries: vec![],
            dest_dir: "/dst".into(),
            opts: ExtractOpts::default(),
        };
        assert_eq!(spec.title(), "Extract 'a.zip' to /dst");
        let out = run_sync(&spec, Arc::new(RealFs), sink);
        assert!(
            matches!(&out, Outcome::Failed { error, .. } if error.contains("not supported")),
            "{out:?}"
        );
    }
}

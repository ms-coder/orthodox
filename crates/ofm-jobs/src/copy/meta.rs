//! Metadata of copies (ADR-0004 step 4): owner (root only), xattrs (`user.*` always,
//! `security.*`/`trusted.*` only as root), mode, ns timestamps. Failures are warnings, never
//! errors: the data is complete either way, and a file that keeps its 0600 temp mode is safe.
//! Warnings are deduplicated per (kind, errno) per job.
//!
//! Order on a file: owner -> xattrs (while the temp is still 0600 and writable) -> mode (after
//! chown, which may clear set-id bits) -> times (last, nothing touches mtime afterwards).
//! Directories: xattrs right after `mkdir`, owner/mode/times post-order (`Copier::dir_meta`).

use std::ffi::OsStr;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::ctx::JobCtx;
use crate::fs::{Fs, FsFile, FsStat};

use super::{Copier, Frame};

/// `EOPNOTSUPP`/`ENOTSUP` (same value on Linux).
const EOPNOTSUPP: i32 = 95;

/// Set-user-ID and set-group-ID bits.
const SET_ID_BITS: u32 = 0o6000;

fn unsupported(e: &io::Error) -> bool {
    e.raw_os_error() == Some(EOPNOTSUPP) || e.kind() == io::ErrorKind::Unsupported
}

impl Copier {
    /// Report a metadata problem once per (what, errno) for the job; repeats go to the log.
    pub(super) fn meta_warn(
        &mut self,
        ctx: &mut JobCtx,
        path: &Path,
        what: &'static str,
        e: &io::Error,
    ) {
        if self.warned.insert((what, e.raw_os_error())) {
            ctx.warn(
                path,
                format!("{what}: {e} (further similar warnings are not repeated)"),
            );
        } else {
            tracing::debug!(path = %path.display(), error = %e, what, "metadata not preserved");
        }
    }

    /// Whether an xattr namespace is copied (`user.*` always; `security.*`/`trusted.*` only as
    /// root; `system.*` (ACLs) is not copied in v1.0).
    fn xattr_wanted(&self, name: &OsStr) -> bool {
        let b = name.as_bytes();
        b.starts_with(b"user.")
            || (self.is_root && (b.starts_with(b"security.") || b.starts_with(b"trusted.")))
    }

    /// Copy the wanted xattrs of `src` onto `dst` (by path, never following `dst`).
    pub(super) fn copy_xattrs(&mut self, ctx: &mut JobCtx, fs: &dyn Fs, src: &Path, dst: &Path) {
        let follow = self.follow;
        let names = match fs.list_xattrs(src, follow) {
            Ok(n) => n,
            Err(e) if unsupported(&e) => return,
            Err(e) => {
                self.meta_warn(ctx, src, "extended attributes not read", &e);
                return;
            }
        };
        let wanted: Vec<_> = names.iter().filter(|n| self.xattr_wanted(n)).collect();
        for name in wanted {
            let value = match fs.get_xattr(src, name, follow) {
                Ok(Some(v)) => v,
                Ok(None) => continue,
                Err(e) => {
                    self.meta_warn(ctx, src, "extended attribute not read", &e);
                    continue;
                }
            };
            if let Err(e) = fs.set_xattr(dst, name, &value, false) {
                self.meta_warn(ctx, dst, "extended attributes not preserved", &e);
            }
        }
    }

    /// Metadata of a regular file onto its open temp file. Returns whether the timestamps were
    /// set (the move verification only compares mtimes that could be preserved).
    pub(super) fn file_meta(
        &mut self,
        ctx: &mut JobCtx,
        fs: &dyn Fs,
        src: &Path,
        tmp: &Path,
        df: &mut dyn FsFile,
        st: &FsStat,
    ) -> bool {
        if self.is_root
            && let Err(e) = df.set_owner(Some(st.uid), Some(st.gid))
        {
            self.meta_warn(ctx, src, "owner not preserved", &e);
        }
        self.copy_xattrs(ctx, fs, src, tmp);
        let mut perm = st.perm();
        // Like `cp -p`: set-id bits survive only together with the original owner and group.
        if perm & SET_ID_BITS != 0 {
            match df.stat() {
                Ok(t) if t.uid == st.uid && t.gid == st.gid => {}
                _ => perm &= !SET_ID_BITS,
            }
        }
        if let Err(e) = df.set_mode(perm) {
            self.meta_warn(ctx, src, "permissions not preserved", &e);
        }
        match df.set_times(st.atime_ns, st.mtime_ns) {
            Ok(()) => true,
            Err(e) => {
                self.meta_warn(ctx, src, "timestamps not preserved", &e);
                false
            }
        }
    }

    /// Final metadata of a directory this job created (post-order, also on cancel).
    pub(super) fn dir_meta(&mut self, ctx: &mut JobCtx, f: &Frame) {
        let fs = ctx.fs();
        if self.is_root
            && let Err(e) = fs.set_owner(&f.dest, Some(f.st.uid), Some(f.st.gid), false)
        {
            self.meta_warn(ctx, &f.dest, "owner not preserved", &e);
        }
        if let Err(e) = fs.set_mode(&f.dest, f.st.perm()) {
            self.meta_warn(ctx, &f.dest, "permissions not preserved", &e);
        }
        if let Err(e) = fs.set_times(&f.dest, f.st.atime_ns, f.st.mtime_ns, false) {
            self.meta_warn(ctx, &f.dest, "timestamps not preserved", &e);
        }
        if self.sync
            && let Err(e) = fs.sync_dir(&f.dest)
        {
            self.meta_warn(ctx, &f.dest, "directory not synced", &e);
        }
    }
}

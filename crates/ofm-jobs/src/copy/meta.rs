//! Metadata of copies (ADR-0004 step 4): owner (root only), xattrs (`user.*` always,
//! `security.*`/`trusted.*` only as root), POSIX ACLs, mode, ns timestamps. Failures are
//! warnings, never errors: the data is complete either way, and a file that keeps its 0600 temp
//! mode is safe. Warnings are deduplicated per (kind, errno) per job.
//!
//! Order on a file: owner -> xattrs and ACL (while the temp is still 0600 and writable) -> mode
//! (after chown, which may clear set-id bits, and after the ACL: `chmod` maps the group bits onto
//! the ACL mask, so setting the mode LAST leaves mode, mask and entries exactly like the source)
//! -> times (last, nothing touches mtime afterwards). Directories: xattrs, access ACL and the
//! default ACL right after `mkdir` (children created afterwards inherit the default ACL),
//! owner/mode/times post-order (`Copier::dir_meta`).
//!
//! Permissions are never widened (SPEC F17, review M1): while a source carries an access ACL,
//! the group-class bits of its `st_mode` show the ACL MASK, not the owning group's own
//! permissions. If the ACL is preserved, the plain mode is copied and ends up identical. If it is
//! NOT preserved (file system without ACLs, not the owner of the copy, ...), the group/other
//! bits are rebuilt from the ACL's own `group::`/`other::` entries instead of the mask: a file
//! with mode 0600 plus an entry `u:nobody:rw` (stat 0660) never becomes plain `group::rw-`.

use std::ffi::{OsStr, OsString};
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

/// Group and other permission bits.
const GROUP_OTHER_BITS: u32 = 0o077;

/// Xattr holding the POSIX access ACL (`acl(5)`).
const ACL_ACCESS: &str = "system.posix_acl_access";
/// Xattr holding the POSIX default ACL (directories only).
const ACL_DEFAULT: &str = "system.posix_acl_default";

// POSIX ACL xattr value layout (linux/posix_acl_xattr.h): a 4-byte version header followed by
// 8-byte entries (tag: u16, perm: u16, id: u32), little-endian on every CPU. `id` is
// `0xffffffff` for the qualifier-less entries (u::, g::, m::, o::) and a uid/gid otherwise.
/// The only version the kernel accepts.
const ACL_VERSION: u32 = 0x0002;
/// Tag of the owning group's own entry (`group::`).
const ACL_GROUP_OBJ: u16 = 0x04;
/// Tag of the `other::` entry.
const ACL_OTHER: u16 = 0x20;

fn unsupported(e: &io::Error) -> bool {
    e.raw_os_error() == Some(EOPNOTSUPP) || e.kind() == io::ErrorKind::Unsupported
}

/// The `group::` and `other::` permission bits (0..=7 each) of a POSIX ACL xattr value, or `None`
/// if the value is malformed (entries are found by tag; their order does not matter).
fn acl_group_other(acl: &[u8]) -> Option<(u32, u32)> {
    fn le16(b: &[u8], at: usize) -> u16 {
        u16::from(b[at]) | (u16::from(b[at + 1]) << 8)
    }
    if acl.len() < 4 || !(acl.len() - 4).is_multiple_of(8) {
        return None;
    }
    let version = u32::from(acl[0])
        | (u32::from(acl[1]) << 8)
        | (u32::from(acl[2]) << 16)
        | (u32::from(acl[3]) << 24);
    if version != ACL_VERSION {
        return None;
    }
    let mut group = None;
    let mut other = None;
    let (entries, _) = acl[4..].as_chunks::<8>();
    for entry in entries {
        let (tag, perm) = (le16(entry, 0), le16(entry, 2));
        if perm > 7 {
            return None;
        }
        match tag {
            ACL_GROUP_OBJ => group = Some(u32::from(perm)),
            ACL_OTHER => other = Some(u32::from(perm)),
            _ => {}
        }
    }
    Some((group?, other?))
}

/// [`perm`](FsStat::perm) with the group/other bits replaced by the ACL's own `group::`/`other::`
/// entries (cleared when they are unknown): the fallback for a source whose access ACL could not
/// be preserved. User and set-id bits always pass through.
fn perm_without_mask(perm: u32, group_other: Option<(u32, u32)>) -> u32 {
    match group_other {
        Some((g, o)) => (perm & !GROUP_OTHER_BITS) | (g << 3) | o,
        None => perm & !GROUP_OTHER_BITS,
    }
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

    /// Like [`meta_warn`](Self::meta_warn) for a condition that is not one failing call: reported
    /// once per job (key without errno), repeats go to the log.
    fn warn_once(&mut self, ctx: &mut JobCtx, path: &Path, what: &'static str) {
        if self.warned.insert((what, None)) {
            ctx.warn(
                path,
                format!("{what} (further similar warnings are not repeated)"),
            );
        } else {
            tracing::debug!(path = %path.display(), what, "metadata not preserved");
        }
    }

    /// Whether an xattr namespace is copied (`user.*` always; `security.*`/`trusted.*` only as
    /// root). `system.*` (the POSIX ACLs) is handled separately by [`copy_acls`](Self::copy_acls),
    /// which must run before the mode is set.
    fn xattr_wanted(&self, name: &OsStr) -> bool {
        let b = name.as_bytes();
        b.starts_with(b"user.")
            || (self.is_root && (b.starts_with(b"security.") || b.starts_with(b"trusted.")))
    }

    /// Copy the wanted xattrs of `src` onto `dst` (by path, never following `dst`), then the
    /// POSIX ACLs ([`copy_acls`](Self::copy_acls)).
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
        self.copy_acls(ctx, fs, src, &names, dst);
    }

    /// Copy the POSIX ACLs of `src` onto `dst`: the access ACL, and the default ACL when the
    /// source has one (only directories do, so a default ACL is never set on a regular file).
    /// ACL xattrs are ordinary xattr names and go through the same `Fs` calls as `user.*` values.
    /// They are set BEFORE the mode (see the module doc): installing an access ACL writes the
    /// mask into the group-class bits, and the `chmod` that follows maps the source's group bits
    /// back into the mask, leaving mode, mask and entries exactly like the source's.
    fn copy_acls(
        &mut self,
        ctx: &mut JobCtx,
        fs: &dyn Fs,
        src: &Path,
        names: &[OsString],
        dst: &Path,
    ) {
        let follow = self.follow;
        for raw in [ACL_ACCESS, ACL_DEFAULT] {
            let name = OsStr::new(raw);
            if !names.iter().any(|n| n == name) {
                continue;
            }
            let what = if raw == ACL_ACCESS {
                "access control list not preserved"
            } else {
                "default access control list not preserved"
            };
            match fs.get_xattr(src, name, follow) {
                Ok(Some(value)) => {
                    if let Err(e) = fs.set_xattr(dst, name, &value, false) {
                        self.meta_warn(ctx, dst, what, &e);
                    }
                }
                Ok(None) => {}
                Err(e) => self.meta_warn(ctx, src, "access control list not read", &e),
            }
        }
    }

    /// The permission bits to give a copy of `src` (at `dst`): while the source has no access ACL,
    /// `perm` (its own mode) is the truth. With an access ACL the group-class bits of `st_mode`
    /// hold the ACL MASK, not the owning group's own permissions, so:
    ///
    /// * ACL preserved on `dst` — `perm` passes through; the `chmod` after the ACL writes the
    ///   group bits into the mask, which reproduces the source exactly.
    /// * ACL not preserved — copying the mask as plain group bits could grant the owning group
    ///   access the source never gave it (mode 0600 + `u:nobody:rw` stats as 0660). The
    ///   group/other bits are rebuilt from the ACL's `group::`/`other::` entries, or cleared
    ///   when the ACL cannot be read or parsed: access is never widened (SPEC F17, review M1).
    fn acl_safe_perm(
        &mut self,
        ctx: &mut JobCtx,
        fs: &dyn Fs,
        src: &Path,
        dst: &Path,
        perm: u32,
    ) -> u32 {
        let acl = match fs.get_xattr(src, OsStr::new(ACL_ACCESS), self.follow) {
            Ok(Some(acl)) => Some(acl),
            // No access ACL, or a file system without ACL support (`EOPNOTSUPP`): the plain
            // permission bits are the real permissions.
            Ok(None) => return perm,
            Err(e) if unsupported(&e) => return perm,
            // There may be an ACL that cannot be read: conservative fallback below.
            Err(_) => None,
        };
        if matches!(
            fs.get_xattr(dst, OsStr::new(ACL_ACCESS), false),
            Ok(Some(_))
        ) {
            // The copy carries the ACL (set by `copy_acls` before this mode).
            return perm;
        }
        let entries = acl.as_deref().and_then(acl_group_other);
        let what = if entries.is_some() {
            "access control list not preserved: group/other permissions reduced to the source's own group and other entries"
        } else {
            "access control list not preserved: group/other permissions cleared"
        };
        self.warn_once(ctx, dst, what);
        perm_without_mask(perm, entries)
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
        // Last, and only after the ACL was set: never widen group/other access when the source's
        // ACL could not be preserved.
        perm = self.acl_safe_perm(ctx, fs, src, tmp, perm);
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
        // The ACL was set when the directory was created; the mode goes on top of it.
        let perm = self.acl_safe_perm(ctx, &*fs, &f.src, &f.dest, f.st.perm());
        if let Err(e) = fs.set_mode(&f.dest, perm) {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// One ACL xattr entry: tag, perm (u16 each), id (u32), little-endian.
    fn entry(tag: u16, perm: u16, id: u32) -> [u8; 8] {
        let mut e = [0u8; 8];
        e[..2].copy_from_slice(&tag.to_le_bytes());
        e[2..4].copy_from_slice(&perm.to_le_bytes());
        e[4..8].copy_from_slice(&id.to_le_bytes());
        e
    }

    /// The qualifier-less entries use this id.
    const UNDEF: u32 = 0xffff_ffff;

    /// An ACL xattr value: version header plus entries.
    fn acl(entries: &[[u8; 8]]) -> Vec<u8> {
        let mut v = ACL_VERSION.to_le_bytes().to_vec();
        for e in entries {
            v.extend_from_slice(e);
        }
        v
    }

    #[test]
    fn parses_group_and_other_entries() {
        // A blob exactly as setfacl writes it for `chmod 600` + `u:65534:rw` (review M1: stats
        // as 0660 — the mask — although the owning group has nothing).
        let blob = acl(&[
            entry(0x01, 0o6, UNDEF),
            entry(0x02, 0o6, 65534),
            entry(0x04, 0o0, UNDEF),
            entry(0x10, 0o6, UNDEF),
            entry(0x20, 0o0, UNDEF),
        ]);
        assert_eq!(acl_group_other(&blob), Some((0o0, 0o0)));

        // Minimal ACL: u::rw-, g::r--, o::---.
        let blob = acl(&[
            entry(0x01, 0o6, UNDEF),
            entry(0x04, 0o4, UNDEF),
            entry(0x20, 0o0, UNDEF),
        ]);
        assert_eq!(acl_group_other(&blob), Some((0o4, 0o0)));
    }

    #[test]
    fn rejects_malformed_acls() {
        // Wrong version.
        let mut bad = acl(&[entry(0x04, 0o4, UNDEF), entry(0x20, 0o0, UNDEF)]);
        bad[0] = 3;
        assert_eq!(acl_group_other(&bad), None);
        // Truncated / empty / not entry-aligned.
        let good = acl(&[entry(0x04, 0o4, UNDEF), entry(0x20, 0o0, UNDEF)]);
        assert_eq!(acl_group_other(&good[..good.len() - 1]), None);
        assert_eq!(acl_group_other(&[]), None);
        assert_eq!(acl_group_other(&[2, 0, 0, 0, 0]), None);
        // A 6-byte-entry blob (the pre-fix misunderstanding of the layout) must not parse: the
        // group:: chunk would swallow id bytes and hit perm > 7.
        assert_eq!(
            acl_group_other(&[
                2, 0, 0, 0, 1, 0, 6, 0, 0xff, 0xff, 4, 0, 4, 0, 0xff, 0xff, 0x20, 0, 4, 0, 0xff,
                0xff
            ]),
            None
        );
        // Permission bits beyond rwx.
        assert_eq!(
            acl_group_other(&acl(&[entry(0x04, 0o10, UNDEF), entry(0x20, 0o0, UNDEF)])),
            None
        );
        // Missing group:: or other:: entry.
        assert_eq!(acl_group_other(&acl(&[entry(0x04, 0o4, UNDEF)])), None);
        assert_eq!(acl_group_other(&acl(&[entry(0x20, 0o0, UNDEF)])), None);
    }

    #[test]
    fn fallback_perm_keeps_user_bits_and_never_widens() {
        // Mask bits (0660 from mode 0600 + u:nobody:rw) must not become group permissions.
        assert_eq!(perm_without_mask(0o660, Some((0o0, 0o0))), 0o600);
        assert_eq!(perm_without_mask(0o664, Some((0o4, 0o4))), 0o644);
        // Same g::/o:: as the mask: nothing changes.
        assert_eq!(perm_without_mask(0o640, Some((0o4, 0o0))), 0o640);
        // Unknown entries: group/other cleared, user and set-id bits kept.
        assert_eq!(perm_without_mask(0o4755, None), 0o4700);
    }
}

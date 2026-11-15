//! T3.7a: copies never inherit ACL entries from a DEFAULT ACL (review N1, SPEC F17). A temp
//! file created inside a directory with a default ACL — the copy of a source directory that
//! carries one, or a pre-existing destination directory — inherits an access ACL the source
//! never had (`setfacl -d -m u:nobody:rwx` on the parent leaks `user:nobody:rwx` into the copy,
//! effective even after the mode is set, because `chmod` only narrows the mask). A copied
//! subdirectory also inherits a default ACL of its own. The copy must end up with exactly the
//! source's permissions: no access-ACL xattr where the source has none, no default ACL where
//! the source directory has none, and the plain mode equal to the source's. Tests that need
//! real ACLs skip cleanly when no candidate temp file system supports them (probe like t36d).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t31_support;

use std::ffi::OsStr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ConflictAnswer, CopyOpts, ErrorAction, Fs, RealFs};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t31_support::*;

const ACL_ACCESS: &str = "system.posix_acl_access";
const ACL_DEFAULT: &str = "system.posix_acl_default";

// POSIX ACL xattr value layout (linux/posix_acl_xattr.h): version-2 header plus 8-byte entries
// (tag, perm: u16 LE; id: u32 LE, `0xffffffff` for qualifier-less).
const UNDEF: u32 = 0xffff_ffff;
const USER_OBJ: u16 = 0x01;
const USER: u16 = 0x02;
const GROUP_OBJ: u16 = 0x04;
const GROUP: u16 = 0x08;
const MASK: u16 = 0x10;
const OTHER: u16 = 0x20;

fn entry(tag: u16, perm: u16, id: u32) -> [u8; 8] {
    let mut e = [0u8; 8];
    e[..2].copy_from_slice(&tag.to_le_bytes());
    e[2..4].copy_from_slice(&perm.to_le_bytes());
    e[4..8].copy_from_slice(&id.to_le_bytes());
    e
}

fn acl_blob(entries: &[[u8; 8]]) -> Vec<u8> {
    let mut v = vec![2, 0, 0, 0]; // version 2
    for e in entries {
        v.extend_from_slice(e);
    }
    v
}

/// The default ACL of the review-N1 repro: `setfacl -d -m u:nobody:rwx,g::r-x D`, i.e.
/// `u::rwx,u:nobody:rwx,g::r-x,m::rwx,o::---`.
fn default_acl_blob() -> Vec<u8> {
    acl_blob(&[
        entry(USER_OBJ, 0o7, UNDEF),
        entry(USER, 0o7, 65534),
        entry(GROUP_OBJ, 0o5, UNDEF),
        entry(MASK, 0o7, UNDEF),
        entry(OTHER, 0o0, UNDEF),
    ])
}

/// A `TempDir` on a file system that stores POSIX ACLs (temp dir, then `/dev/shm`), probed by
/// setting a default ACL as a raw xattr — the same kernel path `setfacl -d` uses. `None` = no
/// ACL support anywhere: skip.
fn acl_temp() -> Option<TempDir> {
    for base in [std::env::temp_dir(), PathBuf::from("/dev/shm")] {
        let Ok(dir) = TempDir::in_dir(&base) else {
            continue;
        };
        let probe = dir.path().join("acl-probe-dir");
        if std::fs::create_dir(&probe).is_err() {
            continue;
        }
        if RealFs
            .set_xattr(&probe, OsStr::new(ACL_DEFAULT), &default_acl_blob(), false)
            .is_ok()
        {
            return Some(dir);
        }
    }
    None
}

/// Put the default ACL (`u:nobody:rwx`) on `dir` as a raw xattr; `false` if refused (skip).
fn set_default_acl(dir: &Path) -> bool {
    RealFs
        .set_xattr(dir, OsStr::new(ACL_DEFAULT), &default_acl_blob(), false)
        .is_ok()
}

fn acl_of(p: &Path, name: &str) -> Option<Vec<u8>> {
    RealFs.get_xattr(p, OsStr::new(name), false).unwrap()
}

fn mode_of(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().mode() & 0o7777
}

/// A file with mode 0640 and NO access ACL (created outside any default-ACL directory).
fn plain_0640(parent: &Path, name: &str) -> PathBuf {
    let f = parent.join(name);
    std::fs::write(&f, b"nobody must not read this\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o640)).unwrap();
    assert_eq!(
        acl_of(&f, ACL_ACCESS),
        None,
        "fixture file must start without ACL"
    );
    assert_eq!(mode_of(&f), 0o640);
    f
}

/// The review-N1 repro: the SOURCE directory's own default ACL leaks into its children's copies.
/// The temp file of `D/f`'s copy is created inside the already-copied `D` (which carries the
/// default ACL), so it inherits `user:nobody:rwx` although `D/f` has no ACL at all.
#[test]
fn copy_into_own_default_acl_dir_leaves_no_inherited_acl() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    let src = dir.path().join("D");
    std::fs::create_dir(&src).unwrap();
    let f = plain_0640(&src, "f");
    assert!(set_default_acl(&src), "probe passed but set failed");

    let out = TempDir::in_dir(dir.path()).unwrap();
    let (res, _) = run_quiet(&copy_spec(
        std::slice::from_ref(&src),
        out.path(),
        CopyOpts::default(),
    ));
    assert!(res.is_success(), "{res:?}");
    let copy_f = out.path().join("D").join("f");
    assert_eq!(
        acl_of(&copy_f, ACL_ACCESS),
        None,
        "copy inherited an access ACL from the directory's default ACL: {:?}",
        acl_of(&copy_f, ACL_ACCESS)
    );
    assert_eq!(mode_of(&copy_f), mode_of(&f));
    // The directory's own default ACL is legitimate and must survive.
    assert_eq!(
        acl_of(&out.path().join("D"), ACL_DEFAULT),
        acl_of(&src, ACL_DEFAULT)
    );
    assert_eq!(acl_of(&out.path().join("D"), ACL_ACCESS), None);
}

/// A pre-existing default ACL in the destination directory must not leak into copies either:
/// a plain file (temp file inherits an access ACL) and a plain subdirectory (inherits an access
/// AND a default ACL, so its own children inherit further).
#[test]
fn copy_into_dir_with_default_acl_leaves_no_inherited_acl() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    // A source area without any default ACL.
    let src = dir.path().join("plain");
    std::fs::create_dir(&src).unwrap();
    let f = plain_0640(&src, "f.txt");
    let sub = src.join("sub");
    std::fs::create_dir(&sub).unwrap();
    std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o750)).unwrap();
    assert_eq!(acl_of(&sub, ACL_DEFAULT), None);
    assert_eq!(acl_of(&sub, ACL_ACCESS), None);
    let inner = plain_0640(&sub, "inner.txt");

    // The destination carries the default ACL.
    let dst = TempDir::in_dir(dir.path()).unwrap();
    assert!(set_default_acl(dst.path()), "probe passed but set failed");

    let (res, _) = run_quiet(&copy_spec(
        &[f.clone(), sub],
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(res.is_success(), "{res:?}");

    let copy_f = dst.path().join("f.txt");
    assert_eq!(
        acl_of(&copy_f, ACL_ACCESS),
        None,
        "file copy inherited an access ACL: {:?}",
        acl_of(&copy_f, ACL_ACCESS)
    );
    assert_eq!(mode_of(&copy_f), mode_of(&f));

    let copy_sub = dst.path().join("sub");
    assert_eq!(
        acl_of(&copy_sub, ACL_ACCESS),
        None,
        "directory copy inherited an access ACL: {:?}",
        acl_of(&copy_sub, ACL_ACCESS)
    );
    assert_eq!(
        acl_of(&copy_sub, ACL_DEFAULT),
        None,
        "directory copy inherited a default ACL: {:?}",
        acl_of(&copy_sub, ACL_DEFAULT)
    );
    assert_eq!(mode_of(&copy_sub), 0o750);
    assert_eq!(acl_of(&copy_sub.join("inner.txt"), ACL_ACCESS), None);
    assert_eq!(mode_of(&copy_sub.join("inner.txt")), mode_of(&inner));
    // Nothing may inherit any more: a new file in the copied subdirectory stays plain.
    std::fs::write(copy_sub.join("new.txt"), b"new\n").unwrap();
    assert_eq!(acl_of(&copy_sub.join("new.txt"), ACL_ACCESS), None);
}

/// A source that DOES have an access ACL keeps it exactly, also when copying into a directory
/// with a default ACL (the source's ACL must replace the inherited one, not coexist with it).
/// This pins the preserve path while N1 removals are added (t36d covers it without inheritance).
#[test]
fn source_acl_replaces_inherited_acl_in_destination() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    // Mode 0640 + `g:65534:r--`: a real ACL (named entry + mask) the kernel keeps as an xattr.
    let blob = acl_blob(&[
        entry(USER_OBJ, 0o6, UNDEF),  // u::rw-
        entry(GROUP_OBJ, 0o0, UNDEF), // g::---
        entry(GROUP, 0o4, 65534),     // g:65534:r--
        entry(MASK, 0o4, UNDEF),      // m::r--
        entry(OTHER, 0o0, UNDEF),     // o::---
    ]);
    let f = plain_0640(dir.path(), "with-acl.txt");
    RealFs
        .set_xattr(&f, OsStr::new(ACL_ACCESS), &blob, false)
        .unwrap();
    assert_eq!(acl_of(&f, ACL_ACCESS), Some(blob.clone()));

    let dst = TempDir::in_dir(dir.path()).unwrap();
    assert!(set_default_acl(dst.path()));
    let (res, _) = run_quiet(&copy_spec(
        std::slice::from_ref(&f),
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(res.is_success(), "{res:?}");
    let copy = dst.path().join("with-acl.txt");
    assert_eq!(
        acl_of(&copy, ACL_ACCESS),
        Some(blob),
        "source ACL not copied exactly: {:?}",
        acl_of(&copy, ACL_ACCESS)
    );
    assert_eq!(mode_of(&copy), mode_of(&f));
    // The destination's own default ACL is untouched.
    assert_eq!(acl_of(dst.path(), ACL_DEFAULT), Some(default_acl_blob()));
}

/// When the inherited ACL cannot be REMOVED (`removexattr` refused here), the fallback writes
/// the minimal ACL of the source's mode: the kernel drops an xattr that carries nothing beyond
/// the mode, so the copy still ends with exactly the source's plain permissions.
#[test]
fn unremovable_inherited_acl_falls_back_to_the_minimal_acl() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    let f = plain_0640(dir.path(), "f.txt");
    let dst = TempDir::in_dir(dir.path()).unwrap();
    assert!(set_default_acl(dst.path()));

    let fs = Arc::new(FaultyFs::over_real(
        FaultPlan::new().fail_from(FaultOp::RemoveXattr, 1, FaultErrno::Raw(95)), // EOPNOTSUPP
    ));
    let (res, _) = run_policy(
        &copy_spec(std::slice::from_ref(&f), dst.path(), CopyOpts::default()),
        fs.clone(),
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    assert!(res.is_success(), "{res:?}");
    let copy = dst.path().join("f.txt");
    // Either the kernel dropped the xattr (expected on ext4/tmpfs) or it holds exactly the
    // minimal ACL `u::rw-,g::r--,o::---`; in both cases no inherited entry survives.
    match acl_of(&copy, ACL_ACCESS) {
        None => {}
        Some(blob) => {
            let minimal = acl_blob(&[
                entry(USER_OBJ, 0o6, UNDEF),
                entry(GROUP_OBJ, 0o4, UNDEF),
                entry(OTHER, 0o0, UNDEF),
            ]);
            assert_eq!(blob, minimal, "unexpected ACL left on the copy");
        }
    }
    assert_eq!(mode_of(&copy), 0o640);
    assert_eq!(fs.calls_of(FaultOp::RemoveXattr), 1);
}

/// Both layers refused (removal AND the minimal-ACL write): then the mode itself must not
/// widen access — group/other bits are cleared and a warning is reported, like the review-M1
/// fallback for an ACL that could not be set.
#[test]
fn inherited_acl_never_widens_when_all_removal_fails() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    let f = plain_0640(dir.path(), "f.txt");
    let dst = TempDir::in_dir(dir.path()).unwrap();
    assert!(set_default_acl(dst.path()));

    // The source is plain, so the engine's only set_xattr call is the minimal-ACL fallback.
    let fs = Arc::new(FaultyFs::over_real(
        FaultPlan::new()
            .fail_from(FaultOp::RemoveXattr, 1, FaultErrno::Raw(95))
            .fail_from(FaultOp::SetXattr, 1, FaultErrno::Raw(95)),
    ));
    let (res, _) = run_policy(
        &copy_spec(std::slice::from_ref(&f), dst.path(), CopyOpts::default()),
        fs,
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    assert!(res.is_success(), "{res:?}");
    let copy = dst.path().join("f.txt");
    // The inherited ACL stayed, so the group/other bits are cleared (mask 0): the
    // `user:nobody:rwx` entry is ineffective, nobody still cannot read the copy.
    assert!(acl_of(&copy, ACL_ACCESS).is_some());
    assert_eq!(mode_of(&copy), 0o600);
    let warnings = &report_of(&res).warnings;
    assert!(
        warnings.iter().any(|w| w
            .message
            .contains("inherited access control list not removed")),
        "{warnings:?}"
    );
}

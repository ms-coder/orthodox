//! T3.6d: POSIX ACLs survive copies (SPEC F17, review M1). `system.posix_acl_access` — and, on
//! directories, `system.posix_acl_default` — is copied BEFORE the mode is set, so mode and ACL
//! mask end up exactly like the source's. When the ACL cannot be set on the copy (file system
//! without ACLs, fault injected here via `FaultyFs`), the group/other bits fall back to the ACL's
//! own `group::`/`other::` entries instead of the mask: a private file (0600 + a named ACL entry,
//! stat 0660) never becomes plain `group::rw-`. Tests that need real ACLs skip cleanly when
//! `setfacl` is missing or no candidate temp file system (temp dir, `/dev/shm`) supports ACLs.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t31_support;

use std::ffi::OsStr;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ConflictAnswer, CopyOpts, ErrorAction, Fs, RealFs};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t31_support::*;

const ACL_ACCESS: &str = "system.posix_acl_access";
const ACL_DEFAULT: &str = "system.posix_acl_default";

/// A `TempDir` on a file system that stores POSIX ACLs, trying `std::env::temp_dir()` and then
/// `/dev/shm`. The probe sets a minimal access ACL as a raw xattr (the same kernel path setfacl
/// uses), so it needs no external tools. `None` = no ACL support anywhere: skip.
fn acl_temp() -> Option<TempDir> {
    // Version 2 + the three mandatory entries u::rw-, g::---, o::--- (each tag, perm: u16,
    // id: u32, little-endian): the smallest complete ACL the kernel accepts.
    const MINIMAL: &[u8] = &[
        2, 0, 0, 0, // version 2
        1, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // u::---
        4, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // g::---
        32, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // o::---
    ];
    for base in [std::env::temp_dir(), PathBuf::from("/dev/shm")] {
        let Ok(dir) = TempDir::in_dir(&base) else {
            continue;
        };
        let probe = dir.path().join("acl-probe");
        if std::fs::write(&probe, b"").is_err() {
            continue;
        }
        if RealFs
            .set_xattr(&probe, OsStr::new(ACL_ACCESS), MINIMAL, false)
            .is_ok()
        {
            return Some(dir);
        }
    }
    None
}

/// Run `setfacl` on `file`; `false` if it is not installed or refuses (then the test skips).
fn setfacl(args: &[&str], file: &Path) -> bool {
    Command::new("setfacl")
        .args(args)
        .arg(file)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Make the review-M1 file: mode 0600 plus `u:65534:rw`, so the stat mode shows 0660 (the ACL
/// mask) while the owning group itself has no permissions. Returns the temp root.
fn private_file_with_acl() -> Option<TempDir> {
    let dir = acl_temp()?;
    let f = dir.path().join("secret.txt");
    std::fs::write(&f, b"private\n").ok()?;
    set_permissions_0600(&f);
    if !setfacl(&["-m", "u:65534:rw"], &f) {
        return None;
    }
    Some(dir)
}

fn set_permissions_0600(p: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).unwrap();
}

fn mode_of(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().mode() & 0o7777
}

fn acl_of(p: &Path, name: &str) -> Option<Vec<u8>> {
    RealFs.get_xattr(p, OsStr::new(name), false).unwrap()
}

#[test]
fn copy_preserves_file_acl_and_mode() {
    let Some(dir) = private_file_with_acl() else {
        eprintln!("SKIP: no setfacl or no ACL support in the temp file systems");
        return;
    };
    let secret = dir.path().join("secret.txt");
    // The premise of review M1: the stat mode shows the mask, not the owning group.
    assert_eq!(
        mode_of(&secret),
        0o660,
        "setfacl did not widen the stat mode"
    );
    let dst = TempDir::in_dir(dir.path()).unwrap();
    let (out, _) = run_quiet(&copy_spec(
        std::slice::from_ref(&secret),
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    let copy = dst.path().join("secret.txt");
    assert_eq!(acl_of(&copy, ACL_ACCESS), acl_of(&secret, ACL_ACCESS));
    assert_eq!(mode_of(&copy), mode_of(&secret));
    // The copy's ACL keeps `group::---`: the owning group cannot read it (the mask 0660 is not
    // plain group permissions). Entry bytes: tag 0x04 (`group::`), perm 0, id `undefined` (u32).
    const GROUP_NONE: [u8; 8] = [0x04, 0, 0, 0, 0xff, 0xff, 0xff, 0xff];
    let blob = acl_of(&copy, ACL_ACCESS).unwrap_or_default();
    assert!(
        blob.windows(8).any(|e| e == GROUP_NONE),
        "copy has no group::--- entry: {blob:?}"
    );
    assert!(
        !report_of(&out)
            .warnings
            .iter()
            .any(|w| w.message.contains("access control list")),
        "{:?}",
        report_of(&out).warnings
    );
}

#[test]
fn copy_preserves_default_acl_on_directories() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    let src = dir.path().join("tree");
    std::fs::create_dir(&src).unwrap();
    if !setfacl(&["-m", "g:65534:rx"], &src) || !setfacl(&["-d", "-m", "g:65534:rx"], &src) {
        eprintln!("SKIP: setfacl not available");
        return;
    }
    std::fs::write(src.join("inner.txt"), b"inner\n").unwrap();
    // inner.txt was created after the default ACL was set, so it carries an inherited ACL too.
    assert!(acl_of(&src.join("inner.txt"), ACL_ACCESS).is_some());

    let dst = TempDir::in_dir(dir.path()).unwrap();
    let (out, _) = run_quiet(&copy_spec(
        std::slice::from_ref(&src),
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    let copy = dst.path().join("tree");
    assert_eq!(acl_of(&copy, ACL_ACCESS), acl_of(&src, ACL_ACCESS));
    assert_eq!(acl_of(&copy, ACL_DEFAULT), acl_of(&src, ACL_DEFAULT));
    assert_eq!(mode_of(&copy), mode_of(&src));
    assert_eq!(
        acl_of(&copy.join("inner.txt"), ACL_ACCESS),
        acl_of(&src.join("inner.txt"), ACL_ACCESS)
    );
    assert_eq!(
        mode_of(&copy.join("inner.txt")),
        mode_of(&src.join("inner.txt"))
    );
    // The default ACL is functional: a NEW file created in the copy inherits an ACL.
    std::fs::write(copy.join("new.txt"), b"new\n").unwrap();
    assert!(
        acl_of(&copy.join("new.txt"), ACL_ACCESS).is_some(),
        "default ACL not inherited"
    );
    assert!(
        !report_of(&out)
            .warnings
            .iter()
            .any(|w| w.message.contains("access control list")),
        "{:?}",
        report_of(&out).warnings
    );
}

/// Review M1's failure mode, forced: the ACL xattr cannot be set on the copy (EOPNOTSUPP on every
/// `set_xattr`), so the group/other bits must come from the ACL's `group::`/`other::` entries,
/// never from the mask — plus a warning.
#[test]
fn acl_that_cannot_be_set_never_widens_group_access() {
    let Some(dir) = private_file_with_acl() else {
        eprintln!("SKIP: no setfacl or no ACL support in the temp file systems");
        return;
    };
    let secret = dir.path().join("secret.txt");
    assert_eq!(mode_of(&secret), 0o660);
    let dst = TempDir::in_dir(dir.path()).unwrap();
    let fs = Arc::new(FaultyFs::over_real(FaultPlan::new().fail_from(
        FaultOp::SetXattr,
        1,
        FaultErrno::Raw(95),
    )));
    let (out, _) = run_policy(
        &copy_spec(
            std::slice::from_ref(&secret),
            dst.path(),
            CopyOpts::default(),
        ),
        fs,
        ConflictAnswer::Abort,
        ErrorAction::Abort,
    );
    assert!(out.is_success(), "{out:?}");
    let copy = dst.path().join("secret.txt");
    assert_eq!(acl_of(&copy, ACL_ACCESS), None, "ACL set unexpectedly");
    // 0600 (user bits only), NOT the 0660 mask: no group access to a private file.
    assert_eq!(mode_of(&copy), 0o600);
    let warnings = &report_of(&out).warnings;
    assert!(
        warnings
            .iter()
            .any(|w| w.message.contains("group/other permissions reduced")),
        "{warnings:?}"
    );
}

/// The whole flow with a synthetic ACL blob (no setfacl needed): the kernel accepts a valid blob
/// set as a raw xattr and folds its mask into the stat mode, exactly like a setfacl-made ACL.
#[test]
fn synthetic_acl_blob_copies_end_to_end() {
    let Some(dir) = acl_temp() else {
        eprintln!("SKIP: no ACL support in the temp file systems");
        return;
    };
    // u::rw-, g::---, m::rw-, o::---: version header plus 8-byte entries (tag, perm: u16 LE,
    // id: u32 LE; 0xffffffff for the qualifier-less entries).
    let blob: Vec<u8> = [
        2u8, 0, 0, 0, // version 2
        1, 0, 6, 0, 0xff, 0xff, 0xff, 0xff, // u::rw-
        4, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // g::---
        16, 0, 6, 0, 0xff, 0xff, 0xff, 0xff, // m::rw-
        32, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, // o::---
    ]
    .to_vec();
    let src = dir.path().join("blob.txt");
    std::fs::write(&src, b"synthetic\n").unwrap();
    set_permissions_0600(&src);
    RealFs
        .set_xattr(&src, OsStr::new(ACL_ACCESS), &blob, false)
        .unwrap();
    // Setting the ACL made the stat mode show the mask.
    assert_eq!(mode_of(&src), 0o660);

    let dst = TempDir::in_dir(dir.path()).unwrap();
    let (out, _) = run_quiet(&copy_spec(
        std::slice::from_ref(&src),
        dst.path(),
        CopyOpts::default(),
    ));
    assert!(out.is_success(), "{out:?}");
    let copy = dst.path().join("blob.txt");
    assert_eq!(acl_of(&copy, ACL_ACCESS), Some(blob));
    assert_eq!(mode_of(&copy), 0o660);
}

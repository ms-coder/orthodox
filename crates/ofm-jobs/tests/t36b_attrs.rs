//! T3.6b (review finding m4): recursive chmod must not lock the job out of its own subtree.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{ErrorAction, Fs, JobSpec, ModeChange, Outcome, run_sync};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t32_support::{TestFs, error_policy, no_questions, questions, report};

fn perm(p: &Path) -> u32 {
    fs::symlink_metadata(p).unwrap().mode() & 0o7777
}

fn chmod_r(path: &Path, set: u32, clear: u32) -> JobSpec {
    JobSpec::SetAttrs {
        paths: vec![path.to_path_buf()],
        mode: Some(ModeChange { set, clear }),
        owner: None,
        recursive: true,
    }
}

fn is_root() -> bool {
    TestFs::real().is_root()
}

/// `top/{f0, d1/{f1, d2/{f2, d3/f3}}}` with 0o755 directories and 0o644 files; returns every
/// entry (including `top`), parents first.
fn tree(t: &TempDir) -> (PathBuf, Vec<PathBuf>) {
    let top = t.path().join("top");
    let mut all = vec![top.clone()];
    let mut dir = top.clone();
    fs::create_dir(&dir).unwrap();
    for i in 0..4 {
        let f = dir.join(format!("f{i}"));
        fs::write(&f, b"x").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        all.push(f);
        if i < 3 {
            dir = dir.join(format!("d{}", i + 1));
            fs::create_dir(&dir).unwrap();
            all.push(dir.clone());
        }
    }
    for p in all.iter().filter(|p| p.is_dir()) {
        fs::set_permissions(p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    (top, all)
}

/// Record the mode of every entry top-down, opening each directory (0o700) after recording it
/// so that its children can be reached. Leaves the tree traversable for the TempDir cleanup.
fn modes_and_open(top: &Path) -> BTreeMap<PathBuf, u32> {
    fn walk(p: &Path, out: &mut BTreeMap<PathBuf, u32>) {
        out.insert(p.to_path_buf(), perm(p));
        if fs::symlink_metadata(p).unwrap().is_dir() {
            fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
            for e in fs::read_dir(p).unwrap() {
                walk(&e.unwrap().path(), out);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(top, &mut out);
    out
}

/// Run `chmod -R` with `set`/`clear` over a fresh tree; assert success, no questions, every entry
/// visited, and the final modes of directories and files.
fn check_variant(set: u32, clear: u32, dir_mode: u32, file_mode: u32) {
    let t = TempDir::new().unwrap();
    let (top, all) = tree(&t);
    let (sink, rec) = no_questions();
    let out = run_sync(&chmod_r(&top, set, clear), Arc::new(TestFs::real()), sink);
    let done = matches!(out, Outcome::Ok(_)).then(|| report(&out).files_done);
    let modes = modes_and_open(&top);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty(), "{:?}", questions(&rec));
    assert_eq!(done, Some(all.len() as u64), "every entry visited");
    assert_eq!(modes.len(), all.len());
    for p in &all {
        let want = if p.file_name().unwrap().to_string_lossy().starts_with('f') {
            file_mode
        } else {
            dir_mode
        };
        assert_eq!(modes[p], want, "{} (-{clear:o} +{set:o})", p.display());
    }
}

#[test]
fn clearing_all_owner_bits_does_not_lock_the_job_out() {
    if is_root() {
        return;
    }
    // chmod -R u-rwx
    check_variant(0, 0o700, 0o055, 0o044);
}

#[test]
fn clearing_owner_read_and_execute_does_not_lock_the_job_out() {
    if is_root() {
        return;
    }
    // chmod -R u-rx
    check_variant(0, 0o500, 0o255, 0o244);
}

#[test]
fn clearing_owner_execute_only_does_not_lock_the_job_out() {
    if is_root() {
        return;
    }
    // chmod -R u-x
    check_variant(0, 0o100, 0o655, 0o644);
}

#[test]
fn go_rwx_clear_changes_every_entry() {
    // chmod -R go-rwx (owner keeps r+x: pre-order is fine)
    check_variant(0, 0o077, 0o700, 0o600);
}

#[test]
fn adding_bits_repairs_unreadable_directories() {
    if is_root() {
        return;
    }
    let t = TempDir::new().unwrap();
    let (top, all) = tree(&t);
    for p in all.iter().rev().filter(|p| p.is_dir()) {
        fs::set_permissions(p, fs::Permissions::from_mode(0o000)).unwrap();
    }
    let (sink, rec) = no_questions();
    let out = run_sync(&chmod_r(&top, 0o700, 0), Arc::new(TestFs::real()), sink);
    let done = matches!(out, Outcome::Ok(_)).then(|| report(&out).files_done);
    let modes = modes_and_open(&top);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty(), "{:?}", questions(&rec));
    assert_eq!(done, Some(all.len() as u64));
    for p in all.iter().filter(|p| modes[*p] != 0o644) {
        assert_eq!(modes[p], 0o700, "{}", p.display());
    }
    // Files got u+rwx added too (set applies to non-directories that already carry x only).
    assert!(all.iter().any(|p| modes[p] == 0o644));
}

#[test]
fn unlistable_directory_is_opened_temporarily_and_ends_unchanged() {
    if is_root() {
        return;
    }
    let t = TempDir::new().unwrap();
    let (top, all) = tree(&t);
    let d1 = top.join("d1");
    fs::set_permissions(&d1, fs::Permissions::from_mode(0o000)).unwrap();
    let (sink, rec) = no_questions();
    // u-rx cannot change d1 (already without owner bits) but its children must still be reached.
    let out = run_sync(&chmod_r(&top, 0, 0o500), Arc::new(TestFs::real()), sink);
    let done = matches!(out, Outcome::Ok(_)).then(|| report(&out).files_done);
    let modes = modes_and_open(&top);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(questions(&rec).is_empty(), "{:?}", questions(&rec));
    assert_eq!(done, Some(all.len() as u64));
    assert_eq!(modes[&d1], 0o000, "d1 ends with its (unchangeable) mode");
    assert_eq!(modes[&d1.join("f1")], 0o244, "child was changed");
    assert_eq!(modes[&d1.join("d2")], 0o255);
}

#[test]
fn abort_restores_the_original_mode_of_a_temporarily_opened_directory() {
    if is_root() {
        return;
    }
    let t = TempDir::new().unwrap();
    let top = t.path().join("top");
    let d1 = top.join("d1");
    fs::create_dir_all(&d1).unwrap();
    fs::write(d1.join("f1"), b"x").unwrap();
    fs::set_permissions(d1.join("f1"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&d1, fs::Permissions::from_mode(0o000)).unwrap();
    fs::set_permissions(&top, fs::Permissions::from_mode(0o755)).unwrap();
    // set_mode calls: 1 = temporary open of d1, 2 = f1 (fails, user aborts).
    let fs_ = Arc::new(FaultyFs::new(
        Arc::new(TestFs::real()),
        FaultPlan::new().fail_nth(FaultOp::SetMode, 2, FaultErrno::PermissionDenied),
    ));
    let (sink, rec) = error_policy(ErrorAction::Abort);
    let out = run_sync(&chmod_r(&top, 0, 0o500), fs_, sink);
    let d1_mode = perm(&d1);
    let top_mode = perm(&top);
    fs::set_permissions(&d1, fs::Permissions::from_mode(0o700)).unwrap();
    let f1_mode = perm(&d1.join("f1"));
    assert!(!matches!(out, Outcome::Ok(_)), "{out:?}");
    assert_eq!(questions(&rec).len(), 1);
    assert_eq!(d1_mode, 0o000, "temporary mode must not be left behind");
    assert_eq!(top_mode, 0o755, "parent untouched after the abort");
    assert_eq!(f1_mode, 0o644);
}

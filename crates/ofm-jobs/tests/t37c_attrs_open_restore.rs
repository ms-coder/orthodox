//! T3.7c (review finding N3, MINOR): the temporary `u+rx` open of a directory during a
//! recursive chmod.
//!
//! (a) The temporarily granted mode is restored **always** — also when the listing itself
//!     still fails and the user answers Abort, and when the job is cancelled at the listing's
//!     checkpoint (the old code propagated the listing result with `?` *before* the restore).
//! (b) A directory the effective user does not own is never opened preemptively: the owner's
//!     r/x bits say nothing about *our* access there (group or other bits may allow the
//!     listing), so a preemptive chmod would only fail with EPERM and ask an unnecessary
//!     error question.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod t32_support;

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ofm_core::CancelToken;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    ErrorAction, Fs, FsFile, FsStat, JobCtx, JobError, JobSpec, ModeChange, Outcome, PauseGate,
    SystemClock, TrashError, ops, run_sync,
};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};
use ofm_testkit::temp::TempDir;
use t32_support::{TestFs, error_policy, no_questions, questions};

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

/// `top/` (0755) containing `top/d1/` (`mode`) with one file `d1/f1` (0600) inside.
fn tree_with_locked_dir(t: &TempDir, mode: u32) -> (PathBuf, PathBuf) {
    let top = t.path().join("top");
    let d1 = top.join("d1");
    fs::create_dir_all(&d1).unwrap();
    fs::write(d1.join("f1"), b"x").unwrap();
    fs::set_permissions(d1.join("f1"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&top, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&d1, fs::Permissions::from_mode(mode)).unwrap();
    (top, d1)
}

/// Make a locked directory traversable again so the `TempDir` cleanup can remove it.
fn reopen(p: &Path) {
    fs::set_permissions(p, fs::Permissions::from_mode(0o700)).unwrap();
}

// ---------------------------------------------------------------------- (a) restore always

/// `chmod -R g+r` over `top/d1` where `d1` is 0000: d1 itself becomes 040, is then opened
/// temporarily with 0540 for the listing — and the listing still fails (injected EACCES).
/// The user answers Abort: d1 must end at 040, not keep the temporary 0540.
#[test]
fn abort_at_denied_listing_restores_the_temporary_mode() {
    if is_root() {
        eprintln!("SKIP (root): the kernel ignores the owner permission bits");
        return;
    }
    let t = TempDir::new().unwrap();
    let (top, d1) = tree_with_locked_dir(&t, 0o000);
    // read_dir calls: #1 = top (fine), #2 = d1, still denied although temporarily 0540.
    let fs_ = Arc::new(FaultyFs::new(
        Arc::new(TestFs::real()),
        FaultPlan::new().fail_nth(FaultOp::ReadDir, 2, FaultErrno::PermissionDenied),
    ));
    let (sink, rec) = error_policy(ErrorAction::Abort);
    let out = run_sync(&chmod_r(&top, 0o040, 0), fs_, sink);
    let d1_mode = perm(&d1);
    reopen(&d1);
    assert!(matches!(out, Outcome::Cancelled(_)), "{out:?}");
    assert_eq!(
        questions(&rec).len(),
        1,
        "exactly the denied listing is asked"
    );
    assert_eq!(
        d1_mode, 0o040,
        "the temporary 0540 must not survive the abort"
    );
}

/// Same walk, but the job is cancelled right after the temporary grant of d1 was applied:
/// the cancel is noticed at the checkpoint in front of the listing, which the old code
/// propagated with `?` before restoring d1.
#[test]
fn cancel_at_the_listing_restores_the_temporary_mode() {
    if is_root() {
        eprintln!("SKIP (root): the kernel ignores the owner permission bits");
        return;
    }
    let t = TempDir::new().unwrap();
    let (top, d1) = tree_with_locked_dir(&t, 0o000);
    let cancel = CancelToken::new();
    let fire = cancel.clone();
    let set_modes = AtomicUsize::new(0);
    let fs_ = TestFs::real().hooked(move |op| {
        // set_mode calls: #1 = d1's own change to 040, #2 = the temporary grant of 0540.
        if op == "set_mode" && set_modes.fetch_add(1, Ordering::SeqCst) == 1 {
            fire.cancel();
        }
    });
    let (sink, rec) = no_questions();
    let mut ctx = JobCtx::new(
        Arc::new(fs_),
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(SystemClock::new()),
    );
    let res = ops::attrs::run(
        &mut ctx,
        &[top],
        Some(ModeChange {
            set: 0o040,
            clear: 0,
        }),
        None,
        true,
    );
    let d1_mode = perm(&d1);
    reopen(&d1);
    assert!(matches!(res, Err(JobError::Cancelled)), "{res:?}");
    assert!(questions(&rec).is_empty(), "{:?}", questions(&rec));
    assert_eq!(
        d1_mode, 0o040,
        "the temporary 0540 must not survive the cancel"
    );
}

// ------------------------------------------------------------------ (b) not our directory

/// A uid that is never the effective uid of the test process.
fn foreign_uid() -> u32 {
    rustix::process::geteuid().as_raw().wrapping_add(1)
}

/// Presents `dir` as a directory of *another* user that we can still list: `lstat` reports
/// the foreign uid and mode `0o070` (no owner bits — the case the old code misjudged), while
/// the directory really keeps its mode so that the kernel really lets a listing through the
/// group class (only the owner uid decides the class, which is why the uid must be faked).
/// A chmod on it fails with EPERM, exactly like the kernel would answer a non-owner.
struct ForeignDirFs {
    inner: Arc<dyn Fs>,
    dir: PathBuf,
    chmod_attempts: AtomicUsize,
}

impl Fs for ForeignDirFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.inner.open_read(path)
    }
    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.inner.open_read_follow(path)
    }
    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        self.inner.create_excl(path, mode)
    }
    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.inner.stat(path)
    }
    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        let mut st = self.inner.lstat(path)?;
        if path == self.dir {
            st.uid = foreign_uid();
            st.mode = (st.mode & !0o777) | 0o070;
        }
        Ok(st)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        self.inner.read_dir(path)
    }
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.inner.mkdir(path, mode)
    }
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename_noreplace(from, to)
    }
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename_replace(from, to)
    }
    fn unlink(&self, path: &Path) -> io::Result<()> {
        self.inner.unlink(path)
    }
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.inner.rmdir(path)
    }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        self.inner.symlink(target, link)
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.hard_link(from, to)
    }
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner.read_link(path)
    }
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        if path == self.dir {
            self.chmod_attempts.fetch_add(1, Ordering::SeqCst);
            return Err(io::Error::from_raw_os_error(1)); // EPERM: not our directory
        }
        self.inner.set_mode(path, mode)
    }
    fn set_times(&self, path: &Path, a: i128, m: i128, follow: bool) -> io::Result<()> {
        self.inner.set_times(path, a, m, follow)
    }
    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()> {
        self.inner.set_owner(path, uid, gid, follow)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        self.inner.list_xattrs(path, follow)
    }
    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        self.inner.get_xattr(path, name, follow)
    }
    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        self.inner.set_xattr(path, name, value, follow)
    }
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        self.inner.trash(path)
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

/// `chmod -R g+r` over a tree whose `top/sub` is 0o070 (group rwx, no owner bits) and owned
/// by another user: the listing goes through the group bits. `sub` must not be chmod'ed
/// (EPERM) and no error question may appear — yet the child below it is still changed.
#[test]
fn foreign_group_readable_directory_is_listed_not_chmoded() {
    let t = TempDir::new().unwrap();
    let top = t.path().join("top");
    let sub = top.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("f1"), b"x").unwrap();
    fs::set_permissions(sub.join("f1"), fs::Permissions::from_mode(0o600)).unwrap();
    // 0750 so the *kernel* allows the listing through the group class; the job sees 0070.
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o750)).unwrap();
    fs::set_permissions(&top, fs::Permissions::from_mode(0o755)).unwrap();
    let fs_ = Arc::new(ForeignDirFs {
        inner: Arc::new(TestFs::real()),
        dir: sub.clone(),
        chmod_attempts: AtomicUsize::new(0),
    });
    let (sink, rec) = no_questions();
    let out = run_sync(&chmod_r(&top, 0o040, 0), fs_.clone(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    assert!(
        questions(&rec).is_empty(),
        "no EPERM question for a directory we can already list: {:?}",
        questions(&rec)
    );
    assert_eq!(
        fs_.chmod_attempts.load(Ordering::SeqCst),
        0,
        "sub was never chmod'ed"
    );
    assert_eq!(perm(&sub), 0o750, "sub keeps its mode");
    assert_eq!(
        perm(&sub.join("f1")),
        0o640,
        "the child was reached through the group bits"
    );
}

/// A directory of another user we *cannot* list (0000 for the job, and the kernel denies the
/// listing too): the reported error is the denied listing itself — never a doomed chmod.
#[test]
fn foreign_denied_directory_reports_the_listing_not_a_chmod() {
    let t = TempDir::new().unwrap();
    let top = t.path().join("top");
    let sub = top.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("f1"), b"x").unwrap();
    fs::set_permissions(sub.join("f1"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::set_permissions(&top, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(&sub, fs::Permissions::from_mode(0o000)).unwrap();
    let fs_ = Arc::new(ForeignDirFs {
        inner: Arc::new(TestFs::real()),
        dir: sub.clone(),
        chmod_attempts: AtomicUsize::new(0),
    });
    let (sink, rec) = error_policy(ErrorAction::Skip);
    let out = run_sync(&chmod_r(&top, 0o040, 0), fs_.clone(), sink);
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    let asked = questions(&rec);
    assert_eq!(asked.len(), 1, "exactly one question: {asked:?}");
    assert!(
        matches!(
            &asked[0],
            ofm_jobs::Question::Error {
                os_error: Some(13), // EACCES on the listing
                ..
            }
        ),
        "{asked:?}"
    );
    assert_eq!(
        fs_.chmod_attempts.load(Ordering::SeqCst),
        0,
        "no chmod attempted on the foreign directory"
    );
    let sub_mode = perm(&sub);
    reopen(&sub);
    assert_eq!(sub_mode, 0o000, "sub keeps its mode");
}

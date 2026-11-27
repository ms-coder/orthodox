//! FIX-M6M7 #5, the fine-grained half: a checked parent is swapped for a symlink immediately
//! before EVERY single file system call of an extraction (also on file systems without
//! `RENAME_NOREPLACE` / hard links), so every window between a check and the write that relies on
//! it is covered. Plus fault injection on the new `Fs::pin`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod extract_support;

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use extract_support::*;
use ofm_jobs::fs::faulty::FaultyFs;
use ofm_jobs::{
    ConflictAnswer, ErrorAction, Fs, FsFile, FsPin, FsStat, JobError, RealFs, TrashError,
};
use ofm_testkit::fault::{FaultErrno, FaultOp, FaultPlan};

/// Delegates every call to `inner`, running `hook` first.
struct HookFs {
    inner: Arc<dyn Fs>,
    hook: Box<dyn Fn() + Send + Sync>,
}

impl HookFs {
    fn h(&self) -> &Arc<dyn Fs> {
        (self.hook)();
        &self.inner
    }
}

impl Fs for HookFs {
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn FsFile>> {
        self.h().open_read(p)
    }
    fn open_read_follow(&self, p: &Path) -> io::Result<Box<dyn FsFile>> {
        self.h().open_read_follow(p)
    }
    fn create_excl(&self, p: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        self.h().create_excl(p, mode)
    }
    fn stat(&self, p: &Path) -> io::Result<FsStat> {
        self.h().stat(p)
    }
    fn lstat(&self, p: &Path) -> io::Result<FsStat> {
        self.h().lstat(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<OsString>> {
        self.h().read_dir(p)
    }
    fn mkdir(&self, p: &Path, mode: u32) -> io::Result<()> {
        self.h().mkdir(p, mode)
    }
    fn rename_noreplace(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.h().rename_noreplace(a, b)
    }
    fn rename_replace(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.h().rename_replace(a, b)
    }
    fn unlink(&self, p: &Path) -> io::Result<()> {
        self.h().unlink(p)
    }
    fn rmdir(&self, p: &Path) -> io::Result<()> {
        self.h().rmdir(p)
    }
    fn symlink(&self, t: &Path, l: &Path) -> io::Result<()> {
        self.h().symlink(t, l)
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        self.h().hard_link(a, b)
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        self.h().read_link(p)
    }
    fn set_mode(&self, p: &Path, mode: u32) -> io::Result<()> {
        self.h().set_mode(p, mode)
    }
    fn set_times(&self, p: &Path, a: i128, m: i128, follow: bool) -> io::Result<()> {
        self.h().set_times(p, a, m, follow)
    }
    fn set_owner(&self, p: &Path, u: Option<u32>, g: Option<u32>, f: bool) -> io::Result<()> {
        self.h().set_owner(p, u, g, f)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        self.h().sync_dir(p)
    }
    fn list_xattrs(&self, p: &Path, f: bool) -> io::Result<Vec<OsString>> {
        self.h().list_xattrs(p, f)
    }
    fn get_xattr(&self, p: &Path, n: &OsStr, f: bool) -> io::Result<Option<Vec<u8>>> {
        self.h().get_xattr(p, n, f)
    }
    fn set_xattr(&self, p: &Path, n: &OsStr, v: &[u8], f: bool) -> io::Result<()> {
        self.h().set_xattr(p, n, v, f)
    }
    fn pin(&self, p: &Path, follow: bool) -> io::Result<FsPin> {
        self.h().pin(p, follow)
    }
    fn trash(&self, p: &Path) -> Result<(), TrashError> {
        self.h().trash(p)
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

/// An archive that exercises every kind of write below `d`.
fn archive(tmp: &Tmp) -> MemArchive {
    let mut a = MemArchive::new(&tmp.archive_file("a.zip", 20_000));
    a.dir("d").mode(0o750).mtime(MTIME);
    a.file("d/f1", &bytes(1, 6000));
    a.symlink("d/link", "f1");
    a.hardlink("d/hard", "d/f1");
    a.dir("d/sub").mode(0o700);
    a.file("d/sub/f2", &bytes(2, 100));
    a.file("d/pre", b"from the archive");
    a.file_with("d/bad", 9000, Data::Corrupt(vec![7; 5000]));
    a.file("top", b"top");
    a
}

/// Everything below `<tmp>/outside`, with contents, modes and times: must never change.
fn fingerprint(outside: &Path) -> Vec<(PathBuf, String, u32, i128, Vec<u8>)> {
    let mut v: Vec<_> = snapshot(outside)
        .into_iter()
        .map(|(rel, desc)| {
            let p = outside.join(&rel);
            let data = if p.is_file() { read(&p) } else { Vec::new() };
            (rel, desc, perm(&p), mtime_ns(&p), data)
        })
        .collect();
    v.push((
        PathBuf::new(),
        "dir".into(),
        perm(outside),
        mtime_ns(outside),
        Vec::new(),
    ));
    v
}

struct Case {
    tmp: Tmp,
    dst: PathBuf,
    outside: PathBuf,
}

/// A destination and a decoy tree outside with the same names as the archive's.
fn case(preexisting: bool) -> Case {
    let tmp = Tmp::new("sweep");
    let dst = tmp.dst();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(outside.join("sub")).unwrap();
    for n in ["f1", "pre", "sub/f2", "hard"] {
        std::fs::write(outside.join(n), b"precious").unwrap();
    }
    std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o711)).unwrap();
    if preexisting {
        std::fs::create_dir(dst.join("d")).unwrap();
        std::fs::write(dst.join("d/pre"), b"old").unwrap();
    }
    Case { tmp, dst, outside }
}

/// Run with the swap armed before call number `at` (it happens at the first call from `at` on
/// where `dst/d` is a real directory). Returns the number of calls and whether it swapped.
fn run_swapping_at(c: &Case, base: Arc<dyn Fs>, at: u64, sync: bool) -> (Run, u64, bool) {
    let calls = Arc::new(AtomicU64::new(0));
    let swapped = Arc::new(AtomicBool::new(false));
    let (n, done) = (Arc::clone(&calls), Arc::clone(&swapped));
    let (d, moved, to) = (c.dst.join("d"), c.dst.join("d.moved"), c.outside.clone());
    let hook = move || {
        if n.fetch_add(1, Ordering::SeqCst) + 1 >= at
            && !done.load(Ordering::SeqCst)
            && std::fs::symlink_metadata(&d).is_ok_and(|m| m.is_dir())
        {
            std::fs::rename(&d, &moved).unwrap();
            symlink(&to, &d).unwrap();
            done.store(true, Ordering::SeqCst);
        }
    };
    let fs = Arc::new(HookFs {
        inner: base,
        hook: Box::new(hook),
    });
    let a = archive(&c.tmp);
    let mut r = a.reader();
    let o = ofm_jobs::ExtractOpts { sync, ..opts() };
    let run = run_on(
        fs,
        &mut r,
        &[],
        &c.dst,
        &o,
        ConflictAnswer::Overwrite,
        ErrorAction::Skip,
    );
    (
        run,
        calls.load(Ordering::SeqCst),
        swapped.load(Ordering::SeqCst),
    )
}

fn sweep(base: impl Fn() -> Arc<dyn Fs>, preexisting: bool, sync: bool) {
    let c = case(preexisting);
    let (run, total, swapped) = run_swapping_at(&c, base(), u64::MAX, sync);
    run.ok();
    assert!(!swapped);
    // The undisturbed run really goes through every kind of write.
    assert_eq!(read(&c.dst.join("d/f1")), bytes(1, 6000));
    assert_eq!(read(&c.dst.join("d/hard")), bytes(1, 6000));
    assert_eq!(read(&c.dst.join("d/pre")), b"from the archive");
    assert_eq!(read(&c.dst.join("d/sub/f2")), bytes(2, 100));
    assert!(std::fs::read_link(c.dst.join("d/link")).is_ok());
    assert_absent(&c.dst.join("d/bad"));
    assert!(total > 30, "{total}");

    let mut swaps = 0;
    for at in 1..=total {
        let c = case(preexisting);
        let before = fingerprint(&c.outside);
        let (run, _, swapped) = run_swapping_at(&c, base(), at, sync);
        assert!(run.result.is_ok(), "call {at}: {:?}", run.result);
        assert_eq!(
            fingerprint(&c.outside),
            before,
            "swap before call {at}: something outside the destination changed"
        );
        assert_no_part_files(c.tmp.path());
        // Whatever was written is complete, wherever the checked directory ended up.
        for dir in ["d.moved", "d"] {
            if std::fs::symlink_metadata(c.dst.join(dir)).is_ok_and(|m| m.is_dir()) {
                let d = c.dst.join(dir);
                assert_absent_or_exact(&d.join("f1"), &bytes(1, 6000));
                assert_absent_or_exact(&d.join("hard"), &bytes(1, 6000));
                assert_absent_or_exact(&d.join("sub/f2"), &bytes(2, 100));
                assert_absent(&d.join("bad"));
            }
        }
        assert_eq!(read(&c.dst.join("top")), b"top", "call {at}");
        swaps += u64::from(swapped);
    }
    assert!(swaps > total / 2, "{swaps} swaps for {total} calls");
}

fn plain() -> Arc<dyn Fs> {
    Arc::new(RealFs)
}

#[test]
fn swap_before_every_call_new_directory() {
    sweep(plain, false, false);
}

#[test]
fn swap_before_every_call_existing_directory_with_sync() {
    sweep(plain, true, true);
}

/// NFS/FUSE: publish via `link` + `unlink`.
#[test]
fn swap_before_every_call_without_rename_noreplace() {
    let base = || -> Arc<dyn Fs> {
        Arc::new(FaultyFs::over_real(FaultPlan::new().fail_from(
            FaultOp::RenameNoreplace,
            1,
            FaultErrno::Raw(95),
        )))
    };
    sweep(base, false, false);
}

/// FAT: no hard links either, publish via "lstat, then rename", hard link entries are copied.
#[test]
fn swap_before_every_call_without_rename_noreplace_or_hard_links() {
    let base = || -> Arc<dyn Fs> {
        Arc::new(FaultyFs::over_real(
            FaultPlan::new()
                .fail_from(FaultOp::RenameNoreplace, 1, FaultErrno::Raw(95))
                .fail_from(FaultOp::HardLink, 1, FaultErrno::Raw(1)),
        ))
    };
    sweep(base, true, false);
}

// ---------------------------------------------------------------------------- pin faults

fn run_with_plan(c: &Case, plan: FaultPlan) -> (Run, Arc<FaultyFs>) {
    let fs = Arc::new(FaultyFs::over_real(plan));
    let a = archive(&c.tmp);
    let mut r = a.reader();
    let run = run_on(
        fs.clone(),
        &mut r,
        &[],
        &c.dst,
        &opts(),
        ConflictAnswer::Skip,
        ErrorAction::Skip,
    );
    (run, fs)
}

#[test]
fn an_unopenable_destination_fails_before_anything_is_written() {
    let c = case(false);
    let (run, _) = run_with_plan(
        &c,
        FaultPlan::new().fail_nth(FaultOp::Pin, 1, FaultErrno::PermissionDenied),
    );
    match &run.result {
        Err(JobError::Io { path, source }) => {
            assert_eq!(path, &c.dst);
            assert_eq!(source.raw_os_error(), Some(13));
        }
        other => panic!("{other:?}"),
    }
    assert!(snapshot(&c.dst).is_empty());
}

#[test]
fn a_failing_pin_is_reported_and_leaves_no_temp_file() {
    let c = case(false);
    let (run, fs) = run_with_plan(&c, FaultPlan::new());
    run.ok();
    let pins = fs.calls_of(FaultOp::Pin);
    assert!(pins >= 3, "{pins}");
    for nth in 2..=pins {
        for errno in [FaultErrno::PermissionDenied, FaultErrno::Raw(24)] {
            let c = case(false);
            let (run, _) = run_with_plan(&c, FaultPlan::new().fail_nth(FaultOp::Pin, nth, errno));
            match &run.result {
                // Per entry (or per directory at the end): a warning.
                Ok(()) => assert!(!run.report.warnings.is_empty(), "pin {nth}/{errno:?}"),
                // Out of descriptors stops the job, naming a destination path.
                Err(JobError::Io { path, .. }) => {
                    assert_eq!(errno, FaultErrno::Raw(24));
                    assert!(path.starts_with(&c.dst), "{path:?}");
                }
                Err(other) => panic!("pin {nth}/{errno:?}: {other:?}"),
            }
            assert_no_part_files(&c.dst);
        }
    }
}

/// The `/proc` paths used internally never reach the user.
#[test]
fn messages_and_questions_show_real_paths() {
    let c = case(false);
    std::fs::create_dir(c.dst.join("d")).unwrap();
    std::fs::write(c.dst.join("d/pre"), b"old").unwrap();
    let (run, _) = run_with_plan(
        &c,
        FaultPlan::new().fail_nth(FaultOp::CreateExcl, 1, FaultErrno::PermissionDenied),
    );
    run.ok();
    let questions = question_conflicts(&run.rec);
    assert!(!questions.is_empty());
    let text = format!(
        "{questions:?} {:?} {:?}",
        run.report.warnings, run.report.created
    );
    assert!(!text.contains("/proc/"), "{text}");
    assert!(
        run.report
            .warnings
            .iter()
            .any(|w| w.path == c.dst.join("d/f1")),
        "{:?}",
        run.report.warnings
    );
}

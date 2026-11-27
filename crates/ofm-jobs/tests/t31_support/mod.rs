//! Shared helpers for the T3.1 integration tests (`t31_*.rs`): job runners, answer scripts,
//! a clock that makes every progress update visible, a sink that cancels at a byte offset, an
//! `Fs` wrapper that disables the kernel copy tiers, and tree assertions.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::BorrowedFd;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ofm_core::{CancelToken, Location};
use ofm_jobs::{
    Answer, Clock, ConflictAnswer, CopyOpts, ErrorAction, Fs, FsFile, FsStat, JobCtx, JobSink,
    JobSpec, Outcome, PauseGate, Progress, Question, RealFs, Recording, ScriptedSink, TrashError,
    run_job, run_sync,
};

pub type Rec = Arc<Mutex<Recording>>;

pub fn local(p: &Path) -> Location {
    Location::Local(p.to_path_buf())
}

pub fn copy_spec(sources: &[PathBuf], dest: &Path, opts: CopyOpts) -> JobSpec {
    JobSpec::Copy {
        sources: sources.iter().map(|p| local(p)).collect(),
        dest_dir: dest.to_path_buf(),
        opts,
    }
}

pub fn move_spec(sources: &[PathBuf], dest: &Path, opts: CopyOpts) -> JobSpec {
    JobSpec::Move {
        sources: sources.iter().map(|p| local(p)).collect(),
        dest_dir: dest.to_path_buf(),
        opts,
    }
}

/// Run with the real fs; no question expected (any question aborts the job).
pub fn run_quiet(spec: &JobSpec) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::refusing();
    (run_sync(spec, Arc::new(RealFs), sink), rec)
}

/// Run with `fs` and a fixed answer policy.
pub fn run_policy(
    spec: &JobSpec,
    fs: Arc<dyn Fs>,
    conflict: ConflictAnswer,
    error: ErrorAction,
) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::policy(conflict, error, false);
    (run_sync(spec, fs, sink), rec)
}

/// Run with `fs` and an answer closure.
pub fn run_with(
    spec: &JobSpec,
    fs: Arc<dyn Fs>,
    f: impl FnMut(&Question) -> Option<Answer> + Send + 'static,
) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::new(f);
    (run_sync(spec, fs, sink), rec)
}

pub fn conflict(action: ConflictAnswer, all: bool) -> Option<Answer> {
    Some(Answer::Conflict {
        action,
        apply_to_all: all,
    })
}

pub fn error(action: ErrorAction) -> Option<Answer> {
    Some(Answer::Error {
        action,
        apply_to_all: false,
    })
}

pub fn questions(rec: &Rec) -> Vec<Question> {
    rec.lock().unwrap().questions.clone()
}

pub fn report_of(o: &Outcome) -> ofm_jobs::Report {
    o.report().cloned().unwrap_or_default()
}

/// A clock that advances 1 s on every read: every progress update reaches the sink.
#[derive(Debug, Default)]
pub struct TickClock(AtomicU64);

impl Clock for TickClock {
    fn now(&self) -> Duration {
        Duration::from_secs(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

/// Sink that fires `cancel` once `bytes_done >= at` (no questions answered).
pub struct CancelAt {
    pub cancel: CancelToken,
    pub at: u64,
    pub seen: Arc<Mutex<Vec<Progress>>>,
}

impl JobSink for CancelAt {
    fn progress(&mut self, p: &Progress) {
        if p.bytes_done >= self.at {
            self.cancel.cancel();
        }
        self.seen.lock().unwrap().push(p.clone());
    }
    fn ask(&mut self, _: Question) -> Option<Answer> {
        None
    }
}

/// Run `spec` on `fs`, cancelling as soon as `at` bytes were copied.
pub fn run_cancel_at(spec: &JobSpec, fs: Arc<dyn Fs>, at: u64) -> (Outcome, Vec<Progress>) {
    let cancel = CancelToken::new();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = CancelAt {
        cancel: cancel.clone(),
        at,
        seen: Arc::clone(&seen),
    };
    let mut ctx = JobCtx::new(
        fs,
        cancel,
        PauseGate::new(),
        Box::new(sink),
        Arc::new(TickClock::default()),
    );
    let out = run_job(spec, &mut ctx);
    let seen = seen.lock().unwrap().clone();
    (out, seen)
}

// ---- tree assertions --------------------------------------------------------------------------

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        out.push(p.clone());
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk(&p, out);
        }
    }
}

/// Every path below `dir` (no symlink following).
pub fn all_paths(dir: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    walk(dir, &mut v);
    v
}

/// No `.ofm-part-` temp entries anywhere below `dir`.
pub fn assert_no_temps(dir: &Path) {
    let temps: Vec<_> = all_paths(dir)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.as_bytes().windows(10).any(|w| w == b".ofm-part-"))
        })
        .collect();
    assert!(temps.is_empty(), "temp files left: {temps:?}");
}

/// Every regular file under `dest` that also exists (by relative path) under `reference` has
/// exactly the reference content: nothing half-written under a final name.
pub fn assert_files_complete(reference: &Path, dest: &Path) {
    for p in all_paths(dest) {
        let md = std::fs::symlink_metadata(&p).unwrap();
        if !md.is_file() {
            continue;
        }
        let rel = p.strip_prefix(dest).unwrap();
        let r = reference.join(rel);
        if r.is_file() {
            assert_eq!(
                std::fs::read(&p).unwrap(),
                std::fs::read(&r).unwrap(),
                "content of {} differs from {}",
                p.display(),
                r.display()
            );
        }
    }
}

/// Data-loss check for a move: every regular file of `reference` exists with its content under
/// `src` or under `dest` (or both).
pub fn assert_nothing_lost(reference: &Path, src: &Path, dest: &Path) {
    for r in all_paths(reference) {
        let md = std::fs::symlink_metadata(&r).unwrap();
        if !md.is_file() {
            continue;
        }
        let rel = r.strip_prefix(reference).unwrap();
        let want = std::fs::read(&r).unwrap();
        let in_src = std::fs::read(src.join(rel)).ok();
        let in_dest = std::fs::read(dest.join(rel)).ok();
        assert!(
            in_src.as_ref() == Some(&want) || in_dest.as_ref() == Some(&want),
            "{} lost (src: {:?} bytes, dest: {:?} bytes)",
            rel.display(),
            in_src.map(|v| v.len()),
            in_dest.map(|v| v.len())
        );
    }
}

/// Deterministic pseudo-random bytes.
pub fn noise(len: usize, seed: u64) -> Vec<u8> {
    let mut x = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

pub fn names_in(dir: &Path) -> Vec<OsString> {
    let mut v: Vec<OsString> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    v.sort();
    v
}

// ---- an Fs without the kernel copy tiers --------------------------------------------------------

/// Wraps [`RealFs`]; with `clone`/`range` false, `try_clone_from` reports "unsupported" and
/// `copy_range_from` reports "unsupported", so the engine must use read/write.
#[derive(Debug, Clone, Copy)]
pub struct Tiers {
    pub clone: bool,
    pub range: bool,
}

impl Tiers {
    pub fn read_write_only() -> Arc<dyn Fs> {
        Arc::new(Tiers {
            clone: false,
            range: false,
        })
    }
}

struct TiersFile {
    inner: Box<dyn FsFile>,
    t: Tiers,
}

impl FsFile for TiersFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }
    fn stat(&self) -> io::Result<FsStat> {
        self.inner.stat()
    }
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.inner.as_fd()
    }
    fn try_clone_from(&mut self, src: &dyn FsFile) -> io::Result<bool> {
        if self.t.clone {
            self.inner.try_clone_from(src)
        } else {
            Ok(false)
        }
    }
    fn copy_range_from(&mut self, src: &mut dyn FsFile, len: u64) -> io::Result<Option<u64>> {
        if self.t.range {
            self.inner.copy_range_from(src, len)
        } else {
            Ok(None)
        }
    }
    fn seek_data(&mut self, from: u64) -> io::Result<Option<u64>> {
        self.inner.seek_data(from)
    }
    fn seek_hole(&mut self, from: u64) -> io::Result<u64> {
        self.inner.seek_hole(from)
    }
    fn seek(&mut self, offset: u64) -> io::Result<()> {
        self.inner.seek(offset)
    }
    fn set_len(&mut self, len: u64) -> io::Result<()> {
        self.inner.set_len(len)
    }
    fn set_mode(&mut self, mode: u32) -> io::Result<()> {
        self.inner.set_mode(mode)
    }
    fn set_times(&mut self, a: i128, m: i128) -> io::Result<()> {
        self.inner.set_times(a, m)
    }
    fn set_owner(&mut self, u: Option<u32>, g: Option<u32>) -> io::Result<()> {
        self.inner.set_owner(u, g)
    }
    fn sync(&mut self) -> io::Result<()> {
        self.inner.sync()
    }
}

impl Tiers {
    fn wrap(&self, f: Box<dyn FsFile>) -> Box<dyn FsFile> {
        Box::new(TiersFile { inner: f, t: *self })
    }
}

impl Fs for Tiers {
    fn open_read(&self, p: &Path) -> io::Result<Box<dyn FsFile>> {
        Ok(self.wrap(RealFs.open_read(p)?))
    }
    fn open_read_follow(&self, p: &Path) -> io::Result<Box<dyn FsFile>> {
        Ok(self.wrap(RealFs.open_read_follow(p)?))
    }
    fn create_excl(&self, p: &Path, m: u32) -> io::Result<Box<dyn FsFile>> {
        Ok(self.wrap(RealFs.create_excl(p, m)?))
    }
    fn stat(&self, p: &Path) -> io::Result<FsStat> {
        RealFs.stat(p)
    }
    fn lstat(&self, p: &Path) -> io::Result<FsStat> {
        RealFs.lstat(p)
    }
    fn read_dir(&self, p: &Path) -> io::Result<Vec<OsString>> {
        RealFs.read_dir(p)
    }
    fn mkdir(&self, p: &Path, m: u32) -> io::Result<()> {
        RealFs.mkdir(p, m)
    }
    fn rename_noreplace(&self, a: &Path, b: &Path) -> io::Result<()> {
        RealFs.rename_noreplace(a, b)
    }
    fn rename_replace(&self, a: &Path, b: &Path) -> io::Result<()> {
        RealFs.rename_replace(a, b)
    }
    fn unlink(&self, p: &Path) -> io::Result<()> {
        RealFs.unlink(p)
    }
    fn rmdir(&self, p: &Path) -> io::Result<()> {
        RealFs.rmdir(p)
    }
    fn symlink(&self, t: &Path, l: &Path) -> io::Result<()> {
        RealFs.symlink(t, l)
    }
    fn hard_link(&self, a: &Path, b: &Path) -> io::Result<()> {
        RealFs.hard_link(a, b)
    }
    fn read_link(&self, p: &Path) -> io::Result<PathBuf> {
        RealFs.read_link(p)
    }
    fn set_mode(&self, p: &Path, m: u32) -> io::Result<()> {
        RealFs.set_mode(p, m)
    }
    fn set_times(&self, p: &Path, a: i128, m: i128, f: bool) -> io::Result<()> {
        RealFs.set_times(p, a, m, f)
    }
    fn set_owner(&self, p: &Path, u: Option<u32>, g: Option<u32>, f: bool) -> io::Result<()> {
        RealFs.set_owner(p, u, g, f)
    }
    fn sync_dir(&self, p: &Path) -> io::Result<()> {
        RealFs.sync_dir(p)
    }
    fn list_xattrs(&self, p: &Path, f: bool) -> io::Result<Vec<OsString>> {
        RealFs.list_xattrs(p, f)
    }
    fn get_xattr(&self, p: &Path, n: &OsStr, f: bool) -> io::Result<Option<Vec<u8>>> {
        RealFs.get_xattr(p, n, f)
    }
    fn set_xattr(&self, p: &Path, n: &OsStr, v: &[u8], f: bool) -> io::Result<()> {
        RealFs.set_xattr(p, n, v, f)
    }
    fn pin(&self, path: &std::path::Path, follow: bool) -> std::io::Result<ofm_jobs::FsPin> {
        RealFs.pin(path, follow)
    }
    fn trash(&self, p: &Path) -> Result<(), TrashError> {
        RealFs.trash(p)
    }
    fn is_root(&self) -> bool {
        RealFs.is_root()
    }
}

/// Whether user xattrs work under `dir` (tmpfs before 6.6 and some sandboxes refuse them).
pub fn xattrs_supported(dir: &Path) -> bool {
    let p = dir.join(".xattr-probe");
    std::fs::write(&p, b"").unwrap();
    let ok = RealFs
        .set_xattr(&p, OsStr::new("user.probe"), b"1", false)
        .is_ok();
    std::fs::remove_file(&p).unwrap();
    ok
}

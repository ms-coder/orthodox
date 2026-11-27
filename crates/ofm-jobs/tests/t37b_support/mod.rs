//! Helpers for the T3.7b regression tests (`t37b_*.rs`): job runners and [`FatFs`], an `Fs`
//! wrapper that makes a plain temp directory behave like a vfat/exFAT stick for the engine:
//!
//! (a) names below `root` are case-insensitive (ASCII-folded to lower case on disk, like the
//!     `t36a` `HookFs`), and
//! (b) inode numbers are UNSTABLE the way they are on vfat/exFAT (review finding N2): once the
//!     wrapper has seen a name published (a rename/mkdir/symlink/link landing in `root`), a
//!     `stat`/`lstat` of that same directory entry under a DIFFERENT case spelling returns a
//!     fresh inode number — what the kernel does when an evicted inode is rebuilt with a new
//!     `iunique()` number under memory pressure. Stats in the spelling the job published keep
//!     the real number, so the move-mode verify step (same spelling) still sees it.
//!
//! A rename of an item from OUTSIDE `root` into `root` answers `EXDEV`, like moving onto a
//! real stick, so the cross-device copy engine runs without a second real file system (no
//! test can silently skip for lack of one).
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_core::Location;
use ofm_jobs::{
    ConflictAnswer, CopyOpts, ErrorAction, Fs, FsStat, JobSpec, Outcome, Question, RealFs,
    Recording, ScriptedSink, TrashError, run_sync,
};

pub type Rec = Arc<Mutex<Recording>>;

/// `EXDEV` (rename across file systems).
const EXDEV: i32 = 18;

/// Added to the real inode number of a published entry when it is stat'ed under a different
/// case spelling. Larger than any inode number a temp file system hands out, so the fake
/// numbers stay distinct per file and distinct from the real ones.
const UNSTABLE_INO_OFFSET: u64 = 0x5EED_0000_0000;

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

pub fn opts() -> CopyOpts {
    CopyOpts::default()
}

/// Run with a fixed policy (`apply_to_all` on every answer).
pub fn run_policy(
    spec: &JobSpec,
    fs: Arc<dyn Fs>,
    conflict: ConflictAnswer,
    error: ErrorAction,
) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::policy(conflict, error, false);
    (run_sync(spec, fs, sink), rec)
}

pub fn questions(rec: &Rec) -> Vec<Question> {
    rec.lock().unwrap().questions.clone()
}

pub fn conflict_kinds(rec: &Rec) -> Vec<ofm_jobs::ConflictKind> {
    questions(rec)
        .into_iter()
        .filter_map(|q| match q {
            Question::Conflict { kind, .. } => Some(kind),
            _ => None,
        })
        .collect()
}

pub fn report_of(o: &Outcome) -> ofm_jobs::Report {
    o.report().cloned().unwrap_or_default()
}

pub fn read(p: &Path) -> Vec<u8> {
    std::fs::read(p).unwrap_or_else(|e| panic!("read {}: {e}", p.display()))
}

/// Sorted names in `dir` (lossy, for assertions).
pub fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

/// No `.ofm-part-` temp anywhere below `dir`.
pub fn assert_no_temps(dir: &Path) {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap() {
            let e = e.unwrap();
            let name = e.file_name();
            assert!(
                !name.as_bytes().windows(10).any(|w| w == b".ofm-part-"),
                "temp left behind: {}",
                e.path().display()
            );
            if e.file_type().unwrap().is_dir() {
                stack.push(e.path());
            }
        }
    }
}

pub fn set_mtime(p: &Path, secs: i64) {
    let f = std::fs::File::options().write(true).open(p).unwrap();
    let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64);
    f.set_times(std::fs::FileTimes::new().set_modified(t).set_accessed(t))
        .unwrap();
}

// --------------------------------------------------------------------------------- FatFs

/// An `Fs` that behaves like a vfat/exFAT stick below `root` (see the module docs): names are
/// case-insensitive, inode numbers of published entries change under a re-lookup with a
/// different case spelling, and renames into `root` from outside answer `EXDEV`.
pub struct FatFs {
    inner: Arc<dyn Fs>,
    fold_root: PathBuf,
    /// Final names this "file system" holds, as the job put them there: folded path ->
    /// spelling of the last component the job used.
    published: Mutex<HashMap<PathBuf, OsString>>,
}

impl FatFs {
    /// The "stick" is everything below `root`.
    pub fn new(root: &Path) -> Self {
        FatFs {
            inner: Arc::new(RealFs),
            fold_root: root.to_path_buf(),
            published: Mutex::new(HashMap::new()),
        }
    }

    fn map(&self, p: &Path) -> PathBuf {
        match p.strip_prefix(&self.fold_root) {
            Ok(rest) if !rest.as_os_str().is_empty() => {
                let lower = rest.as_os_str().as_bytes().to_ascii_lowercase();
                self.fold_root.join(OsString::from_vec(lower))
            }
            _ => p.to_path_buf(),
        }
    }

    /// `p` is not on the "stick" (the source side).
    fn outside(&self, p: &Path) -> bool {
        !p.starts_with(&self.fold_root)
    }

    /// Remember `to` as a name the job published.
    fn note(&self, to: &Path) {
        if let Some(name) = to.file_name() {
            self.published
                .lock()
                .unwrap()
                .insert(self.map(to), name.to_owned());
        }
    }

    /// Give an already published entry a fresh inode number when it is looked up under a
    /// different case spelling than the one it was published with (N2: `iunique()` renumbers
    /// an evicted vfat/exFAT inode; the case-insensitive re-lookup finds the entry, new inode).
    fn maybe_renumber(&self, p: &Path, st: &mut FsStat) {
        let mapped = self.map(p);
        let same_spelling = |orig: &OsString| {
            p.file_name()
                .is_some_and(|n| n.as_bytes() == orig.as_bytes())
        };
        let renumber = self
            .published
            .lock()
            .unwrap()
            .get(&mapped)
            .is_some_and(|orig| !same_spelling(orig));
        if renumber {
            st.ino = st.ino.wrapping_add(UNSTABLE_INO_OFFSET);
        }
    }

    fn lstat_mapped(&self, path: &Path) -> io::Result<FsStat> {
        let mut st = self.inner.lstat(&self.map(path))?;
        self.maybe_renumber(path, &mut st);
        Ok(st)
    }

    fn stat_mapped(&self, path: &Path) -> io::Result<FsStat> {
        let mut st = self.inner.stat(&self.map(path))?;
        self.maybe_renumber(path, &mut st);
        Ok(st)
    }

    /// Rename into the stick from outside: cross-device.
    fn rename(&self, from: &Path, to: &Path, replace: bool) -> io::Result<()> {
        if self.outside(from) {
            return Err(io::Error::from_raw_os_error(EXDEV));
        }
        let r = if replace {
            self.inner.rename_replace(&self.map(from), &self.map(to))
        } else {
            self.inner.rename_noreplace(&self.map(from), &self.map(to))
        };
        if r.is_ok() {
            self.note(to);
        }
        r
    }
}

impl Fs for FatFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn ofm_jobs::FsFile>> {
        self.inner.open_read(&self.map(path))
    }
    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn ofm_jobs::FsFile>> {
        self.inner.open_read_follow(&self.map(path))
    }
    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn ofm_jobs::FsFile>> {
        self.inner.create_excl(&self.map(path), mode)
    }
    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.stat_mapped(path)
    }
    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        self.lstat_mapped(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        self.inner.read_dir(&self.map(path))
    }
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        let r = self.inner.mkdir(&self.map(path), mode);
        if r.is_ok() {
            self.note(path);
        }
        r
    }
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.rename(from, to, false)
    }
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.rename(from, to, true)
    }
    fn unlink(&self, path: &Path) -> io::Result<()> {
        self.inner.unlink(&self.map(path))
    }
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.inner.rmdir(&self.map(path))
    }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        let r = self.inner.symlink(target, &self.map(link));
        if r.is_ok() {
            self.note(link);
        }
        r
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        let r = self.inner.hard_link(&self.map(from), &self.map(to));
        if r.is_ok() {
            self.note(to);
        }
        r
    }
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner.read_link(&self.map(path))
    }
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.inner.set_mode(&self.map(path), mode)
    }
    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()> {
        self.inner
            .set_times(&self.map(path), atime_ns, mtime_ns, follow)
    }
    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()> {
        self.inner.set_owner(&self.map(path), uid, gid, follow)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(&self.map(path))
    }
    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        self.inner.list_xattrs(&self.map(path), follow)
    }
    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        self.inner.get_xattr(&self.map(path), name, follow)
    }
    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        self.inner.set_xattr(&self.map(path), name, value, follow)
    }
    fn pin(&self, path: &std::path::Path, follow: bool) -> std::io::Result<ofm_jobs::FsPin> {
        self.inner.pin(&self.map(path), follow)
    }
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        self.inner.trash(&self.map(path))
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

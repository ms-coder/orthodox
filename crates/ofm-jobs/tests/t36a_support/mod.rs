//! Helpers for the T3.6a regression tests (`t36a_*.rs`): job runners and [`HookFs`], an `Fs`
//! wrapper that can (a) fold the case of every path below a root, so a plain temp dir behaves like
//! a FAT/exFAT stick (`IMG.JPG` and `img.jpg` are one name), and (b) run a hook before every
//! call, which may change the file system (race simulation) or fail the call with an errno.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_core::Location;
use ofm_jobs::{
    Answer, ConflictAnswer, CopyOpts, ErrorAction, Fs, FsFile, FsStat, JobSpec, Outcome, Question,
    RealFs, Recording, ScriptedSink, TrashError, run_sync,
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

pub fn opts() -> CopyOpts {
    CopyOpts::default()
}

pub fn with_mask(mask: &str) -> CopyOpts {
    CopyOpts {
        rename_mask: Some(mask.into()),
        ..CopyOpts::default()
    }
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

/// Run with an answer closure.
pub fn run_with(
    spec: &JobSpec,
    fs: Arc<dyn Fs>,
    f: impl FnMut(&Question) -> Option<Answer> + Send + 'static,
) -> (Outcome, Rec) {
    let (sink, rec) = ScriptedSink::new(f);
    (run_sync(spec, fs, sink), rec)
}

pub fn questions(rec: &Rec) -> Vec<Question> {
    rec.lock().unwrap().questions.clone()
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

/// Error questions with their raw OS error.
pub fn error_questions(rec: &Rec) -> Vec<(PathBuf, Option<i32>)> {
    questions(rec)
        .into_iter()
        .filter_map(|q| match q {
            Question::Error { path, os_error, .. } => Some((path, os_error)),
            _ => None,
        })
        .collect()
}

// ------------------------------------------------------------------------------- HookFs

/// Hook run before a call: `(operation name, first path, second path)`; `Some(err)` fails the
/// call without delegating. Paths are the ones the engine passed (before case folding).
pub type Hook = Box<dyn Fn(&str, &Path, Option<&Path>) -> Option<io::Error> + Send + Sync>;

/// Delegating [`Fs`] with optional case folding below `fold_root` and a pre-call [`Hook`].
pub struct HookFs {
    inner: Arc<dyn Fs>,
    fold_root: Option<PathBuf>,
    hook: Option<Hook>,
}

impl HookFs {
    pub fn new() -> Self {
        HookFs {
            inner: Arc::new(RealFs),
            fold_root: None,
            hook: None,
        }
    }

    pub fn over(inner: Arc<dyn Fs>) -> Self {
        HookFs {
            inner,
            fold_root: None,
            hook: None,
        }
    }

    /// Below `root`, names are case-insensitive (ASCII folded to lower case on disk).
    pub fn case_insensitive_below(mut self, root: &Path) -> Self {
        self.fold_root = Some(root.to_path_buf());
        self
    }

    pub fn hook(
        mut self,
        f: impl Fn(&str, &Path, Option<&Path>) -> Option<io::Error> + Send + Sync + 'static,
    ) -> Self {
        self.hook = Some(Box::new(f));
        self
    }

    fn map(&self, p: &Path) -> PathBuf {
        let Some(root) = &self.fold_root else {
            return p.to_path_buf();
        };
        match p.strip_prefix(root) {
            Ok(rest) if !rest.as_os_str().is_empty() => {
                let lower = rest.as_os_str().as_bytes().to_ascii_lowercase();
                root.join(OsString::from_vec(lower))
            }
            _ => p.to_path_buf(),
        }
    }

    fn pre(&self, op: &str, a: &Path, b: Option<&Path>) -> io::Result<()> {
        match self.hook.as_ref().and_then(|h| h(op, a, b)) {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Fs for HookFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.pre("open_read", path, None)?;
        self.inner.open_read(&self.map(path))
    }
    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.pre("open_read_follow", path, None)?;
        self.inner.open_read_follow(&self.map(path))
    }
    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        self.pre("create_excl", path, None)?;
        self.inner.create_excl(&self.map(path), mode)
    }
    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.pre("stat", path, None)?;
        self.inner.stat(&self.map(path))
    }
    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        self.pre("lstat", path, None)?;
        self.inner.lstat(&self.map(path))
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        self.pre("read_dir", path, None)?;
        self.inner.read_dir(&self.map(path))
    }
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.pre("mkdir", path, None)?;
        self.inner.mkdir(&self.map(path), mode)
    }
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.pre("rename_noreplace", from, Some(to))?;
        self.inner.rename_noreplace(&self.map(from), &self.map(to))
    }
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.pre("rename_replace", from, Some(to))?;
        self.inner.rename_replace(&self.map(from), &self.map(to))
    }
    fn unlink(&self, path: &Path) -> io::Result<()> {
        self.pre("unlink", path, None)?;
        self.inner.unlink(&self.map(path))
    }
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.pre("rmdir", path, None)?;
        self.inner.rmdir(&self.map(path))
    }
    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        self.pre("symlink", link, Some(target))?;
        self.inner.symlink(target, &self.map(link))
    }
    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.pre("hard_link", from, Some(to))?;
        self.inner.hard_link(&self.map(from), &self.map(to))
    }
    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.pre("read_link", path, None)?;
        self.inner.read_link(&self.map(path))
    }
    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.pre("set_mode", path, None)?;
        self.inner.set_mode(&self.map(path), mode)
    }
    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()> {
        self.pre("set_times", path, None)?;
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
        self.pre("set_owner", path, None)?;
        self.inner.set_owner(&self.map(path), uid, gid, follow)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.pre("sync_dir", path, None)?;
        self.inner.sync_dir(&self.map(path))
    }
    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        self.pre("list_xattrs", path, None)?;
        self.inner.list_xattrs(&self.map(path), follow)
    }
    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        self.pre("get_xattr", path, None)?;
        self.inner.get_xattr(&self.map(path), name, follow)
    }
    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        self.pre("set_xattr", path, None)?;
        self.inner.set_xattr(&self.map(path), name, value, follow)
    }
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        self.pre("trash", path, None)?;
        self.inner.trash(&self.map(path))
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

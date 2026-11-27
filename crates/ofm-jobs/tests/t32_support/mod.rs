//! Shared helpers of the T3.2 tests: a delegating `Fs` with a private trash directory and an
//! operation hook (used to cancel in the middle of a walk). Never touches the real trash.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ofm_jobs::ops::trash::{TrashEnv, trash_path_with};
use ofm_jobs::{
    Answer, ErrorAction, Fs, FsFile, FsPin, FsStat, Outcome, Question, RealFs, ScriptedSink,
    TrashError,
};

/// What `Fs::trash` does.
pub enum TrashMode {
    /// Really move into the private trash.
    Real(TrashEnv),
    /// Always `Unavailable`.
    Unavailable,
}

type Hook = Box<dyn Fn(&'static str) + Send + Sync>;

/// `RealFs` with a private trash and an optional per-operation hook (`"lstat"`, `"unlink"`, ...).
pub struct TestFs {
    inner: RealFs,
    trash: TrashMode,
    hook: Option<Hook>,
}

impl TestFs {
    pub fn with_trash(home_trash: &Path) -> Self {
        TestFs {
            inner: RealFs,
            trash: TrashMode::Real(TrashEnv {
                home_trash: home_trash.to_path_buf(),
                uid: rustix::process::getuid().as_raw(),
            }),
            hook: None,
        }
    }

    pub fn trash_unavailable() -> Self {
        TestFs {
            inner: RealFs,
            trash: TrashMode::Unavailable,
            hook: None,
        }
    }

    pub fn real() -> Self {
        Self::trash_unavailable()
    }

    pub fn hooked(mut self, f: impl Fn(&'static str) + Send + Sync + 'static) -> Self {
        self.hook = Some(Box::new(f));
        self
    }

    fn hit(&self, op: &'static str) {
        if let Some(h) = &self.hook {
            h(op);
        }
    }
}

impl Fs for TestFs {
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
        self.hit("stat");
        self.inner.stat(path)
    }
    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        self.hit("lstat");
        self.inner.lstat(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        self.hit("read_dir");
        self.inner.read_dir(path)
    }
    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.hit("mkdir");
        self.inner.mkdir(path, mode)
    }
    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename_noreplace(from, to)
    }
    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename_replace(from, to)
    }
    fn unlink(&self, path: &Path) -> io::Result<()> {
        self.hit("unlink");
        self.inner.unlink(path)
    }
    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.hit("rmdir");
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
        self.hit("set_mode");
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
        self.hit("set_owner");
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
    fn pin(&self, path: &Path, follow: bool) -> io::Result<FsPin> {
        self.inner.pin(path, follow)
    }
    fn trash(&self, path: &Path) -> Result<(), TrashError> {
        self.hit("trash");
        match &self.trash {
            TrashMode::Real(env) => trash_path_with(env, path),
            TrashMode::Unavailable => Err(TrashError::Unavailable("test: no trash".into())),
        }
    }
    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

/// Shared handle to the recording of a `ScriptedSink`.
pub type Rec = Arc<Mutex<ofm_jobs::Recording>>;

/// Answer every error with `action` (not remembered: each failure asks).
pub fn error_policy(action: ErrorAction) -> (ScriptedSink, Rec) {
    ScriptedSink::new(move |q| match q {
        Question::Error { .. } => Some(Answer::Error {
            action,
            apply_to_all: false,
        }),
        _ => None,
    })
}

/// Answer the trash-unavailable question.
pub fn trash_policy(permanent: bool, apply_to_all: bool) -> (ScriptedSink, Rec) {
    ScriptedSink::new(move |q| match q {
        Question::TrashUnavailable { .. } => Some(Answer::Confirm {
            permanent,
            apply_to_all,
        }),
        _ => None,
    })
}

/// A sink that must never be asked anything.
pub fn no_questions() -> (ScriptedSink, Rec) {
    ScriptedSink::refusing()
}

pub fn questions(rec: &Rec) -> Vec<Question> {
    rec.lock().unwrap().questions.clone()
}

pub fn report(out: &Outcome) -> &ofm_jobs::Report {
    out.report().expect("outcome has a report")
}

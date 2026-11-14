//! Self-cleaning temporary directories (no external dependencies).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A directory removed recursively (best effort) when dropped.
///
/// Uniqueness comes from the process id, a process-wide counter and a random
/// hash seed, so no `tempfile` dependency is needed. Tests that need the
/// directory to outlive the guard can call [`TempDir::into_path`].
///
/// # Example
///
/// ```
/// use ofm_testkit::temp::TempDir;
///
/// let dir = TempDir::with_name("my-test").unwrap();
/// assert!(dir.path().is_dir());
/// let path = dir.into_path(); // keep it
/// assert!(path.is_dir());
/// std::fs::remove_dir_all(path).unwrap();
/// ```
#[derive(Debug)]
pub struct TempDir {
    path: PathBuf,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempDir {
    /// New directory under [`std::env::temp_dir`] with the default prefix `ofm`.
    pub fn new() -> io::Result<Self> {
        Self::in_dir_named(&std::env::temp_dir(), "ofm")
    }

    /// New directory under [`std::env::temp_dir`] with the given name prefix.
    pub fn with_name(prefix: &str) -> io::Result<Self> {
        Self::in_dir_named(&std::env::temp_dir(), prefix)
    }

    /// New directory inside `parent` with the default prefix `ofm`.
    ///
    /// `parent` must already exist; it is NOT created (use
    /// [`std::fs::create_dir_all`] first when probing candidate roots).
    pub fn in_dir(parent: &Path) -> io::Result<Self> {
        Self::in_dir_named(parent, "ofm")
    }

    /// New directory inside `parent`, named `<prefix>-<pid>-<counter>-<random>`.
    pub fn in_dir_named(parent: &Path, prefix: &str) -> io::Result<Self> {
        let mut last: Option<io::Error> = None;
        for _ in 0..8 {
            let path = parent.join(unique_name(prefix));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => last = Some(e),
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::other(format!(
            "could not create a unique dir under {}: {}",
            parent.display(),
            last.map(|e| e.to_string()).unwrap_or_default()
        )))
    }

    /// Path of the directory.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Give up cleanup; the directory survives this [`TempDir`].
    pub fn into_path(mut self) -> PathBuf {
        let path = std::mem::take(&mut self.path);
        std::mem::forget(self);
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Best effort only: a test must not fail because cleanup raced with a
        // watcher or a read-only entry refused deletion.
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// `<prefix>-<pid>-<counter>-<random-hex>`; the random part comes from the
/// per-process random seed of `RandomState`, good enough for test dirs.
fn unique_name(prefix: &str) -> String {
    use std::hash::{BuildHasher, Hasher, RandomState};

    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut h = RandomState::new().build_hasher();
    h.write_u64(n);
    h.write_u64(u64::from(std::process::id()));
    format!("{prefix}-{}-{n}-{:016x}", std::process::id(), h.finish())
}

//! Handle-anchored file system access below one root directory (FIX-M6M7 #5, ADR-0004 "Anchored
//! access").
//!
//! A path such as `dest/d/f` is resolved by the kernel again on every call, so whoever can rename
//! inside `dest` can replace the already checked `dest/d` with a symlink and redirect the next
//! call. [`AnchoredFs`] wraps an [`Fs`] and removes that re-resolution: every directory below the
//! root is opened once, component by component, without following symlinks ([`Fs::pin`]), and the
//! operation is then performed on `/proc/self/fd/<handle>/<name>`. The kernel jumps straight to the
//! pinned directory object, whatever its path means by now. Callers keep using ordinary paths (so
//! messages, questions and reports show real names).
//!
//! Rules:
//! - the root path itself is opened FOLLOWING symlinks, once: the user's chosen destination may
//!   legitimately go through them; nothing below it is ever followed;
//! - changing calls outside the root, or with `..` below it, are refused;
//! - `stat` and `open_read_follow` ("follow" queries) and every read-only call outside the root
//!   pass through unchanged;
//! - calls that follow their last component (`chmod`, ...) pin that object `O_NOFOLLOW` first and
//!   refuse a symlink.
//!
//! Descriptors are bounded: an LRU cache of [`CACHE_DIRS`] directory handles. A handle the caller
//! [`hold`](AnchoredFs::hold)s is never evicted, so all calls of one item reach the same directory.
//! An evicted directory is opened again from the root, again without following symlinks.

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use super::{Fs, FsFile, FsStat, TrashError, real};
use crate::copy::fold_path;

/// Directory handles kept open (apart from held ones and the root).
const CACHE_DIRS: usize = 32;

/// Linux `PATH_MAX`: the short `/proc` paths must not let the job create files whose real path no
/// other program could use.
const PATH_MAX: usize = 4096;

/// `ENAMETOOLONG`.
const ENAMETOOLONG: i32 = 36;

/// A pinned file system object: an `O_PATH` descriptor, obtained from [`Fs::pin`]. It keeps
/// referring to the same object when the path it was opened by is renamed or replaced.
#[derive(Debug)]
pub struct FsPin {
    fd: OwnedFd,
    path: PathBuf,
}

impl FsPin {
    /// Wrap a descriptor (usually opened with `O_PATH`).
    pub fn new(fd: OwnedFd) -> Self {
        let path = PathBuf::from(format!("/proc/self/fd/{}", fd.as_raw_fd()));
        FsPin { fd, path }
    }

    /// A path that names the pinned object itself (`/proc/self/fd/N`), valid while `self` lives.
    /// For a pinned directory, `path().join(name)` addresses `name` inside it.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `fstat` of the pinned object.
    pub fn stat(&self) -> io::Result<FsStat> {
        Ok(real::convert(&rustix::fs::fstat(&self.fd)?))
    }

    /// Fail unless [`path`](Self::path) really leads to the pinned object (`/proc` not mounted).
    fn check_reachable(&self) -> io::Result<()> {
        let here = self.stat()?;
        let there = rustix::fs::stat(&self.path).map(|st| real::convert(&st));
        match there {
            Ok(st) if (st.dev, st.ino) == (here.dev, here.ino) => Ok(()),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "/proc/self/fd is not usable (is /proc mounted?)",
            )),
        }
    }
}

/// A path to hand to the inner `Fs`, together with the handle that keeps it meaningful.
struct At {
    _dir: Option<Arc<FsPin>>,
    path: PathBuf,
}

/// One cached directory handle.
struct Cached {
    dir: PathBuf,
    /// `dir` case-folded, for [`AnchoredFs::forget`].
    folded: PathBuf,
    pin: Arc<FsPin>,
}

/// An [`Fs`] whose access below `root` goes through pinned directory handles.
pub(crate) struct AnchoredFs {
    inner: Arc<dyn Fs>,
    root: PathBuf,
    root_pin: Arc<FsPin>,
    /// Least recently used first.
    cache: Mutex<Vec<Cached>>,
}

fn outside(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("refusing to change '{}' outside the root", path.display()),
    )
}

fn not_followed(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotADirectory,
        format!(
            "'{}' was replaced by a symlink or file; not followed",
            path.display()
        ),
    )
}

/// Whether `path` is `dir` or lies below it, judged on the bytes. Every path the cache sees is
/// built with `join`/`parent` from the same root, so one directory has one spelling; comparing
/// components instead is far too slow for deep (hostile) trees. A different spelling would only be
/// a cache miss, answered by another no-follow walk.
fn is_at_or_below(path: &Path, dir: &Path) -> bool {
    path.as_os_str()
        .as_bytes()
        .strip_prefix(dir.as_os_str().as_bytes())
        .is_some_and(|rest| rest.is_empty() || rest[0] == b'/')
}

fn check_length(path: &Path) -> io::Result<()> {
    if path.as_os_str().len() >= PATH_MAX {
        return Err(io::Error::from_raw_os_error(ENAMETOOLONG));
    }
    Ok(())
}

impl AnchoredFs {
    /// Pin the existing directory `root` (symlinks in `root` itself are followed).
    pub(crate) fn new(inner: Arc<dyn Fs>, root: &Path) -> io::Result<Self> {
        let pin = inner.pin(root, true)?;
        if !pin.stat()?.is_dir() {
            return Err(io::Error::from(io::ErrorKind::NotADirectory));
        }
        pin.check_reachable()?;
        Ok(AnchoredFs {
            inner,
            root: root.to_path_buf(),
            root_pin: Arc::new(pin),
            cache: Mutex::new(Vec::new()),
        })
    }

    /// Pin the directory `dir` (the root or below it) for as long as the result lives: every
    /// call below `dir` in that time uses exactly this handle.
    pub(crate) fn hold(&self, dir: &Path) -> io::Result<Arc<FsPin>> {
        self.dir(dir)
    }

    /// `path` strictly below the root through plain names only.
    fn is_below(&self, path: &Path) -> bool {
        path.strip_prefix(&self.root).is_ok_and(|rel| {
            let mut comps = rel.components().peekable();
            comps.peek().is_some() && comps.all(|c| matches!(c, Component::Normal(_)))
        })
    }

    /// A handle already open for `dir`.
    fn cached(&self, dir: &Path) -> Option<Arc<FsPin>> {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        let i = cache
            .iter()
            .position(|c| c.dir.as_os_str() == dir.as_os_str())?;
        let hit = cache.remove(i);
        let pin = Arc::clone(&hit.pin);
        cache.push(hit);
        Some(pin)
    }

    /// The open handle closest above `dir`.
    fn nearest(&self, dir: &Path) -> Option<(PathBuf, Arc<FsPin>)> {
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .filter(|c| is_at_or_below(dir, &c.dir))
            .max_by_key(|c| c.dir.as_os_str().len())
            .map(|c| (c.dir.clone(), Arc::clone(&c.pin)))
    }

    fn remember(&self, dir: &Path, pin: Arc<FsPin>) {
        let mut cache = self.cache.lock().unwrap_or_else(PoisonError::into_inner);
        cache.push(Cached {
            dir: dir.to_path_buf(),
            folded: fold_path(dir),
            pin,
        });
        while cache.len() > CACHE_DIRS {
            // Held handles (count > 1) stay.
            let Some(i) = cache.iter().position(|c| Arc::strong_count(&c.pin) == 1) else {
                break;
            };
            cache.remove(i);
        }
    }

    /// The job itself changed what `path` names: handles at or below it are stale. Judged
    /// ignoring case, because on vfat `foo` and `FOO` are one directory; dropping a handle too
    /// many only means opening it again.
    fn forget(&self, path: &Path) {
        let folded = fold_path(path);
        self.cache
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|c| !is_at_or_below(&c.folded, &folded));
    }

    /// The handle of directory `dir`: cached, or opened component by component from the nearest
    /// cached ancestor (or the root), never following a symlink.
    fn dir(&self, dir: &Path) -> io::Result<Arc<FsPin>> {
        if dir == self.root {
            return Ok(Arc::clone(&self.root_pin));
        }
        if !self.is_below(dir) {
            return Err(outside(dir));
        }
        if let Some(pin) = self.cached(dir) {
            return Ok(pin);
        }
        let (mut done, mut pin) = self
            .nearest(dir)
            .unwrap_or_else(|| (self.root.clone(), Arc::clone(&self.root_pin)));
        let rest = dir.strip_prefix(&done).map_err(|_| outside(dir))?;
        for comp in rest.components() {
            done.push(comp);
            let next = self.inner.pin(&pin.path().join(comp), false)?;
            if !next.stat()?.is_dir() {
                return Err(not_followed(&done));
            }
            pin = Arc::new(next);
        }
        self.remember(dir, Arc::clone(&pin));
        Ok(pin)
    }

    /// The anchored path of an entry below the root (for calls that do not follow it).
    fn at(&self, path: &Path) -> io::Result<At> {
        check_length(path)?;
        if !self.is_below(path) {
            return Err(outside(path));
        }
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(outside(path));
        };
        let dir = self.dir(parent)?;
        let io_path = dir.path().join(name);
        Ok(At {
            _dir: Some(dir),
            path: io_path,
        })
    }

    /// For read-only calls: anchored below the root, unchanged elsewhere.
    fn view(&self, path: &Path) -> io::Result<At> {
        if self.is_below(path) {
            return self.at(path);
        }
        if path == self.root {
            return Ok(At {
                _dir: Some(Arc::clone(&self.root_pin)),
                path: self.root_pin.path().to_path_buf(),
            });
        }
        Ok(At {
            _dir: None,
            path: path.to_path_buf(),
        })
    }

    /// The object at `path` itself, for calls that would follow a symlink: a directory the job
    /// already has open is that directory; anything else is pinned now and must not be a symlink.
    fn object(&self, path: &Path) -> io::Result<Arc<FsPin>> {
        if let Some(pin) = self.cached(path) {
            return Ok(pin);
        }
        let at = self.at(path)?;
        let pin = self.inner.pin(&at.path, false)?;
        if pin.stat()?.is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("'{}' is a symlink; not followed", path.display()),
            ));
        }
        Ok(Arc::new(pin))
    }

    /// For a call with a `follow` flag: the path and flag to pass to the inner `Fs`.
    fn target(&self, path: &Path, follow: bool) -> io::Result<(At, bool)> {
        if follow || self.cached(path).is_some() {
            let pin = self.object(path)?;
            let io_path = pin.path().to_path_buf();
            // Following the `/proc` link lands on the pinned object and goes no further.
            let at = At {
                _dir: Some(pin),
                path: io_path,
            };
            return Ok((at, true));
        }
        Ok((self.at(path)?, false))
    }
}

impl Fs for AnchoredFs {
    fn open_read(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.inner.open_read(&self.view(path)?.path)
    }

    fn open_read_follow(&self, path: &Path) -> io::Result<Box<dyn FsFile>> {
        self.inner.open_read_follow(path)
    }

    fn create_excl(&self, path: &Path, mode: u32) -> io::Result<Box<dyn FsFile>> {
        self.inner.create_excl(&self.at(path)?.path, mode)
    }

    fn stat(&self, path: &Path) -> io::Result<FsStat> {
        self.inner.stat(path)
    }

    fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        if path == self.root {
            return self.inner.stat(self.root_pin.path());
        }
        self.inner.lstat(&self.view(path)?.path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<OsString>> {
        if path == self.root || self.is_below(path) {
            return self.inner.read_dir(self.dir(path)?.path());
        }
        self.inner.read_dir(path)
    }

    fn mkdir(&self, path: &Path, mode: u32) -> io::Result<()> {
        self.inner.mkdir(&self.at(path)?.path, mode)?;
        self.forget(path);
        Ok(())
    }

    fn rename_noreplace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (a, b) = (self.at(from)?, self.at(to)?);
        self.inner.rename_noreplace(&a.path, &b.path)?;
        self.forget(from);
        self.forget(to);
        Ok(())
    }

    fn rename_replace(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (a, b) = (self.at(from)?, self.at(to)?);
        self.inner.rename_replace(&a.path, &b.path)?;
        self.forget(from);
        self.forget(to);
        Ok(())
    }

    fn unlink(&self, path: &Path) -> io::Result<()> {
        self.inner.unlink(&self.at(path)?.path)?;
        self.forget(path);
        Ok(())
    }

    fn rmdir(&self, path: &Path) -> io::Result<()> {
        self.inner.rmdir(&self.at(path)?.path)?;
        self.forget(path);
        Ok(())
    }

    fn symlink(&self, target: &Path, link: &Path) -> io::Result<()> {
        // The target is link text, stored verbatim.
        self.inner.symlink(target, &self.at(link)?.path)
    }

    fn hard_link(&self, from: &Path, to: &Path) -> io::Result<()> {
        let (a, b) = (self.at(from)?, self.at(to)?);
        self.inner.hard_link(&a.path, &b.path)
    }

    fn read_link(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner.read_link(&self.view(path)?.path)
    }

    fn set_mode(&self, path: &Path, mode: u32) -> io::Result<()> {
        let pin = self.object(path)?;
        self.inner.set_mode(pin.path(), mode)
    }

    fn set_times(
        &self,
        path: &Path,
        atime_ns: i128,
        mtime_ns: i128,
        follow: bool,
    ) -> io::Result<()> {
        let (at, follow) = self.target(path, follow)?;
        self.inner.set_times(&at.path, atime_ns, mtime_ns, follow)
    }

    fn set_owner(
        &self,
        path: &Path,
        uid: Option<u32>,
        gid: Option<u32>,
        follow: bool,
    ) -> io::Result<()> {
        let (at, follow) = self.target(path, follow)?;
        self.inner.set_owner(&at.path, uid, gid, follow)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(self.dir(path)?.path())
    }

    fn list_xattrs(&self, path: &Path, follow: bool) -> io::Result<Vec<OsString>> {
        if self.is_below(path) {
            let (at, follow) = self.target(path, follow)?;
            return self.inner.list_xattrs(&at.path, follow);
        }
        self.inner.list_xattrs(path, follow)
    }

    fn get_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<Option<Vec<u8>>> {
        if self.is_below(path) {
            let (at, follow) = self.target(path, follow)?;
            return self.inner.get_xattr(&at.path, name, follow);
        }
        self.inner.get_xattr(path, name, follow)
    }

    fn set_xattr(&self, path: &Path, name: &OsStr, value: &[u8], follow: bool) -> io::Result<()> {
        let (at, follow) = self.target(path, follow)?;
        self.inner.set_xattr(&at.path, name, value, follow)
    }

    fn remove_xattr(&self, path: &Path, name: &OsStr, follow: bool) -> io::Result<()> {
        let (at, follow) = self.target(path, follow)?;
        self.inner.remove_xattr(&at.path, name, follow)
    }

    fn pin(&self, path: &Path, follow: bool) -> io::Result<FsPin> {
        if follow {
            return Err(outside(path));
        }
        self.inner.pin(&self.at(path)?.path, false)
    }

    fn trash(&self, _path: &Path) -> Result<(), TrashError> {
        // The trash records the original path, which an anchored path is not.
        Err(TrashError::Io(io::Error::new(
            io::ErrorKind::Unsupported,
            "trash is not available through an anchored file system",
        )))
    }

    fn is_root(&self) -> bool {
        self.inner.is_root()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::*;
    use crate::fs::RealFs;
    use ofm_testkit::temp::TempDir;

    /// `<tmp>/root` (anchored) and `<tmp>/out` next to it.
    fn setup() -> (TempDir, PathBuf, PathBuf, AnchoredFs) {
        let t = TempDir::new().unwrap();
        let (root, out) = (t.path().join("root"), t.path().join("out"));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&out).unwrap();
        let fs = AnchoredFs::new(Arc::new(RealFs), &root).unwrap();
        (t, root, out, fs)
    }

    fn count(dir: &Path) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    #[test]
    fn changes_outside_the_root_or_through_dot_dot_are_refused() {
        let (_t, root, out, fs) = setup();
        for p in [
            out.join("x"),
            root.clone(),
            root.join("../out/x"),
            root.join("a/../../out/x"),
        ] {
            assert!(fs.mkdir(&p, 0o700).is_err(), "{p:?}");
            assert!(fs.create_excl(&p, 0o600).is_err(), "{p:?}");
            assert!(fs.unlink(&p).is_err(), "{p:?}");
            assert!(fs.set_mode(&p, 0o777).is_err(), "{p:?}");
        }
        assert_eq!(count(&out), 0);
        // Read-only queries elsewhere still work.
        assert!(fs.lstat(&out).unwrap().is_dir());
        assert!(fs.lstat(&root).unwrap().is_dir());
    }

    #[test]
    fn no_symlink_below_the_root_is_followed() {
        let (_t, root, out, fs) = setup();
        std::fs::write(out.join("victim"), b"v").unwrap();
        std::fs::set_permissions(out.join("victim"), std::fs::Permissions::from_mode(0o600))
            .unwrap();
        symlink(&out, root.join("l")).unwrap();
        symlink(out.join("victim"), root.join("f")).unwrap();
        assert!(fs.create_excl(&root.join("l/new"), 0o600).is_err());
        assert!(fs.mkdir(&root.join("l/new"), 0o700).is_err());
        assert!(fs.unlink(&root.join("l/victim")).is_err());
        assert!(fs.lstat(&root.join("l/victim")).is_err());
        assert!(fs.sync_dir(&root.join("l")).is_err());
        // Following calls refuse a symlink as the last component.
        assert!(fs.set_mode(&root.join("f"), 0o777).is_err());
        assert!(fs.set_times(&root.join("f"), 1, 1, true).is_err());
        assert_eq!(count(&out), 1);
        let st = std::fs::metadata(out.join("victim")).unwrap();
        assert_eq!(st.permissions().mode() & 0o7777, 0o600);
    }

    #[test]
    fn held_directories_survive_eviction_and_the_cache_stays_bounded() {
        let (_t, root, out, fs) = setup();
        fs.mkdir(&root.join("d"), 0o700).unwrap();
        let hold = fs.hold(&root.join("d")).unwrap();
        std::fs::rename(root.join("d"), root.join("moved")).unwrap();
        symlink(&out, root.join("d")).unwrap();
        for i in 0..3 * CACHE_DIRS {
            let p = root.join(format!("x{i}"));
            fs.mkdir(&p, 0o700).unwrap();
            fs.create_excl(&p.join("f"), 0o600).unwrap();
        }
        assert!(fs.cache.lock().unwrap().len() <= CACHE_DIRS);
        fs.create_excl(&root.join("d/held"), 0o600).unwrap();
        assert!(root.join("moved/held").exists());
        drop(hold);
        for i in 0..3 * CACHE_DIRS {
            fs.sync_dir(&root.join(format!("x{i}"))).unwrap();
        }
        // Opened again from the root: the symlink is refused.
        assert!(fs.create_excl(&root.join("d/late"), 0o600).is_err());
        assert_eq!(count(&out), 0);
    }

    #[test]
    fn the_jobs_own_renames_drop_stale_handles() {
        let (_t, root, _out, fs) = setup();
        let d = root.join("d");
        fs.mkdir(&d, 0o700).unwrap();
        fs.create_excl(&d.join("a"), 0o600).unwrap();
        fs.rename_noreplace(&d, &root.join("aside")).unwrap();
        fs.mkdir(&d, 0o700).unwrap();
        fs.create_excl(&d.join("b"), 0o600).unwrap();
        assert!(d.join("b").exists());
        assert!(!root.join("aside/b").exists());
    }

    /// vfat: renaming `FOO` renames `foo`.
    #[test]
    fn renames_drop_handles_of_every_spelling() {
        let (_t, root, _out, fs) = setup();
        fs.mkdir(&root.join("foo"), 0o700).unwrap();
        fs.mkdir(&root.join("FOO"), 0o700).unwrap();
        fs.sync_dir(&root.join("foo")).unwrap();
        assert!(fs.cached(&root.join("foo")).is_some());
        fs.rename_noreplace(&root.join("FOO"), &root.join("aside"))
            .unwrap();
        assert!(fs.cached(&root.join("foo")).is_none());
    }

    #[test]
    fn very_long_paths_are_still_too_long() {
        let (_t, root, _out, fs) = setup();
        let mut p = root.clone();
        while p.as_os_str().len() < PATH_MAX {
            p.push("n".repeat(200));
        }
        assert_eq!(fs.lstat(&p).unwrap_err().raw_os_error(), Some(ENAMETOOLONG));
    }
}

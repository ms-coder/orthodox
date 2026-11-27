//! Checked directories for the walks that change or remove what they find (M9, review finding
//! m1; ADR-0004 "Checked directory walks"): permanent delete, recursive chmod/chown and the
//! source side of a move.
//!
//! A path such as `tree/d/a` is resolved by the kernel again on every call, so a `tree/d` that
//! was swapped for a symlink after the walk looked at it redirects the next `unlink` or `chmod`.
//! A [`CheckedDir`] is a directory together with the `(dev, ino)` the walk saw. Every operation
//! on an entry of it:
//!
//! 1. takes a handle on the directory ([`AnchoredFs`]: opened component by component below the
//!    walk's root, never following a symlink);
//! 2. checks that the handle is the recorded directory AND that its path still leads there (a
//!    directory moved out of the selected tree is left alone, too);
//! 3. acts on `/proc/self/fd/<handle>/<name>`, so a swap after the check changes nothing.
//!
//! The walk's root is the folder holding the selected item; it is resolved by name, following
//! symlinks, when the item is opened. All calls go through the job's [`Fs`].

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use super::{AnchoredFs, Fs, FsPin, FsStat};

/// Down to this depth the handle of every directory the walk is inside stays open, so its inode
/// (and thereby its number, unstable on vfat once evicted) cannot go away. Deeper levels are
/// opened again when needed and checked the same way.
const HOLD_DEPTH: u32 = 128;

struct Tree {
    fs: Arc<dyn Fs>,
    anchored: AnchoredFs,
}

/// A directory a walk has looked at; see the module documentation.
#[derive(Clone)]
pub(crate) struct CheckedDir {
    tree: Rc<Tree>,
    path: PathBuf,
    id: (u64, u64),
    /// 0 = the walk's root.
    depth: u32,
    _held: Option<Arc<FsPin>>,
}

fn replaced(path: &Path) -> io::Error {
    io::Error::other(format!(
        "'{}' was replaced or moved meanwhile; not touched",
        path.display()
    ))
}

fn no_name(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("'{}' does not name an entry of a folder", path.display()),
    )
}

/// `item` split into the folder that holds it and the item's path below exactly that spelling.
fn split(item: &Path) -> io::Result<(PathBuf, PathBuf)> {
    let item: PathBuf = item.components().collect();
    let name = item.file_name().ok_or_else(|| no_name(&item))?;
    let root = match item.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };
    let item = root.join(name);
    Ok((root, item))
}

impl CheckedDir {
    /// Open the folder `root` as the root of a walk (symlinks in `root` itself are followed).
    fn root(fs: &Arc<dyn Fs>, root: PathBuf) -> io::Result<CheckedDir> {
        let anchored = AnchoredFs::new(Arc::clone(fs), &root)?;
        let pin = anchored.hold(&root)?;
        let st = pin.stat()?;
        Ok(CheckedDir {
            tree: Rc::new(Tree {
                fs: Arc::clone(fs),
                anchored,
            }),
            path: root,
            id: (st.dev, st.ino),
            depth: 0,
            _held: Some(pin),
        })
    }

    /// The folder that holds `item`, and the path to use for `item` with it.
    pub(crate) fn holding(fs: &Arc<dyn Fs>, item: &Path) -> io::Result<(CheckedDir, PathBuf)> {
        let (root, item) = split(item)?;
        Ok((Self::root(fs, root)?, item))
    }

    /// The handle of this directory, checked. A directory that is no longer found is never
    /// `NotFound` to the caller: that would read as "the entry vanished, nothing left to do",
    /// while the directory and its files may only have been moved elsewhere.
    fn pin(&self) -> io::Result<Arc<FsPin>> {
        let checked = self.tree.anchored.hold(&self.path).and_then(|pin| {
            let held = pin.stat()?;
            // The root may legitimately be reached through symlinks; nothing below it.
            let named = if self.depth == 0 {
                self.tree.fs.stat(&self.path)?
            } else {
                self.tree.fs.lstat(&self.path)?
            };
            if (held.dev, held.ino) == self.id && (named.dev, named.ino) == self.id {
                Ok(pin)
            } else {
                Err(replaced(&self.path))
            }
        });
        checked.map_err(|e| {
            // Open it afresh next time (the user may retry).
            self.tree.anchored.forget(&self.path);
            if e.kind() == io::ErrorKind::NotFound {
                replaced(&self.path)
            } else {
                e
            }
        })
    }

    /// Enter the subdirectory `path` (an entry of this one) which `lstat` reported as `st`.
    pub(crate) fn child(&self, path: &Path, st: &FsStat) -> io::Result<CheckedDir> {
        self.pin()?;
        let mut child = CheckedDir {
            tree: Rc::clone(&self.tree),
            path: path.to_path_buf(),
            id: (st.dev, st.ino),
            depth: self.depth.saturating_add(1),
            _held: None,
        };
        let pin = child.pin()?;
        if child.depth <= HOLD_DEPTH {
            child._held = Some(pin);
        }
        Ok(child)
    }

    /// The file system of the walk (for calls on a handle from [`pin_entry`](Self::pin_entry)).
    pub(crate) fn fs(&self) -> &dyn Fs {
        &*self.tree.fs
    }

    /// Names in this directory.
    pub(crate) fn list(&self) -> io::Result<Vec<OsString>> {
        let pin = self.pin()?;
        self.tree.fs.read_dir(pin.path())
    }

    /// Run `op` on the entry `path` of this directory. `op` gets the path to use for it.
    pub(crate) fn entry<T>(
        &self,
        path: &Path,
        op: impl FnOnce(&dyn Fs, &Path) -> io::Result<T>,
    ) -> io::Result<T> {
        let name = path.file_name().ok_or_else(|| no_name(path))?;
        let pin = self.pin()?;
        op(&*self.tree.fs, &pin.path().join(name))
    }

    /// `lstat` of the entry `path`.
    pub(crate) fn lstat(&self, path: &Path) -> io::Result<FsStat> {
        self.entry(path, |fs, p| fs.lstat(p))
    }

    /// A handle on the very object `lstat` reported as `st` under `path`, for calls by path that
    /// follow symlinks: use [`FsPin::path`] with a following call. For a symlink `st` the handle
    /// is the link itself, and following the handle's path ends there.
    pub(crate) fn pin_entry(&self, path: &Path, st: &FsStat) -> io::Result<FsPin> {
        self.entry(path, |fs, p| {
            let pin = fs.pin(p, false)?;
            let now = pin.stat()?;
            if (now.dev, now.ino) == (st.dev, st.ino) && now.file_type() == st.file_type() {
                Ok(pin)
            } else {
                Err(replaced(path))
            }
        })
    }
}

/// The folder holding the selected items of a job, opened once for items that share it, so that
/// it cannot be swapped between two of them either.
#[derive(Default)]
pub(crate) struct Selection {
    last: Option<CheckedDir>,
}

impl Selection {
    /// Like [`CheckedDir::holding`].
    pub(crate) fn open(
        &mut self,
        fs: &Arc<dyn Fs>,
        item: &Path,
    ) -> io::Result<(CheckedDir, PathBuf)> {
        let (root, item) = split(item)?;
        if let Some(dir) = self.last.as_ref().filter(|d| d.path == root) {
            return Ok((dir.clone(), item));
        }
        let dir = CheckedDir::root(fs, root)?;
        self.last = Some(dir.clone());
        Ok((dir, item))
    }
}

/// Run `op` on `path`: through `parent` when the walk has one for it, else on the path itself
/// (a selected item, resolved by name).
pub(crate) fn within<T>(
    fs: &dyn Fs,
    parent: Option<&CheckedDir>,
    path: &Path,
    op: impl FnOnce(&dyn Fs, &Path) -> io::Result<T>,
) -> io::Result<T> {
    match parent {
        Some(dir) => dir.entry(path, op),
        None => op(fs, path),
    }
}

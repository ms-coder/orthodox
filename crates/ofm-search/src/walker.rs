//! Iterative directory walker (private): one thread, explicit stack, no recursion.
//!
//! Per directory, entries are read with `std::fs::read_dir`, sorted by name bytes and
//! visited in that order (stable, deterministic results). Traversal rules — hidden names,
//! `max_depth`, `same_filesystem` (`st_dev` vs the root), symlink following with
//! (dev, ino) loop detection — are applied here; *reporting* filters (name mask, size,
//! mtime) are the engine's business and never stop the walk.
//!
//! Depth convention: the root's direct children are depth 0; `max_depth = Some(0)` visits
//! exactly those, `Some(1)` one level deeper, and so on.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use ofm_core::{CancelToken, EntryKind, TargetKind};

/// Traversal options (from the query).
pub(crate) struct WalkOptions {
    /// Follow symlinks (loops are detected and warned).
    pub(crate) follow_symlinks: bool,
    /// Do not cross into other file systems.
    pub(crate) same_filesystem: bool,
    /// Include dot-names.
    pub(crate) include_hidden: bool,
    /// Maximum depth below the root (`None` = unlimited).
    pub(crate) max_depth: Option<u32>,
}

/// A root, pre-statted on the caller's thread: the followed metadata plus whether the root
/// path itself is a symlink.
pub(crate) struct RootInfo {
    /// The root path as given.
    pub(crate) path: PathBuf,
    /// `fs::metadata` (a symlinked root is followed; the root is the user's choice).
    pub(crate) md: Metadata,
    /// The root path is itself a symlink.
    pub(crate) is_symlink: bool,
}

/// One visited entry.
pub(crate) struct Walked {
    /// Full path.
    pub(crate) path: PathBuf,
    /// File name only.
    pub(crate) name: OsString,
    /// Kind (a symlink's target kind resolved for display; `None` target = broken link).
    pub(crate) kind: EntryKind,
    /// Metadata matching the reporting semantics: the symlink's own lstat when not
    /// following, the target's stat when following (like `find -L`).
    pub(crate) md: Metadata,
}

/// Walk `roots`, calling `on_dir` when a directory is opened (progress), `on_entry` per
/// entry (return `false` to stop) and `on_warn` for non-fatal problems. Returns `false` if
/// the walk stopped early (cancelled, or a callback asked to stop).
pub(crate) fn walk(
    roots: &[RootInfo],
    opts: &WalkOptions,
    cancel: &CancelToken,
    on_dir: &mut dyn FnMut(&Path),
    on_entry: &mut dyn FnMut(&Walked) -> bool,
    on_warn: &mut dyn FnMut(PathBuf, String),
) -> bool {
    for root in roots {
        if cancel.is_cancelled() {
            return false;
        }
        if !walk_root(root, opts, cancel, on_dir, on_entry, on_warn) {
            return false;
        }
    }
    true
}

fn walk_root(
    root: &RootInfo,
    opts: &WalkOptions,
    cancel: &CancelToken,
    on_dir: &mut dyn FnMut(&Path),
    on_entry: &mut dyn FnMut(&Walked) -> bool,
    on_warn: &mut dyn FnMut(PathBuf, String),
) -> bool {
    let root_dev = root.md.dev();
    // A non-directory root (file, symlink to a file, ...) is reported as a single entry.
    if !root.md.is_dir() {
        let kind = if root.is_symlink {
            EntryKind::Symlink {
                target: Some(plain_target_kind(&root.md)),
            }
        } else {
            plain_kind(&root.md)
        };
        return on_entry(&Walked {
            path: root.path.clone(),
            name: file_name(&root.path),
            kind,
            md: root.md.clone(),
        });
    }

    struct Frame {
        dir: PathBuf,
        /// Sorted names left to visit.
        entries: Vec<OsString>,
        next: usize,
    }
    let root_id = (root.md.dev(), root.md.ino());
    // Every directory ever descended into (per root): symlink loops warn, DAG revisits
    // are skipped silently so a tree reachable twice is not searched twice.
    let mut visited: HashSet<(u64, u64)> = HashSet::from([root_id]);
    // (dev, ino) of the directories on the current path (loop = revisit of an ancestor).
    let mut ancestors: Vec<(u64, u64)> = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    match open_dir(&root.path, on_dir, on_warn) {
        Some(entries) => stack.push(Frame {
            dir: root.path.clone(),
            entries,
            next: 0,
        }),
        None => return true, // warned; other roots continue
    }
    ancestors.push(root_id);

    while !stack.is_empty() {
        if cancel.is_cancelled() {
            return false;
        }
        let name = {
            let Some(f) = stack.last_mut() else {
                break;
            };
            let n = f.entries.get(f.next).cloned();
            if n.is_some() {
                f.next += 1;
            }
            n
        };
        let Some(name) = name else {
            stack.pop();
            ancestors.pop();
            continue;
        };
        let path = stack
            .last()
            .map_or_else(|| root.path.clone(), |f| f.dir.join(&name));
        // Depth of this entry: the root frame's entries are depth 0.
        let child_depth = stack.len() as u32 - 1;

        if !opts.include_hidden && name.as_encoded_bytes().first() == Some(&b'.') {
            continue;
        }
        let lmd = match std::fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(e) => {
                on_warn(path.clone(), format!("cannot stat: {e}"));
                continue;
            }
        };
        let is_symlink = lmd.file_type().is_symlink();
        // With `follow_symlinks`, a symlink reports and filters by its target's metadata.
        let (md, resolved) = if is_symlink && opts.follow_symlinks {
            match std::fs::metadata(&path) {
                Ok(tmd) => (tmd, true),
                // Broken link: keep the lstat, no descend, not an error.
                Err(_) => (lmd, false),
            }
        } else {
            (lmd, false)
        };

        let kind = if !is_symlink {
            plain_kind(&md)
        } else if resolved {
            EntryKind::Symlink {
                target: Some(plain_target_kind(&md)),
            }
        } else {
            // Not following: resolve the target kind for display only.
            EntryKind::Symlink {
                target: std::fs::metadata(&path).ok().map(|t| plain_target_kind(&t)),
            }
        };

        if !on_entry(&Walked {
            path: path.clone(),
            name,
            kind,
            md: md.clone(),
        }) {
            return false;
        }

        // Descend into directories; symlinked ones only when following (a resolved
        // symlink's `md.is_dir()` is the target's).
        let descend = md.is_dir() && (!is_symlink || resolved);
        if !descend {
            continue;
        }
        if let Some(max) = opts.max_depth
            && child_depth >= max
        {
            continue; // the directory's children would exceed the limit
        }
        if opts.same_filesystem && md.dev() != root_dev {
            tracing::trace!(dir = %path.display(), "skipping: other file system");
            continue;
        }
        let id = (md.dev(), md.ino());
        if visited.contains(&id) {
            if ancestors.contains(&id) {
                on_warn(path.clone(), "symlink loop detected, skipping".to_owned());
            } else {
                tracing::trace!(dir = %path.display(), "skipping: already visited");
            }
            continue;
        }
        if let Some(entries) = open_dir(&path, on_dir, on_warn) {
            visited.insert(id);
            ancestors.push(id);
            stack.push(Frame {
                dir: path.clone(),
                entries,
                next: 0,
            });
        }
    }
    true
}

/// Read and sort a directory's entry names; `None` (with a warning) if unreadable.
fn open_dir(
    dir: &Path,
    on_dir: &mut dyn FnMut(&Path),
    on_warn: &mut dyn FnMut(PathBuf, String),
) -> Option<Vec<OsString>> {
    on_dir(dir);
    let rd = match std::fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(e) => {
            on_warn(dir.to_owned(), format!("cannot read directory: {e}"));
            return None;
        }
    };
    let mut names: Vec<OsString> = Vec::new();
    for e in rd {
        match e {
            Ok(entry) => names.push(entry.file_name()),
            Err(e) => on_warn(dir.to_owned(), format!("directory listing failed: {e}")),
        }
    }
    names.sort_by(|a, b| a.as_encoded_bytes().cmp(b.as_encoded_bytes()));
    Some(names)
}

fn file_name(path: &Path) -> OsString {
    path.file_name()
        .map_or_else(|| path.as_os_str().to_owned(), |n| n.to_owned())
}

fn plain_kind(md: &Metadata) -> EntryKind {
    if md.is_dir() {
        EntryKind::Dir
    } else if md.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

fn plain_target_kind(md: &Metadata) -> TargetKind {
    if md.is_dir() {
        TargetKind::Dir
    } else if md.is_file() {
        TargetKind::File
    } else {
        TargetKind::Other
    }
}

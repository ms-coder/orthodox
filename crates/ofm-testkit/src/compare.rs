//! Deep comparison of two directory trees.
//!
//! [`compare_trees`] never follows symlinks and never crosses the given roots;
//! every problem it hits (including I/O errors such as an unreadable
//! directory) becomes one [`TreeDiff`]. [`assert_trees_equal`] is the
//! assert-style wrapper for tests.

use std::ffi::OsString;
use std::fmt;
use std::fs::{File, Metadata};
use std::io::{self, Read};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::sys;

/// Which aspects of two trees [`compare_trees`] should look at.
///
/// Entry names, entry kinds (file/dir/symlink) and symlink targets are ALWAYS
/// compared — they are the shape of the tree. The fields add data and metadata:
///
/// - `content` compares file sizes and file data byte by byte.
/// - `mode` compares permission bits (`mode & 0o7777`).
/// - `mtime` compares mtimes of files and directories, exact to the
///   nanosecond. Symlink mtimes are never compared: the engine recreates
///   symlinks (`readlink` + `symlink`) and does not preserve their times.
/// - `xattrs` compares extended attributes of files and directories
///   (names and values; symlink xattrs are skipped — not settable portably).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compare {
    /// Compare file sizes and file data.
    pub content: bool,
    /// Compare permission bits of files and directories.
    pub mode: bool,
    /// Compare mtimes (ns) of files and directories.
    pub mtime: bool,
    /// Compare extended attributes of files and directories.
    pub xattrs: bool,
}

impl Default for Compare {
    fn default() -> Self {
        Self {
            content: true,
            mode: true,
            mtime: true,
            xattrs: true,
        }
    }
}

impl Compare {
    /// Compare everything (same as `Default`).
    pub fn all() -> Self {
        Self::default()
    }

    /// Compare only names, entry kinds and symlink targets.
    pub fn names_only() -> Self {
        Self {
            content: false,
            mode: false,
            mtime: false,
            xattrs: false,
        }
    }
}

/// Entry kind used in diffs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link.
    Symlink,
    /// Anything else (FIFO, socket, device).
    Other,
}

impl fmt::Display for EntryKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            EntryKind::File => "file",
            EntryKind::Dir => "dir",
            EntryKind::Symlink => "symlink",
            EntryKind::Other => "other",
        })
    }
}

/// One difference between two trees. `path` is relative to the compared roots
/// and may contain non-UTF-8 bytes (shown escaped in messages).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeDiff {
    /// Entry exists only in tree `a`.
    OnlyInA {
        /// Relative path of the entry.
        path: PathBuf,
    },
    /// Entry exists only in tree `b`.
    OnlyInB {
        /// Relative path of the entry.
        path: PathBuf,
    },
    /// Both exist but with different entry kinds.
    Kind {
        /// Relative path of the entry.
        path: PathBuf,
        /// Kind in tree `a`.
        a: EntryKind,
        /// Kind in tree `b`.
        b: EntryKind,
    },
    /// File sizes differ (only checked when `Compare::content`).
    Size {
        /// Relative path of the entry.
        path: PathBuf,
        /// Size in tree `a`.
        a: u64,
        /// Size in tree `b`.
        b: u64,
    },
    /// File data differs; `offset` is the first differing byte.
    Content {
        /// Relative path of the entry.
        path: PathBuf,
        /// First differing byte offset.
        offset: u64,
    },
    /// Permission bits differ.
    Mode {
        /// Relative path of the entry.
        path: PathBuf,
        /// `mode & 0o7777` in tree `a`.
        a: u32,
        /// `mode & 0o7777` in tree `b`.
        b: u32,
    },
    /// Mtime differs (files and directories, ns precision).
    Mtime {
        /// Relative path of the entry.
        path: PathBuf,
        /// mtime (ns) in tree `a`.
        a_ns: i128,
        /// mtime (ns) in tree `b`.
        b_ns: i128,
    },
    /// Symlink targets differ (always checked).
    LinkTarget {
        /// Relative path of the entry.
        path: PathBuf,
        /// Target in tree `a`.
        a: PathBuf,
        /// Target in tree `b`.
        b: PathBuf,
    },
    /// Extended attribute `name` is missing on one side.
    XattrMissing {
        /// Relative path of the entry.
        path: PathBuf,
        /// Missing in tree `b` when `true`, in tree `a` when `false`.
        in_b: bool,
        /// Attribute name.
        name: OsString,
    },
    /// Extended attribute `name` has different values.
    XattrValue {
        /// Relative path of the entry.
        path: PathBuf,
        /// Attribute name.
        name: OsString,
    },
    /// A metadata/listing error on one side (e.g. an unreadable directory).
    Io {
        /// Relative path of the entry (`""` for a root).
        path: PathBuf,
        /// Failed on tree `b` when `true`, on tree `a` when `false`.
        in_b: bool,
        /// Error message.
        error: String,
    },
}

impl fmt::Display for TreeDiff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TreeDiff::OnlyInA { path } => write!(f, "only in a: {path:?}"),
            TreeDiff::OnlyInB { path } => write!(f, "only in b: {path:?}"),
            TreeDiff::Kind { path, a, b } => write!(f, "{path:?}: kind {a} != {b}"),
            TreeDiff::Size { path, a, b } => write!(f, "{path:?}: size {a} != {b}"),
            TreeDiff::Content { path, offset } => {
                write!(f, "{path:?}: content differs at {offset}")
            }
            TreeDiff::Mode { path, a, b } => write!(f, "{path:?}: mode {a:o} != {b:o}"),
            TreeDiff::Mtime { path, a_ns, b_ns } => write!(f, "{path:?}: mtime {a_ns} != {b_ns}"),
            TreeDiff::LinkTarget { path, a, b } => {
                write!(f, "{path:?}: link target {a:?} != {b:?}")
            }
            TreeDiff::XattrMissing { path, in_b, name } => write!(
                f,
                "{path:?}: xattr {name:?} missing on {}",
                if *in_b { "b" } else { "a" }
            ),
            TreeDiff::XattrValue { path, name } => {
                write!(f, "{path:?}: xattr {name:?} value differs")
            }
            TreeDiff::Io { path, in_b, error } => write!(
                f,
                "{path:?}: io error on {}: {error}",
                if *in_b { "b" } else { "a" }
            ),
        }
    }
}

/// Recursively compare `a` and `b`; an empty result means "equal" with respect
/// to `cmp`. Names are compared as bytes; symlinks are never followed.
///
/// # Example
/// ```
/// use ofm_testkit::compare::{compare_trees, Compare};
/// use ofm_testkit::tree::{FileSpec, TreeSpec};
///
/// let spec = TreeSpec::new().file("f.txt", FileSpec::content(b"x"));
/// let a = spec.build_temp().unwrap();
/// let b = spec.build_temp().unwrap();
/// assert!(compare_trees(a.path(), b.path(), Compare::all()).is_empty());
/// ```
pub fn compare_trees(a: &Path, b: &Path, cmp: Compare) -> Vec<TreeDiff> {
    let mut diffs = Vec::new();
    walk(Path::new(""), a, b, &cmp, &mut diffs);
    diffs
}

/// Assert that two trees are equal with respect to `cmp`; panics with up to
/// 20 listed differences otherwise. The assert-style companion of
/// [`compare_trees`].
///
/// ```
/// use ofm_testkit::compare::{assert_trees_equal, Compare};
/// use ofm_testkit::tree::{FileSpec, TreeSpec};
///
/// let spec = TreeSpec::new().file("f.txt", FileSpec::content(b"x"));
/// let a = spec.build_temp().unwrap();
/// let b = spec.build_temp().unwrap();
/// assert_trees_equal(a.path(), b.path(), Compare::all());
/// ```
pub fn assert_trees_equal(a: &Path, b: &Path, cmp: Compare) {
    let diffs = compare_trees(a, b, cmp);
    if diffs.is_empty() {
        return;
    }
    const MAX_LISTED: usize = 20;
    let mut msg = format!(
        "trees differ ({} difference(s)):\n  a: {}\n  b: {}\n",
        diffs.len(),
        a.display(),
        b.display()
    );
    for d in diffs.iter().take(MAX_LISTED) {
        msg.push_str("  - ");
        msg.push_str(&d.to_string());
        msg.push('\n');
    }
    if diffs.len() > MAX_LISTED {
        msg.push_str(&format!(
            "  ... and {} more difference(s)\n",
            diffs.len() - MAX_LISTED
        ));
    }
    panic!("{}", msg.trim_end());
}

fn walk(rel: &Path, a_root: &Path, b_root: &Path, cmp: &Compare, out: &mut Vec<TreeDiff>) {
    let a = a_root.join(rel);
    let b = b_root.join(rel);
    let mds = match (std::fs::symlink_metadata(&a), std::fs::symlink_metadata(&b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) => return out.push(io_diff(rel, false, e)),
        (_, Err(e)) => return out.push(io_diff(rel, true, e)),
    };
    let (mda, mdb) = mds;
    let (ka, kb) = (kind_of(&mda), kind_of(&mdb));
    if ka != kb {
        out.push(TreeDiff::Kind {
            path: rel.to_path_buf(),
            a: ka,
            b: kb,
        });
        return;
    }
    match ka {
        EntryKind::Symlink => compare_links(rel, &a, &b, out),
        EntryKind::Dir => compare_dirs(rel, a_root, b_root, cmp, out),
        _ => compare_entries(rel, &mda, &mdb, &a, &b, cmp, out),
    }
}

fn io_diff(rel: &Path, in_b: bool, e: io::Error) -> TreeDiff {
    TreeDiff::Io {
        path: rel.to_path_buf(),
        in_b,
        error: e.to_string(),
    }
}

fn compare_links(rel: &Path, a: &Path, b: &Path, out: &mut Vec<TreeDiff>) {
    let (ta, tb) = match (std::fs::read_link(a), std::fs::read_link(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) => return out.push(io_diff(rel, false, e)),
        (_, Err(e)) => return out.push(io_diff(rel, true, e)),
    };
    if ta != tb {
        out.push(TreeDiff::LinkTarget {
            path: rel.to_path_buf(),
            a: ta,
            b: tb,
        });
    }
}

fn compare_dirs(rel: &Path, a_root: &Path, b_root: &Path, cmp: &Compare, out: &mut Vec<TreeDiff>) {
    let a = a_root.join(rel);
    let b = b_root.join(rel);
    let lists = match (sorted_entries(&a), sorted_entries(&b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) => return out.push(io_diff(rel, false, e)),
        (_, Err(e)) => return out.push(io_diff(rel, true, e)),
    };
    let (na, nb) = lists;
    // Merge-join the sorted, unique name lists.
    let mut ib = 0;
    for name in &na {
        while ib < nb.len() && nb[ib].as_bytes() < name.as_bytes() {
            out.push(TreeDiff::OnlyInB {
                path: rel.join(&nb[ib]),
            });
            ib += 1;
        }
        if ib < nb.len() && &nb[ib] == name {
            ib += 1;
            walk(&rel.join(name), a_root, b_root, cmp, out);
        } else {
            out.push(TreeDiff::OnlyInA {
                path: rel.join(name),
            });
        }
    }
    for name in &nb[ib..] {
        out.push(TreeDiff::OnlyInB {
            path: rel.join(name),
        });
    }
}

fn compare_entries(
    rel: &Path,
    mda: &Metadata,
    mdb: &Metadata,
    a: &Path,
    b: &Path,
    cmp: &Compare,
    out: &mut Vec<TreeDiff>,
) {
    use std::os::unix::fs::MetadataExt;
    let kind = kind_of(mda);
    if cmp.mode && mda.mode() & 0o7777 != mdb.mode() & 0o7777 {
        out.push(TreeDiff::Mode {
            path: rel.to_path_buf(),
            a: mda.mode() & 0o7777,
            b: mdb.mode() & 0o7777,
        });
    }
    if cmp.mtime {
        let (ta, tb) = (sys::mtime_ns(mda), sys::mtime_ns(mdb));
        if ta != tb {
            out.push(TreeDiff::Mtime {
                path: rel.to_path_buf(),
                a_ns: ta,
                b_ns: tb,
            });
        }
    }
    if cmp.xattrs {
        compare_xattrs(rel, a, b, out);
    }
    if cmp.content && kind == EntryKind::File {
        if mda.len() != mdb.len() {
            out.push(TreeDiff::Size {
                path: rel.to_path_buf(),
                a: mda.len(),
                b: mdb.len(),
            });
            return;
        }
        compare_content(rel, a, b, mda.len(), out);
    }
}

fn compare_xattrs(rel: &Path, a: &Path, b: &Path, out: &mut Vec<TreeDiff>) {
    let lists = match (sys::list_xattrs(a), sys::list_xattrs(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) => return out.push(io_diff(rel, false, e)),
        (_, Err(e)) => return out.push(io_diff(rel, true, e)),
    };
    let (na, nb) = lists;
    let mut ib = 0;
    for name in &na {
        while ib < nb.len() && nb[ib].as_bytes() < name.as_bytes() {
            // name exists only in b -> missing in a
            out.push(TreeDiff::XattrMissing {
                path: rel.to_path_buf(),
                in_b: false,
                name: nb[ib].clone(),
            });
            ib += 1;
        }
        if ib < nb.len() && &nb[ib] == name {
            ib += 1;
            // A read error counts as "different"; listing already succeeded.
            let va = sys::get_xattr(a, name).ok().flatten();
            let vb = sys::get_xattr(b, name).ok().flatten();
            if va != vb {
                out.push(TreeDiff::XattrValue {
                    path: rel.to_path_buf(),
                    name: name.clone(),
                });
            }
        } else {
            // name exists only in a -> missing in b
            out.push(TreeDiff::XattrMissing {
                path: rel.to_path_buf(),
                in_b: true,
                name: name.clone(),
            });
        }
    }
    for name in &nb[ib..] {
        // leftover b names -> missing in a
        out.push(TreeDiff::XattrMissing {
            path: rel.to_path_buf(),
            in_b: false,
            name: name.clone(),
        });
    }
}

/// Byte-by-byte comparison of two equally-sized files; reports the offset of
/// the first differing byte.
fn compare_content(rel: &Path, a: &Path, b: &Path, len: u64, out: &mut Vec<TreeDiff>) {
    let files = match (File::open(a), File::open(b)) {
        (Ok(x), Ok(y)) => (x, y),
        (Err(e), _) => return out.push(io_diff(rel, false, e)),
        (_, Err(e)) => return out.push(io_diff(rel, true, e)),
    };
    let (mut fa, mut fb) = files;
    const CHUNK: usize = 64 * 1024;
    let mut ba = vec![0u8; CHUNK];
    let mut bb = vec![0u8; CHUNK];
    let mut offset: u64 = 0;
    while offset < len {
        let want = CHUNK.min((len - offset) as usize);
        match (fa.read(&mut ba[..want]), fb.read(&mut bb[..want])) {
            (Ok(na), Ok(nb)) => {
                if ba[..na] != bb[..nb] {
                    let pos = ba[..na]
                        .iter()
                        .zip(&bb[..nb])
                        .position(|(x, y)| x != y)
                        .unwrap_or(0);
                    out.push(TreeDiff::Content {
                        path: rel.to_path_buf(),
                        offset: offset + pos as u64,
                    });
                    return;
                }
                offset += na.max(nb) as u64;
            }
            (Err(e), _) => return out.push(io_diff(rel, false, e)),
            (_, Err(e)) => return out.push(io_diff(rel, true, e)),
        }
    }
}

fn kind_of(md: &Metadata) -> EntryKind {
    let ft = md.file_type();
    if ft.is_symlink() {
        EntryKind::Symlink
    } else if ft.is_dir() {
        EntryKind::Dir
    } else if ft.is_file() {
        EntryKind::File
    } else {
        EntryKind::Other
    }
}

/// Sorted raw directory entry names (no `.`/`..`).
fn sorted_entries(dir: &Path) -> io::Result<Vec<OsString>> {
    let mut names: Vec<OsString> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<io::Result<Vec<_>>>()?;
    names.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    Ok(names)
}

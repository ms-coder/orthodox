//! Fixture tree builder DSL.
//!
//! A [`TreeSpec`] is a value that describes a directory tree; it can be
//! materialized any number of times with [`TreeSpec::build`] (into an existing
//! directory) or [`TreeSpec::build_temp`] (into a self-cleaning temp dir).
//! Builds are deterministic (fixed default mtime, umask-independent modes), so
//! building the same spec twice yields trees that pass
//! [`assert_trees_equal`](crate::compare::assert_trees_equal) with
//! [`Compare::all`](crate::compare::Compare::all()).
//!
//! Names are [`OsString`] (bytes): pass non-UTF-8 names as
//! `raw_name(b"bad-\xff.txt")` — never through `str`.
//!
//! # Example
//!
//! ```
//! use ofm_testkit::tree::{raw_name, DirSpec, FileSpec, TreeSpec};
//!
//! let spec = TreeSpec::new()
//!     .file("readme.txt", FileSpec::content(b"hello\n"))
//!     .file("run.sh", FileSpec::content(b"#!/bin/sh\n").mode(0o755))
//!     .file("big.bin", FileSpec::sparse(2 << 30))
//!     .file(raw_name(b"bad-utf8-\xfe.txt"), FileSpec::content(b"x"))
//!     .dir("sub", DirSpec::new(TreeSpec::new().file("a", FileSpec::size(1024))))
//!     .symlink("link", "readme.txt")
//!     .hardlink("twin.txt", "readme.txt");
//!
//! let dir = spec.build_temp().unwrap();
//! assert!(dir.path().join("sub").join("a").is_file());
//! ```

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::sys;
use crate::temp::TempDir;

/// How a regular file's data is laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// Exact bytes.
    Bytes(Vec<u8>),
    /// Exactly `n` zero bytes, written out (NOT sparse — use [`FileContent::Sparse`]).
    Zeros(u64),
    /// Logical size `size`, created with `set_len` — sparse wherever the
    /// filesystem supports it (hole everywhere).
    Sparse {
        /// Logical size in bytes.
        size: u64,
    },
    /// Sparse file of logical `size` with explicit data ranges `(offset, bytes)`
    /// written into it — e.g. data at 1 MiB and 3 MiB inside a 4 MiB hole.
    /// For testing `SEEK_DATA`/`SEEK_HOLE` copy paths.
    SparseRanges {
        /// Logical size in bytes.
        size: u64,
        /// Data ranges as `(offset, bytes)`; offsets must be within `size`
        /// and non-overlapping (overlaps produce concatenated garbage, not an error).
        ranges: Vec<(u64, Vec<u8>)>,
    },
}

/// Deterministic default mtime for every entry: 2000-01-01T00:00:00Z.
///
/// Fixture trees are reproducible: building the same spec twice yields trees
/// that compare equal with [`Compare::all`](crate::compare::Compare::all()).
/// Override per entry with [`FileSpec::mtime_ns`] / [`DirSpec::mtime_ns`].
pub const DEFAULT_MTIME_NS: i128 = 946_684_800_000_000_000;

/// Spec of one regular file.
///
/// Defaults: mode `0o644`, mtime [`DEFAULT_MTIME_NS`], no xattrs. Builder
/// methods consume and return `self`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    content: FileContent,
    mode: u32,
    mtime_ns: i128,
    xattrs: BTreeMap<OsString, Vec<u8>>,
}

impl FileSpec {
    /// File with the exact given content.
    pub fn content(bytes: &[u8]) -> Self {
        Self {
            content: FileContent::Bytes(bytes.to_vec()),
            mode: 0o644,
            mtime_ns: DEFAULT_MTIME_NS,
            xattrs: BTreeMap::new(),
        }
    }

    /// File with `n` zero bytes, written out (not sparse).
    pub fn size(n: u64) -> Self {
        Self::content(&[]).with_content(FileContent::Zeros(n))
    }

    /// Fully sparse file of `size` bytes (via `set_len`).
    pub fn sparse(size: u64) -> Self {
        Self::content(&[]).with_content(FileContent::Sparse { size })
    }

    /// Sparse file of `size` bytes with data `ranges` of `(offset, bytes)`.
    pub fn sparse_ranges(size: u64, ranges: &[(u64, &[u8])]) -> Self {
        Self::content(&[]).with_content(FileContent::SparseRanges {
            size,
            ranges: ranges.iter().map(|(o, b)| (*o, b.to_vec())).collect(),
        })
    }

    /// Replace the data layout (keeps mode/mtime/xattrs).
    pub fn with_content(mut self, content: FileContent) -> Self {
        self.content = content;
        self
    }

    /// Permission bits (e.g. `0o755`).
    pub fn mode(mut self, mode: u32) -> Self {
        self.mode = mode & 0o7777;
        self
    }

    /// Fixed mtime, ns since the Unix epoch (negative = before).
    pub fn mtime_ns(mut self, ns: i128) -> Self {
        self.mtime_ns = ns;
        self
    }

    /// Fixed mtime.
    pub fn mtime(mut self, t: SystemTime) -> Self {
        self.mtime_ns = sys::systemtime_to_ns(t);
        self
    }

    /// Extended attribute `name = value` (Linux: use a `user.*` name).
    pub fn xattr<N: Into<OsString>>(mut self, name: N, value: &[u8]) -> Self {
        self.xattrs.insert(name.into(), value.to_vec());
        self
    }
}

/// Spec of one directory: final metadata plus children.
///
/// While building, the directory is created writable and its final mode and
/// mtime are applied after all children exist (post-order), so a restricted
/// mode like `0o500` still allows children to be created. Defaults:
/// mode `0o755`, mtime [`DEFAULT_MTIME_NS`], no xattrs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirSpec {
    mode: u32,
    mtime_ns: i128,
    xattrs: BTreeMap<OsString, Vec<u8>>,
    children: TreeSpec,
}

impl DirSpec {
    /// Directory with `children`, default mode `0o755`.
    pub fn new(children: TreeSpec) -> Self {
        Self {
            mode: 0o755,
            mtime_ns: DEFAULT_MTIME_NS,
            xattrs: BTreeMap::new(),
            children,
        }
    }

    /// Final permission bits.
    pub fn mode(mut self, mode: u32) -> Self {
        self.mode = mode & 0o7777;
        self
    }

    /// Final mtime, ns since the Unix epoch (applied post-order).
    pub fn mtime_ns(mut self, ns: i128) -> Self {
        self.mtime_ns = ns;
        self
    }

    /// Final mtime.
    pub fn mtime(mut self, t: SystemTime) -> Self {
        self.mtime_ns = sys::systemtime_to_ns(t);
        self
    }

    /// Extended attribute `name = value` (Linux: use a `user.*` name).
    pub fn xattr<N: Into<OsString>>(mut self, name: N, value: &[u8]) -> Self {
        self.xattrs.insert(name.into(), value.to_vec());
        self
    }
}

impl From<TreeSpec> for DirSpec {
    fn from(children: TreeSpec) -> Self {
        Self::new(children)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Node {
    File { name: OsString, spec: FileSpec },
    Dir { name: OsString, spec: DirSpec },
    Symlink { name: OsString, target: PathBuf },
    HardLink { name: OsString, target: PathBuf },
}

/// A whole fixture tree, described as data.
///
/// Builder methods consume and return `self`, so specs compose inline (see the
/// [module](self) example). Node order matters: a [`TreeSpec::hardlink`]
/// target must be listed before the hardlink.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeSpec {
    nodes: Vec<Node>,
}

impl TreeSpec {
    /// Empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Regular file (name may be non-UTF-8, see [`raw_name`]).
    pub fn file<N: Into<OsString>>(mut self, name: N, spec: FileSpec) -> Self {
        self.nodes.push(Node::File {
            name: name.into(),
            spec,
        });
        self
    }

    /// Directory; accepts a plain [`TreeSpec`] (defaults) or a [`DirSpec`]
    /// with final mode/mtime/xattrs.
    pub fn dir<N: Into<OsString>>(mut self, name: N, spec: impl Into<DirSpec>) -> Self {
        self.nodes.push(Node::Dir {
            name: name.into(),
            spec: spec.into(),
        });
        self
    }

    /// Symlink to `target` (raw path bytes; relative or absolute, may dangle).
    /// Note: symlink mtimes/xattrs cannot be set portably and are neither set
    /// nor compared by this kit.
    pub fn symlink<N: Into<OsString>, T: Into<PathBuf>>(mut self, name: N, target: T) -> Self {
        self.nodes.push(Node::Symlink {
            name: name.into(),
            target: target.into(),
        });
        self
    }

    /// Hard link to `target`, resolved against the tree root; `target` must be
    /// built by an earlier node of this spec.
    pub fn hardlink<N: Into<OsString>, T: Into<PathBuf>>(mut self, name: N, target: T) -> Self {
        self.nodes.push(Node::HardLink {
            name: name.into(),
            target: target.into(),
        });
        self
    }

    /// Number of entries in the spec, recursively.
    pub fn len(&self) -> usize {
        self.nodes
            .iter()
            .map(|n| match n {
                Node::Dir { spec, .. } => 1 + spec.children.len(),
                _ => 1,
            })
            .sum()
    }

    /// Whether the spec is empty.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Materialize the tree inside `root` (created if missing; existing
    /// entries of the same name are replaced/truncated where the OS allows).
    pub fn build(&self, root: &Path) -> io::Result<()> {
        std::fs::create_dir_all(root)?;
        build_nodes(&self.nodes, root)
    }

    /// Materialize the tree into a fresh [`TempDir`].
    pub fn build_temp(&self) -> io::Result<TempDir> {
        let dir = TempDir::new()?;
        self.build(dir.path())?;
        Ok(dir)
    }
}

/// An [`OsString`] from raw bytes — the way to spell a non-UTF-8 name
/// (invariant: paths are bytes, never `to_str().unwrap()`).
pub fn raw_name(bytes: &[u8]) -> OsString {
    OsStr::from_bytes(bytes).to_os_string()
}

fn build_nodes(nodes: &[Node], dir: &Path) -> io::Result<()> {
    // Duplicate sibling names would silently overwrite each other (files) or
    // fail halfway (dirs/symlinks) — reject the spec instead.
    let mut seen = std::collections::HashSet::with_capacity(nodes.len());
    for node in nodes {
        let name = match node {
            Node::File { name, .. } | Node::Dir { name, .. } => name,
            Node::Symlink { name, .. } | Node::HardLink { name, .. } => name,
        };
        if !seen.insert(name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("duplicate name in TreeSpec: {name:?}"),
            ));
        }
    }
    for node in nodes {
        match node {
            Node::File { name, spec } => build_file(&dir.join(name), spec)?,
            Node::Dir { name, spec } => build_dir(&dir.join(name), spec)?,
            Node::Symlink { name, target } => symlink(target, dir.join(name))?,
            Node::HardLink { name, target } => {
                let src = dir.join(target);
                if !src.exists() {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "hardlink target {} does not exist (list it before the hardlink)",
                            src.display()
                        ),
                    ));
                }
                std::fs::hard_link(&src, dir.join(name))?;
            }
        }
    }
    Ok(())
}

fn build_file(path: &Path, spec: &FileSpec) -> io::Result<()> {
    let mut f = File::create(path)?;
    match &spec.content {
        FileContent::Bytes(b) => f.write_all(b)?,
        FileContent::Zeros(n) => write_zeros(&mut f, *n)?,
        FileContent::Sparse { size } => f.set_len(*size)?,
        FileContent::SparseRanges { size, ranges } => {
            use std::io::Seek;
            for (offset, data) in ranges {
                if *offset >= *size {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        format!("sparse range offset {offset} outside size {size}"),
                    ));
                }
                f.seek(std::io::SeekFrom::Start(*offset))?;
                f.write_all(data)?;
            }
            f.set_len(*size)?;
        }
    }
    for (name, value) in &spec.xattrs {
        sys::set_xattr(path, name, value)?;
    }
    // mtime before the final mode: set_mtime opens the path read-only, which
    // fails on a mode like 0o000; chmod changes ctime only, never mtime.
    sys::set_mtime(path, spec.mtime_ns)?;
    // Always set explicitly: creation mode is 0o666 & !umask and must not
    // depend on the umask of the test process.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(spec.mode))?;
    Ok(())
}

fn build_dir(path: &Path, spec: &DirSpec) -> io::Result<()> {
    std::fs::create_dir(path)?;
    build_nodes(&spec.children.nodes, path)?;
    for (name, value) in &spec.xattrs {
        sys::set_xattr(path, name, value)?;
    }
    // mtime last-but-one: every child created above bumped the dir's mtime.
    // (Before the final mode — set_mtime opens the dir, which must still be
    // readable; chmod itself never changes mtime.)
    sys::set_mtime(path, spec.mtime_ns)?;
    // Final mode post-order (children are already in place); set explicitly
    // so the result never depends on the process umask.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(spec.mode))?;
    Ok(())
}

/// Write `n` zero bytes in 256 KiB chunks (real blocks, unlike `set_len`).
fn write_zeros(f: &mut File, n: u64) -> io::Result<()> {
    let chunk = vec![0u8; 256 * 1024];
    let mut left = n;
    while left > 0 {
        let take = left.min(chunk.len() as u64) as usize;
        f.write_all(&chunk[..take])?;
        left -= take as u64;
    }
    Ok(())
}

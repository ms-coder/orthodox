//! Test kit for OFM: fixture trees, tree comparison, cross-device helpers and
//! fault-injection plans. A **dev-dependency only** — no other crate may
//! depend on this in its real dependency list.
//!
//! Linux-only by design (xattrs, sparse files, `st_dev`), like OFM itself.
//!
//! # Overview
//!
//! - [`tree`]: build fixture trees from data (`TreeSpec`) into any directory
//!   or a self-cleaning [`TempDir`](temp::TempDir). Files with content /
//!   exact size / sparse layout, dirs with final mode/mtime/xattrs, symlinks,
//!   hardlinks, non-UTF-8 names ([`tree::raw_name`]).
//! - [`compare`]: `compare_trees` / `assert_trees_equal` with
//!   [`compare::Compare`] selecting which aspects (content, mode, mtime,
//!   xattrs) are checked; names, kinds and symlink targets are always checked.
//! - [`crossdev`]: `two_filesystems()` for cross-device (EXDEV) tests.
//! - [`fault`]: `FaultPlan` / `FaultState` — plain data describing injected
//!   faults (ENOSPC/EIO/EACCES, short writes at the N-th call) for the
//!   `FaultyFs` that ofm-jobs implements on top of its `Fs` trait (ADR-0004).
//! - [`temp`]: dependency-free temp dirs.
//!
//! # Example
//!
//! ```
//! use ofm_testkit::compare::{assert_trees_equal, Compare};
//! use ofm_testkit::tree::{FileSpec, TreeSpec};
//!
//! let spec = TreeSpec::new()
//!     .file("readme.txt", FileSpec::content(b"hello\n").mtime_ns(946_684_800_000_000_000))
//!     .file("hole.bin", FileSpec::sparse(1 << 20))
//!     .dir("sub", TreeSpec::new().file("a", FileSpec::size(4096)));
//!
//! // The same spec materializes into equal trees:
//! let a = spec.build_temp().unwrap();
//! let b = spec.build_temp().unwrap();
//! assert_trees_equal(a.path(), b.path(), Compare::all());
//! ```

pub mod compare;
pub mod crossdev;
pub mod fault;
pub mod temp;
pub mod tree;

mod sys;

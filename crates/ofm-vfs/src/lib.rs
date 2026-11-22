//! ofm-vfs: virtual file system providers behind one trait. No GTK.
//!
//! File ownership for parallel tasks:
//! - T1.2: `local/**`, `watch.rs`, `pool.rs`
//! - M6 (T6.1): `archive/**`; M7 (T7.2): `list/**`
//! - frozen API (architect only): `lib.rs`, `vfs.rs`, `error.rs`, `registry.rs`
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]

pub mod archive;
pub mod error;
pub mod list;
pub mod local;
pub mod pool;
pub mod registry;
pub mod vfs;
pub mod watch;

pub use error::VfsError;
pub use registry::VfsRegistry;
pub use vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs};

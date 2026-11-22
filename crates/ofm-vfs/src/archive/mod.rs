//! Read-only archive provider (M6, ADR-0008). Frozen by T6.0; implemented by T6.1.
//!
//! Layering: [`open_archive`] (magic detection) -> `backend::{zip,tar,sevenz}` -> [`ArchiveReader`]
//! (index + streaming `visit`). [`ArchiveVfs`] adapts an index to the `Vfs` trait for panels;
//! extract jobs (`ofm-jobs`) use `ArchiveReader::visit` directly and write through their own `Fs`.
//!
//! Safety contract: every stored path and link target goes through [`normalize_entry_path`] /
//! [`resolve_link_target`] inside [`IndexBuilder::push`]; entries that are absolute, climb with
//! `..`, or point out of the archive are flagged [`EntrySafety::Unsafe`] at index time and are
//! invisible to listings, `select` and `visit`.
//!
//! File ownership (T6.1): everything below `archive/`. Frozen API (architect only): the item names
//! and signatures re-exported here.

pub mod backend;
pub mod error;
pub mod format;
pub mod index;
pub mod limits;
pub mod password;
pub mod path;
pub mod reader;
pub mod vfs;

pub use error::{ArchiveError, LimitKind, UnsafeReason};
pub use format::{ArchiveFormat, MAGIC_LEN, detect_format, detect_magic};
pub use index::{
    ArchiveEntry, ArchiveEntryKind, ArchiveIndex, EntryId, EntrySafety, IndexBuilder,
    IndexWarnings, RawEntry,
};
pub use limits::ArchiveLimits;
pub use password::{
    FixedPassword, MAX_PASSWORD_ATTEMPTS, NoPassword, PasswordProvider, PasswordSource,
};
pub use path::{normalize_entry_path, resolve_link_target};
pub use reader::{ArchiveReader, VisitFn, copy_stream, open_archive};
pub use vfs::{ArchiveVfs, MAX_OPEN_READ_BYTES};

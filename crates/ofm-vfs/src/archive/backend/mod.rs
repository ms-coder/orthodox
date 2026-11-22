//! Format backends (owned by T6.1; one file per library so they can be built in parallel).
//!
//! | file | formats | crate (ADR-0008) |
//! |---|---|---|
//! | `zip.rs` | zip | `zip` (random access: central directory, `visit` seeks per entry) |
//! | `tar.rs` | tar, tar.gz/bz2/xz/zst | `tar` + `flate2`, `bzip2`, `lzma-rust2` (xz), `zstd` |
//! | `sevenz.rs` | 7z | `sevenz-rust2` |
//!
//! Every backend exposes `pub fn open(...) -> Result<Box<dyn ArchiveReader>, ArchiveError>`:
//! build the index with `IndexBuilder`, never trust a stored size/path, honour `ArchiveLimits`.

use std::path::Path;

use ofm_core::CancelToken;

use super::error::ArchiveError;
use super::format::ArchiveFormat;
use super::limits::ArchiveLimits;
use super::password::PasswordProvider;
use super::reader::ArchiveReader;

pub mod sevenz;
pub mod tar;
pub mod zip;

/// Dispatch to the backend for `format`.
pub fn open(
    format: ArchiveFormat,
    path: &Path,
    passwords: &dyn PasswordProvider,
    limits: &ArchiveLimits,
    cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader>, ArchiveError> {
    match format {
        ArchiveFormat::Zip => zip::open(path, passwords, limits, cancel),
        ArchiveFormat::SevenZ => sevenz::open(path, passwords, limits, cancel),
        ArchiveFormat::Tar
        | ArchiveFormat::TarGz
        | ArchiveFormat::TarBz2
        | ArchiveFormat::TarXz
        | ArchiveFormat::TarZst => tar::open(format, path, passwords, limits, cancel),
    }
}

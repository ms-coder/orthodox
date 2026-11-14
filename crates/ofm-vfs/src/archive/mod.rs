//! Read-only archive provider (owned by M6).
//!
//! On first entry the whole archive is listed in a worker into an `ArchiveIndex` tree. Formats:
//! zip (AES), tar + gz/bz2/xz/zst, 7z; detection by magic bytes. Entries with absolute paths or
//! `..` are rejected (zip-slip). Extraction = ordinary copy jobs reading through `Vfs::open_read`.

use ofm_core::{CancelToken, Entry, Location};

use crate::error::VfsError;
use crate::vfs::{Caps, ListEvent, ReadSeek, SpaceInfo, Vfs};

/// Supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    /// zip (incl. AES-encrypted).
    Zip,
    /// Uncompressed tar.
    Tar,
    /// tar + gzip.
    TarGz,
    /// tar + bzip2.
    TarBz2,
    /// tar + xz.
    TarXz,
    /// tar + zstd.
    TarZst,
    /// 7z.
    SevenZ,
}

/// Supplies passwords for encrypted archives; called on the worker thread (blocks on the UI via a
/// `Responder`). `None` = user cancelled.
pub trait PasswordProvider: Send + Sync {
    /// Ask for the password of `archive`; `retry` is true if the previous one was wrong.
    fn password(&self, archive: &std::path::Path, retry: bool) -> Option<String>;
}

/// Detect the format by magic bytes (not by extension); `None` if not an archive we support.
pub fn detect_format(_path: &std::path::Path) -> Result<Option<ArchiveFormat>, VfsError> {
    todo!("M6")
}

/// Provider for `Location::Archive`.
#[derive(Debug, Default)]
pub struct ArchiveVfs {}

impl Vfs for ArchiveVfs {
    fn list(&self, _loc: &Location, _cancel: &CancelToken, _sink: &mut dyn FnMut(ListEvent)) {
        todo!("M6")
    }
    fn stat(&self, _loc: &Location) -> Result<Entry, VfsError> {
        todo!("M6")
    }
    fn open_read(&self, _loc: &Location) -> Result<Box<dyn ReadSeek + Send>, VfsError> {
        todo!("M6")
    }
    fn capabilities(&self, _loc: &Location) -> Caps {
        Caps {
            read: true,
            ..Caps::default()
        }
    }
    fn parent(&self, _loc: &Location) -> Option<Location> {
        todo!("M6")
    }
    fn space(&self, _loc: &Location) -> Result<SpaceInfo, VfsError> {
        Err(VfsError::Unsupported("space of an archive"))
    }
}

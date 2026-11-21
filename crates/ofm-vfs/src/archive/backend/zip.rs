//! zip backend (T6.1). Crate `zip` (features `aes-crypto deflate deflate64 bzip2 zstd xz lzma`).
//!
//! Notes for the implementer: index from the central directory only; zip64; names flagged UTF-8 by
//! the language-encoding bit else CP437/lossy bytes kept as bytes; convert `\` to `/` when the
//! entry's "made by" host is FAT/NTFS before `IndexBuilder::push`; ZipCrypto and AES via the
//! password provider; the unix mode comes from the external attributes when the host is Unix.

use std::path::Path;

use ofm_core::CancelToken;

use crate::archive::error::ArchiveError;
use crate::archive::limits::ArchiveLimits;
use crate::archive::password::PasswordProvider;
use crate::archive::reader::ArchiveReader;

/// Open a zip archive and index it.
pub fn open(
    _path: &Path,
    _passwords: &dyn PasswordProvider,
    _limits: &ArchiveLimits,
    _cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader>, ArchiveError> {
    Err(ArchiveError::NotImplemented("zip backend (T6.1)"))
}

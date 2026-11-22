//! 7z backend (T6.1). Crate `sevenz-rust2` (features `aes256 bzip2 ppmd zstd deflate`; pure Rust).
//!
//! Notes for the implementer: encrypted headers need the password at open (the library reports a
//! wrong password as a decode error: map it to `WrongPassword` after `MAX_PASSWORD_ATTEMPTS`);
//! use the library's memory-limit constructor with `ArchiveLimits::max_codec_memory`; `visit` maps
//! onto `for_each_entries` (one pass per solid block); empty files/dirs have no stream.

use std::path::Path;

use ofm_core::CancelToken;

use crate::archive::error::ArchiveError;
use crate::archive::limits::ArchiveLimits;
use crate::archive::password::PasswordProvider;
use crate::archive::reader::ArchiveReader;

/// Open a 7z archive and index it.
pub fn open(
    _path: &Path,
    _passwords: &dyn PasswordProvider,
    _limits: &ArchiveLimits,
    _cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader>, ArchiveError> {
    Err(ArchiveError::NotImplemented("7z backend (T6.1)"))
}

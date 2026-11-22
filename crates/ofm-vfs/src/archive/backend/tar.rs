//! tar backend: tar, tar.gz, tar.bz2, tar.xz, tar.zst (T6.1). Crates `tar`, `flate2`, `bzip2`,
//! `lzma-rust2` (xz), `zstd`.
//!
//! Notes for the implementer: the index is one sequential pass (no seeking in compressed streams);
//! GNU long names, pax headers (non-UTF-8 paths stay bytes), sparse entries; hardlinks become
//! `ArchiveEntryKind::Hardlink`; `visit` re-opens the stream and walks it once. Compressed variants
//! verify the inner tar header (checksum) so a plain `.gz` is `NotAnArchive`. xz block size and
//! zstd window are capped by `ArchiveLimits::max_codec_memory`.

use std::path::Path;

use ofm_core::CancelToken;

use crate::archive::error::ArchiveError;
use crate::archive::format::ArchiveFormat;
use crate::archive::limits::ArchiveLimits;
use crate::archive::password::PasswordProvider;
use crate::archive::reader::ArchiveReader;

/// Open a tar (optionally compressed) archive and index it. `format` is one of the `Tar*` variants.
pub fn open(
    _format: ArchiveFormat,
    _path: &Path,
    _passwords: &dyn PasswordProvider,
    _limits: &ArchiveLimits,
    _cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader>, ArchiveError> {
    Err(ArchiveError::NotImplemented("tar backend (T6.1)"))
}

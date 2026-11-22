//! Archive formats and detection by magic bytes (never by extension).

use std::io::Read;
use std::path::Path;

use super::error::ArchiveError;

/// Supported archive formats (F26).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArchiveFormat {
    /// zip (deflate, deflate64, bzip2, zstd, xz/lzma; ZipCrypto and AES).
    Zip,
    /// Uncompressed tar (ustar, pax, GNU).
    Tar,
    /// tar + gzip.
    TarGz,
    /// tar + bzip2.
    TarBz2,
    /// tar + xz.
    TarXz,
    /// tar + zstd.
    TarZst,
    /// 7z (solid blocks, encrypted headers).
    SevenZ,
}

impl ArchiveFormat {
    /// Short human-readable name for the UI ("zip", "tar.gz", ...).
    pub fn name(self) -> &'static str {
        match self {
            ArchiveFormat::Zip => "zip",
            ArchiveFormat::Tar => "tar",
            ArchiveFormat::TarGz => "tar.gz",
            ArchiveFormat::TarBz2 => "tar.bz2",
            ArchiveFormat::TarXz => "tar.xz",
            ArchiveFormat::TarZst => "tar.zst",
            ArchiveFormat::SevenZ => "7z",
        }
    }

    /// True for formats where reading one entry means decompressing everything before it
    /// (tar.*, solid 7z). Extraction must then use one sequential pass (`ArchiveReader::visit`).
    pub fn is_sequential(self) -> bool {
        !matches!(self, ArchiveFormat::Zip)
    }
}

/// Number of leading bytes [`detect_magic`] needs (a tar header block).
pub const MAGIC_LEN: usize = 512;

/// Classify the first bytes of a file (at least [`MAGIC_LEN`] if the file has that many).
///
/// Compression wrappers map to the tar variant (`1F 8B` -> `TarGz`, ...): this is the **outer**
/// container only. Whether a gzip stream really holds a tar is checked by [`detect_format`]
/// (T6.1: decompress the first block and verify the tar header checksum).
pub fn detect_magic(head: &[u8]) -> Option<ArchiveFormat> {
    const ZIP: [&[u8]; 3] = [b"PK\x03\x04", b"PK\x05\x06", b"PK\x07\x08"];
    if ZIP.iter().any(|m| head.starts_with(m)) {
        return Some(ArchiveFormat::Zip);
    }
    if head.starts_with(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C]) {
        return Some(ArchiveFormat::SevenZ);
    }
    if head.starts_with(&[0x1F, 0x8B]) {
        return Some(ArchiveFormat::TarGz);
    }
    if head.starts_with(b"BZh") {
        return Some(ArchiveFormat::TarBz2);
    }
    if head.starts_with(&[0xFD, b'7', b'z', b'X', b'Z', 0x00]) {
        return Some(ArchiveFormat::TarXz);
    }
    if head.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        return Some(ArchiveFormat::TarZst);
    }
    if head.get(257..262) == Some(b"ustar".as_slice()) {
        return Some(ArchiveFormat::Tar);
    }
    None
}

/// Detect the format of the file at `path` by magic bytes; `Ok(None)` if it is not an archive we
/// support. A compressed stream that is not a tar (plain `.gz`) must give `Ok(None)`: T6.1 refines
/// the `detect_magic` result for the four compressed variants by checking the inner tar header.
pub fn detect_format(path: &Path) -> Result<Option<ArchiveFormat>, ArchiveError> {
    let mut head = Vec::with_capacity(MAGIC_LEN);
    std::fs::File::open(path)
        .and_then(|f| f.take(MAGIC_LEN as u64).read_to_end(&mut head))
        .map_err(|e| ArchiveError::io(path, e))?;
    Ok(detect_magic(&head))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_table() {
        assert_eq!(detect_magic(b"PK\x03\x04rest"), Some(ArchiveFormat::Zip));
        assert_eq!(detect_magic(b"PK\x05\x06"), Some(ArchiveFormat::Zip));
        assert_eq!(
            detect_magic(&[0x37, 0x7A, 0xBC, 0xAF, 0x27, 0x1C, 0, 4]),
            Some(ArchiveFormat::SevenZ)
        );
        assert_eq!(detect_magic(&[0x1F, 0x8B, 8]), Some(ArchiveFormat::TarGz));
        assert_eq!(detect_magic(b"BZh9"), Some(ArchiveFormat::TarBz2));
        assert_eq!(
            detect_magic(&[0x28, 0xB5, 0x2F, 0xFD, 0]),
            Some(ArchiveFormat::TarZst)
        );
        let mut tar = vec![0u8; 512];
        tar[257..262].copy_from_slice(b"ustar");
        assert_eq!(detect_magic(&tar), Some(ArchiveFormat::Tar));
        assert_eq!(detect_magic(b"hello"), None);
        assert_eq!(detect_magic(b""), None);
        assert!(ArchiveFormat::SevenZ.is_sequential() && !ArchiveFormat::Zip.is_sequential());
    }
}

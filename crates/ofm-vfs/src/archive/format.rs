//! Archive formats and detection by magic bytes (never by extension).

use std::io::Read;
use std::path::Path;

use super::backend::stream::{catch_panic, map_io};
use super::backend::tar::open_tar_stream;
use super::error::ArchiveError;
use super::limits::ArchiveLimits;

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
/// support.
///
/// The four compressed-tar variants are refined here (T6.1): the first tar block is
/// decompressed and verified by header checksum, so a plain `.gz`/`.bz2`/`.xz`/`.zst` file gives
/// `Ok(None)` and is opened as an ordinary file. A container whose first block cannot be
/// decompressed at all is likewise not treated as an archive (the tar backend reports deeper
/// corruption with `Corrupt` once the format is confirmed).
pub fn detect_format(path: &Path) -> Result<Option<ArchiveFormat>, ArchiveError> {
    detect_format_with_limits(path, &ArchiveLimits::default())
}

/// Detect with the same codec limits and panic isolation as the backend that will read the file.
pub(crate) fn detect_format_with_limits(
    path: &Path,
    limits: &ArchiveLimits,
) -> Result<Option<ArchiveFormat>, ArchiveError> {
    let mut head = Vec::with_capacity(MAGIC_LEN);
    std::fs::File::open(path)
        .and_then(|f| f.take(MAGIC_LEN as u64).read_to_end(&mut head))
        .map_err(|e| ArchiveError::io(path, e))?;
    let format = detect_magic(&head);
    match format {
        Some(
            f @ (ArchiveFormat::TarGz
            | ArchiveFormat::TarBz2
            | ArchiveFormat::TarXz
            | ArchiveFormat::TarZst),
        ) => {
            let block = first_tar_block(path, f, limits);
            match block {
                Ok(Some(_)) => Ok(Some(f)),
                // Not a tar inside: a plain compressed file (or a broken stream we cannot
                // identify) is not an archive we browse. Resource limits remain hard errors.
                Ok(None) => Ok(None),
                Err(e @ ArchiveError::LimitExceeded { .. }) => Err(e),
                Err(_) => Ok(None),
            }
        }
        other => Ok(other),
    }
}

/// Decompress the first 512 bytes of the container at `path` and verify the tar header
/// checksum (accepts pre-POSIX tars without the `ustar` magic too).
fn first_tar_block(
    path: &Path,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
) -> Result<Option<[u8; 512]>, ArchiveError> {
    catch_panic(path, || {
        let mut reader = open_tar_stream(path, format, limits)?;
        let mut block = [0u8; 512];
        let mut filled = 0;
        while filled < 512 {
            match reader.read(&mut block[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(map_io(path, e)),
            }
        }
        if filled == 512 && tar_header_checksum_ok(&block) {
            Ok(Some(block))
        } else {
            Ok(None)
        }
    })
}

/// Verify a tar header block by its checksum field (bytes 148..156, space-padded octal).
pub(crate) fn tar_header_checksum_ok(block: &[u8; 512]) -> bool {
    let stored = parse_octal(&block[148..156]);
    let Some(stored) = stored else {
        return false;
    };
    let mut sum: u64 = 0;
    for (i, b) in block.iter().enumerate() {
        sum += match i {
            148..=155 => b' ' as u64, // checksum field treated as spaces
            _ => *b as u64,
        };
    }
    sum == stored
}

/// Parse a tar octal field (digits until space/NUL; also the GNU base-256 binary form).
fn parse_octal(field: &[u8]) -> Option<u64> {
    if field.is_empty() {
        return None;
    }
    // GNU base-256: high bit of the first byte set.
    if field[0] & 0x80 != 0 {
        let mut v: u64 = (field[0] & 0x7f) as u64;
        for b in &field[1..] {
            v = v.checked_shl(8).unwrap_or(0).wrapping_add(*b as u64);
        }
        return Some(v);
    }
    let mut v: u64 = 0;
    let mut any = false;
    for &b in field {
        match b {
            b'0'..=b'7' => {
                v = v.saturating_mul(8).saturating_add((b - b'0') as u64);
                any = true;
            }
            b' ' | 0 => {
                if any {
                    break;
                }
            }
            _ => return None,
        }
    }
    any.then_some(v)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
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

    #[test]
    fn compressed_tar_probe_honours_codec_memory_limit() {
        let mut bytes = Vec::new();
        {
            let mut tar = tar::Builder::new(&mut bytes);
            let mut header = tar::Header::new_gnu();
            header.set_path("f").unwrap();
            header.set_size(1);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append(&header, &b"x"[..]).unwrap();
            tar.finish().unwrap();
        }
        let compressed = zstd::stream::encode_all(&bytes[..], 1).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.tar.zst");
        std::fs::write(&path, compressed).unwrap();
        let limits = ArchiveLimits {
            max_codec_memory: 1,
            ..ArchiveLimits::default()
        };
        assert!(matches!(
            detect_format_with_limits(&path, &limits),
            Err(ArchiveError::LimitExceeded {
                kind: super::super::LimitKind::CodecMemory,
                ..
            })
        ));
    }
}

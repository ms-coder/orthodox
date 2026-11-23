//! zip backend (T6.1). Crate `zip` (features `aes-crypto deflate deflate64 bzip2 zstd xz lzma`).
//!
//! Indexing reads the central directory only (`by_index_data`, no decompression); the only data
//! reads at index time are symlink targets (unencrypted entries, capped at 64 KiB), read in a
//! second phase *before* the entries are pushed so the safety classification in
//! [`IndexBuilder::push`] sees the real target. Entry *names* honour the UTF-8 language-encoding
//! flag; other names are decoded from CP437. Names stored by FAT/NTFS/VFAT/OS2 hosts get `\`
//! converted to `/` before classification (ADR-0008). `visit` seeks per entry (zip is the one
//! random-access format) and verifies CRC/MAC by draining each entry to EOF. ZipCrypto and AES
//! ask the `PasswordProvider` (`retry = true` after a wrong one, at most [`MAX_PASSWORD_ATTEMPTS`]);
//! a verified password is reported via `accepted`.
//!
//! The reader borrows the `PasswordProvider` (`Box<dyn ArchiveReader + 'p>`): visit asks for
//! passwords lazily, so the provider must outlive the reader, which it does in every caller
//! (worker thread holding the shared `PasswordSource`).

use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_core::CancelToken;
use zip::result::ZipError;
use zip::{DateTime, System, ZipArchive};

use crate::archive::EntrySafety;
use crate::archive::error::ArchiveError;
use crate::archive::format::ArchiveFormat;
use crate::archive::index::{ArchiveEntryKind, ArchiveIndex, EntryId, IndexBuilder, RawEntry};
use crate::archive::limits::ArchiveLimits;
use crate::archive::password::MAX_PASSWORD_ATTEMPTS;
use crate::archive::reader::{ArchiveReader, VisitFn};

use super::stream::{EntryStream, catch_panic, drain, map_io};

/// Symlink targets longer than this are not read at index time (the entry is flagged unsafe).
const MAX_SYMLINK_TARGET: u64 = 64 * 1024;

struct ZipReader<'p> {
    archive: ZipArchive<std::fs::File>,
    index: Arc<ArchiveIndex>,
    /// zip central-directory position for each index entry id (`None` = skipped archive root;
    /// synthetic dirs are beyond `id_to_zip.len()`).
    id_to_zip: Vec<u32>,
    passwords: &'p dyn crate::archive::PasswordProvider,
}

/// Owned metadata of one central-directory entry (frees the archive borrow for phase 2).
struct ZipMeta {
    raw_name: Vec<u8>,
    utf8: bool,
    host: System,
    kind: ArchiveEntryKind,
    size: u64,
    packed_size: u64,
    mtime_ns: Option<i128>,
    mode: Option<u32>,
    encrypted: bool,
}

/// Open a zip archive and index it.
pub fn open<'p>(
    path: &Path,
    passwords: &'p dyn crate::archive::PasswordProvider,
    limits: &ArchiveLimits,
    cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader + 'p>, ArchiveError> {
    let file = std::fs::File::open(path).map_err(|e| ArchiveError::io(path, e))?;
    let mut zf = catch_panic(path, || {
        ZipArchive::new(file).map_err(|e| map_zip_err(path, e))
    })?;

    // Phase 1: metadata of every entry (shared borrow, no decompression).
    let mut metas = Vec::with_capacity(zf.len());
    for i in 0..zf.len() {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        metas.push(catch_panic(path, || {
            let entry = zf.by_index_data(i).map_err(|e| map_zip_err(path, e))?;
            Ok::<ZipMeta, ArchiveError>(ZipMeta {
                raw_name: entry.name_raw().to_vec(),
                utf8: entry.flags().as_u16() & 0x0800 != 0,
                host: entry.system(),
                kind: zip_kind(&entry),
                size: entry.size(),
                packed_size: entry.compressed_size(),
                mtime_ns: entry.last_modified().and_then(dos_dt_to_ns),
                mode: entry.unix_mode(),
                encrypted: entry.encrypted(),
            })
        })?);
    }

    // Phase 2: symlink targets (needs &mut; only unencrypted, reasonably sized ones).
    let mut targets: Vec<Option<PathBuf>> = Vec::with_capacity(metas.len());
    for (i, m) in metas.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        if m.kind != ArchiveEntryKind::Symlink || m.encrypted || m.size > MAX_SYMLINK_TARGET {
            targets.push(None);
            continue;
        }
        targets.push(catch_panic(path, || {
            let mut f = zf.by_index(i).map_err(|e| map_zip_err(path, e))?;
            let mut buf = Vec::with_capacity(f.size().min(MAX_SYMLINK_TARGET) as usize);
            f.read_to_end(&mut buf).map_err(|e| map_io(path, e))?;
            Ok(Some(PathBuf::from(OsString::from_vec(buf))))
        })?);
    }

    // Phase 3: classify and build the index.
    let mut builder = IndexBuilder::new(path, ArchiveFormat::Zip, *limits);
    let mut id_to_zip = Vec::with_capacity(metas.len());
    for (i, (m, target)) in metas.iter().zip(targets).enumerate() {
        let mut raw = RawEntry::new(raw_name_to_path(&m.raw_name, m.utf8, m.host), m.kind);
        raw.size = m.size;
        raw.packed_size = Some(m.packed_size);
        raw.mtime_ns = m.mtime_ns;
        raw.mode = m.mode;
        raw.encrypted = m.encrypted;
        raw.link_target = target;
        if builder.push(raw)?.is_some() {
            id_to_zip.push(i as u32);
        }
    }
    let index = builder.finish();

    Ok(Box::new(ZipReader {
        archive: zf,
        index: Arc::new(index),
        id_to_zip,
        passwords,
    }))
}

/// Kind of a central-directory entry (via unix mode bits; falls back to the dir flag).
fn zip_kind(entry: &zip::read::ZipFileEntry<'_>) -> ArchiveEntryKind {
    let mode = entry.unix_mode();
    if entry.is_dir() {
        ArchiveEntryKind::Dir
    } else if entry.is_symlink() || mode.is_some_and(|m| m & 0o170000 == 0o120000) {
        ArchiveEntryKind::Symlink
    } else if mode
        .is_some_and(|m| matches!(m & 0o170000, 0o020000 | 0o060000 | 0o010000 | 0o140000))
    {
        ArchiveEntryKind::Other
    } else {
        ArchiveEntryKind::File
    }
}

/// Decode a raw zip name to a path: honour the UTF-8 flag, else CP437; convert `\` to `/` for
/// entries made on FAT/NTFS/VFAT/OS2 hosts (they use `\` as separator).
fn raw_name_to_path(raw: &[u8], utf8: bool, host: System) -> PathBuf {
    let mut name = if utf8 {
        raw.to_vec()
    } else {
        cp437_to_string(raw).into_bytes()
    };
    if matches!(
        host,
        System::Dos | System::WindowsNTFS | System::VFAT | System::Os2
    ) {
        for b in &mut name {
            if *b == b'\\' {
                *b = b'/';
            }
        }
    }
    PathBuf::from(OsString::from_vec(name))
}

/// DOS date/time to ns since the unix epoch; `None` when the stored value is invalid.
fn dos_dt_to_ns(dt: DateTime) -> Option<i128> {
    if !dt.is_valid() {
        return None;
    }
    let days = days_from_civil(
        i64::from(dt.year()),
        u32::from(dt.month()),
        u32::from(dt.day()),
    );
    let secs = days * 86_400
        + i64::from(dt.hour()) * 3_600
        + i64::from(dt.minute()) * 60
        + i64::from(dt.second());
    Some(i128::from(secs) * 1_000_000_000)
}

/// Days from civil date (Howard Hinnant's algorithm), valid for the full DOS range.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = i64::from((m + 9) % 12);
    let doy = (153 * mp + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Map `zip` crate errors to [`ArchiveError`].
fn map_zip_err(path: &Path, e: ZipError) -> ArchiveError {
    match e {
        ZipError::Io(io) => ArchiveError::io(path, io),
        ZipError::InvalidArchive(d) => ArchiveError::Corrupt {
            archive: path.to_owned(),
            detail: d.into_owned(),
        },
        ZipError::UnsupportedArchive(what) => ArchiveError::Unsupported {
            archive: path.to_owned(),
            what: what.to_owned(),
        },
        ZipError::InvalidPassword => ArchiveError::WrongPassword(path.to_owned()),
        ZipError::CompressionMethodNotSupported(m) => ArchiveError::Unsupported {
            archive: path.to_owned(),
            what: format!("compression method {m}"),
        },
        other => ArchiveError::Corrupt {
            archive: path.to_owned(),
            detail: format!("zip error: {other}"),
        },
    }
}

impl ArchiveReader for ZipReader<'_> {
    fn index(&self) -> &Arc<ArchiveIndex> {
        &self.index
    }

    fn visit(
        &mut self,
        ids: &[EntryId],
        cancel: &CancelToken,
        f: &mut VisitFn<'_>,
    ) -> Result<(), ArchiveError> {
        let archive = self.index.archive().to_owned();
        let index = self.index.clone();
        // Validate every id before reading any data (contract).
        for id in ids {
            let entry = index.get(*id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            if let EntrySafety::Unsafe(reason) = entry.safety {
                return Err(ArchiveError::Unsafe {
                    path: entry.path.clone(),
                    reason,
                });
            }
        }
        let mut wanted = ids.to_vec();
        wanted.sort();
        catch_panic(&archive, || {
            for id in wanted {
                if cancel.is_cancelled() {
                    return Err(ArchiveError::Cancelled);
                }
                let entry = index.get(id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
                let size = entry.size;
                let path_display = entry.path.clone();
                let Some(&zip_i) = self.id_to_zip.get(id.0 as usize) else {
                    // Synthetic directory (or skipped root): no data in the archive.
                    f(entry, &mut (&[0u8; 0][..]))?;
                    continue;
                };
                let mut zf = self.open_entry(zip_i as usize, entry.encrypted, &archive)?;
                let mut stream = EntryStream::new(&mut zf, cancel, &archive, size);
                f(entry, &mut stream)?;
                let seen = stream.bytes_read();
                drain(&mut stream, &archive, cancel)?;
                let total = seen.max(stream.bytes_read());
                if total < size {
                    return Err(ArchiveError::Corrupt {
                        archive: archive.clone(),
                        detail: format!(
                            "entry {} ended after {total} of {size} declared bytes",
                            path_display.display(),
                        ),
                    });
                }
            }
            Ok(())
        })
    }
}

impl ZipReader<'_> {
    /// Open one entry's data, asking the password provider for encrypted entries.
    fn open_entry(
        &mut self,
        zip_i: usize,
        encrypted: bool,
        archive: &Path,
    ) -> Result<zip::read::ZipFile<'_, std::fs::File>, ArchiveError> {
        if !encrypted {
            return self
                .archive
                .by_index(zip_i)
                .map_err(|e| map_zip_err(archive, e));
        }
        // Validate the password in the loop (dropping each attempt's file so the
        // borrow ends within the iteration), then open once more for the caller.
        let mut attempts = 0;
        let pw = loop {
            let Some(pw) = self.passwords.password(archive, attempts > 0) else {
                return Err(ArchiveError::NeedsPassword(archive.to_owned()));
            };
            match self.archive.by_index_decrypt(zip_i, pw.as_bytes()) {
                Ok(_) => break pw,
                Err(ZipError::InvalidPassword) => {
                    attempts += 1;
                    if attempts >= MAX_PASSWORD_ATTEMPTS {
                        return Err(ArchiveError::WrongPassword(archive.to_owned()));
                    }
                }
                Err(e) => return Err(map_zip_err(archive, e)),
            }
        };
        self.passwords.accepted(archive, &pw);
        self.archive
            .by_index_decrypt(zip_i, pw.as_bytes())
            .map_err(|e| map_zip_err(archive, e))
    }
}

/// Decode CP437 bytes to a `String` (every byte maps to a char, so this never fails).
fn cp437_to_string(raw: &[u8]) -> String {
    raw.iter().map(|&b| CP437[b as usize]).collect()
}

/// IBM codepage 437 (byte -> char), matching the table used by the `zip` crate.
#[rustfmt::skip]
const CP437: [char; 256] = [
    '\u{0000}', '\u{0001}', '\u{0002}', '\u{0003}', '\u{0004}', '\u{0005}', '\u{0006}', '\u{0007}',
    '\u{0008}', '\u{0009}', '\u{000A}', '\u{000B}', '\u{000C}', '\u{000D}', '\u{000E}', '\u{000F}',
    '\u{0010}', '\u{0011}', '\u{0012}', '\u{0013}', '\u{0014}', '\u{0015}', '\u{0016}', '\u{0017}',
    '\u{0018}', '\u{0019}', '\u{001A}', '\u{001B}', '\u{001C}', '\u{001D}', '\u{001E}', '\u{001F}',
    ' ', '!', '"', '#', '$', '%', '&', '\'', '(', ')', '*', '+', ',', '-', '.', '/',
    '0', '1', '2', '3', '4', '5', '6', '7', '8', '9', ':', ';', '<', '=', '>', '?',
    '@', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O',
    'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z', '[', '\\', ']', '^', '_',
    '`', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l', 'm', 'n', 'o',
    'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '{', '|', '}', '~', '\u{007F}',
    '\u{00C7}', '\u{00FC}', '\u{00E9}', '\u{00E2}', '\u{00E4}', '\u{00E0}', '\u{00E5}', '\u{00E7}',
    '\u{00EA}', '\u{00EB}', '\u{00E8}', '\u{00EF}', '\u{00EE}', '\u{00EC}', '\u{00C4}', '\u{00C5}',
    '\u{00C9}', '\u{00E6}', '\u{00C6}', '\u{00F4}', '\u{00F6}', '\u{00F2}', '\u{00FB}', '\u{00F9}',
    '\u{00FF}', '\u{00D6}', '\u{00DC}', '\u{00A2}', '\u{00A3}', '\u{00A5}', '\u{20A7}', '\u{0192}',
    '\u{00E1}', '\u{00ED}', '\u{00F3}', '\u{00FA}', '\u{00F1}', '\u{00D1}', '\u{00AA}', '\u{00BA}',
    '\u{00BF}', '\u{2310}', '\u{00AC}', '\u{00BD}', '\u{00BC}', '\u{00A1}', '\u{00AB}', '\u{00BB}',
    '\u{2591}', '\u{2592}', '\u{2593}', '\u{2502}', '\u{2524}', '\u{2561}', '\u{2562}', '\u{2556}',
    '\u{2555}', '\u{2563}', '\u{2551}', '\u{2557}', '\u{255D}', '\u{255C}', '\u{255B}', '\u{2510}',
    '\u{2514}', '\u{2534}', '\u{252C}', '\u{251C}', '\u{2500}', '\u{253C}', '\u{255E}', '\u{255F}',
    '\u{255A}', '\u{2554}', '\u{2569}', '\u{2566}', '\u{2560}', '\u{2550}', '\u{256C}', '\u{2567}',
    '\u{2568}', '\u{2564}', '\u{2565}', '\u{2559}', '\u{2558}', '\u{2552}', '\u{2553}', '\u{256B}',
    '\u{256A}', '\u{2518}', '\u{250C}', '\u{2588}', '\u{2584}', '\u{258C}', '\u{2590}', '\u{2580}',
    '\u{03B1}', '\u{00DF}', '\u{0393}', '\u{03C0}', '\u{03A3}', '\u{03C3}', '\u{00B5}', '\u{03C4}',
    '\u{03A6}', '\u{0398}', '\u{03A9}', '\u{03B4}', '\u{221E}', '\u{03C6}', '\u{03B5}', '\u{2229}',
    '\u{2261}', '\u{00B1}', '\u{2265}', '\u{2264}', '\u{2320}', '\u{2321}', '\u{00F7}', '\u{2248}',
    '\u{00B0}', '\u{2219}', '\u{00B7}', '\u{221A}', '\u{207F}', '\u{00B2}', '\u{25A0}', '\u{00A0}',
];

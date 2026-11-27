//! 7z backend (T6.1). Crate `sevenz-rust2` (features `aes256 bzip2 ppmd zstd deflate util`; pure
//! Rust).
//!
//! Passwords come in two places. Encrypted headers need one at open: the library reports a missing
//! password as `PasswordRequired` and a wrong one as `MaybeBadPassword`; both drive the retry loop
//! against the `PasswordProvider` (`retry = true` once a candidate failed, at most
//! [`MAX_PASSWORD_ATTEMPTS`] provider calls, `accepted` on success). With plain headers (the 7-Zip
//! default) only the data is encrypted, and a wrong password is noticed only while decoding: so
//! `visit` first checks the password against one of the blocks it needs (decode until a CRC
//! matches), re-prompting the same bounded way, and only then hands data to the visitor.
//!
//! The library's memory limit enforces `ArchiveLimits::max_codec_memory`
//! (`LimitExceeded(CodecMemory)`). Entry names are UTF-16 in the 7z format, so the `String` the
//! library returns is complete; symlink detection uses the POSIX attributes in the high 16 bits of
//! `windows_attributes` (stored when the `0x8000` flag is set). Symlink *targets* are entry data,
//! read before indexing from just the blocks that hold them. An encrypted target is read only if
//! the header password is known (indexing never prompts for data); otherwise the link has no
//! target and is flagged unsafe.
//!
//! Streaming order note: `visit` decodes only the blocks that hold selected data, in block order,
//! entries in file order, and stops after the last selected one; then come the selected entries
//! without a data stream (empty files, directories). That is the natural "archive order" for a
//! solid format.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_core::CancelToken;
use sevenz_rust2::{Archive, ArchiveEntry, Error as SzError, Password};

use crate::archive::EntrySafety;
use crate::archive::error::{ArchiveError, LimitKind};
use crate::archive::format::ArchiveFormat;
use crate::archive::index::{ArchiveEntryKind, ArchiveIndex, EntryId, IndexBuilder, RawEntry};
use crate::archive::limits::ArchiveLimits;
use crate::archive::password::{MAX_PASSWORD_ATTEMPTS, PasswordProvider};
use crate::archive::reader::{ArchiveReader, VisitFn};

use super::stream::{EntryStream, catch_panic, map_io};

mod blocks;

use blocks::{BlockSource, PasswordCheck, WalkFail, block_is_encrypted, group_by_block};

/// Encrypted blocks a password is tried against before it counts as wrong.
const MAX_CHECKED_BLOCKS: usize = 3;
/// Symlink targets longer than this are not read at index time (the entry is flagged unsafe).
const MAX_SYMLINK_TARGET: u64 = 64 * 1024;

struct SevenzReader<'p> {
    archive: Archive,
    file: File,
    index: Arc<ArchiveIndex>,
    passwords: &'p dyn PasswordProvider,
    mem_kb: usize,
    /// The password that opened the headers or passed the last data check.
    password: Option<String>,
    /// Index id -> position in `archive.files`, for the stored entries (ids `0..len`). The stored
    /// root (`./`) has no index id, so positions cannot be used directly as ids. Ids at or beyond
    /// the length are synthetic directories.
    id_to_file: Vec<usize>,
}

/// Open a 7z archive and index it.
pub fn open<'p>(
    path: &Path,
    passwords: &'p dyn PasswordProvider,
    limits: &ArchiveLimits,
    cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader + 'p>, ArchiveError> {
    let mem_kb = usize::try_from(limits.max_codec_memory / 1024).unwrap_or(usize::MAX);
    let (archive, mut file, password) = open_with_password_loop(path, passwords, mem_kb, cancel)?;

    // Metadata of every file.
    let mut metas = Vec::with_capacity(archive.files.len());
    for (i, f) in archive.files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let mut raw = RawEntry::new(
            PathBuf::from(std::ffi::OsString::from_vec(f.name.as_bytes().to_vec())),
            sevenz_kind(f),
        );
        raw.size = f.size;
        raw.mtime_ns = f
            .has_last_modified_date
            .then(|| nt_to_unix_ns(u64::from(f.last_modified_date)));
        raw.mode = sevenz_mode(f);
        raw.encrypted = archive.is_file_encrypted(i);
        metas.push(raw);
    }

    // Symlink targets, before push (the safety classification needs them).
    let mut source = BlockSource {
        archive: &archive,
        file: &mut file,
        mem_kb,
        path,
        cancel,
    };
    read_symlink_targets(&mut source, &mut metas, password.as_deref())?;

    // Classify and build the index.
    let mut builder = IndexBuilder::new(path, ArchiveFormat::SevenZ, *limits);
    let mut id_to_file = Vec::new();
    for (i, raw) in metas.into_iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        if builder.push(raw)?.is_some() {
            id_to_file.push(i);
        }
    }
    let index = builder.finish()?;

    Ok(Box::new(SevenzReader {
        archive,
        file,
        index: Arc::new(index),
        passwords,
        mem_kb,
        password,
        id_to_file,
    }))
}

/// Fill in `link_target` of the symlink entries. Decodes only the blocks that hold one, and in
/// each only up to its last symlink. Encrypted blocks are skipped without a password.
fn read_symlink_targets(
    source: &mut BlockSource<'_>,
    metas: &mut [RawEntry],
    password: Option<&str>,
) -> Result<(), ArchiveError> {
    let path = source.path;
    let links = metas
        .iter()
        .enumerate()
        .filter(|(_, m)| m.kind == ArchiveEntryKind::Symlink && m.size <= MAX_SYMLINK_TARGET)
        .map(|(i, _)| i);
    for (block, positions) in group_by_block(source.archive, links) {
        let encrypted = block_is_encrypted(source.archive, &positions);
        let key = match (encrypted, password) {
            (false, _) => Password::empty(),
            (true, Some(p)) => Password::from(p),
            (true, None) => continue,
        };
        let walked = source.walk(block, &key, &positions, &mut |pos, stream| {
            metas[pos].link_target = read_symlink_target(metas[pos].size, stream, path)?;
            Ok(())
        });
        match walked {
            Ok(()) => {}
            Err(WalkFail::Visit(ArchiveError::Cancelled)) => return Err(ArchiveError::Cancelled),
            // The data may use another password than the headers: leave the targets unknown.
            Err(_) if encrypted => {}
            Err(e) => return Err(e.into_error(path)),
        }
    }
    Ok(())
}

/// Parse the headers, asking the provider until they decrypt (or give up). Returns the password
/// that worked, if one was needed.
fn open_with_password_loop(
    path: &Path,
    passwords: &dyn PasswordProvider,
    mem_kb: usize,
    cancel: &CancelToken,
) -> Result<(Archive, File, Option<String>), ArchiveError> {
    let mut attempt = 0u32;
    loop {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        // Attempt 0 tries the empty password: unencrypted archives open without prompting, and a
        // provider-side cache can still be consulted on attempt 1 with retry = false.
        let text = if attempt == 0 {
            None
        } else {
            Some(ask(path, passwords, attempt > 1, cancel)?)
        };
        let password = text.as_deref().map_or_else(Password::empty, Password::from);
        let mut file = File::open(path).map_err(|e| ArchiveError::io(path, e))?;
        let parsed = catch_panic(path, || {
            Ok(Archive::read_with_memory_limit(
                &mut file, &password, mem_kb,
            ))
        })?;
        match parsed {
            Ok(archive) => {
                if let Some(t) = &text {
                    passwords.accepted(path, t);
                }
                return Ok((archive, file, text));
            }
            Err(SzError::PasswordRequired) | Err(SzError::MaybeBadPassword(_)) => {
                attempt += 1;
                if attempt > MAX_PASSWORD_ATTEMPTS {
                    return Err(ArchiveError::WrongPassword(path.to_owned()));
                }
            }
            Err(e) => return Err(map_sevenz_err(path, e)),
        }
    }
}

/// One question to the provider; no answer is `NeedsPassword` (or `Cancelled` if that is why).
fn ask(
    path: &Path,
    passwords: &dyn PasswordProvider,
    retry: bool,
    cancel: &CancelToken,
) -> Result<String, ArchiveError> {
    passwords.password(path, retry).ok_or_else(|| {
        if cancel.is_cancelled() {
            ArchiveError::Cancelled
        } else {
            ArchiveError::NeedsPassword(path.to_owned())
        }
    })
}

/// Windows file time (100 ns units since 1601) to unix ns.
fn nt_to_unix_ns(ft: u64) -> i128 {
    const EPOCH_DIFF: i128 = 116_444_736_000_000_000; // 1601 -> 1970 in 100 ns units
    (i128::from(ft) - EPOCH_DIFF) * 100
}

/// Kind of a 7z entry (directories, streams, POSIX mode bits in the attributes).
fn sevenz_kind(f: &ArchiveEntry) -> ArchiveEntryKind {
    if f.is_anti_item {
        return ArchiveEntryKind::Other;
    }
    if f.is_directory {
        return ArchiveEntryKind::Dir;
    }
    if !f.has_stream {
        return ArchiveEntryKind::File; // empty file
    }
    match sevenz_mode(f) {
        Some(m) if m & 0o170000 == 0o120000 => ArchiveEntryKind::Symlink,
        Some(m) if !matches!(m & 0o170000, 0o100000 | 0) => ArchiveEntryKind::Other,
        _ => ArchiveEntryKind::File,
    }
}

/// POSIX mode from the 7z "windows attributes" (upper 16 bits when the unix flag is set).
fn sevenz_mode(f: &ArchiveEntry) -> Option<u32> {
    if !f.has_windows_attributes || f.windows_attributes & 0x8000 == 0 {
        return None;
    }
    // Keep the file-type bits: `sevenz_kind` needs S_IFLNK/S_IFREG. IndexBuilder masks
    // permissions to 0o7777 when it stores the entry.
    Some(f.windows_attributes >> 16)
}

/// Read a symlink entry's target text (UTF-16LE with or without BOM, else raw bytes).
fn read_symlink_target(
    size: u64,
    stream: &mut dyn Read,
    path: &Path,
) -> Result<Option<PathBuf>, ArchiveError> {
    if size > MAX_SYMLINK_TARGET {
        return Ok(None);
    }
    let mut buf = Vec::with_capacity(size.min(MAX_SYMLINK_TARGET) as usize);
    stream.read_to_end(&mut buf).map_err(|e| map_io(path, e))?;
    let text: Vec<u8> =
        if buf.starts_with(&[0xFF, 0xFE]) || (buf.len() >= 2 && buf[0] != 0 && buf[1] == 0) {
            let raw = if buf.starts_with(&[0xFF, 0xFE]) {
                &buf[2..]
            } else {
                &buf[..]
            };
            let (pairs, _rem) = raw.as_chunks::<2>();
            let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
            String::from_utf16_lossy(&units).into_bytes()
        } else {
            buf
        };
    Ok(Some(PathBuf::from(std::ffi::OsString::from_vec(text))))
}

/// Map `sevenz-rust2` errors to [`ArchiveError`].
fn map_sevenz_err(path: &Path, e: SzError) -> ArchiveError {
    match e {
        SzError::PasswordRequired => ArchiveError::NeedsPassword(path.to_owned()),
        SzError::MaybeBadPassword(_) => ArchiveError::WrongPassword(path.to_owned()),
        SzError::MaxMemLimited { .. } => ArchiveError::LimitExceeded {
            archive: path.to_owned(),
            kind: LimitKind::CodecMemory,
        },
        SzError::UnsupportedCompressionMethod(m) => ArchiveError::Unsupported {
            archive: path.to_owned(),
            what: m,
        },
        SzError::Unsupported(what) => ArchiveError::Unsupported {
            archive: path.to_owned(),
            what: what.into_owned(),
        },
        SzError::BadSignature(_)
        | SzError::NextHeaderCrcMismatch
        | SzError::ChecksumVerificationFailed => ArchiveError::Corrupt {
            archive: path.to_owned(),
            detail: format!("{e:?}"),
        },
        SzError::Io(io, ctx) => ArchiveError::Corrupt {
            archive: path.to_owned(),
            detail: format!("io error: {io} ({ctx})"),
        },
        other => ArchiveError::Corrupt {
            archive: path.to_owned(),
            detail: format!("7z error: {other:?}"),
        },
    }
}

impl SevenzReader<'_> {
    /// Find a password that decrypts one of `blocks` (cheapest first; more than one, so a damaged
    /// block does not lock the rest): the known one, then up to [`MAX_PASSWORD_ATTEMPTS`] answers
    /// of the provider.
    fn unlock(&mut self, blocks: &[usize], cancel: &CancelToken) -> Result<Password, ArchiveError> {
        let path = self.index.archive().to_owned();
        let mut known = self.password.take();
        let (mut asked, mut retry) = (0u32, false);
        loop {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let text = match known.take() {
                Some(t) => t,
                None if asked < MAX_PASSWORD_ATTEMPTS => {
                    asked += 1;
                    ask(&path, self.passwords, retry, cancel)?
                }
                None => return Err(ArchiveError::WrongPassword(path)),
            };
            let password = Password::from(text.as_str());
            let mut source = BlockSource {
                archive: &self.archive,
                file: &mut self.file,
                mem_kb: self.mem_kb,
                path: &path,
                cancel,
            };
            let mut outcome = PasswordCheck::Wrong;
            for block in blocks.iter().take(MAX_CHECKED_BLOCKS) {
                match source.check_password(*block, &password)? {
                    PasswordCheck::Verified => {
                        outcome = PasswordCheck::Verified;
                        break;
                    }
                    PasswordCheck::Unchecked => outcome = PasswordCheck::Unchecked,
                    PasswordCheck::Wrong => {}
                }
            }
            match outcome {
                PasswordCheck::Verified => self.passwords.accepted(&path, &text),
                // No checksum to compare with: use it, but do not let a cache remember it.
                PasswordCheck::Unchecked => {}
                PasswordCheck::Wrong => {
                    // `retry = true` also makes a caching provider forget the stale entry.
                    retry = true;
                    continue;
                }
            }
            self.password = Some(text);
            return Ok(password);
        }
    }
}

impl ArchiveReader for SevenzReader<'_> {
    fn index(&self) -> &Arc<ArchiveIndex> {
        &self.index
    }

    fn visit(
        &mut self,
        ids: &[EntryId],
        cancel: &CancelToken,
        f: &mut VisitFn<'_>,
    ) -> Result<(), ArchiveError> {
        let index = Arc::clone(&self.index);
        let path = index.archive();
        // Stored entries by file position (= archive order), and synthetic directories.
        let mut pending: BTreeMap<usize, EntryId> = BTreeMap::new();
        let mut synthetic: Vec<EntryId> = Vec::new();
        for id in ids {
            let entry = index.get(*id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            if let EntrySafety::Unsafe(reason) = entry.safety {
                return Err(ArchiveError::Unsafe {
                    path: entry.path.clone(),
                    reason,
                });
            }
            match self.id_to_file.get(id.0 as usize) {
                Some(pos) => {
                    pending.insert(*pos, *id);
                }
                None => synthetic.push(*id),
            }
        }
        synthetic.sort();

        // Only the blocks that hold selected data are decoded.
        let mut work = group_by_block(&self.archive, pending.keys().copied());
        work.retain(|_, positions| positions.iter().any(|p| self.archive.files[*p].has_stream));

        // Before anything is delivered: one password that is known to work, checked against the
        // smallest encrypted blocks needed.
        let mut locked: Vec<usize> = work
            .iter()
            .filter(|(_, positions)| block_is_encrypted(&self.archive, positions))
            .map(|(block, _)| *block)
            .collect();
        locked.sort_by_key(|block| self.archive.blocks[*block].get_unpack_size());
        let password = if locked.is_empty() {
            Password::empty()
        } else {
            self.unlock(&locked, cancel)?
        };

        for (block, positions) in &work {
            let key = if block_is_encrypted(&self.archive, positions) {
                password.clone()
            } else {
                Password::empty()
            };
            let mut source = BlockSource {
                archive: &self.archive,
                file: &mut self.file,
                mem_kb: self.mem_kb,
                path,
                cancel,
            };
            source
                .walk(*block, &key, positions, &mut |pos, stream| {
                    let Some(entry) = pending.remove(&pos).and_then(|id| index.get(id)) else {
                        return Ok(());
                    };
                    let mut wrapped = EntryStream::new(stream, cancel, path, entry.size);
                    f(entry, &mut wrapped)
                })
                .map_err(|e| e.into_error(path))?;
        }

        // Entries without data (empty files, directories), then synthetic directories.
        if let Some(pos) = pending.keys().find(|p| self.archive.files[**p].has_stream) {
            return Err(ArchiveError::Corrupt {
                archive: path.to_owned(),
                detail: format!("no data for entry {pos}"),
            });
        }
        for id in pending.into_values().chain(synthetic) {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = index.get(id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            f(entry, &mut (&[0u8; 0][..]))?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn root_entry_does_not_shift_stream_ids() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("root.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
        writer
            .push_archive_entry(
                ArchiveEntry {
                    name: "./".to_owned(),
                    is_directory: true,
                    ..ArchiveEntry::default()
                },
                None::<&[u8]>,
            )
            .unwrap();
        for (name, data) in [("a.txt", &b"aa"[..]), ("b.txt", &b"bb"[..])] {
            writer
                .push_archive_entry(
                    ArchiveEntry {
                        name: name.to_owned(),
                        has_stream: true,
                        ..ArchiveEntry::default()
                    },
                    Some(data),
                )
                .unwrap();
        }
        writer.finish().unwrap();

        let mut reader = open(
            &path,
            &crate::archive::NoPassword,
            &ArchiveLimits::default(),
            &CancelToken::new(),
        )
        .unwrap();
        for (name, expected) in [("a.txt", b"aa"), ("b.txt", b"bb")] {
            let id = reader.index().lookup(Path::new(name)).unwrap().id;
            let mut data = Vec::new();
            reader
                .visit(&[id], &CancelToken::new(), &mut |_, stream| {
                    stream
                        .read_to_end(&mut data)
                        .map_err(|e| map_io(&path, e))?;
                    Ok(())
                })
                .unwrap();
            assert_eq!(data, expected);
        }
    }

    #[test]
    fn symlink_target_pass_honours_cancel() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("link.7z");
        let mut writer = sevenz_rust2::ArchiveWriter::create(&path).unwrap();
        writer
            .push_archive_entry(
                ArchiveEntry {
                    name: "l".to_owned(),
                    has_stream: true,
                    ..ArchiveEntry::default()
                },
                Some(&b"target"[..]),
            )
            .unwrap();
        writer.finish().unwrap();

        let mut file = File::open(&path).unwrap();
        let archive = Archive::read(&mut file, &Password::empty()).unwrap();
        let cancel = CancelToken::new();
        cancel.cancel();
        let mut source = BlockSource {
            archive: &archive,
            file: &mut file,
            mem_kb: 1 << 20,
            path: &path,
            cancel: &cancel,
        };
        let mut metas = [RawEntry::new("l", ArchiveEntryKind::Symlink)];
        metas[0].size = 6;
        let result = read_symlink_targets(&mut source, &mut metas, None);
        assert!(matches!(result, Err(ArchiveError::Cancelled)));
        assert!(metas[0].link_target.is_none());
    }

    #[test]
    fn nt_time_converts_to_unix_time() {
        // 2023-01-01T00:00:00Z.
        assert_eq!(
            nt_to_unix_ns(133_170_048_000_000_000),
            1_672_531_200 * 1_000_000_000
        );
        assert_eq!(nt_to_unix_ns(116_444_735_990_000_000), -1_000_000_000);
    }

    #[test]
    fn posix_symlink_attributes_determine_the_kind() {
        let entry = ArchiveEntry {
            has_stream: true,
            has_windows_attributes: true,
            windows_attributes: 0x8000 | (0o120777 << 16),
            ..ArchiveEntry::default()
        };
        assert_eq!(sevenz_kind(&entry), ArchiveEntryKind::Symlink);
        assert_eq!(sevenz_mode(&entry), Some(0o120777));
    }
}

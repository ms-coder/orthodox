//! 7z backend (T6.1). Crate `sevenz-rust2` (features `aes256 bzip2 ppmd zstd deflate util`; pure
//! Rust).
//!
//! Encrypted headers need the password at open: the library reports a missing password as
//! `PasswordRequired` and a wrong one as `MaybeBadPassword`; both drive the retry loop against
//! the `PasswordProvider` (`retry = true` once the cached candidate failed, at most
//! [`MAX_PASSWORD_ATTEMPTS`] user attempts, `accepted` on success). The library's memory-limit
//! constructor enforces `ArchiveLimits::max_codec_memory` (`LimitExceeded(CodecMemory)`).
//! `visit` maps onto `for_each_entries_with_decision` (one pass; unread data of unselected
//! entries is drained and verified by the library). Entry names are UTF-16 in the 7z format, so
//! the `String` the library returns is complete; symlink detection uses the POSIX attributes in
//! the high 16 bits of `windows_attributes` (stored when the `0x8000` flag is set), and symlink
//! *targets* are read in a `for_each_entries` pass *before* indexing, only when the archive
//! actually contains symlinks (that pass decodes the archive once).
//!
//! Streaming order note: the library visits the entries of each block in file order, then
//! entries without a data stream (empty files, directories) in file order. That is the natural
//! "archive order" for a solid format; directory entries are delivered before/without data.

use std::collections::HashSet;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_core::CancelToken;
use sevenz_rust2::{
    ArchiveEntry, ArchiveReader as SzReader, EntryDecision, Error as SzError, Password,
};

use crate::archive::EntrySafety;
use crate::archive::error::{ArchiveError, LimitKind};
use crate::archive::format::ArchiveFormat;
use crate::archive::index::{ArchiveEntryKind, ArchiveIndex, EntryId, IndexBuilder, RawEntry};
use crate::archive::limits::ArchiveLimits;
use crate::archive::password::{MAX_PASSWORD_ATTEMPTS, PasswordProvider};
use crate::archive::reader::{ArchiveReader, VisitFn};

use super::stream::{EntryStream, catch_panic, map_io};

/// Symlink targets longer than this are not read at index time (the entry is flagged unsafe).
const MAX_SYMLINK_TARGET: u64 = 64 * 1024;

struct SevenzReader {
    reader: SzReader<std::fs::File>,
    index: Arc<ArchiveIndex>,
    /// Number of stored (pushed) entries; ids at or beyond this are synthetic directories.
    stored: u32,
}

/// Open a 7z archive and index it.
pub fn open<'p>(
    path: &Path,
    passwords: &'p dyn PasswordProvider,
    limits: &ArchiveLimits,
    cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader + 'p>, ArchiveError> {
    let mem_kb = usize::try_from(limits.max_codec_memory / 1024).unwrap_or(usize::MAX);
    let mut reader = open_with_password_loop(path, passwords, mem_kb)?;
    let files = reader.archive().files.clone();

    // Metadata of every file (owned; frees the reader for the target pass).
    let mut metas = Vec::with_capacity(files.len());
    let mut has_symlinks = false;
    for (i, f) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let kind = sevenz_kind(f);
        has_symlinks |= kind == ArchiveEntryKind::Symlink;
        let mut raw = RawEntry::new(
            PathBuf::from(std::ffi::OsString::from_vec(f.name.as_bytes().to_vec())),
            kind,
        );
        raw.size = f.size;
        raw.mtime_ns = f
            .has_last_modified_date
            .then(|| nt_to_unix_ns(u64::from(f.last_modified_date)));
        raw.mode = sevenz_mode(f);
        raw.encrypted = reader.archive().is_file_encrypted(i);
        metas.push(raw);
    }

    // Symlink targets, before push (the safety classification needs them).
    if has_symlinks {
        let mut targets: Vec<Option<PathBuf>> = vec![None; metas.len()];
        let files_base = reader.archive().files.as_ptr() as usize;
        let entry_size = std::mem::size_of::<ArchiveEntry>();
        catch_panic(path, || {
            reader
                .for_each_entries(|lib_entry, stream| {
                    let Some(id) = entry_id(lib_entry, files_base, entry_size) else {
                        return Ok(true);
                    };
                    if metas[id].kind == ArchiveEntryKind::Symlink
                        && !metas[id].encrypted
                        && lib_entry.size <= MAX_SYMLINK_TARGET
                    {
                        targets[id] = read_symlink_target(lib_entry, stream, path).ok().flatten();
                    }
                    Ok(true)
                })
                .map_err(|e| map_sevenz_err(path, e))?;
            Ok::<_, ArchiveError>(())
        })?;
        for (raw, target) in metas.iter_mut().zip(targets) {
            raw.link_target = target;
        }
    }

    // Classify and build the index.
    let mut builder = IndexBuilder::new(path, ArchiveFormat::SevenZ, *limits);
    let mut stored = 0u32;
    for raw in metas {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        if builder.push(raw)?.is_some() {
            stored += 1;
        }
    }
    let index = builder.finish();

    Ok(Box::new(SevenzReader {
        reader,
        index: Arc::new(index),
        stored,
    }))
}

/// Map a library entry reference back to its index in `archive.files` (the library hands out
/// references into that vector).
fn entry_id(entry: &ArchiveEntry, files_base: usize, entry_size: usize) -> Option<usize> {
    let offset = (entry as *const ArchiveEntry as usize).saturating_sub(files_base);
    if offset % entry_size == 0 {
        Some(offset / entry_size)
    } else {
        None
    }
}

/// Open the archive, asking the provider until the headers decrypt (or give up).
fn open_with_password_loop(
    path: &Path,
    passwords: &dyn PasswordProvider,
    mem_kb: usize,
) -> Result<SzReader<std::fs::File>, ArchiveError> {
    let mut attempt = 0u32;
    loop {
        // Attempt 0 tries the empty password: unencrypted archives open without prompting, and a
        // provider-side cache can still be consulted on attempt 1 with retry = false.
        let (pw, text) = if attempt == 0 {
            (Password::empty(), None)
        } else {
            let Some(p) = passwords.password(path, attempt > 1) else {
                return Err(ArchiveError::NeedsPassword(path.to_owned()));
            };
            (Password::from(p.as_str()), Some(p))
        };
        let file = std::fs::File::open(path).map_err(|e| ArchiveError::io(path, e))?;
        match SzReader::new_with_memory_limit(file, pw, mem_kb) {
            Ok(r) => {
                if let Some(t) = text {
                    passwords.accepted(path, &t);
                }
                return Ok(r);
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

/// Windows file time (100 ns units since 1601) to unix ns.
fn nt_to_unix_ns(ft: u64) -> i128 {
    const EPOCH_DIFF: u64 = 11_644_473_600_000_000; // 1601 -> 1970 in 100 ns units
    ft.saturating_sub(EPOCH_DIFF) as i128 * 100
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
    Some((f.windows_attributes >> 16) & 0o7777)
}

/// Read a symlink entry's target text (UTF-16LE with or without BOM, else raw bytes).
fn read_symlink_target(
    entry: &ArchiveEntry,
    stream: &mut dyn Read,
    path: &Path,
) -> Result<Option<PathBuf>, ArchiveError> {
    if entry.size > MAX_SYMLINK_TARGET {
        return Ok(None);
    }
    let mut buf = Vec::with_capacity(entry.size.min(MAX_SYMLINK_TARGET) as usize);
    stream.read_to_end(&mut buf).map_err(|e| map_io(path, e))?;
    let text: Vec<u8> =
        if buf.starts_with(&[0xFF, 0xFE]) || (buf.len() >= 2 && buf[0] != 0 && buf[1] == 0) {
            let raw = if buf.starts_with(&[0xFF, 0xFE]) {
                &buf[2..]
            } else {
                &buf[..]
            };
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
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

impl ArchiveReader for SevenzReader {
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
        for id in ids {
            let entry = self.index.get(*id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            if let EntrySafety::Unsafe(reason) = entry.safety {
                return Err(ArchiveError::Unsafe {
                    path: entry.path.clone(),
                    reason,
                });
            }
        }
        let wanted: HashSet<u32> = ids.iter().map(|i| i.0).collect();
        let mut synthetic: Vec<EntryId> = ids
            .iter()
            .copied()
            .filter(|id| id.0 >= self.stored)
            .collect();
        synthetic.sort();

        // The callback cannot return our error type, so failures are reported out-of-band and
        // the iteration is stopped.
        let mut aborted: Option<ArchiveError> = None;
        let files_base = self.reader.archive().files.as_ptr() as usize;
        let entry_size = std::mem::size_of::<ArchiveEntry>();
        catch_panic(&archive, || {
            let result = self
                .reader
                .for_each_entries_with_decision(|lib_entry, stream| {
                    if aborted.is_some() || cancel.is_cancelled() {
                        aborted.get_or_insert_with(|| ArchiveError::Cancelled);
                        return Ok(EntryDecision::Stop);
                    }
                    let Some(i) = entry_id(lib_entry, files_base, entry_size) else {
                        aborted = Some(ArchiveError::Corrupt {
                            archive: archive.clone(),
                            detail: "entry identity mismatch".to_owned(),
                        });
                        return Ok(EntryDecision::Stop);
                    };
                    let id = EntryId(i as u32);
                    if !wanted.contains(&id.0) {
                        // Unselected: leave unread; the library drains and verifies it.
                        return Ok(EntryDecision::Continue);
                    }
                    let Some(ix_entry) = self.index.get(id) else {
                        return Ok(EntryDecision::Continue);
                    };
                    let mut wrapped = EntryStream::new(stream, cancel, &archive, ix_entry.size);
                    match f(ix_entry, &mut wrapped) {
                        Ok(()) => Ok(EntryDecision::Continue),
                        Err(e) => {
                            aborted = Some(e);
                            Ok(EntryDecision::Stop)
                        }
                    }
                });
            match result {
                Ok(()) => Ok::<_, ArchiveError>(()),
                Err(e) => Err(map_sevenz_err(&archive, e)),
            }
        })?;
        if let Some(e) = aborted {
            return Err(e);
        }
        // Synthetic directories: f with an empty stream.
        for id in synthetic {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = self.index.get(id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            f(entry, &mut (&[0u8; 0][..]))?;
        }
        Ok(())
    }
}

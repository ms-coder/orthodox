//! Block-level decoding for the 7z backend: walk the wanted entries of one compression block, and
//! check a password against a block before any data reaches a caller.
//!
//! Both loops read every byte themselves (the library's own drain cannot be cancelled), so the
//! cancel token is polled at least once per buffer.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use ofm_core::CancelToken;
use sevenz_rust2::{Archive, ArchiveEntry, BlockDecoder, Error as SzError, Password};

use crate::archive::error::{ArchiveError, LimitKind};

use super::super::stream::{catch_panic, map_io};
use super::map_sevenz_err;

/// A password check stops once this many CRC-verified bytes decoded correctly. More than a few
/// bytes, so a wrong key cannot pass by matching the CRC of a tiny first file by chance.
const VERIFY_MIN_BYTES: u64 = 64;

/// What every block operation needs.
pub(super) struct BlockSource<'a> {
    pub(super) archive: &'a Archive,
    pub(super) file: &'a mut File,
    pub(super) mem_kb: usize,
    pub(super) path: &'a Path,
    pub(super) cancel: &'a CancelToken,
}

/// Why [`BlockSource::walk`] stopped early.
pub(super) enum WalkFail {
    /// The library could not set up or finish the block.
    Library(SzError),
    /// The data did not decode (bad CRC, short stream, wrong password).
    Data(io::Error),
    /// Cancel, or the error of the visitor.
    Visit(ArchiveError),
}

impl WalkFail {
    /// The error to report when no password question is involved any more.
    pub(super) fn into_error(self, path: &Path) -> ArchiveError {
        match self {
            WalkFail::Library(e) => map_sevenz_err(path, e),
            WalkFail::Data(e) => map_io(path, e),
            WalkFail::Visit(e) => e,
        }
    }
}

/// Outcome of [`BlockSource::check_password`].
pub(super) enum PasswordCheck {
    /// A checksum matched.
    Verified,
    /// The block decoded but stores no checksum to compare with.
    Unchecked,
    /// The password does not decrypt the block.
    Wrong,
}

/// Group the file positions by the block that holds them; positions without a block are left out.
pub(super) fn group_by_block(
    archive: &Archive,
    positions: impl Iterator<Item = usize>,
) -> BTreeMap<usize, BTreeSet<usize>> {
    let mut blocks: BTreeMap<usize, BTreeSet<usize>> = BTreeMap::new();
    for pos in positions {
        if let Some(Some(block)) = archive.stream_map.file_block_index.get(pos) {
            blocks.entry(*block).or_default().insert(pos);
        }
    }
    blocks
}

/// Whether the block holding these file positions is AES encrypted.
pub(super) fn block_is_encrypted(archive: &Archive, positions: &BTreeSet<usize>) -> bool {
    positions
        .first()
        .is_some_and(|pos| archive.is_file_encrypted(*pos))
}

impl BlockSource<'_> {
    /// Decode `block` and call `each(position, stream)` for the positions in `wanted`, in file
    /// order. Other entries (solid predecessors) are decoded and discarded; nothing after the last
    /// wanted entry is decoded.
    pub(super) fn walk(
        &mut self,
        block: usize,
        password: &Password,
        wanted: &BTreeSet<usize>,
        each: &mut dyn FnMut(usize, &mut dyn Read) -> Result<(), ArchiveError>,
    ) -> Result<(), WalkFail> {
        let (archive, cancel, path) = (self.archive, self.cancel, self.path);
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get().min(256) as u32);
        let decoder = BlockDecoder::new_with_memory_limit(
            threads,
            self.mem_kb,
            block,
            archive,
            password,
            &mut *self.file,
        );
        let first = archive.stream_map.block_first_file_index[block];
        let last_stream = decoder
            .entries()
            .iter()
            .rposition(|f| f.has_stream)
            .map_or(first, |i| first + i);
        let mut remaining = wanted.len();
        let mut failure = None;
        let result = catch_panic(path, || {
            Ok(decoder.for_each_entries(&mut |entry, stream| {
                if cancel.is_cancelled() {
                    failure = Some(WalkFail::Visit(ArchiveError::Cancelled));
                    return Ok(false);
                }
                let Some(pos) = position(archive, entry) else {
                    failure = Some(WalkFail::Visit(ArchiveError::Corrupt {
                        archive: path.to_owned(),
                        detail: "entry identity mismatch".to_owned(),
                    }));
                    return Ok(false);
                };
                if wanted.contains(&pos) {
                    remaining = remaining.saturating_sub(1);
                    if let Err(e) = each(pos, &mut *stream) {
                        failure = Some(WalkFail::Visit(e));
                        return Ok(false);
                    }
                }
                // Whatever is left: unwanted data, or what the visitor did not read. Reading to
                // the end is also what verifies the CRC.
                if let Err(e) = drain(stream, cancel) {
                    failure = Some(if cancel.is_cancelled() {
                        WalkFail::Visit(ArchiveError::Cancelled)
                    } else {
                        WalkFail::Data(e)
                    });
                    return Ok(false);
                }
                // At the end of the block let the library run its final size/CRC checks.
                Ok(remaining > 0 || pos >= last_stream)
            }))
        })
        .map_err(WalkFail::Visit)?;
        match (failure, result) {
            (Some(f), _) => Err(f),
            (None, Err(e)) => Err(WalkFail::Library(e)),
            (None, Ok(_)) => Ok(()),
        }
    }

    /// Decode the start of an encrypted `block` with `password` until a checksum confirms it.
    pub(super) fn check_password(
        &mut self,
        block: usize,
        password: &Password,
    ) -> Result<PasswordCheck, ArchiveError> {
        let (archive, cancel, path) = (self.archive, self.cancel, self.path);
        if password.is_empty() {
            return Ok(PasswordCheck::Wrong);
        }
        // One thread: the check usually stops after a few bytes.
        let decoder = BlockDecoder::new_with_memory_limit(
            1,
            self.mem_kb,
            block,
            archive,
            password,
            &mut *self.file,
        );
        let mut checked = 0u64;
        let mut failure = None;
        let result = catch_panic(path, || {
            Ok(decoder.for_each_entries(&mut |entry, stream| {
                if let Err(e) = drain(stream, cancel) {
                    failure = Some(e);
                    return Ok(false);
                }
                if entry.has_stream && entry.has_crc {
                    checked = checked.saturating_add(entry.size);
                }
                Ok(checked < VERIFY_MIN_BYTES)
            }))
        })?;
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        match (failure, result) {
            (Some(e), _) => decode_failure(path, e),
            (None, Err(SzError::PasswordRequired | SzError::ChecksumVerificationFailed)) => {
                Ok(PasswordCheck::Wrong)
            }
            (None, Err(SzError::MaybeBadPassword(e) | SzError::Io(e, _))) => {
                decode_failure(path, e)
            }
            (None, Err(e)) => Err(map_sevenz_err(path, e)),
            (None, Ok(completed)) => {
                let block_crc = completed && archive.blocks[block].has_crc;
                Ok(if checked > 0 || block_crc {
                    PasswordCheck::Verified
                } else {
                    PasswordCheck::Unchecked
                })
            }
        }
    }
}

/// Classify an `io::Error` met while decoding an encrypted block with a candidate password:
/// garbage from a wrong key, as opposed to a real read error or the memory limit.
fn decode_failure(path: &Path, e: io::Error) -> Result<PasswordCheck, ArchiveError> {
    if e.raw_os_error().is_some() {
        Err(ArchiveError::io(path, e))
    } else if e.kind() == io::ErrorKind::OutOfMemory {
        Err(ArchiveError::LimitExceeded {
            archive: path.to_owned(),
            kind: LimitKind::CodecMemory,
        })
    } else {
        Ok(PasswordCheck::Wrong)
    }
}

/// Read `stream` to its end, polling `cancel`.
fn drain(stream: &mut dyn Read, cancel: &CancelToken) -> io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        if cancel.is_cancelled() {
            return Err(io::Error::other(ArchiveError::Cancelled));
        }
        match stream.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Map a library entry reference back to its position in `archive.files` (the library hands out
/// references into that vector).
fn position(archive: &Archive, entry: &ArchiveEntry) -> Option<usize> {
    let size = std::mem::size_of::<ArchiveEntry>();
    let offset =
        (entry as *const ArchiveEntry as usize).checked_sub(archive.files.as_ptr() as usize)?;
    let pos = offset / size;
    (offset.is_multiple_of(size) && pos < archive.files.len()).then_some(pos)
}

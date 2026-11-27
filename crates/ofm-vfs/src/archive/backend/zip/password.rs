//! Finding the password of encrypted zip entries.
//!
//! The header check is weak: ZipCrypto compares one byte, so about 1 in 256 wrong passwords pass
//! it (AES compares two bytes). Only the CRC/MAC at the end of the data tells. So a candidate is
//! checked by decrypting entry data to its end *before* anything reaches the visitor.

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use ofm_core::CancelToken;
use zip::ZipArchive;
use zip::result::ZipError;

use crate::archive::error::{ArchiveError, LimitKind};
use crate::archive::password::{MAX_PASSWORD_ATTEMPTS, PasswordProvider};

use super::super::stream::{EntryStream, map_io};
use super::map_zip_err;

/// A check stops once this many bytes passed their CRC/MAC.
const VERIFY_MIN_BYTES: u64 = 16;
/// Damaged entries a check looks past before it blames the password. With a right password, one
/// damaged entry must not lock the others.
const MAX_FAILED_ENTRIES: u32 = 3;
/// Upper bound of entries one check opens.
const MAX_CHECKED_ENTRIES: usize = 32;

/// An encrypted entry a password can be checked against.
#[derive(Debug, Clone, Copy)]
pub(super) struct Candidate {
    pub(super) zip_i: usize,
    pub(super) size: u64,
    pub(super) packed: u64,
}

/// What one password did.
enum Check {
    /// Data passed its CRC/MAC.
    Verified,
    /// The header check passed and there is no data to compare.
    Unchecked,
    /// The header check failed.
    Rejected,
    /// The header check passed, the data did not (wrong password or damaged entry).
    Damaged(ArchiveError),
}

/// What [`unlock`] needs besides the candidates.
pub(super) struct Unlock<'a> {
    pub(super) zip: &'a mut ZipArchive<File>,
    pub(super) passwords: &'a dyn PasswordProvider,
    pub(super) path: &'a Path,
    pub(super) cancel: &'a CancelToken,
}

/// A password that opens `candidates`: `known` first, then at most [`MAX_PASSWORD_ATTEMPTS`]
/// answers of the provider.
///
/// Errors: `NeedsPassword` (no answer at all), `WrongPassword`, and `Corrupt` once two or more
/// passwords passed the header check but not the data (a wrong password rarely does that once).
pub(super) fn unlock(
    ctx: &mut Unlock<'_>,
    candidates: &mut [Candidate],
    mut known: Option<String>,
) -> Result<String, ArchiveError> {
    // Cheapest data first; empty entries prove nothing, so they come last.
    candidates.sort_by_key(|c| (c.size == 0, c.packed, c.zip_i));
    let path = ctx.path;
    let (mut asked, mut rejected) = (0u32, false);
    let (mut damaged, mut damage) = (0u32, None);
    loop {
        if ctx.cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let answer = match known.take() {
            Some(text) => Some(text),
            None if asked < MAX_PASSWORD_ATTEMPTS => {
                asked += 1;
                ctx.passwords.password(path, rejected)
            }
            None => None,
        };
        let Some(text) = answer else {
            if ctx.cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            return Err(match damage {
                Some(e) if damaged >= 2 => e,
                _ if rejected => ArchiveError::WrongPassword(path.to_owned()),
                _ => ArchiveError::NeedsPassword(path.to_owned()),
            });
        };
        match check(ctx, candidates, &text)? {
            Check::Verified => {
                ctx.passwords.accepted(path, &text);
                return Ok(text);
            }
            // Nothing to compare with: use it, but do not let a cache remember it.
            Check::Unchecked => return Ok(text),
            Check::Rejected => {}
            Check::Damaged(e) => {
                damaged += 1;
                damage = Some(e);
            }
        }
        rejected = true;
    }
}

fn check(
    ctx: &mut Unlock<'_>,
    candidates: &[Candidate],
    password: &str,
) -> Result<Check, ArchiveError> {
    let (path, cancel) = (ctx.path, ctx.cancel);
    let (mut verified, mut failed, mut damage) = (0u64, 0u32, None);
    for (n, c) in candidates.iter().take(MAX_CHECKED_ENTRIES).enumerate() {
        if cancel.is_cancelled() {
            return Err(ArchiveError::Cancelled);
        }
        let mut file = match ctx.zip.by_index_decrypt(c.zip_i, password.as_bytes()) {
            Ok(file) => file,
            Err(ZipError::InvalidPassword) if n == 0 => return Ok(Check::Rejected),
            Err(ZipError::InvalidPassword) => continue,
            Err(e) => return Err(map_zip_err(path, e)),
        };
        if c.size == 0 {
            continue;
        }
        let mut stream = EntryStream::new(&mut file, cancel, path, c.size);
        match read_to_end(&mut stream) {
            Ok(total) if total == c.size => {
                verified = verified.saturating_add(total);
                if verified >= VERIFY_MIN_BYTES {
                    break;
                }
                continue;
            }
            Ok(total) => {
                damage = Some(damaged_data(
                    path,
                    format!("entry ended after {total} of {} declared bytes", c.size),
                ));
            }
            Err(e) => damage = Some(data_failure(path, cancel, e)?),
        }
        failed += 1;
        if failed == MAX_FAILED_ENTRIES {
            break;
        }
    }
    Ok(match damage {
        _ if verified > 0 => Check::Verified,
        Some(e) => Check::Damaged(e),
        None => Check::Unchecked,
    })
}

/// Read and discard; the stream polls the cancel token itself.
fn read_to_end(stream: &mut dyn Read) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        match stream.read(&mut buf) {
            Ok(0) => return Ok(total),
            Ok(n) => total += n as u64,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
}

/// Split a read error: `Ok` = the data is wrong (blame password or archive, as the returned error
/// describes), `Err` = something no other password would change.
fn data_failure(
    path: &Path,
    cancel: &CancelToken,
    e: io::Error,
) -> Result<ArchiveError, ArchiveError> {
    if cancel.is_cancelled() {
        Err(ArchiveError::Cancelled)
    } else if e.raw_os_error().is_some() {
        Err(ArchiveError::io(path, e))
    } else if e.kind() == io::ErrorKind::OutOfMemory {
        Err(ArchiveError::LimitExceeded {
            archive: path.to_owned(),
            kind: LimitKind::CodecMemory,
        })
    } else {
        Ok(match map_io(path, e) {
            ArchiveError::Corrupt { detail, .. } => damaged_data(path, detail),
            other => other,
        })
    }
}

fn damaged_data(path: &Path, detail: String) -> ArchiveError {
    ArchiveError::Corrupt {
        archive: path.to_owned(),
        detail: format!(
            "encrypted data fails its check (damaged archive or wrong password): {detail}"
        ),
    }
}

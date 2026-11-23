//! tar backend: tar, tar.gz, tar.bz2, tar.xz, tar.zst (T6.1). Crates `tar`, `flate2`, `bzip2`,
//! `lzma-rust2` (xz), `zstd` (+ `zstd-safe` for the window cap).
//!
//! The index is one sequential pass (no seeking in compressed streams). The inner tar header is
//! verified by checksum at open, so a compressed stream that is not a tar never reaches this
//! backend (`detect_format` filters first) and mid-stream corruption surfaces as `Corrupt` for
//! the affected entry. `visit` re-opens the stream and walks it once, in archive order.
//! Hardlinks carry no data of their own; their stream is the target's data, delivered in a
//! second sequential pass (the `ArchiveReader` contract explicitly allows that for sequential
//! backends). xz dictionary memory is capped with `XzReader::new_mem_limit` and the zstd window
//! with a frame-header check, both from `ArchiveLimits::max_codec_memory`
//! (`LimitExceeded(CodecMemory)`).

use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ofm_core::CancelToken;

use crate::archive::EntrySafety;
use crate::archive::error::{ArchiveError, LimitKind};
use crate::archive::format::{ArchiveFormat, tar_header_checksum_ok};
use crate::archive::index::{
    ArchiveEntry, ArchiveEntryKind, ArchiveIndex, EntryId, IndexBuilder, RawEntry,
};
use crate::archive::limits::ArchiveLimits;
use crate::archive::reader::{ArchiveReader, VisitFn};

use super::stream::{EntryStream, catch_panic, drain, map_io};

struct TarReader {
    path: PathBuf,
    format: ArchiveFormat,
    index: Arc<ArchiveIndex>,
    limits: ArchiveLimits,
    /// Number of stored (pushed) entries; ids at or beyond this are synthetic directories.
    stored: u32,
    /// tar stream position (push order) for each stored entry id.
    id_to_pos: Vec<u32>,
    /// stream position -> entry id (`None` for skipped root entries like `./`).
    pos_to_id: Vec<Option<EntryId>>,
}

/// Open a tar (optionally compressed) archive and index it. `format` is one of the `Tar*` variants.
pub fn open<'p>(
    format: ArchiveFormat,
    path: &Path,
    _passwords: &'p dyn crate::archive::PasswordProvider,
    limits: &ArchiveLimits,
    cancel: &CancelToken,
) -> Result<Box<dyn ArchiveReader + 'p>, ArchiveError> {
    let stream = open_tar_stream(path, format, limits)?;
    let index_pass = start_index_pass(stream, path)?;

    let mut builder = IndexBuilder::new(path, format, *limits);
    let mut id_to_pos = Vec::new();
    let mut pos_to_id = Vec::new();
    catch_panic(path, || {
        let mut archive = tar::Archive::new(index_pass);
        let entries = archive.entries().map_err(|e| map_io(path, e))?;
        let mut pos = 0u32;
        for entry in entries {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = entry.map_err(|e| map_io(path, e))?;
            let raw = tar_raw_entry(&entry, path)?;
            match builder.push(raw)? {
                Some(id) => {
                    id_to_pos.push(pos);
                    pos_to_id.push(Some(id));
                }
                None => pos_to_id.push(None),
            }
            pos = pos.wrapping_add(1);
        }
        Ok::<_, ArchiveError>(())
    })?;

    let stored = id_to_pos.len() as u32;
    let index = builder.finish();
    Ok(Box::new(TarReader {
        path: path.to_owned(),
        format,
        index: Arc::new(index),
        limits: *limits,
        stored,
        id_to_pos,
        pos_to_id,
    }))
}

/// Builds one [`RawEntry`] from a tar header + entry (paths stay raw bytes; `tar` resolves GNU
/// long names and pax paths for us).
fn tar_raw_entry(entry: &tar::Entry<'_, impl Read>, path: &Path) -> Result<RawEntry, ArchiveError> {
    let header = entry.header();
    let ty = header.entry_type();
    let kind = if ty.is_dir() {
        ArchiveEntryKind::Dir
    } else if ty.is_symlink() {
        ArchiveEntryKind::Symlink
    } else if ty.is_hard_link() {
        ArchiveEntryKind::Hardlink
    } else if ty.is_file() || ty.is_contiguous() || ty.is_gnu_sparse() {
        ArchiveEntryKind::File
    } else {
        ArchiveEntryKind::Other
    };
    let mut raw = RawEntry::new(
        PathBuf::from(std::ffi::OsString::from_vec(
            entry.path_bytes().into_owned(),
        )),
        kind,
    );
    raw.size = header.size().map_err(|e| map_io(path, e))?;
    raw.mtime_ns = header.mtime().map(|s| i128::from(s) * 1_000_000_000).ok();
    raw.mode = header.mode().ok();
    raw.uid = header.uid().ok().and_then(|u| u32::try_from(u).ok());
    raw.gid = header.gid().ok().and_then(|g| u32::try_from(g).ok());
    if matches!(kind, ArchiveEntryKind::Symlink | ArchiveEntryKind::Hardlink) {
        raw.link_target = entry
            .link_name_bytes()
            .map(|b| PathBuf::from(std::ffi::OsString::from_vec(b.into_owned())));
    }
    Ok(raw)
}

/// Opens the (possibly decompressed) tar byte stream with codec memory caps applied.
fn open_tar_stream(
    path: &Path,
    format: ArchiveFormat,
    limits: &ArchiveLimits,
) -> Result<Box<dyn Read>, ArchiveError> {
    let file = std::fs::File::open(path).map_err(|e| ArchiveError::io(path, e))?;
    let max_kb = u32::try_from(limits.max_codec_memory / 1024)
        .unwrap_or(u32::MAX)
        .max(1);
    match format {
        ArchiveFormat::Tar => Ok(Box::new(file)),
        ArchiveFormat::TarGz => Ok(Box::new(flate2::read::GzDecoder::new(file))),
        ArchiveFormat::TarBz2 => Ok(Box::new(bzip2::read::BzDecoder::new(file))),
        ArchiveFormat::TarXz => Ok(Box::new(lzma_rust2::XzReader::new_mem_limit(
            file, false, max_kb,
        ))),
        ArchiveFormat::TarZst => zstd_stream(file, limits, path),
        other => Err(ArchiveError::Unsupported {
            archive: path.to_owned(),
            what: format!("format {other:?} is not a tar variant"),
        }),
    }
}

/// zstd window cap: parse the frame header (magic, frame header descriptor, window descriptor)
/// from the first bytes, refuse frames whose window exceeds `max_codec_memory`, then replay the
/// peeked bytes into the streaming decoder.
fn zstd_stream(
    mut file: std::fs::File,
    limits: &ArchiveLimits,
    path: &Path,
) -> Result<Box<dyn Read>, ArchiveError> {
    let mut head = [0u8; 18];
    let mut filled = 0;
    while filled < head.len() {
        match file.read(&mut head[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ArchiveError::io(path, e)),
        }
    }
    if let Some(window) = zstd_frame_window(&head[..filled])
        && window > limits.max_codec_memory
    {
        return Err(ArchiveError::LimitExceeded {
            archive: path.to_owned(),
            kind: LimitKind::CodecMemory,
        });
    }
    let decoder =
        zstd::stream::read::Decoder::new(PrependReader::new(head[..filled].to_vec(), file))
            .map_err(|e| map_io(path, e))?;
    Ok(Box::new(decoder))
}

/// Window size declared by a zstd frame header (`None` when unparseable; the decoder will
/// reject a truly broken frame itself).
fn zstd_frame_window(buf: &[u8]) -> Option<u64> {
    if buf.len() < 6 || buf[0..4] != [0x28, 0xB5, 0x2F, 0xFD] {
        return None;
    }
    let fhd = buf[4];
    let single_segment = fhd & 0x20 != 0;
    let dict_flag = fhd & 0x03;
    let fcs_flag = (fhd >> 6) & 0x03;
    let mut pos = 5usize;
    let window = if single_segment {
        None
    } else {
        let wd = *buf.get(pos)?;
        pos += 1;
        let log = 10 + u32::from(wd >> 3);
        let base = 1u64.checked_shl(log)?;
        Some(base + u64::from(wd & 7) * (base / 8))
    };
    pos += match dict_flag {
        0x01 => 1usize,
        0x02 => 2,
        0x03 => 4,
        _ => 0,
    };
    let fcs_size = match (fcs_flag, single_segment) {
        (0, true) => 1,
        (0, false) => 0,
        (1, _) => 2,
        (2, _) => 4,
        _ => 8,
    };
    match window {
        Some(w) => Some(w),
        None => {
            // Single-segment: the window equals the frame content size.
            let fcs = match fcs_size {
                1 => u64::from(*buf.get(pos)?),
                2 => u64::from(u16::from_le_bytes(buf.get(pos..pos + 2)?.try_into().ok()?)) + 256,
                4 => u64::from(u32::from_le_bytes(buf.get(pos..pos + 4)?.try_into().ok()?)),
                8 => u64::from_le_bytes(buf.get(pos..pos + 8)?.try_into().ok()?),
                _ => return None,
            };
            Some(fcs)
        }
    }
}

/// Reads and validates the first tar block, returning a reader positioned at the archive start
/// (the validated block is replayed before the stream).
fn start_index_pass(
    stream: Box<dyn Read>,
    path: &Path,
) -> Result<PrependReader<Box<dyn Read>>, ArchiveError> {
    let mut stream = stream;
    let mut block = [0u8; 512];
    let mut filled = 0;
    while filled < 512 {
        match stream.read(&mut block[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(map_io(path, e)),
        }
    }
    if filled < 512 || !tar_header_checksum_ok(&block) {
        return Err(ArchiveError::Corrupt {
            archive: path.to_owned(),
            detail: "bad tar header checksum".to_owned(),
        });
    }
    Ok(PrependReader::new(block.to_vec(), stream))
}

/// A reader that replays a buffered prefix before continuing with the inner reader.
struct PrependReader<R> {
    head: io::Cursor<Vec<u8>>,
    inner: R,
}

impl<R> PrependReader<R> {
    fn new(head: Vec<u8>, inner: R) -> Self {
        PrependReader {
            head: io::Cursor::new(head),
            inner,
        }
    }
}

impl<R: Read> Read for PrependReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.head.position() < self.head.get_ref().len() as u64 {
            return self.head.read(buf);
        }
        self.inner.read(buf)
    }
}

impl ArchiveReader for TarReader {
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
        let wanted: std::collections::HashSet<u32> = ids.iter().map(|i| i.0).collect();
        let mut hardlinks: Vec<EntryId> = Vec::new();
        catch_panic(&archive, || {
            // Main pass: everything except hardlinks, in archive order.
            let stream = open_tar_stream(&self.path, self.format, &self.limits)?;
            let mut tar = tar::Archive::new(start_index_pass(stream, &self.path)?);
            let entries = tar.entries().map_err(|e| map_io(&archive, e))?;
            for (pos, entry) in entries.enumerate() {
                if cancel.is_cancelled() {
                    return Err(ArchiveError::Cancelled);
                }
                let mut entry = entry.map_err(|e| map_io(&archive, e))?;
                let Some(id) = self.pos_to_id.get(pos).copied().flatten() else {
                    drain(&mut entry, &archive, cancel)?;
                    continue;
                };
                let Some(ix_entry) = self.index.get(id) else {
                    drain(&mut entry, &archive, cancel)?;
                    continue;
                };
                if !wanted.contains(&id.0) {
                    drain(&mut entry, &archive, cancel)?;
                    continue;
                }
                if ix_entry.kind == ArchiveEntryKind::Hardlink {
                    hardlinks.push(id); // second pass delivers the target's data
                    continue;
                }
                if id.0 >= self.stored {
                    f(ix_entry, &mut (&[0u8; 0][..]))?; // synthetic dir
                    continue;
                }
                let mut stream = EntryStream::new(&mut entry, cancel, &archive, ix_entry.size);
                f(ix_entry, &mut stream)?;
                let seen = stream.bytes_read();
                drain(&mut stream, &archive, cancel)?;
                let total = seen.max(stream.bytes_read());
                if total < ix_entry.size {
                    return Err(ArchiveError::Corrupt {
                        archive: archive.clone(),
                        detail: format!(
                            "entry {} ended after {total} of {} declared bytes",
                            ix_entry.path.display(),
                            ix_entry.size
                        ),
                    });
                }
            }
            Ok::<_, ArchiveError>(())
        })?;
        // Synthetic directories (never present in the stream) still get their f call.
        let mut synthetic: Vec<EntryId> = ids
            .iter()
            .copied()
            .filter(|id| id.0 >= self.stored)
            .collect();
        synthetic.sort();
        for id in synthetic {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let entry = self.index.get(id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            f(entry, &mut (&[0u8; 0][..]))?;
        }
        // Second pass: hardlink data (target's bytes), in archive order of the hardlinks.
        for id in hardlinks {
            if cancel.is_cancelled() {
                return Err(ArchiveError::Cancelled);
            }
            let link_entry = self.index.get(id).ok_or(ArchiveError::NoSuchEntry(id.0))?;
            self.visit_hardlink_target(link_entry, cancel, f)?;
        }
        Ok(())
    }
}

impl TarReader {
    /// Stream the data of a hardlink's target to `f` (one extra sequential pass).
    fn visit_hardlink_target(
        &self,
        link_entry: &ArchiveEntry,
        cancel: &CancelToken,
        f: &mut VisitFn<'_>,
    ) -> Result<(), ArchiveError> {
        let archive = self.index.archive().to_owned();
        // The link target was normalized (root-relative) at index time.
        let Some(target_path) = link_entry.link_target.as_deref() else {
            return f(link_entry, &mut (&[0u8; 0][..]));
        };
        let Some(target) = self.index.lookup(target_path) else {
            return f(link_entry, &mut (&[0u8; 0][..]));
        };
        let Some(&pos) = self.id_to_pos.get(target.id.0 as usize) else {
            return f(link_entry, &mut (&[0u8; 0][..]));
        };
        catch_panic(&archive, || {
            let stream = open_tar_stream(&self.path, self.format, &self.limits)?;
            let mut tar = tar::Archive::new(start_index_pass(stream, &self.path)?);
            let entries = tar.entries().map_err(|e| map_io(&archive, e))?;
            for (i, entry) in entries.enumerate() {
                if i as u32 != pos {
                    continue;
                }
                if cancel.is_cancelled() {
                    return Err(ArchiveError::Cancelled);
                }
                let mut entry = entry.map_err(|e| map_io(&archive, e))?;
                let mut stream = EntryStream::new(&mut entry, cancel, &archive, target.size);
                f(target, &mut stream)?;
                let seen = stream.bytes_read();
                drain(&mut stream, &archive, cancel)?;
                let total = seen.max(stream.bytes_read());
                if total < target.size {
                    return Err(ArchiveError::Corrupt {
                        archive: archive.clone(),
                        detail: format!(
                            "hardlink target {} ended after {total} of {} declared bytes",
                            target.path.display(),
                            target.size
                        ),
                    });
                }
                return Ok(());
            }
            Err(ArchiveError::Corrupt {
                archive: archive.clone(),
                detail: format!("hardlink target {} not found", target.path.display()),
            })
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn zstd_window_header_parsing() {
        // Minimal synthetic frames: magic + fhd + window descriptor.
        let frame = |fhd: u8, wd: u8| {
            [
                0x28, 0xB5, 0x2F, 0xFD, fhd, wd, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ]
        };
        // Not single segment, window descriptor 0 -> windowLog 10 -> 1 KiB.
        assert_eq!(zstd_frame_window(&frame(0x00, 0x00)), Some(1024));
        // windowLog 20 base + add 7 * base/8.
        let wd = 10 << 3 | 7;
        assert_eq!(
            zstd_frame_window(&frame(0x00, wd)),
            Some((1 << 20) + 7 * (1 << 20) / 8)
        );
        // Single segment, fcs flag 1 (2-byte fcs at offset 5); stored value is FCS - 256.
        let mut f = frame(0x40 | 0x20, 0);
        f[5..7].copy_from_slice(&300u16.to_le_bytes());
        assert_eq!(zstd_frame_window(&f), Some(556));
        // Garbage / short input.
        assert_eq!(zstd_frame_window(&[0u8; 6]), None);
        assert_eq!(zstd_frame_window(&[]), None);
    }

    #[test]
    fn prepend_reader_replays_head() {
        let mut r = PrependReader::new(vec![1, 2, 3], &[4u8, 5][..]);
        let mut out = Vec::new();
        io::Read::read_to_end(&mut r, &mut out).unwrap();
        assert_eq!(out, vec![1, 2, 3, 4, 5]);
    }
}

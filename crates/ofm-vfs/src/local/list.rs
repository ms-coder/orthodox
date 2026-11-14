//! Streaming directory listing.

use std::path::Path;
use std::time::{Duration, Instant};

use ofm_core::{CancelToken, Entry, EntryKind};
use rustix::fs::{CWD, Dir, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::stat::{entry_at, os_name};
use crate::error::VfsError;
use crate::vfs::ListEvent;

/// Entries per batch.
pub const BATCH_SIZE: usize = 512;
/// Maximum time between batches.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(16);

pub(super) fn map_errno(path: &Path, e: Errno) -> VfsError {
    if e == Errno::NOTDIR {
        VfsError::NotADirectory(path.to_path_buf())
    } else {
        VfsError::from_io(path, e.into())
    }
}

/// Stream `dir` into `sink`; always ends with exactly one `Done` or `Error` event.
pub fn list_dir(dir: &Path, cancel: &CancelToken, sink: &mut dyn FnMut(ListEvent)) {
    match run(dir, cancel, sink) {
        Ok(total) => sink(ListEvent::Done { total }),
        Err(e) => sink(ListEvent::Error(e)),
    }
}

fn run(
    dir: &Path,
    cancel: &CancelToken,
    sink: &mut dyn FnMut(ListEvent),
) -> Result<usize, VfsError> {
    if cancel.is_cancelled() {
        return Err(VfsError::Cancelled);
    }
    let fd = rustix::fs::openat(
        CWD,
        dir,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| map_errno(dir, e))?;
    let iter = Dir::read_from(&fd).map_err(|e| map_errno(dir, e))?;

    let mut batch: Vec<Entry> = Vec::with_capacity(BATCH_SIZE);
    let mut total = 0usize;
    let mut last_flush = Instant::now();
    for item in iter {
        if cancel.is_cancelled() {
            return Err(VfsError::Cancelled);
        }
        let de = item.map_err(|e| map_errno(dir, e))?;
        let bytes = de.file_name().to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let entry = match entry_at(&fd, de.file_name(), os_name(bytes)) {
            Ok(e) => e,
            // Vanished between readdir and stat: not an error.
            Err(Errno::NOENT) => continue,
            Err(e) => {
                tracing::debug!(dir = %dir.display(), error = %e, "stat failed; listing with type only");
                let kind = match de.file_type() {
                    FileType::RegularFile => EntryKind::File,
                    FileType::Directory => EntryKind::Dir,
                    FileType::Symlink => EntryKind::Symlink { target: None },
                    _ => EntryKind::Other,
                };
                Entry::new(os_name(bytes), kind)
            }
        };
        batch.push(entry);
        total += 1;
        if batch.len() >= BATCH_SIZE || last_flush.elapsed() >= BATCH_INTERVAL {
            sink(ListEvent::Batch(std::mem::replace(
                &mut batch,
                Vec::with_capacity(BATCH_SIZE),
            )));
            last_flush = Instant::now();
        }
    }
    if !batch.is_empty() {
        sink(ListEvent::Batch(batch));
    }
    Ok(total)
}

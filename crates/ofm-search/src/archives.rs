//! Search inside archives (private), behind the frozen `ofm_vfs::archive::ArchiveReader`
//! API (ADR-0008/0009). One level, no nesting; a search never prompts for a password
//! (`NoPassword`), so encrypted archives are skipped with a warning.
//!
//! The format backends (T6.1) are still stubs on this branch: `open_archive` returns
//! `NotImplemented` after magic detection. The engine turns that into one
//! "archive search not available yet" warning per search and moves on; the code below is
//! compiled and ready, and the test for it is `#[ignore = "needs T6.1"]`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ofm_core::{CancelToken, EntryKind, Location};
use ofm_vfs::archive::{
    ArchiveEntryKind, ArchiveError, ArchiveLimits, ArchiveReader, NoPassword, open_archive,
};

use crate::compile::ContentSpec;
use crate::content::search_stream;
use crate::engine::EntryFilter;
use crate::event::{ContentHit, Hit};

/// What to do with a candidate file when `search_archives` is on.
pub(crate) enum ArchiveCheck {
    /// Not an archive (magic says so): search it as a plain file.
    PlainFile,
    /// The format's backend is not implemented yet (T6.1 stubs).
    StubBackend,
    /// The file itself could not be read, so it may not be an archive at all.
    Unreadable(String),
    /// A supported archive we cannot search inside (encrypted, corrupt, limit); the
    /// reason is for the warning.
    Unsearchable(String),
    /// Opened: search inside.
    Inside(Box<dyn ArchiveReader>),
}

/// Sniff `path` for a supported archive format and open it if possible. Cheap (magic
/// bytes) for plain files.
pub(crate) fn try_open_archive(path: &Path, cancel: &CancelToken) -> ArchiveCheck {
    match open_archive(path, &NoPassword, &ArchiveLimits::default(), cancel) {
        Ok(reader) => ArchiveCheck::Inside(reader),
        Err(ArchiveError::NotAnArchive(_)) => ArchiveCheck::PlainFile,
        Err(ArchiveError::NotImplemented(_)) => ArchiveCheck::StubBackend,
        Err(ArchiveError::Cancelled) => ArchiveCheck::Unsearchable("cancelled".to_owned()),
        Err(e @ ArchiveError::Io { .. }) => ArchiveCheck::Unreadable(format!("{e}")),
        Err(e) => ArchiveCheck::Unsearchable(format!("{e}")),
    }
}

/// Engine plumbing for archive searches: cancellation, counters and output routing.
pub(crate) struct ArchiveSink<'a> {
    /// Shared cancel token.
    pub(crate) cancel: &'a CancelToken,
    /// Files-examined counter of the search.
    pub(crate) files_scanned: &'a AtomicU64,
    /// Hit sink.
    pub(crate) out: &'a mut dyn FnMut(Hit),
    /// Warning sink (message only; the engine adds the archive path context).
    pub(crate) warn: &'a mut dyn FnMut(String),
}

/// Search the entries of an opened archive with the same reporting filters as the tree
/// walk; `match_dirs` as in the query (a content query never reports directories). The
/// caller wraps this in `catch_unwind` (invariant 7).
pub(crate) fn search_inside(
    archive: &Path,
    reader: &mut dyn ArchiveReader,
    filter: &EntryFilter,
    content: Option<&ContentSpec>,
    match_dirs: bool,
    sink: &mut ArchiveSink<'_>,
) {
    let index = reader.index().clone();
    // Name/size/mtime filters pick candidates; a content query additionally requires the
    // text to match. Unsafe entries are never reported.
    let mut report: Vec<(PathBuf, ofm_core::Entry)> = Vec::new();
    let mut content_paths: Vec<PathBuf> = Vec::new();
    for e in index.entries().iter().filter(|e| e.is_safe()) {
        if sink.cancel.is_cancelled() {
            return;
        }
        sink.files_scanned.fetch_add(1, Ordering::Relaxed);
        let kind = match e.kind {
            ArchiveEntryKind::File | ArchiveEntryKind::Hardlink => EntryKind::File,
            ArchiveEntryKind::Dir if match_dirs && content.is_none() => EntryKind::Dir,
            ArchiveEntryKind::Dir => continue,
            // Symlinks and special entries: never searched, never reported.
            _ => continue,
        };
        if !filter.accepts(&e.path, kind, e.size, e.mtime_ns) {
            continue;
        }
        if kind == EntryKind::File && content.is_some() {
            content_paths.push(e.path.clone());
        } else {
            report.push((e.path.clone(), index.to_entry(e)));
        }
    }
    for (inner, entry) in report {
        (sink.out)(archive_hit(archive, inner, entry, None));
    }
    let Some(spec) = content else { return };
    // A search never prompts, and one encrypted entry in the selection would fail the whole
    // visit: leave them out and say so.
    let (locked, selected): (Vec<_>, Vec<_>) = index
        .select(&content_paths)
        .into_iter()
        .partition(|id| index.get(*id).is_some_and(|e| e.encrypted));
    if !locked.is_empty() {
        (sink.warn)(format!(
            "{}: {} encrypted entries not searched (password required)",
            archive.display(),
            locked.len()
        ));
    }
    let archive_path = archive.to_owned();
    // `visit` streams in archive order; the visitor checks each entry against the
    // selection. A set, so a large archive is not searched in quadratic time.
    let selected_paths: HashSet<&Path> = content_paths.iter().map(PathBuf::as_path).collect();
    let cancel: &CancelToken = sink.cancel; // copied out: the visitor mutably borrows the sink
    let result = reader.visit(&selected, cancel, &mut |entry, data| {
        if !selected_paths.contains(entry.path.as_path()) {
            return Ok(());
        }
        match search_stream(data, spec, cancel) {
            Ok(Some(hit)) => {
                (sink.out)(archive_hit(
                    &archive_path,
                    entry.path.clone(),
                    index.to_entry(entry),
                    Some(hit),
                ));
            }
            Ok(None) => {}
            Err(e) => (sink.warn)(format!(
                "{}#{}: {e}",
                archive_path.display(),
                entry.path.display()
            )),
        }
        Ok(())
    });
    if let Err(e) = result
        && !matches!(e, ArchiveError::Cancelled)
    {
        (sink.warn)(format!("{}: {e}", archive.display()));
    }
}

/// One archive hit: `Location::Archive`, panel row from the index, `real_path = None`.
fn archive_hit(
    archive: &Path,
    inner: PathBuf,
    mut entry: ofm_core::Entry,
    content: Option<ContentHit>,
) -> Hit {
    entry.real_path = None;
    Hit {
        location: Location::Archive {
            archive: archive.to_owned(),
            inner,
        },
        entry,
        content,
    }
}

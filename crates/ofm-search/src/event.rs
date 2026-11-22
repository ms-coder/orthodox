//! What a running search reports.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::Duration;

use ofm_core::{Entry, Location};

use crate::error::SearchError;

/// First content match in a file (what the results list shows and F3 jumps to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentMatch {
    /// 1-based line number of the match.
    pub line: u64,
    /// Byte offset of the match start in the file (raw bytes, so the viewer can seek there).
    pub byte_offset: u64,
    /// The matching line decoded lossily, trimmed to at most 200 chars around the match, without
    /// the line terminator.
    pub snippet: String,
    /// Byte range of the match inside `snippet` (for highlighting).
    pub snippet_match: Range<usize>,
}

/// Content result of one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContentHit {
    /// The first match.
    pub first: ContentMatch,
    /// Number of matches in the file, counted up to 10 000.
    pub count: u32,
}

/// One found item. Exactly one hit per file/directory (content hits carry the first match).
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Where it is: `Location::Local(path)`, or `Location::Archive` for hits inside archives.
    pub location: Location,
    /// Metadata; `name` = file name only, `real_path = Some(path)` for local hits, `None` for
    /// archive hits.
    pub entry: Entry,
    /// Set when a content query matched.
    pub content: Option<ContentHit>,
}

impl Hit {
    /// Local path of the hit (what file operations use); `None` for archive hits.
    pub fn real_path(&self) -> Option<&Path> {
        self.entry.real_path.as_deref()
    }

    /// Row for a panelized list (`ofm_vfs::list::PanelList::from_entries`); `None` for hits inside
    /// archives (v1.0 lists hold real files only; the UI reports how many were left out).
    pub fn to_list_entry(&self) -> Option<Entry> {
        self.entry.real_path.as_ref()?;
        Some(self.entry.clone())
    }
}

/// Counters while running; sent at most every `PROGRESS_INTERVAL`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchProgress {
    /// Directories visited.
    pub dirs_scanned: u64,
    /// Files examined (name/size/date checked or content searched).
    pub files_scanned: u64,
    /// Hits sent so far.
    pub hits: u64,
    /// Directory currently being scanned (for the status line).
    pub current: Option<PathBuf>,
}

/// A problem with one path that does not stop the search (permission denied, unreadable file,
/// symlink loop, encrypted archive skipped).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchWarning {
    /// Path concerned.
    pub path: PathBuf,
    /// Human-readable message.
    pub message: String,
}

/// Final numbers, carried by `SearchEvent::Done`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchSummary {
    /// Hits sent.
    pub hits: u64,
    /// Files examined.
    pub files_scanned: u64,
    /// Directories visited.
    pub dirs_scanned: u64,
    /// Wall time.
    pub elapsed: Duration,
    /// Ended because of `cancel()` (or the handle being dropped); results so far are valid.
    pub cancelled: bool,
    /// Ended because `max_hits` was reached.
    pub truncated: bool,
}

/// Events of one search. Order: `(Found | Progress | Warning)*` then exactly one terminal event
/// (`Done` or `Failed`), then the channel closes.
#[derive(Debug)]
pub enum SearchEvent {
    /// A batch of hits (<= `BATCH_SIZE`, flushed at least every `BATCH_INTERVAL`), in discovery
    /// order (not sorted: the results list sorts).
    Found(Vec<Hit>),
    /// Throttled counters.
    Progress(SearchProgress),
    /// Non-fatal problem.
    Warning(SearchWarning),
    /// Finished normally, cancelled or truncated.
    Done(SearchSummary),
    /// The search could not run (bad root, worker panic).
    Failed(SearchError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use ofm_core::EntryKind;

    #[test]
    fn only_local_hits_can_be_panelized() {
        let mut e = Entry::new("a.txt", EntryKind::File);
        e.real_path = Some("/srv/a.txt".into());
        let local = Hit {
            location: Location::Local("/srv/a.txt".into()),
            entry: e,
            content: None,
        };
        assert_eq!(local.real_path(), Some(Path::new("/srv/a.txt")));
        assert!(local.to_list_entry().is_some());
        let inside = Hit {
            location: Location::Archive {
                archive: "/srv/x.zip".into(),
                inner: "a.txt".into(),
            },
            entry: Entry::new("a.txt", EntryKind::File),
            content: None,
        };
        assert!(inside.real_path().is_none() && inside.to_list_entry().is_none());
    }
}

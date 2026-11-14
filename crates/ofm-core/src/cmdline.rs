//! Command-line helpers: pure functions (owned by M4 tasks).

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

/// Quote a file name or path for safe insertion into a POSIX shell command line
/// (single quotes, with `'` -> `'\''`); works on bytes so non-UTF-8 names survive.
pub fn shell_quote(_name: &OsStr) -> OsString {
    todo!("M4")
}

/// If `line` is a `cd` command (`cd`, `cd x`, `cd -- x`, `cd ~/x`, `cd -`), resolve its target
/// relative to `cwd`/`home`; `None` if the line is not a plain cd.
pub fn parse_cd_target(_line: &str, _cwd: &Path, _home: &Path) -> Option<PathBuf> {
    todo!("M4")
}

/// Decode an OSC 7 payload (`file://host/percent%20encoded/path`) into a local path.
pub fn osc7_to_path(_uri: &str) -> Option<PathBuf> {
    todo!("M4")
}

/// Bounded most-recent-first history ring with de-duplication of consecutive repeats.
#[derive(Debug, Default)]
pub struct HistoryRing {
    #[allow(dead_code, reason = "M4 fills in")]
    items: VecDeque<String>,
}

impl HistoryRing {
    /// New ring holding at most `capacity` entries.
    pub fn new(_capacity: usize) -> Self {
        todo!("M4")
    }
    /// Add a line (moves to front; ignores empty).
    pub fn push(&mut self, _line: &str) {
        todo!("M4")
    }
    /// Entries, newest first.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        std::iter::empty()
    }
}

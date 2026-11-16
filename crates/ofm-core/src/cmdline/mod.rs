//! Command-line helpers for the shell integration (F11/F12, ADR-0003): pure functions, no I/O,
//! no GTK (invariant 1). T4.0 froze this API; T4.3 may extend tests, not signatures.
//!
//! - `quote`: [`shell_quote_for`]/[`shell_quote_bytes`] (POSIX and fish quoting, control bytes
//!   escaped) and [`cd_command_for`] (the exact ` cd -- '…'\n` bytes OFM feeds to the shell;
//!   `None` for paths with control bytes).
//! - `osc7`: [`osc7_encode`]/[`osc7_parse`] — the `file://host/percent-encoded-path` cwd
//!   protocol the injected rc snippets emit on every prompt.
//! - `cd`: [`parse_cd`]/[`parse_cd_target`] — where a typed `cd` goes (pure parsing).
//! - `complete`: path completion over already-listed names ([`complete_in`], [`common_prefix`]).
//! - [`HistoryRing`]: bounded command history.

mod cd;
mod complete;
mod osc7;
mod quote;

#[cfg(test)]
mod proptests;

pub use cd::{CdTarget, parse_cd, parse_cd_target};
pub use complete::{Completion, common_prefix, complete_in};
pub use osc7::{osc7_encode, osc7_parse, osc7_to_path};
pub use quote::{
    QuoteStyle, cd_command, cd_command_for, fish_quote_bytes, has_control_bytes, shell_quote,
    shell_quote_bytes, shell_quote_for,
};

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ffi::OsStr;

/// The bytes of an `OsStr` (raw on unix; lossy UTF-8 elsewhere — OFM targets Linux).
pub(crate) fn bytes_of(s: &OsStr) -> Cow<'_, [u8]> {
    #[cfg(unix)]
    {
        Cow::Borrowed(std::os::unix::ffi::OsStrExt::as_bytes(s))
    }
    #[cfg(not(unix))]
    {
        Cow::Owned(s.to_string_lossy().into_owned().into_bytes())
    }
}

/// An `OsString` from raw bytes (raw on unix; lossy UTF-8 elsewhere).
pub(crate) fn os_from_bytes(bytes: &[u8]) -> std::ffi::OsString {
    #[cfg(unix)]
    {
        std::os::unix::ffi::OsStringExt::from_vec(bytes.to_vec())
    }
    #[cfg(not(unix))]
    {
        std::ffi::OsString::from(String::from_utf8_lossy(bytes).into_owned())
    }
}

/// Bounded most-recent-first history ring with de-duplication (a re-run line moves to the front).
#[derive(Debug, Clone)]
pub struct HistoryRing {
    items: VecDeque<String>,
    capacity: usize,
}

impl HistoryRing {
    /// New ring holding at most `capacity` entries (0 = stores nothing).
    pub fn new(capacity: usize) -> Self {
        HistoryRing {
            items: VecDeque::new(),
            capacity,
        }
    }

    /// Add a line: trimmed, empty lines ignored, a line equal to the newest is ignored, any older
    /// copy of it is removed first (most recently used stays at the front). Oldest entries are
    /// dropped beyond the capacity.
    pub fn push(&mut self, line: &str) {
        let line = line.trim();
        if self.capacity == 0 || line.is_empty() {
            return;
        }
        if let Some(pos) = self.items.iter().position(|i| i == line) {
            self.items.remove(pos);
        }
        self.items.push_front(line.to_owned());
        while self.items.len() > self.capacity {
            self.items.pop_back();
        }
    }

    /// Entries, newest first.
    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.items.iter().map(String::as_str)
    }

    /// Number of stored entries.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// True when no entry is stored.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Maximum number of entries.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

impl Default for HistoryRing {
    fn default() -> Self {
        HistoryRing::new(64)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn history_ring_push_dedup_and_capacity() {
        let mut h = HistoryRing::new(2);
        h.push("ls");
        h.push("ls"); // consecutive duplicate ignored
        assert_eq!(h.len(), 1);
        h.push("cd /tmp");
        h.push("  ls  "); // trimmed, moved to front again
        assert_eq!(h.iter().collect::<Vec<_>>(), ["ls", "cd /tmp"]);
        h.push("make"); // capacity 2: "cd /tmp" drops out
        assert_eq!(h.iter().collect::<Vec<_>>(), ["make", "ls"]);
        h.push("");
        h.push("   ");
        assert_eq!(h.len(), 2, "empty lines are not stored");
        let mut z = HistoryRing::new(0);
        z.push("x");
        assert!(z.is_empty());
    }
}

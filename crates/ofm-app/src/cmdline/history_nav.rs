//! Stepping through the command history with Ctrl+E / Ctrl+Shift+E (pure; unit-tested).
//!
//! The history is a newest-first list ([`ofm_core::cmdline::HistoryRing`] order). Stepping away
//! from the live text saves it as the *draft*; stepping past the newest entry (`next` from
//! position 0) restores that draft, so a half-typed command survives a history excursion.

/// Position in the command history while stepping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryNav {
    /// Index into the newest-first list; `None` = the draft (the live entry text).
    pos: Option<usize>,
    /// Entry text saved when stepping away from the draft.
    draft: String,
}

impl HistoryNav {
    /// New navigation state (on the draft).
    pub fn new() -> Self {
        Self::default()
    }

    /// Step to the previous (older) entry from `current` (the live entry text, saved as the
    /// draft on the first step). Returns the text to show, or `None` when there is no history.
    pub fn step_prev(&mut self, items: &[String], current: &str) -> Option<String> {
        let last = items.len().checked_sub(1)?;
        self.pos = Some(match self.pos {
            None => {
                self.draft = current.to_owned();
                0
            }
            Some(i) => (i + 1).min(last),
        });
        self.pos.and_then(|i| items.get(i)).cloned()
    }

    /// Step to the next (newer) entry; past the newest it returns to the draft. Returns the text
    /// to show, or `None` when already on the draft.
    pub fn step_next(&mut self, items: &[String]) -> Option<String> {
        match self.pos {
            None => None,
            Some(0) => {
                self.pos = None;
                Some(self.draft.clone())
            }
            Some(i) => {
                self.pos = Some(i - 1);
                self.pos.and_then(|i| items.get(i)).cloned()
            }
        }
    }

    /// Jump to `index` (newest-first; a history-list pick).
    pub fn jump(&mut self, index: usize) {
        self.pos = Some(index);
    }

    /// Back to the draft (a fresh line was executed or the entry cleared).
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn items() -> Vec<String> {
        vec!["two".to_owned(), "one".to_owned()] // newest first
    }

    #[test]
    fn steps_and_clamps() {
        let mut nav = HistoryNav::new();
        assert_eq!(nav.step_prev(&items(), "draf"), Some("two".to_owned()));
        assert_eq!(nav.step_prev(&items(), "x"), Some("one".to_owned()));
        // Clamped at the oldest entry.
        assert_eq!(nav.step_prev(&items(), "x"), Some("one".to_owned()));
        assert_eq!(nav.step_next(&items()), Some("two".to_owned()));
        // Past the newest: the draft comes back, and further stepping does nothing.
        assert_eq!(nav.step_next(&items()), Some("draf".to_owned()));
        assert_eq!(nav.step_next(&items()), None);
    }

    #[test]
    fn empty_history_does_not_leave_the_draft() {
        let mut nav = HistoryNav::new();
        assert_eq!(nav.step_prev(&[], "typed"), None);
        assert_eq!(nav.step_next(&[]), None);
    }

    #[test]
    fn jump_keeps_stepping_from_there() {
        let mut nav = HistoryNav::new();
        nav.jump(1);
        assert_eq!(nav.step_next(&items()), Some("two".to_owned()));
    }
}

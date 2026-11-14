//! Per-tab navigation history (F07): visited locations with a current position.

use ofm_core::Location;

/// Back/forward list. `visit` truncates the forward part like a browser.
#[derive(Debug, Clone, Default)]
pub struct NavHistory {
    items: Vec<Location>,
    pos: usize,
}

impl NavHistory {
    /// Maximum remembered locations per tab.
    pub const CAP: usize = 200;

    /// Empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a navigation to `loc` (ignored when it equals the current entry).
    pub fn visit(&mut self, loc: Location) {
        if self.items.get(self.pos) == Some(&loc) {
            return;
        }
        if !self.items.is_empty() {
            self.items.truncate(self.pos + 1);
        }
        self.items.push(loc);
        if self.items.len() > Self::CAP {
            self.items.remove(0);
        }
        self.pos = self.items.len() - 1;
    }

    /// Step back; returns the location to go to.
    pub fn back(&mut self) -> Option<Location> {
        if self.pos == 0 || self.items.is_empty() {
            return None;
        }
        self.pos -= 1;
        self.items.get(self.pos).cloned()
    }

    /// Step forward; returns the location to go to.
    pub fn forward(&mut self) -> Option<Location> {
        if self.pos + 1 >= self.items.len() {
            return None;
        }
        self.pos += 1;
        self.items.get(self.pos).cloned()
    }

    /// Jump to the entry at `index` of [`entries`](Self::entries) (history list popup).
    pub fn jump(&mut self, index: usize) -> Option<Location> {
        let loc = self.items.get(index).cloned()?;
        self.pos = index;
        Some(loc)
    }

    /// All entries, oldest first.
    pub fn entries(&self) -> &[Location] {
        &self.items
    }

    /// Index of the current entry in [`entries`](Self::entries).
    pub fn position(&self) -> usize {
        self.pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(p: &str) -> Location {
        Location::Local(p.into())
    }

    #[test]
    fn back_forward_and_truncate() {
        let mut h = NavHistory::new();
        h.visit(l("/a"));
        h.visit(l("/a")); // duplicate ignored
        h.visit(l("/b"));
        h.visit(l("/c"));
        assert_eq!(h.back(), Some(l("/b")));
        assert_eq!(h.back(), Some(l("/a")));
        assert_eq!(h.back(), None);
        assert_eq!(h.forward(), Some(l("/b")));
        h.visit(l("/d")); // drops /c
        assert_eq!(h.entries(), &[l("/a"), l("/b"), l("/d")]);
        assert_eq!(h.forward(), None);
        assert_eq!(h.jump(0), Some(l("/a")));
        assert_eq!(h.position(), 0);
    }

    #[test]
    fn capped() {
        let mut h = NavHistory::new();
        for i in 0..(NavHistory::CAP + 10) {
            h.visit(l(&format!("/{i}")));
        }
        assert_eq!(h.entries().len(), NavHistory::CAP);
        assert_eq!(h.position(), NavHistory::CAP - 1);
    }
}

//! Rate limiting of UI updates: results arrive in many small batches from the engine, the list
//! model is told about them at most [`MAX_UPDATES_PER_SEC`] times per second, so a million hits
//! cost a few thousand `items-changed` emissions instead of a million.

use std::time::{Duration, Instant};

/// Upper bound of result-list / counter updates per second.
pub const MAX_UPDATES_PER_SEC: u32 = 20;

/// Time between two UI updates.
pub const UPDATE_INTERVAL: Duration = Duration::from_millis(1000 / MAX_UPDATES_PER_SEC as u64);

/// Collects items between flushes.
#[derive(Debug)]
pub struct Batcher<T> {
    pending: Vec<T>,
    last_flush: Instant,
    interval: Duration,
}

impl<T> Batcher<T> {
    /// Batcher flushing at most every `interval`; the first interval starts at `now`.
    pub fn new(interval: Duration, now: Instant) -> Self {
        Batcher {
            pending: Vec::new(),
            last_flush: now,
            interval,
        }
    }

    /// Add items.
    pub fn push(&mut self, mut items: Vec<T>) {
        if self.pending.is_empty() {
            self.pending = items;
        } else {
            self.pending.append(&mut items);
        }
    }

    /// Number of buffered items.
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// True when nothing is buffered.
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Whether a flush is allowed at `now` (an interval has passed since the last one).
    pub fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_flush) >= self.interval
    }

    /// The buffered items when a flush is [`due`](Self::due) and there are any; restarts the
    /// interval. Otherwise empty.
    pub fn take_if_due(&mut self, now: Instant) -> Vec<T> {
        if self.pending.is_empty() || !self.due(now) {
            return Vec::new();
        }
        self.last_flush = now;
        std::mem::take(&mut self.pending)
    }

    /// The buffered items regardless of the interval (end of the search).
    pub fn take_all(&mut self, now: Instant) -> Vec<T> {
        self.last_flush = now;
        std::mem::take(&mut self.pending)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flushes_at_most_every_interval_and_loses_nothing() {
        let t0 = Instant::now();
        let mut b = Batcher::new(UPDATE_INTERVAL, t0);
        let mut flushes = 0;
        let mut seen = 0;
        // A million items in batches of 256, one batch every 0.1 ms of simulated time.
        let mut now = t0;
        let mut sent = 0usize;
        while sent < 1_000_000 {
            b.push(vec![0u8; 256]);
            sent += 256;
            now += Duration::from_micros(100);
            let got = b.take_if_due(now);
            if !got.is_empty() {
                flushes += 1;
                seen += got.len();
            }
        }
        seen += b.take_all(now).len();
        assert_eq!(seen, sent);
        let secs = now.duration_since(t0).as_secs_f64();
        assert!(
            f64::from(flushes) <= secs * f64::from(MAX_UPDATES_PER_SEC) + 1.0,
            "{flushes} flushes in {secs} s"
        );
        assert!(flushes >= 1);
    }

    #[test]
    fn nothing_is_taken_before_the_interval_or_when_empty() {
        let t0 = Instant::now();
        let mut b: Batcher<u32> = Batcher::new(Duration::from_millis(50), t0);
        assert!(b.take_if_due(t0 + Duration::from_secs(1)).is_empty());
        b.push(vec![1, 2]);
        assert!(!b.due(t0 + Duration::from_millis(49)));
        assert!(b.take_if_due(t0 + Duration::from_millis(49)).is_empty());
        assert_eq!(b.len(), 2);
        assert_eq!(b.take_if_due(t0 + Duration::from_millis(50)), [1, 2]);
        b.push(vec![3]);
        // The interval restarted at the flush.
        assert!(b.take_if_due(t0 + Duration::from_millis(60)).is_empty());
        assert_eq!(b.take_all(t0 + Duration::from_millis(61)), [3]);
        assert!(b.is_empty());
    }
}

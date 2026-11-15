//! Progress accounting with an injectable clock. Delivered by T3.0 (T3.1 owns it afterwards).
//!
//! [`ProgressTracker`] only *accumulates*; it never blocks and never sends. `JobCtx` asks
//! [`ProgressTracker::take_due`] after each update and hands the snapshot to the sink, which
//! yields the "<= 10 Hz per job, never blocks the worker" guarantee (ADR-0006).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::event::Progress;

/// Minimum time between two emitted snapshots (10 Hz).
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// Monotonic time source (injected so tests are deterministic).
pub trait Clock: Send + Sync {
    /// Time since an arbitrary fixed origin; never goes backwards.
    fn now(&self) -> Duration;
}

/// The real clock.
#[derive(Debug)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Clock whose origin is "now".
    pub fn new() -> Self {
        SystemClock {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}

/// A clock tests advance by hand.
#[derive(Debug, Default)]
pub struct ManualClock(AtomicU64);

impl ManualClock {
    /// Clock at t = 0.
    pub fn new() -> Self {
        Self::default()
    }
    /// Move time forward.
    pub fn advance(&self, d: Duration) {
        let ns = u64::try_from(d.as_nanos()).unwrap_or(u64::MAX);
        self.0.fetch_add(ns, Ordering::Relaxed);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> Duration {
        Duration::from_nanos(self.0.load(Ordering::Relaxed))
    }
}

/// Smoothing factor of the speed estimate (weight of the newest sample).
const ALPHA: f64 = 0.3;

/// Accumulates counters and derives speed/ETA.
pub struct ProgressTracker {
    clock: Arc<dyn Clock>,
    p: Progress,
    last_emit: Option<Duration>,
    sample_at: Duration,
    sample_bytes: u64,
    speed: Option<f64>,
}

impl ProgressTracker {
    /// New tracker (planning phase: unknown totals).
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        let now = clock.now();
        ProgressTracker {
            clock,
            p: Progress {
                planning: true,
                ..Progress::default()
            },
            last_emit: None,
            sample_at: now,
            sample_bytes: 0,
            speed: None,
        }
    }

    /// Mark the plan phase running/finished.
    pub fn set_planning(&mut self, planning: bool) {
        self.p.planning = planning;
    }

    /// Plan phase: totals grew by this much.
    pub fn add_total(&mut self, files: u64, bytes: u64) {
        self.p.files_total = self.p.files_total.saturating_add(files);
        self.p.bytes_total = self.p.bytes_total.saturating_add(bytes);
    }

    /// Set the final totals and leave the planning phase.
    pub fn set_totals(&mut self, files: u64, bytes: u64) {
        self.p.files_total = files;
        self.p.bytes_total = bytes;
        self.p.planning = false;
    }

    /// `n` more bytes processed.
    pub fn add_bytes(&mut self, n: u64) {
        self.p.bytes_done = self.p.bytes_done.saturating_add(n);
    }

    /// One more file processed.
    pub fn file_done(&mut self) {
        self.p.files_done = self.p.files_done.saturating_add(1);
    }

    /// The item being processed now.
    pub fn set_current(&mut self, path: Option<std::path::PathBuf>) {
        self.p.current = path;
    }

    /// The counters so far (no speed refresh).
    pub fn snapshot(&self) -> &Progress {
        &self.p
    }

    /// Forget speed history (call after a pause so the idle time does not skew the estimate).
    pub fn reset_speed(&mut self) {
        self.sample_at = self.clock.now();
        self.sample_bytes = self.p.bytes_done;
        self.speed = None;
        self.p.bytes_per_sec = 0;
        self.p.eta_secs = None;
    }

    /// Snapshot to emit if at least [`MIN_INTERVAL`] passed since the last emission (the first
    /// call always emits). Refreshes speed and ETA.
    pub fn take_due(&mut self) -> Option<&Progress> {
        let now = self.clock.now();
        if let Some(last) = self.last_emit
            && now.saturating_sub(last) < MIN_INTERVAL
        {
            return None;
        }
        self.refresh(now);
        self.last_emit = Some(now);
        Some(&self.p)
    }

    /// Exact snapshot regardless of throttling (final progress, before `Finished`).
    pub fn force(&mut self) -> &Progress {
        let now = self.clock.now();
        self.refresh(now);
        self.last_emit = Some(now);
        &self.p
    }

    fn refresh(&mut self, now: Duration) {
        let dt = now.saturating_sub(self.sample_at).as_secs_f64();
        if dt >= MIN_INTERVAL.as_secs_f64() {
            let delta = self.p.bytes_done.saturating_sub(self.sample_bytes) as f64;
            let inst = delta / dt;
            let next = match self.speed {
                Some(prev) => ALPHA * inst + (1.0 - ALPHA) * prev,
                None => inst,
            };
            self.speed = Some(next);
            self.sample_at = now;
            self.sample_bytes = self.p.bytes_done;
        }
        let speed = self.speed.unwrap_or(0.0);
        self.p.bytes_per_sec = speed as u64;
        self.p.eta_secs = if self.p.planning || speed < 1.0 || self.p.bytes_total == 0 {
            None
        } else {
            let left = self.p.bytes_total.saturating_sub(self.p.bytes_done) as f64;
            Some((left / speed).ceil() as u64)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracker() -> (Arc<ManualClock>, ProgressTracker) {
        let clock = Arc::new(ManualClock::new());
        let t = ProgressTracker::new(clock.clone());
        (clock, t)
    }

    #[test]
    fn emits_at_most_every_100ms() {
        let (clock, mut t) = tracker();
        assert!(t.take_due().is_some(), "first snapshot is immediate");
        let mut emitted = 0;
        for _ in 0..1000 {
            t.add_bytes(1000);
            clock.advance(Duration::from_millis(1));
            if t.take_due().is_some() {
                emitted += 1;
            }
        }
        // 1000 ms of work -> about 10 snapshots, never more than 10 Hz.
        assert!((9..=10).contains(&emitted), "emitted {emitted}");
    }

    #[test]
    fn speed_and_eta() {
        let (clock, mut t) = tracker();
        t.set_totals(1, 10_000_000);
        assert!(t.take_due().is_some());
        for _ in 0..5 {
            t.add_bytes(1_000_000);
            clock.advance(Duration::from_millis(100));
            let _ = t.take_due();
        }
        let p = t.force().clone();
        assert!(!p.planning);
        assert!((9_000_000..=11_000_000).contains(&p.bytes_per_sec), "{p:?}");
        // 5 MB left at ~10 MB/s -> about 1 s
        assert!(matches!(p.eta_secs, Some(1..=2)), "{p:?}");
    }

    #[test]
    fn no_eta_while_planning_and_speed_resets() {
        let (clock, mut t) = tracker();
        t.add_total(5, 100);
        t.add_bytes(50);
        clock.advance(Duration::from_millis(200));
        assert_eq!(t.force().eta_secs, None);
        t.set_totals(5, 100);
        t.reset_speed();
        assert_eq!(t.force().bytes_per_sec, 0);
        assert_eq!(t.snapshot().files_total, 5);
    }
}

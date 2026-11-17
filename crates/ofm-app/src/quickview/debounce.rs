//! Trailing-edge debounce, as pure logic with explicit time (the GTK side drives it with
//! `glib::timeout_add_local_once` and `Instant::now()`).
//!
//! Every [`Debouncer::push`] replaces the pending value and restarts the delay; the value is
//! handed out by [`Debouncer::tick`] once `delay` passed without a newer push. So holding a
//! cursor key down (events every ~30 ms) starts exactly one preview, `delay` after the last move.

use std::time::{Duration, Instant};

/// Result of [`Debouncer::tick`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tick<T> {
    /// Nothing is pending.
    Idle,
    /// A value is pending but not due yet: tick again after this long.
    Wait(Duration),
    /// The delay elapsed: act on this value (it is no longer pending).
    Fire(T),
}

/// Holds the latest pushed value until it has been quiet for `delay`.
#[derive(Debug)]
pub struct Debouncer<T> {
    delay: Duration,
    pending: Option<(Instant, T)>,
}

impl<T> Debouncer<T> {
    /// Debouncer with the given quiet period.
    pub fn new(delay: Duration) -> Self {
        Debouncer {
            delay,
            pending: None,
        }
    }

    /// The quiet period.
    pub fn delay(&self) -> Duration {
        self.delay
    }

    /// Record `value` at time `now`, replacing (and so cancelling) a pending one.
    pub fn push(&mut self, now: Instant, value: T) {
        self.pending = Some((now + self.delay, value));
    }

    /// Drop the pending value, if any.
    pub fn cancel(&mut self) {
        self.pending = None;
    }

    /// Whether a value is waiting.
    pub fn is_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// Check the clock: hand out the pending value when due, else say how long to wait.
    pub fn tick(&mut self, now: Instant) -> Tick<T> {
        match &self.pending {
            None => Tick::Idle,
            Some((due, _)) if *due > now => Tick::Wait(*due - now),
            Some(_) => match self.pending.take() {
                Some((_, v)) => Tick::Fire(v),
                None => Tick::Idle,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const D: Duration = Duration::from_millis(60);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn nothing_pending_is_idle() {
        let mut d: Debouncer<u32> = Debouncer::new(D);
        assert_eq!(d.tick(Instant::now()), Tick::Idle);
    }

    #[test]
    fn fires_after_the_delay_exactly_once() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(D);
        d.push(t0, 1);
        assert_eq!(d.tick(t0 + ms(10)), Tick::Wait(ms(50)));
        assert_eq!(d.tick(t0 + ms(60)), Tick::Fire(1));
        assert_eq!(d.tick(t0 + ms(200)), Tick::Idle);
    }

    #[test]
    fn a_burst_yields_only_the_last_value() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(D);
        // 20 cursor moves 30 ms apart (key repeat): never quiet long enough
        for i in 0..20u64 {
            d.push(t0 + ms(i * 30), i);
            assert!(matches!(d.tick(t0 + ms(i * 30 + 29)), Tick::Wait(_)));
        }
        let last = 19 * 30;
        assert_eq!(d.tick(t0 + ms(last + 60)), Tick::Fire(19));
        assert_eq!(d.tick(t0 + ms(last + 500)), Tick::Idle);
    }

    #[test]
    fn push_restarts_the_delay() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(D);
        d.push(t0, "a");
        d.push(t0 + ms(50), "b");
        assert_eq!(d.tick(t0 + ms(70)), Tick::Wait(ms(40)));
        assert_eq!(d.tick(t0 + ms(110)), Tick::Fire("b"));
    }

    #[test]
    fn cancel_drops_the_pending_value() {
        let t0 = Instant::now();
        let mut d = Debouncer::new(D);
        d.push(t0, 7);
        assert!(d.is_pending());
        d.cancel();
        assert!(!d.is_pending());
        assert_eq!(d.tick(t0 + ms(500)), Tick::Idle);
    }

    #[test]
    fn delay_is_within_the_spec_budget() {
        // F22: "follows the cursor with <= 100 ms debounce"
        assert!(super::super::DEBOUNCE <= ms(100));
    }
}

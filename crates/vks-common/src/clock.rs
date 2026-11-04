//! Unified monotonic media clock with support for real-time (wall-clock) and deterministic virtual stepping.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Mode of operation for a media clock.
#[derive(Debug, Clone)]
pub enum ClockMode {
    /// Monotonic wall-clock anchored to a base Instant.
    Wall { epoch: Instant },
    /// Deterministic virtual clock advanced manually or via step increments.
    Virtual {
        now_ms: Arc<AtomicU64>,
        step_ms: u64,
    },
}

/// Unified media clock used across media engines, tickers, and bridges.
#[derive(Debug, Clone)]
pub struct MediaClock {
    mode: ClockMode,
}

impl MediaClock {
    /// Creates a monotonic wall-clock anchored to the current Instant.
    #[must_use]
    pub fn wall() -> Self {
        Self {
            mode: ClockMode::Wall {
                epoch: Instant::now(),
            },
        }
    }

    /// Creates a virtual deterministic clock starting at `start_ms` with default step increment `step_ms`.
    #[must_use]
    pub fn virtual_clock(start_ms: u64, step_ms: u64) -> Self {
        Self {
            mode: ClockMode::Virtual {
                now_ms: Arc::new(AtomicU64::new(start_ms)),
                step_ms: step_ms.max(1),
            },
        }
    }

    /// Returns the current timestamp in milliseconds.
    #[must_use]
    pub fn now_ms(&self) -> u64 {
        match &self.mode {
            ClockMode::Wall { epoch } => epoch.elapsed().as_millis() as u64,
            ClockMode::Virtual { now_ms, .. } => now_ms.load(Ordering::Relaxed),
        }
    }

    /// Advances the clock by its default step (for virtual mode) or returns current ms.
    pub fn step(&self) -> u64 {
        match &self.mode {
            ClockMode::Wall { epoch } => epoch.elapsed().as_millis() as u64,
            ClockMode::Virtual { now_ms, step_ms } => {
                now_ms.fetch_add(*step_ms, Ordering::Relaxed) + *step_ms
            }
        }
    }

    /// Advances a virtual clock by `ms` (no-op for wall clock). Returns updated `now_ms`.
    pub fn advance(&self, ms: u64) -> u64 {
        match &self.mode {
            ClockMode::Wall { epoch } => epoch.elapsed().as_millis() as u64,
            ClockMode::Virtual { now_ms, .. } => {
                now_ms.fetch_add(ms, Ordering::Relaxed) + ms
            }
        }
    }

    /// Returns whether this clock is virtual.
    #[must_use]
    pub fn is_virtual(&self) -> bool {
        matches!(self.mode, ClockMode::Virtual { .. })
    }
}

impl Default for MediaClock {
    fn default() -> Self {
        Self::wall()
    }
}

/// Non-drifting periodic ticker for fixed-interval audio/media processing.
///
/// Anchors to the base Instant and uses arithmetic tick counts (`start + N * interval`)
/// to eliminate cumulative phase drift, while guaranteeing that consecutive ticks
/// are never delivered closer together than `interval` (preventing burst storms that
/// would violate MCU rate guards).
#[derive(Debug)]
pub struct AudioTicker {
    start: Instant,
    last_tick: Instant,
    tick_count: u64,
    interval: Duration,
}

impl AudioTicker {
    /// Creates a new ticker with the given interval.
    #[must_use]
    pub fn new(interval: Duration) -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last_tick: now,
            tick_count: 0,
            interval,
        }
    }

    /// Creates an audio ticker with 20 ms interval (standard 50 Hz MCU cadence).
    #[must_use]
    pub fn new_20ms() -> Self {
        Self::new(Duration::from_millis(20))
    }

    /// Asynchronously waits until the next scheduled tick boundary.
    pub async fn tick(&mut self) -> Instant {
        self.tick_count = self.tick_count.saturating_add(1);
        let elapsed_needed = Duration::from_millis(self.tick_count.saturating_mul(self.interval.as_millis() as u64));
        let mut target = self.start + elapsed_needed;
        let now = Instant::now();
        if now.saturating_duration_since(target) > Duration::from_millis(100) {
            // Under extreme CPU stall (> 5 ticks behind), re-anchor base to prevent burst storms.
            self.start = now;
            self.tick_count = 1;
            target = now + self.interval;
        }
        // Ensure consecutive ticks are spaced by at least `interval` to avoid bursting into MCU guards.
        let scheduled = target.max(self.last_tick + self.interval);
        if scheduled > now {
            tokio::time::sleep_until(tokio::time::Instant::from_std(scheduled)).await;
        }
        let fired = Instant::now();
        self.last_tick = fired;
        fired
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wall_clock_is_monotonic() {
        let clock = MediaClock::wall();
        let t1 = clock.now_ms();
        std::thread::sleep(Duration::from_millis(10));
        let t2 = clock.now_ms();
        assert!(t2 >= t1);
    }

    #[test]
    fn virtual_clock_steps_deterministically() {
        let clock = MediaClock::virtual_clock(1000, 33);
        assert_eq!(clock.now_ms(), 1000);
        assert!(clock.is_virtual());

        let t1 = clock.step();
        assert_eq!(t1, 1033);
        assert_eq!(clock.now_ms(), 1033);

        let t2 = clock.advance(67);
        assert_eq!(t2, 1100);
        assert_eq!(clock.now_ms(), 1100);
    }

    #[test]
    fn virtual_clock_shared_across_clones() {
        let c1 = MediaClock::virtual_clock(0, 20);
        let c2 = c1.clone();

        c1.step();
        assert_eq!(c2.now_ms(), 20);

        c2.advance(80);
        assert_eq!(c1.now_ms(), 100);
    }

    #[tokio::test]
    async fn audio_ticker_advances_without_drift() {
        let mut ticker = AudioTicker::new(Duration::from_millis(5));
        let t0 = Instant::now();
        for _ in 0..3 {
            ticker.tick().await;
        }
        let elapsed = t0.elapsed();
        assert!(elapsed >= Duration::from_millis(14));
    }
}

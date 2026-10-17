//! Rate limiting (CLUSTERING.md:12, BACKEND_RULES.md:1):
//! WS frame 16KB, admin 600 req/min per key, UDP per-IP.
//!
//! Token-bucket per key, deterministic tick-based refill.
//!
//! Что реально: token-bucket, per-key, refill over time.
//! Что мокнуто: реальные WS/UDP/admin gateways, per-IP UDP DTLS rate limit.

use std::collections::BTreeMap;

/// Default rate limits (BACKEND_RULES.md:1, CLUSTERING.md:12).
pub const ADMIN_PER_MIN: u64 = 600;
pub const WS_FRAME_MAX_BYTES: u64 = 16 * 1024;

/// Token bucket for a single key.
#[derive(Debug, Clone)]
struct TokenBucket {
    capacity: u64,
    tokens: f64,
    last_refill_tick: u64,
}

impl TokenBucket {
    fn new(capacity: u64) -> Self {
        Self { capacity, tokens: capacity as f64, last_refill_tick: 0 }
    }

    /// Refill based on elapsed ticks since last_refill_tick.
    fn refill(&mut self, now_tick: u64, refill_per_tick: f64) {
        let elapsed = now_tick.saturating_sub(self.last_refill_tick);
        self.tokens = (self.tokens + elapsed as f64 * refill_per_tick).min(self.capacity as f64);
        self.last_refill_tick = now_tick;
    }

    /// Try to consume 1 token.
    fn try_consume(&mut self) -> bool {
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

pub const DEFAULT_MAX_BUCKETS: usize = 10_000;

/// Rate limiter: per-key token buckets with bounded memory protection against OOM.
pub struct RateLimiter {
    buckets: BTreeMap<String, TokenBucket>,
    /// Refill rate per tick (tokens/tick) for each key class.
    refill_per_tick: f64,
    capacity: u64,
    max_buckets: usize,
}

impl RateLimiter {
    /// Создаёт limiter с capacity = `capacity` tokens, refill
    /// `refill_per_tick` tokens/tick per key.
    ///
    /// Пример: admin 600/min → capacity=600, refill_per_tick=10 (если tick=1s,
    /// 600/60=10). WS frame → capacity=16384 (16KB), refill=large.
    pub fn new(capacity: u64, refill_per_tick: f64) -> Self {
        Self { buckets: BTreeMap::new(), refill_per_tick, capacity, max_buckets: DEFAULT_MAX_BUCKETS }
    }

    /// Настраивает максимальное число отслеживаемых ключей.
    pub fn with_max_buckets(mut self, max_buckets: usize) -> Self {
        self.max_buckets = max_buckets;
        self
    }

    /// Создаёт limiter для admin (600/min, tick=1s → 10/s).
    pub fn admin() -> Self {
        Self::new(ADMIN_PER_MIN, 10.0)
    }

    /// Создаёт limiter для WS frames (16KB/frame, 1000 frames/s).
    pub fn ws_frames() -> Self {
        Self::new(WS_FRAME_MAX_BYTES, 1000.0)
    }

    /// Allow a request for `key` at `now_tick`.
    ///
    /// - First call for key: creates bucket with full capacity → allow.
    /// - Subsequent: refill + try_consume.
    pub fn allow(&mut self, key: &str, now_tick: u64) -> bool {
        if !self.buckets.contains_key(key) && self.buckets.len() >= self.max_buckets {
            // Evict full/idle buckets first, or oldest by last_refill_tick
            if let Some(idle_key) =
                self.buckets.iter().find(|(_, b)| b.tokens >= b.capacity as f64 - 0.001).map(|(k, _)| k.clone())
            {
                self.buckets.remove(&idle_key);
            } else if let Some(oldest_key) =
                self.buckets.iter().min_by_key(|(_, b)| b.last_refill_tick).map(|(k, _)| k.clone())
            {
                self.buckets.remove(&oldest_key);
            }
        }
        let bucket = self.buckets.entry(key.to_string()).or_insert_with(|| TokenBucket::new(self.capacity));
        bucket.refill(now_tick, self.refill_per_tick);
        bucket.try_consume()
    }

    /// Текущий token count for key (for debugging/metrics).
    pub fn tokens(&self, key: &str) -> Option<f64> {
        self.buckets.get(key).map(|b| b.tokens)
    }

    /// Number of tracked keys.
    pub fn key_count(&self) -> usize {
        self.buckets.len()
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::admin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_limit_allows() {
        let mut rl = RateLimiter::new(600, 0.0); // 600 tokens, no refill
        for i in 0..600u64 {
            assert!(rl.allow("key", i));
        }
        // 601st → deny.
        assert!(!rl.allow("key", 601));
    }

    #[test]
    fn over_limit_denies() {
        let mut rl = RateLimiter::new(5, 0.0); // 5 tokens, no refill
        for _ in 0..5u64 {
            assert!(rl.allow("k", 0));
        }
        assert!(!rl.allow("k", 0));
    }

    #[test]
    fn refill_over_time() {
        let mut rl = RateLimiter::new(5, 1.0); // 1 token/s
                                               // Single allow at tick 0 (no drift), then drain.
        assert!(rl.allow("k", 0));
        for _ in 1..5u64 {
            rl.allow("k", 0);
        }
        assert!(!rl.allow("k", 0));
        // After 3 ticks → +3 tokens (refill 1/tick).
        assert!(rl.allow("k", 3));
        assert!(rl.allow("k", 3));
        assert!(rl.allow("k", 3));
        assert!(!rl.allow("k", 3));
    }

    #[test]
    fn per_key_isolated() {
        let mut rl = RateLimiter::new(2, 0.0);
        assert!(rl.allow("a", 0));
        assert!(rl.allow("a", 1));
        assert!(!rl.allow("a", 2));
        // Different key still has full capacity.
        assert!(rl.allow("b", 0));
    }

    #[test]
    fn admin_default_capacity() {
        let rl = RateLimiter::admin();
        assert_eq!(rl.capacity, 600);
    }

    #[test]
    fn bounded_capacity_prevents_oom() {
        let mut rl = RateLimiter::new(10, 1.0).with_max_buckets(3);
        rl.allow("user1", 1);
        rl.allow("user2", 2);
        rl.allow("user3", 3);
        assert_eq!(rl.key_count(), 3);

        // Fourth user triggers bounded eviction
        rl.allow("user4", 4);
        assert!(rl.key_count() <= 3);
    }
}

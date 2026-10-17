//! Backpressure (CLUSTERING.md §5): очередь > 10000 → drop non-critical,
//! retry critical.
//!
//! `Bus::push_backpressure` использует `PushResult`. Здесь — standalone
//! модель очереди с max size и idempotent push.
//!
//! Что реально: queue with cap, critical/non-critical policy, DLQ for drops.
//! Что мокнуто: реальный NATS JetStream queue, cross-subject ordering.

use std::collections::VecDeque;

use crate::bus::Msg;

/// Результат push в очередь под backpressure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushResult {
    /// Принято в очередь.
    Accepted,
    /// Non-critical dropped (очередь полна) → в DLQ.
    DroppedNonCritical,
    /// Critical: retry (остаётся в буферной зоне очереди, не drop-ится).
    RetriedCritical,
    /// Critical: жесткий предел исчерпан (len >= hard_max) → в DLQ для защиты от OOM.
    DroppedCritical,
}

/// Max size очереди перед backpressure (CLUSTERING.md §5: >10000).
pub const DEFAULT_MAX_QUEUE: usize = 10_000;

/// Очередь с backpressure.
pub struct Backpressure {
    queue: VecDeque<Msg>,
    max: usize,
    hard_max: usize,
    /// Dropped messages (DLQ).
    dlq: Vec<Msg>,
}

impl Backpressure {
    /// Создаёт очередь с max = DEFAULT_MAX_QUEUE и hard_max = 2 * DEFAULT_MAX_QUEUE.
    pub fn new() -> Self {
        Self::with_limits(DEFAULT_MAX_QUEUE, DEFAULT_MAX_QUEUE.saturating_mul(2))
    }

    /// Создаёт очередь с кастомным max и hard_max = max * 2.
    pub fn with_max(max: usize) -> Self {
        Self::with_limits(max, max.saturating_mul(2))
    }

    /// Создаёт очередь с явным max и hard_max.
    pub fn with_limits(max: usize, hard_max: usize) -> Self {
        Self { queue: VecDeque::new(), max, hard_max: hard_max.max(max), dlq: Vec::new() }
    }

    /// Текущая длина очереди.
    pub fn len(&self) -> usize {
        self.queue.len()
    }

    /// Пусто ли.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// Soft max size.
    pub fn max(&self) -> usize {
        self.max
    }

    /// Hard max size (защита от OOM).
    pub fn hard_max(&self) -> usize {
        self.hard_max
    }

    /// Push msg.
    /// - Если len < max: Accepted.
    /// - Если max <= len < hard_max и msg.critical: RetriedCritical (остаётся в очереди).
    /// - Если len >= hard_max и msg.critical: DroppedCritical (в DLQ).
    /// - Если len >= max и !msg.critical: DroppedNonCritical (в DLQ).
    pub fn push(&mut self, msg: Msg) -> PushResult {
        let len = self.queue.len();
        if len < self.max {
            self.queue.push_back(msg);
            PushResult::Accepted
        } else if len < self.hard_max && msg.critical {
            self.queue.push_back(msg);
            PushResult::RetriedCritical
        } else if msg.critical {
            self.dlq.push(msg);
            PushResult::DroppedCritical
        } else {
            self.dlq.push(msg);
            PushResult::DroppedNonCritical
        }
    }

    /// Pop next message (FIFO).
    pub fn pop(&mut self) -> Option<Msg> {
        self.queue.pop_front()
    }

    /// DLQ (dropped messages).
    pub fn dlq(&self) -> &[Msg] {
        &self.dlq
    }

    /// Количество dropped.
    pub fn dropped_count(&self) -> usize {
        self.dlq.len()
    }
}

impl Default for Backpressure {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus;

    fn msg(critical: bool) -> Msg {
        bus::Msg::new("vks.cluster.q", vec![1], critical)
    }

    #[test]
    fn accepts_under_max() {
        let mut bp = Backpressure::with_max(3);
        assert_eq!(bp.push(msg(true)), PushResult::Accepted);
        assert_eq!(bp.len(), 1);
    }

    #[test]
    fn drops_noncritical_when_full() {
        let mut bp = Backpressure::with_max(2);
        bp.push(msg(false));
        bp.push(msg(false));
        assert_eq!(bp.len(), 2);
        let r = bp.push(msg(false));
        assert_eq!(r, PushResult::DroppedNonCritical);
        assert_eq!(bp.dropped_count(), 1);
        assert_eq!(bp.len(), 2); // queue unchanged
    }

    #[test]
    fn retries_critical_when_full() {
        let mut bp = Backpressure::with_max(2);
        bp.push(msg(false));
        bp.push(msg(false));
        let r = bp.push(msg(true));
        assert_eq!(r, PushResult::RetriedCritical);
        assert_eq!(bp.len(), 3); // critical stays
        assert_eq!(bp.dropped_count(), 0);
    }

    #[test]
    fn drops_critical_when_hard_max_reached() {
        let mut bp = Backpressure::with_limits(2, 3);
        bp.push(msg(false));
        bp.push(msg(false));
        // len is 2 == max.
        assert_eq!(bp.push(msg(true)), PushResult::RetriedCritical);
        // len is now 3 == hard_max. Next critical is dropped to DLQ to prevent unbounded growth.
        assert_eq!(bp.push(msg(true)), PushResult::DroppedCritical);
        assert_eq!(bp.len(), 3);
        assert_eq!(bp.dropped_count(), 1);
        assert!(bp.dlq()[0].critical);
    }

    #[test]
    fn fifo_order_preserved() {
        let mut bp = Backpressure::with_max(10);
        bp.push(msg(true));
        bp.push(msg(false));
        assert_eq!(bp.pop().unwrap().payload, vec![1]);
    }

    #[test]
    fn default_max_is_10000() {
        let bp = Backpressure::new();
        assert_eq!(bp.max(), 10_000);
    }
}

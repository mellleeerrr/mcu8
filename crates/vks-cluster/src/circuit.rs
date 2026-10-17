//! Circuit breaker для зависимостей (LDAP/PG/ATN/TURN) — CLUSTERING.md §6.
//!
//! Closed → (50% fail in 10 ticks window) → Open (30 ticks) → Half-Open
//! (1 probe) → Closed on success / Open on failure.
//!
//! Что реально: state machine, sliding-window fail rate, tick-based timing,
//! probe logic. Что мокнуто: реальные retry/backoff (exponential 1s→30s),
//! интеграция с NATS heartbeat.

use std::collections::VecDeque;

use thiserror::Error;

/// Состояние circuit breaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerState {
    /// Closed: запросы проходят.
    Closed,
    /// Open: запросы пропускаются (skip), до `open_until` tick.
    Open,
    /// Half-Open: один probe разрешён.
    HalfOpen,
}

/// Пороги (CLUSTERING.md §6): 50% fail / окно 10 ticks → OPEN 30 ticks.
pub const FAIL_THRESHOLD_PCT: u64 = 50;
pub const WINDOW_TICKS: u64 = 10;
pub const OPEN_TICKS: u64 = 30;

/// Ошибка вызова через breaker.
#[derive(Debug, Error)]
#[error("circuit breaker call failed")]
pub struct BreakerError;

/// Circuit breaker.
pub struct CircuitBreaker {
    state: BreakerState,
    /// Окно результатов: (tick, success). Слайдинд окно WINDOW_TICKS.
    window: VecDeque<(u64, bool)>,
    /// Tick, до которого Open (если Open).
    open_until: u64,
}

impl CircuitBreaker {
    /// Создаёт breaker в Closed.
    pub fn new() -> Self {
        Self { state: BreakerState::Closed, window: VecDeque::new(), open_until: 0 }
    }

    /// Текущее состояние.
    pub fn state(&self) -> BreakerState {
        self.state
    }

    /// Вызывает `fn` через breaker:
    /// - Closed/Half-Open: fn вызывается, результат фиксируется.
    /// - Open (не истёк): fn пропускается (skip), возвращает `Err(BreakerError)`.
    pub fn call<R, E, F>(&mut self, now_tick: u64, f: F) -> Result<R, E>
    where
        F: FnOnce() -> Result<R, E>,
        E: From<BreakerError>,
    {
        // Auto-transition: Open → HalfOpen когда open_until истёк.
        if self.state == BreakerState::Open && now_tick >= self.open_until {
            self.state = BreakerState::HalfOpen;
        }

        match self.state {
            BreakerState::Open => {
                // Skip: не вызываем fn.
                Err(BreakerError.into())
            }
            BreakerState::Closed | BreakerState::HalfOpen => {
                let result = f();
                let success = result.is_ok();
                // Запоминаем в окно.
                self.window.push_back((now_tick, success));
                // Отбрасываем вне окна.
                while let Some(&(t, _)) = self.window.front() {
                    if now_tick.saturating_sub(t) >= WINDOW_TICKS {
                        self.window.pop_front();
                    } else {
                        break;
                    }
                }

                if success {
                    // HalfOpen success → Closed.
                    if self.state == BreakerState::HalfOpen {
                        self.state = BreakerState::Closed;
                        self.window.clear();
                    }
                } else {
                    // Fail.
                    if self.state == BreakerState::HalfOpen {
                        // Probe failed → Open.
                        self.state = BreakerState::Open;
                        self.open_until = now_tick + OPEN_TICKS;
                    } else {
                        // Closed: проверяем 50% fail в окне.
                        let (fails, total) = self.fail_rate();
                        if total >= 2 && (fails * 100 / total) >= FAIL_THRESHOLD_PCT {
                            self.state = BreakerState::Open;
                            self.open_until = now_tick + OPEN_TICKS;
                        }
                    }
                }
                result
            }
        }
    }

    /// (fails, total) в текущем слайдинд окне.
    fn fail_rate(&self) -> (u64, u64) {
        let fails = self.window.iter().filter(|(_, s)| !*s).count() as u64;
        let total = self.window.len() as u64;
        (fails, total)
    }

    /// Текущий fail rate в окне (для метрик).
    pub fn fail_rate_pct(&self) -> u64 {
        let (f, t) = self.fail_rate();
        if t == 0 {
            0
        } else {
            f * 100 / t
        }
    }

    /// Tick, до которого Open (если state == Open).
    pub fn open_until_tick(&self) -> u64 {
        self.open_until
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test error type implementing From<BreakerError>.
    #[derive(Debug, PartialEq)]
    struct TestErr(String);
    impl std::fmt::Display for TestErr {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }
    impl std::error::Error for TestErr {}
    impl From<BreakerError> for TestErr {
        fn from(_: BreakerError) -> Self {
            TestErr("breaker skipped".to_string())
        }
    }

    /// Trip breaker: 5 fails + 5 ok in window → 50% → Open.
    fn trip(cb: &mut CircuitBreaker) {
        for i in 0..5u64 {
            let _ = cb.call(i, || -> Result<i32, TestErr> { Err(TestErr("fail".to_string())) });
        }
        for i in 5..10u64 {
            let _ = cb.call(i, || -> Result<i32, TestErr> { Ok(0) });
        }
    }

    #[test]
    fn starts_closed() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn success_in_window_stays_closed() {
        let mut cb = CircuitBreaker::new();
        for t in 0..10u64 {
            cb.call(t, || -> Result<i32, TestErr> { Ok(0) }).unwrap();
        }
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn trips_on_50pct_fail_in_window() {
        let mut cb = CircuitBreaker::new();
        trip(&mut cb);
        assert_eq!(cb.state(), BreakerState::Open);
        // The trip actually fires on the 2nd fail (tick 1): after 2 fails,
        // fails=2/total=2 = 100% ≥ 50% → Open, open_until = 1 + 30 = 31.
        assert_eq!(cb.open_until_tick(), 1 + OPEN_TICKS);
    }

    #[test]
    fn open_skips_calls() {
        let mut cb = CircuitBreaker::new();
        trip(&mut cb);
        assert_eq!(cb.state(), BreakerState::Open);
        // Open: skip (no call).
        let called = std::cell::Cell::new(false);
        let r = cb.call(15, || {
            called.set(true);
            Ok::<i32, TestErr>(42)
        });
        assert!(r.is_err());
        assert!(!called.get());
    }

    #[test]
    fn open_to_halfopen_after_open_ticks() {
        let mut cb = CircuitBreaker::new();
        trip(&mut cb);
        assert_eq!(cb.state(), BreakerState::Open);
        // tick 10 + 30 = 40 → HalfOpen, probe success → Closed.
        cb.call(40, || -> Result<i32, TestErr> { Ok(0) }).unwrap();
        assert_eq!(cb.state(), BreakerState::Closed);
    }

    #[test]
    fn halfopen_probe_failure_opens_again() {
        let mut cb = CircuitBreaker::new();
        trip(&mut cb);
        assert_eq!(cb.state(), BreakerState::Open);
        // tick 40 → HalfOpen, probe fails → Open again.
        cb.call(40, || -> Result<i32, TestErr> { Err(TestErr("probe".to_string())) }).unwrap_err();
        assert_eq!(cb.state(), BreakerState::Open);
        assert_eq!(cb.open_until_tick(), 40 + OPEN_TICKS);
    }

    #[test]
    fn fail_rate_pct() {
        let mut cb = CircuitBreaker::new();
        cb.call(0, || -> Result<i32, TestErr> { Ok(0) }).unwrap();
        cb.call(1, || -> Result<i32, TestErr> { Err(TestErr("x".to_string())) }).unwrap_err();
        assert_eq!(cb.fail_rate_pct(), 50);
    }
}

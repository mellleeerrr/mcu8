//! Политика переподключения и resume (join nonce).
//!
//! Основание: `docs/architecture/SIGNALING_PROTOCOL.md` §7 (Reconnect / Resume).
//! - `join_nonce` (UUIDv7) TTL **60s**, привязан к `(room_id, participant_id)`.
//! - Client backoff: **exponential 1s → 16s**, максимум **5 попыток**.
//! - Истёк nonce → полный re-join (новый `participant_id`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// TTL join nonce (SIGNALING_PROTOCOL §7).
pub const NONCE_TTL: Duration = Duration::from_secs(60);

/// Максимум попыток переподключения (SIGNALING_PROTOCOL §7).
pub const MAX_ATTEMPTS: u32 = 5;

/// Backoff-таймеры: 1s, 2s, 4s, 8s, 16s (exponential, cap 16s).
const BACKOFF: [Duration; MAX_ATTEMPTS as usize] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
];

/// Backoff-планировщик (deterministic, testable — без sleep).
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    attempt: u32,
}

impl Backoff {
    #[must_use]
    pub fn new() -> Self {
        Self { attempt: 0 }
    }

    /// `true`, если попытки ещё остались (до MAX_ATTEMPTS).
    #[must_use]
    pub fn has_attempts(&self) -> bool {
        self.attempt < MAX_ATTEMPTS
    }

    /// Длительность текущей паузы (1s→16s).
    #[must_use]
    pub fn current_delay(&self) -> Duration {
        BACKOFF[(self.attempt as usize).min(BACKOFF.len() - 1)]
    }

    /// Переходит к следующей попытке (вызывается после неудачи).
    /// Возвращает `false`, если попытки исчерпаны.
    #[must_use]
    pub fn advance(&mut self) -> bool {
        if self.attempt < MAX_ATTEMPTS {
            self.attempt += 1;
            true
        } else {
            false
        }
    }

    /// Сброс после успешного подключения.
    pub fn reset(&mut self) {
        self.attempt = 0;
    }
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

/// Join nonce (значение + issued_at) для resume (SIGNALING_PROTOCOL §7).
#[derive(Debug, Clone)]
pub struct ResumeNonce {
    nonce: String,
    issued_at: SystemTime,
}

impl ResumeNonce {
    #[must_use]
    pub fn new(nonce: impl Into<String>) -> Self {
        Self { nonce: nonce.into(), issued_at: SystemTime::now() }
    }

    /// `true`, если nonce ещё валиден (TTL 60s).
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.issued_at.elapsed().map(|d| d < NONCE_TTL).unwrap_or(false)
    }

    /// Осталось времени до истечения (или 0).
    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.issued_at.elapsed().map(|d| NONCE_TTL.saturating_sub(d)).unwrap_or(Duration::ZERO)
    }

    #[must_use]
    pub fn value(&self) -> &str {
        &self.nonce
    }
}

/// Текущее время в unix-ms (для `issued_at`-полей протокола).
#[must_use]
pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn backoff_sequence_is_exponential_1_to_16() {
        let mut b = Backoff::new();
        assert_eq!(b.current_delay(), Duration::from_secs(1));
        assert!(b.has_attempts());
        let mut delays = Vec::new();
        while b.has_attempts() {
            delays.push(b.current_delay());
            let _ = b.advance();
        }
        assert_eq!(
            delays,
            vec![
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
            ]
        );
        assert!(!b.has_attempts());
        assert!(!b.advance());
    }

    #[test]
    fn backoff_reset_after_success() {
        let mut b = Backoff::new();
        let _ = b.advance();
        let _ = b.advance();
        assert_eq!(b.current_delay(), Duration::from_secs(4));
        b.reset();
        assert_eq!(b.current_delay(), Duration::from_secs(1));
    }

    proptest! {
        #[test]
        fn nonce_ttl_bounds(_attempts: u32) {
            let n = ResumeNonce::new("abc");
            prop_assert!(n.is_valid());
            prop_assert!(n.remaining() <= NONCE_TTL);
            prop_assert!(n.remaining() > Duration::ZERO);
        }
    }

    #[test]
    fn nonce_value_and_ttl_constants() {
        assert_eq!(NONCE_TTL, Duration::from_secs(60));
        assert_eq!(MAX_ATTEMPTS, 5);
        assert_eq!(ResumeNonce::new("xyz").value(), "xyz");
        assert!(unix_ms() > 0);
    }
}

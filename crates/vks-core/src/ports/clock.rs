// Порт Clock — для детерминированных тестов.
//
// Ядро не владеет системным временем; все временныe метки приходят
// через этот интерфейс. Тесты подставляют `FixedClock` (или
// `SteppingClock`), чтобы логика была детерминированной.

use async_trait::async_trait;
use std::time::SystemTime;

/// Ошибка Clock.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ClockError {
    /// Ошибка реализации.
    #[error("clock error: {0}")]
    Other(String),
}

/// Порт Clock — источник времени для ядра.
#[async_trait]
pub trait Clock: Send + Sync + 'static {
    /// Возвращает текущее время (unix epoch, SystemTime).
    async fn now(&self) -> Result<SystemTime, ClockError>;
}

/// Фиксированное время (для детерминированных тестов).
#[derive(Debug, Clone, Copy)]
pub struct FixedClock(SystemTime);

impl FixedClock {
    /// Создаёт фиксированное время с unix-секундами.
    pub fn from_unix_secs(seconds: u64) -> Self {
        Self(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds))
    }
}

impl Default for FixedClock {
    fn default() -> Self {
        Self::from_unix_secs(0)
    }
}

#[async_trait]
impl Clock for FixedClock {
    async fn now(&self) -> Result<SystemTime, ClockError> {
        Ok(self.0)
    }
}

/// Реальное системное время (production).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    async fn now(&self) -> Result<SystemTime, ClockError> {
        Ok(std::time::SystemTime::now())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Мини-async runtime (только для тестов): запускает future синхронно.
    fn run<F: std::future::Future>(fut: F) -> F::Output {
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        rt.block_on(fut)
    }

    #[test]
    fn fixed_clock_returns_set_time() {
        let clock = FixedClock::from_unix_secs(1_000_000);
        let now = run(clock.now());
        let dur = now.unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        assert_eq!(dur.as_secs(), 1_000_000);
    }

    #[test]
    fn system_clock_returns_sane_time() {
        let clock = SystemClock;
        let now = run(clock.now());
        let dur = now.unwrap().duration_since(SystemTime::UNIX_EPOCH).unwrap();
        // sanity: after 2001
        assert!(dur.as_secs() > 900_000_000);
    }
}

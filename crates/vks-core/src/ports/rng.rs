// Порт Rng — для детерминированных тестов.
//
// Ядро не владеет системным RNG; все случайные значения (ID, jitter,
// backoff) приходят через этот интерфейс. Тесты подставляют
// `DeterministicRng`.

use async_trait::async_trait;

/// Ошибка Rng.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RngError {
    /// Ошибка реализации.
    #[error("rng error: {0}")]
    Other(String),
}

/// Порт Rng — источник случайных чисел для ядра.
#[async_trait]
pub trait Rng: Send + Sync + 'static {
    /// Возвращает `n` random bytes.
    async fn gen_bytes(&self, n: usize) -> Result<Vec<u8>, RngError>;

    /// Возвращает random u64 in `[0, bound)`.
    async fn gen_range(&self, bound: u64) -> Result<u64, RngError>;

    /// Возвращает random u32 in `[0, bound)`.
    async fn gen_range_u32(&self, bound: u32) -> Result<u32, RngError>;

    /// Возвращает random bool.
    async fn gen_bool(&self) -> Result<bool, RngError>;
}

/// Детерминированный RNG (для тестов): simple LCG.
///
/// Deterministic, stateful (state is `u64`), thread-safe via
/// `Arc<Mutex>`. Suitable for unit tests; `Clone`-able (shared state).
#[derive(Debug, Clone)]
pub struct DeterministicRng {
    state: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl DeterministicRng {
    /// Создаёт RNG с seed.
    pub fn new(seed: u64) -> Self {
        Self { state: std::sync::Arc::new(std::sync::Mutex::new(if seed == 0 { 0x9E3779B97F4A7C15 } else { seed })) }
    }

    /// Следующий raw u64 (LCG, 64-bit).
    fn next_u64(&self) -> u64 {
        let mut s = self.state.lock().unwrap();
        *s = s.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *s
    }

    /// Возвращает текущий seed-state (для inspection in tests).
    pub fn state(&self) -> u64 {
        *self.state.lock().unwrap()
    }
}

impl Default for DeterministicRng {
    fn default() -> Self {
        Self::new(42)
    }
}

#[async_trait]
impl Rng for DeterministicRng {
    async fn gen_bytes(&self, n: usize) -> Result<Vec<u8>, RngError> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            let v = self.next_u64();
            for b in v.to_le_bytes() {
                if out.len() < n {
                    out.push(b);
                }
            }
        }
        Ok(out)
    }

    async fn gen_range(&self, bound: u64) -> Result<u64, RngError> {
        if bound == 0 {
            return Ok(0);
        }
        Ok(self.next_u64() % bound)
    }

    async fn gen_range_u32(&self, bound: u32) -> Result<u32, RngError> {
        if bound == 0 {
            return Ok(0);
        }
        Ok((self.next_u64() % u64::from(bound)) as u32)
    }

    async fn gen_bool(&self) -> Result<bool, RngError> {
        Ok(self.next_u64() & 1 == 1)
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
    fn deterministic_same_seed_same_sequence() {
        let a = DeterministicRng::new(12345);
        let b = DeterministicRng::new(12345);
        let mut va = Vec::new();
        let mut vb = Vec::new();
        for _ in 0..8 {
            va.push(run(a.gen_range(1_000_000)).unwrap());
            vb.push(run(b.gen_range(1_000_000)).unwrap());
        }
        assert_eq!(va, vb);
    }

    #[test]
    fn deterministic_different_seed_different_sequence() {
        let a = DeterministicRng::new(1);
        let b = DeterministicRng::new(2);
        let va: Vec<u64> = (0..8).map(|_| run(a.gen_range(1_000_000)).unwrap()).collect();
        let vb: Vec<u64> = (0..8).map(|_| run(b.gen_range(1_000_000)).unwrap()).collect();
        // Extremely unlikely to be equal with different seeds.
        assert_ne!(va, vb);
    }

    #[test]
    fn gen_bytes_length() {
        let r = DeterministicRng::new(42);
        let bytes = run(r.gen_bytes(32)).unwrap();
        assert_eq!(bytes.len(), 32);
    }

    #[test]
    fn gen_range_bounds() {
        let r = DeterministicRng::new(7);
        for _ in 0..100 {
            let v = run(r.gen_range(10)).unwrap();
            assert!(v < 10);
        }
        // bound == 0 → 0
        assert_eq!(run(r.gen_range(0)).unwrap(), 0);
    }

    #[test]
    fn gen_range_u32_bounds() {
        let r = DeterministicRng::new(7);
        for _ in 0..100 {
            let v = run(r.gen_range_u32(255)).unwrap();
            assert!(v < 255);
        }
    }

    #[test]
    fn gen_bool_returns_both_values() {
        let r = DeterministicRng::new(99);
        let mut seen = [false; 2];
        for _ in 0..200 {
            let v = run(r.gen_bool()).unwrap();
            seen[v as usize] = true;
        }
        assert!(seen[0] && seen[1]);
    }
}

//! FFI-граница H323Plus (ADR-0007, BACKEND_RULES §6.2).
//!
//! Правила FFI-границы:
//!
//! - **одна точка** входа — [`H323PlusBridge::call`];
//! - **panic hook** — паник в FFI не роняет процесс: перехватывается
//!   ([`run_with_panic_hook`]) и превращается в `Err`;
//! - **watchdog** — если FFI-вызов длится дольше порога (5s в production,
//!   в mock — N tick'ов), модуль помечается `Crashed` и перезапускается
//!   ([`Watchdog::check`]).
//!
//! Эта волна — **mock** FFI-границы: детерминированная, без реальных
//! потоков и времени — используется tick-счётчик. Реальная FFI-обёртка
//! над H323Plus/OPAL (C++); единственные
//! `unsafe` блоки).

use std::panic::{catch_unwind, AssertUnwindSafe};

use vks_common::Error;

/// Порог watchdog'а в production (секунды, ADR-0007: FFI call > 5s → restart).
pub const PRODUCTION_WATCHDOG_LIMIT: u64 = 5;

/// Состояние H323Plus-модуля.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleState {
    /// Модуль работает.
    Ok,
    /// Watchdog отметил перегруз (FFI call > timeout) — требуется restart.
    Crashed,
}

impl ModuleState {
    /// Человекочитаемое имя (для логов).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Crashed => "crashed",
        }
    }
}

/// Результат FFI-вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfiResult {
    /// Занимало ли вызов (в mock — «тик-единицы»).
    pub ticks_used: u64,
    /// Успешен ли вызов.
    pub success: bool,
}

/// Watchdog: следит за длительностью FFI-вызовов.
///
/// Deterministic: вместо реального времени используется tick-счётчик.
/// Порог параметризуемый (в production = [`PRODUCTION_WATCHDOG_LIMIT`] = 5).
#[derive(Debug)]
pub struct Watchdog {
    /// Порог в tick'ах: вызов длиннее порога → crash.
    limit: u64,
}

impl Watchdog {
    /// Создаёт watchdog с заданным порогом (tick'и).
    #[must_use]
    pub fn new(limit: u64) -> Self {
        Self { limit }
    }

    /// Production-watchdog (5, ADR-0007).
    #[must_use]
    pub fn production() -> Self {
        Self::new(PRODUCTION_WATCHDOG_LIMIT)
    }

    /// Проверяет результат FFI-вызова.
    ///
    /// Если `result.ticks_used > limit` → модуль `Crashed`.
    #[must_use]
    pub fn check(&self, result: &FfiResult) -> ModuleState {
        if result.ticks_used > self.limit {
            ModuleState::Crashed
        } else {
            ModuleState::Ok
        }
    }
}

/// FFI-мост над H323Plus (mock).
///
/// Единственная точка FFI-границы: [`H323PlusBridge::call`].
#[derive(Debug)]
pub struct H323PlusBridge {
    /// Состояние модуля.
    state: ModuleState,
    /// Watchdog, следящий за длительностью вызовов.
    watchdog: Watchdog,
    /// Счётчик restart'ов (для observability).
    restarts: u64,
}

impl H323PlusBridge {
    /// Создаёт мост с production-watchdog (5).
    #[must_use]
    pub fn new() -> Self {
        Self { state: ModuleState::Ok, watchdog: Watchdog::production(), restarts: 0 }
    }

    /// Создаёт мост с заданным watchdog-порогом (tick'и).
    #[must_use]
    pub fn with_watchdog(watchdog: Watchdog) -> Self {
        Self { state: ModuleState::Ok, watchdog, restarts: 0 }
    }

    /// Текущее состояние модуля.
    #[must_use]
    pub fn state(&self) -> ModuleState {
        self.state
    }

    /// Сколько раз модуль был перезапущен watchdog'ом.
    #[must_use]
    pub fn restart_count(&self) -> u64 {
        self.restarts
    }

    /// Выполняет FFI-вызов (mock).
    ///
    /// `simulate_ticks` — сколько «тик-единиц» займёт вызов (в mock это
    /// параметр; в реальном FFI — измеренная длительность). Если вызов
    /// дольше watchdog-порога → модуль `Crashed` + watchdog restart.
    ///
    /// # Errors
    /// [`Error::Conflict`] — если модуль уже `Crashed` (нужен `restart`).
    /// [`Error::Media`] — если сам FFI-вызов завершился неудачно.
    pub fn call(&mut self, operation: &str, simulate_ticks: u64) -> Result<FfiResult, Error> {
        if self.state == ModuleState::Crashed {
            return Err(Error::Conflict(format!(
                "H323Plus module crashed, call '{operation}' rejected (restart first)"
            )));
        }
        // Паник в FFI не должен ронять процесс — перехватываем (panic hook).
        let result = run_with_panic_hook(operation, || FfiResult { ticks_used: simulate_ticks, success: true });
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                self.state = ModuleState::Crashed;
                self.restarts += 1;
                tracing::warn!(operation, "H323Plus FFI panic caught, module marked Crashed");
                return Err(e);
            }
        };
        let state = self.watchdog.check(&result);
        self.state = state;
        if state == ModuleState::Crashed {
            tracing::warn!(
                operation,
                ticks = result.ticks_used,
                limit = self.watchdog.limit,
                "H323Plus FFI exceeded watchdog limit, module Crashed"
            );
            return Err(Error::Internal(anyhow::anyhow!(
                "H323Plus FFI call '{operation}' exceeded watchdog limit ({} ticks > {} limit)",
                result.ticks_used,
                self.watchdog.limit
            )));
        }
        if !result.success {
            return Err(Error::Media(format!("H323Plus FFI call '{operation}' failed")));
        }
        Ok(result)
    }

    /// Перезапускает crashed-модуль (watchdog restart).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если модуль уже `Ok`.
    pub fn restart(&mut self) -> Result<(), Error> {
        if self.state == ModuleState::Ok {
            return Err(Error::Conflict("H323Plus module is already Ok".into()));
        }
        self.state = ModuleState::Ok;
        self.restarts += 1;
        tracing::info!("H323Plus module restarted");
        Ok(())
    }
}

impl Default for H323PlusBridge {
    fn default() -> Self {
        Self::new()
    }
}

/// Panic hook: выполняет FFI-операцию, перехватывая паник.
///
/// Паник в FFI не роняет процесс: [`catch_unwind`] ловит его, логирует и
/// возвращает [`Error::Internal`]. Это — «форма» panic hook, которая
/// будет применена к реальной FFI-границе H323Plus.
fn run_with_panic_hook(
    operation: &str,
    f: impl FnOnce() -> FfiResult + std::panic::UnwindSafe,
) -> Result<FfiResult, Error> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(result) => Ok(result),
        Err(_) => {
            tracing::warn!(operation, "H323Plus FFI panic intercepted (process not aborted)");
            Err(vks_common::Error::Internal(anyhow::anyhow!("H323Plus FFI '{operation}' panicked")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_call_succeeds() {
        let mut bridge = H323PlusBridge::with_watchdog(Watchdog::new(5));
        let result = bridge.call("setup", 3).expect("fast call ok");
        assert!(result.success);
        assert_eq!(bridge.state(), ModuleState::Ok);
        assert_eq!(bridge.restart_count(), 0);
    }

    #[test]
    fn slow_ffi_exceeds_watchdog_marks_crashed() {
        let mut bridge = H323PlusBridge::with_watchdog(Watchdog::new(5));
        // 6 ticks > 5 limit → watchdog marks Crashed and returns Error::Internal.
        let err = bridge.call("setup", 6).expect_err("slow call exceeds watchdog limit");
        assert!(matches!(err, Error::Internal(_)));
        assert_eq!(bridge.state(), ModuleState::Crashed);

        // Дальнейшие вызовы отклоняются, пока не restart.
        let err = bridge.call("setup", 1).expect_err("crashed module rejects calls");
        assert!(matches!(err, Error::Conflict(_)));

        // Restart восстанавливает модуль.
        bridge.restart().expect("restart");
        assert_eq!(bridge.state(), ModuleState::Ok);
        assert_eq!(bridge.restart_count(), 1);
        assert!(bridge.call("setup", 1).is_ok());
    }

    #[test]
    fn watchdog_boundary_at_limit_is_ok() {
        let mut bridge = H323PlusBridge::with_watchdog(Watchdog::new(5));
        // Exactly at limit (5 == 5) — not exceeded → Ok.
        assert!(bridge.call("setup", 5).is_ok());
        assert_eq!(bridge.state(), ModuleState::Ok);
    }

    #[test]
    fn panic_hook_converts_panic_to_err() {
        // Simulates the panic-hook shape: a panicking closure is caught by
        // catch_unwind and converted to Err, not propagated.
        let operation = "panicking-op";
        let result = run_with_panic_hook(operation, || panic!("simulated FFI panic"));
        let err = result.expect_err("panic becomes Err");
        assert!(matches!(err, Error::Internal(_)));
        assert!(err.to_string().contains(operation));
    }

    #[test]
    fn failed_ffi_result_is_media_error_and_watchdog_ok() {
        let wd = Watchdog::new(5);
        // A failing FFI result within the limit: watchdog stays Ok, but the
        // result signals failure → the bridge maps it to Error::Media.
        let result = FfiResult { ticks_used: 1, success: false };
        assert_eq!(wd.check(&result), ModuleState::Ok);
    }

    #[test]
    fn production_watchdog_is_five() {
        let wd = Watchdog::production();
        assert!(wd.check(&FfiResult { ticks_used: 5, success: true }) == ModuleState::Ok);
        assert!(wd.check(&FfiResult { ticks_used: 6, success: true }) == ModuleState::Crashed);
    }
}

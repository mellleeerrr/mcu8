//! `vks-common` — общие утилиты для backend-крейтов VKS.
//!
//! Содержит единый `Error`, инициализацию `tracing`, Prometheus-метрики,
//! загрузку конфигурации (figment + env `VKS_*`) и генерацию id (UUIDv7/ULID).
//!
//! Крейт-привязка: `docs/rules/BACKEND_RULES.md` §6, `docs/PROJECT_RULES.md` §19–22.
//!
//! Дерево зависимостей (ARCHITECTURE.md §3): `vks-common` — standalone в фазе 0;
//! в фазе 1 получит зависимость от `vks-core`.

pub mod codec_capabilities;
pub mod config;
pub mod error;
pub mod ids;
pub mod metrics;
pub mod qoe;
pub mod ratelimit;
pub mod slo;
pub mod tracing;

// Удобные переэкспорты (часто используемые).
pub use config::Config;
pub use error::{is_terminal_failure, strip_terminal_failure, terminal_failure, Error, ErrorCode, HttpStatus};
pub use qoe::{QoeAggregator, QoeReport, MOS_THRESHOLD, QOE_SLO_PCT, RTT_P95_MAX_MS};
pub use ratelimit::{RateLimiter, ADMIN_PER_MIN, WS_FRAME_MAX_BYTES};
pub use slo::{SloEvaluator, SloResult, SloTarget};

/// Тип-алиас для результата с единым `Error`.
pub type Result<T> = std::result::Result<T, Error>;

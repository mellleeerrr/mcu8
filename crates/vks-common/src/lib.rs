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
pub mod codec_spec;
pub mod config;
pub mod error;
pub mod ids;
pub mod layout_defaults;
pub mod metrics;
#[cfg(unix)]
pub mod net_tuning;
pub mod ratelimit;
pub mod text;
pub mod tracing;
pub mod uri;

// Удобные переэкспорты (часто используемые).
pub use config::Config;
pub use error::{is_terminal_failure, strip_terminal_failure, terminal_failure, Error, ErrorCode, HttpStatus};
pub use ratelimit::{RateLimiter, ADMIN_PER_MIN, WS_FRAME_MAX_BYTES};
pub use text::{sanitize_label, sanitize_label_or, LABEL_MAX_CHARS};
pub use uri::canonical_uri;

/// Тип-алиас для результата с единым `Error`.
pub type Result<T> = std::result::Result<T, Error>;

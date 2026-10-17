//! Настройка `tracing` / `tracing-subscriber`.
//!
//! Основание: `docs/PROJECT_RULES.md` §19.4, `docs/rules/BACKEND_RULES.md` §6.3.
//!
//! Правила:
//! - Production: уровень `info`, structured fields (JSON).
//! - НЕ логируем: SDP, media payload, JWT (только `jti`), пароли, API-ключи (только префикс).

use tracing_subscriber::{layer::SubscriberExt, prelude::*, EnvFilter};

/// Формат вывода логов.
#[derive(Debug, Clone, Copy)]
pub enum LogFormat {
    /// Человекочитаемый (dev).
    Human,
    /// JSON (production, лог-агрегаторы).
    Json,
}

/// Инициализирует глобальный tracing-subscriber.
///
/// `filter` — EnvFilter-строка (например, `"vks=debug,info"`). Если `None` —
/// используется `RUST_LOG` из окружения, иначе `info`.
///
/// Возвращает `true`, если subscriber установлен; `false` — если уже есть.
pub fn init(filter: Option<&str>, format: LogFormat) -> bool {
    let env = filter
        .map(EnvFilter::new)
        .unwrap_or_else(|| EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")));

    let registry = tracing_subscriber::registry().with(env);

    match format {
        LogFormat::Human => registry
            .with(tracing_subscriber::fmt::layer().with_target(true).with_file(true).with_line_number(true))
            .try_init()
            .is_ok(),
        // `.json()` — JSON-формат событий (feature `json` в workspace-зависимости).
        LogFormat::Json => registry.with(tracing_subscriber::fmt::layer().json()).try_init().is_ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // subscriber можно установить один раз на процесс — проверяем, что путь
    // не падает и возвращает bool (повторный вызов — false, без паники).
    #[test]
    fn init_returns_bool() {
        let _ = init(Some("info"), LogFormat::Human);
        let _again = init(Some("info"), LogFormat::Human);
    }
}

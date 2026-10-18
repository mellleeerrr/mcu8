//! Ошибки `vks-storage`.
//!
//! Crate самодостаточен (порт `Storage` живёт в `vks-core`); поэтому здесь
//! собственный `Error` + `Result`, а не `vks_common::Error`.

use thiserror::Error;

/// Тип-алиас результата с единым `Error` хранилища.
pub type Result<T> = std::result::Result<T, Error>;

/// Ошибка хранилища.
#[derive(Debug, Error)]
pub enum Error {
    /// Объект не найден.
    #[error("not found: {0}")]
    NotFound(String),
    /// Некорректные данные.
    #[error("invalid data: {0}")]
    InvalidData(String),
    /// Конфликт (дубликат ключа/имени, нарушение целостности).
    #[error("conflict: {0}")]
    Conflict(String),
    /// Ошибка драйвера sqlx (подключение, запрос, миграция).
    #[error("storage error: {0}")]
    Sqlx(#[from] sqlx::Error),
    /// Ошибка миграции sqlx.
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    /// Ошибка serde (JSON-колонки).
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
}

impl From<String> for Error {
    fn from(s: String) -> Self {
        Error::InvalidData(s)
    }
}

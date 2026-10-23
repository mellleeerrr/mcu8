//! Типизированные строки хранилища (users/rooms/keys/audit).
//!
//! Crate пока не зависит от `vks-core` (порт `Storage` строится параллельно
//! в отдельном крейте), поэтому свои доменные типы живут здесь. Когда
//! `vks-core` будет готов, это модуль заменяется маппингом на типы ядра.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Тип комнаты: ре-экспорт канонического типа из `vks-core` (M0-M1).
pub use vks_core::RoomMode;

/// Пользователь (PII: имя/email — GDPR 152-ФЗ, см. BACKEND_RULES §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct User {
    /// Уникальный идентификатор.
    pub id: Uuid,
    /// Логин.
    pub login: String,
    /// Email (PII).
    pub email: String,
    /// Момент создания.
    pub created_at: Uuid,
}

/// Комната.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Room {
    /// Уникальный идентификатор.
    pub id: Uuid,
    /// Имя (уникально).
    pub name: String,
    /// Режим комнаты.
    pub mode: RoomMode,
    /// Момент создания.
    pub created_at: Uuid,
}

/// API-ключ (хранится только hash + prefix, см. CODE_STYLE §3: не логировать key).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApiKey {
    /// Уникальный идентификатор.
    pub id: Uuid,
    /// Идентификатор владельца (пользователь).
    pub user_id: Uuid,
    /// Хэш значения ключа.
    pub hashed_value: String,
    /// Префикс для логирования/отладки.
    pub prefix: String,
    /// Момент создания.
    pub created_at: Uuid,
}

/// Запись журнала аудита (append-only, retention 2y — BACKEND_RULES §5.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Уникальный идентификатор.
    pub id: Uuid,
    /// Момент события.
    pub created_at: Uuid,
    /// Тип события (напр. `room.create`).
    pub event: String,
    /// Идентификатор субъекта.
    pub subject_id: Option<Uuid>,
    /// JSON-payload.
    pub payload: Option<String>,
}

/// Текущий `Uuid` как timestamp (time-ordered UUIDv7).
#[must_use]
pub fn now() -> Uuid {
    Uuid::now_v7()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_mode_roundtrip() {
        assert_eq!(RoomMode::Sfu.to_string(), "sfu");
        assert_eq!("mcu".parse::<RoomMode>().unwrap(), RoomMode::Mcu);
        assert!("x".parse::<RoomMode>().is_err());
    }
}

//! Типизированные строки хранилища (users/rooms/layouts/keys/audit).
//!
//! Crate пока не зависит от `vks-core` (порт `Storage` строится параллельно
//! в отдельном крейте), поэтому свои доменные типы живут здесь. Когда
//! `vks-core` будет готов, это модуль заменяется маппингом на типы ядра.

use serde::{Deserialize, Serialize};
use std::str::FromStr;
use uuid::Uuid;

/// Тип комнаты: `sfu` (SFU-forwarding) или `mcu` (MCU-mixing).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RoomMode {
    /// SFU: peer-to-peer forwarding без перекодирования.
    Sfu,
    /// MCU: смешивание потоков на сервере.
    Mcu,
}

impl RoomMode {
    /// Строковое представление (значение в БД).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Sfu => "sfu",
            Self::Mcu => "mcu",
        }
    }
}

impl std::fmt::Display for RoomMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for RoomMode {
    type Err = crate::Error;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "sfu" => Ok(Self::Sfu),
            "mcu" => Ok(Self::Mcu),
            other => Err(crate::Error::Conflict(format!("unknown room mode: {other}"))),
        }
    }
}

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

/// Раскладка (layout) комнаты: канвас + прямоугольники тайлов.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Layout {
    /// Уникальный идентификатор.
    pub id: Uuid,
    /// Идентификатор комнаты.
    pub room_id: Uuid,
    /// Ширина канваса, px.
    pub width: i32,
    /// Высота канваса, px.
    pub height: i32,
    /// Прямоугольники тайлов (x, y, w, h).
    pub tiles: Vec<Tile>,
}

/// Прямоугольник тайла в layout.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Tile {
    /// X, px.
    pub x: i32,
    /// Y, px.
    pub y: i32,
    /// Ширина, px.
    pub width: i32,
    /// Высота, px.
    pub height: i32,
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

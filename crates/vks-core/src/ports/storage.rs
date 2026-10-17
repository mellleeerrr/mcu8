// Порт Storage — persistence (memory / sqlite / postgres).
//
// Ядро определяет интерфейс CRUD для Room/Participant/Layout/ApiKey.
// Реализации: `vks-storage` (memory, sqlite, postgres).
//
// Ядро не зависит от конкретной БД (ADR-0008).

use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::participant::{Participant, ParticipantId, Role, UserId};
use crate::room::{Room, RoomId};

/// Ошибка Storage.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum StorageError {
    /// Сущность не найдена.
    #[error("not found: {0}")]
    NotFound(String),
    /// Сущность уже существует.
    #[error("already exists: {0}")]
    AlreadyExists(String),
    /// Сущность уничтожена/неактивна.
    #[error("inactive: {0}")]
    Inactive(String),
    /// Ошибка реализации.
    #[error("storage error: {0}")]
    Other(String),
}

/// Раскладка (layout) — упрощённый доменный вид.
///
/// Полная геометрия (slots, rects) живёт в `vks-layout`. Здесь —
/// только метаданные, которые ядро использует для CRUD.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Layout {
    /// Идентикатор layout.
    pub id: String,
    /// Название.
    pub name: String,
    /// Version (инкремент при изменении).
    pub version: u32,
    /// Идентикаторы rooms, для которых назначен (BTreeMap = deterministic).
    pub assigned_rooms: BTreeMap<RoomId, bool>,
    /// Является ли раскладкой по умолчанию.
    pub is_default: bool,
}

/// API-ключ для управления (R7).
///
/// Формат: `vks_key_<base62(32)>`. Хранится как SHA-256 (ADR-0009).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiKey {
    /// Идентикатор ключа.
    pub id: String,
    /// Scopes (например, ["read", "write", "admin"]).
    pub scopes: Vec<String>,
    /// Владелец (user id).
    pub owner: UserId,
    /// Отозван ли ключ.
    pub revoked: bool,
}

/// Порт Storage — CRUD для Room/Participant/Layout/ApiKey.
///
/// Ядро использует этот порт для persistence; конкретные реализации —
/// в `vks-storage` (memory/sqlite/postgres).
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // --- Room CRUD ---

    /// Сохраняет комнату (create или update).
    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError>;

    /// Загружает комнату по id.
    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError>;

    /// Удаляет комнату по id.
    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError>;

    /// Список всех комнат.
    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError>;

    // --- Room Mode Overrides ---

    /// Возвращает все per-room overrides режима (room_id -> mode).
    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, crate::room::RoomMode>, StorageError> {
        Ok(std::collections::HashMap::new())
    }

    /// Сохраняет или обновляет per-room override режима.
    async fn set_room_mode_override(&self, _room_id: RoomId, _mode: crate::room::RoomMode) -> Result<(), StorageError> {
        Ok(())
    }

    /// Удаляет per-room override режима.
    async fn delete_room_mode_override(&self, _room_id: RoomId) -> Result<(), StorageError> {
        Ok(())
    }

    // --- Participant CRUD ---

    /// Сохраняет участника (create или update).
    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError>;

    /// Загружает участника по id.
    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError>;

    /// Удаляет участника по id.
    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError>;

    /// Список участников (all rooms).
    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError>;

    // --- Layout CRUD ---

    /// Сохраняет раскладку.
    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError>;

    /// Загружает раскладку по id.
    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError>;

    /// Удаляет раскладку по id.
    async fn delete_layout(&self, id: &str) -> Result<(), StorageError>;

    /// Список всех раскладок.
    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError>;

    // --- ApiKey CRUD ---

    /// Сохраняет API-ключ.
    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError>;

    /// Загружает API-ключ по id.
    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError>;

    /// Удаляет API-ключ по id.
    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError>;

    /// Список всех API-ключей.
    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError>;

    // --- Helpers ---

    /// Проверка живости хранилища (readiness `/readyz`, BACKEND_RULES §4.3).
    ///
    /// Дефолт — OK: in-memory реализации всегда готовы; SQL-адаптеры
    /// переопределяют реальным запросом (`SELECT 1`).
    async fn ping(&self) -> Result<(), StorageError> {
        Ok(())
    }

    /// Сбрасывает всё (для тестов).
    async fn reset(&self) -> Result<(), StorageError>;
}

/// Утилита: создаёт participant из role + name (для тестов).
#[must_use]
pub fn make_participant(user_id: UserId, name: &str, role: Role) -> Participant {
    Participant::new(user_id, name, role)
}

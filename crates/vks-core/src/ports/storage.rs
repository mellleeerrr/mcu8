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

/// Запись пользователя для persistence (ADR-0022).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredUser {
    /// Идентификатор пользователя.
    pub id: UserId,
    /// Имя пользователя (username).
    pub username: String,
    /// Роль пользователя (ADR-0022).
    pub role: Role,
    /// Отключен ли микрофон административно.
    pub muted: bool,
    /// Помечен ли на удаление (soft delete).
    pub deleted: bool,
}

impl From<crate::ports::auth::User> for StoredUser {
    fn from(u: crate::ports::auth::User) -> Self {
        Self { id: u.id, username: u.username, role: u.role, muted: false, deleted: false }
    }
}

/// Запись аудита для persistence (ADR-0019).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditEntry {
    /// Время (ms epoch).
    pub ts_ms: u64,
    /// Пользователь (username или id).
    pub user: String,
    /// Действие (например "room.created").
    pub action: String,
    /// Комната (Option: None = глобальное).
    pub room: Option<String>,
    /// IP клиента.
    pub ip: String,
}

/// Запись учетных данных пользователя для persistence (M1.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserCredentials {
    /// Идентификатор пользователя.
    pub user_id: UserId,
    /// Хеш пароля (hex).
    pub password_hash_hex: String,
    /// Соль пароля (hex).
    pub salt_hex: String,
    /// Время последней смены пароля (unix seconds epoch).
    pub password_changed_at: Option<i64>,
    /// Монотонный счетчик поколения токенов пользователя.
    pub generation: u64,
}

/// Метаданные завершенной видео-записи для persistence (M5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordEntry {
    /// Идентификатор записи.
    pub id: String,
    /// Комната, к которой относится запись.
    pub room_id: RoomId,
    /// Время создания, ms epoch.
    pub created_ms: u64,
    /// Длительность, сек.
    pub duration_sec: u32,
    /// Размер, байты.
    pub size_bytes: u64,
    /// Целевой битрейт записи, kbps (0 = неизвестен).
    pub bitrate_kbps: u32,
    /// Ширина кадра (0 = неизвестна).
    pub width: u32,
    /// Высота кадра (0 = неизвестна).
    pub height: u32,
}

/// Порт Storage — CRUD для Room/Participant/Layout/ApiKey/User/Audit.
///
/// Ядро использует этот порт для persistence; конкретные реализации —
/// в `vks-storage` (memory/sqlite/postgres).
#[async_trait]
pub trait Storage: Send + Sync + 'static {
    // --- User CRUD ---

    /// Сохраняет или обновляет пользователя.
    async fn upsert_user(&self, _user: &StoredUser) -> Result<(), StorageError> {
        Ok(())
    }

    /// Загружает пользователя по id.
    async fn get_user(&self, _id: UserId) -> Result<Option<StoredUser>, StorageError> {
        Ok(None)
    }

    /// Удаляет пользователя по id.
    async fn delete_user(&self, _id: UserId) -> Result<(), StorageError> {
        Ok(())
    }

    /// Список всех пользователей.
    async fn list_users(&self) -> Result<Vec<StoredUser>, StorageError> {
        Ok(Vec::new())
    }

    // --- Audit Log ---

    /// Добавляет запись аудита (append-only).
    async fn append_audit(&self, _entry: &AuditEntry) -> Result<(), StorageError> {
        Ok(())
    }

    /// Список всех записей аудита.
    async fn list_audit(&self) -> Result<Vec<AuditEntry>, StorageError> {
        Ok(Vec::new())
    }

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
    async fn list_room_mode_overrides(
        &self,
    ) -> Result<std::collections::HashMap<RoomId, crate::room::RoomMode>, StorageError> {
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

    // --- User Credentials (M1.3) ---

    /// Сохраняет или обновляет учетные данные пользователя.
    async fn upsert_user_credentials(&self, _creds: &UserCredentials) -> Result<(), StorageError> {
        Ok(())
    }

    /// Загружает учетные данные пользователя по user_id.
    async fn get_user_credentials(&self, _user_id: UserId) -> Result<Option<UserCredentials>, StorageError> {
        Ok(None)
    }

    /// Список всех учетных данных пользователей.
    async fn list_user_credentials(&self) -> Result<Vec<UserCredentials>, StorageError> {
        Ok(Vec::new())
    }

    /// Удаляет учетные данные пользователя (при удалении логина в справочнике).
    async fn delete_user_credentials(&self, _user_id: UserId) -> Result<(), StorageError> {
        Ok(())
    }

    // --- Server Settings (M1.5) ---

    /// Atomically replace a versioned application configuration document.
    async fn put_configuration_document(&self, _key: &str, _document: &str) -> Result<(), StorageError> {
        Err(StorageError::Other("Configuration document persistence is unavailable".into()))
    }

    /// Read a configuration document. Runtime calls must never be restored from it.
    async fn get_configuration_document(&self, _key: &str) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    /// Сохраняет или обновляет настройки сервера (JSON).
    async fn upsert_settings(&self, _settings_json: &str) -> Result<(), StorageError> {
        Ok(())
    }

    /// Загружает настройки сервера (JSON).
    async fn get_settings(&self) -> Result<Option<String>, StorageError> {
        Ok(None)
    }

    // --- Records (M5.5) ---

    /// Сохраняет или обновляет запись.
    async fn upsert_record(&self, record: &RecordEntry) -> Result<(), StorageError>;

    /// Загружает запись по id.
    async fn get_record(&self, id: &str) -> Result<Option<RecordEntry>, StorageError>;

    /// Список всех записей.
    async fn list_records(&self, room_id: Option<RoomId>) -> Result<Vec<RecordEntry>, StorageError>;

    /// Удаляет запись по id.
    async fn delete_record(&self, id: &str) -> Result<(), StorageError>;

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

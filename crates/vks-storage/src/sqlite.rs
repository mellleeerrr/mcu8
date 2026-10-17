//! SQLite backend (single-node, ADR-0008) на `sqlx::SqlitePool` (runtime API).
//!
//! Инициализация запускает migrations из `migrations/` (`sqlx::migrate!`).
//! CRUD: users, rooms (+ room_mode_override), layouts, api_keys, audit
//! (append-only).

use std::str::FromStr;
use std::time::Duration;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::error::Result;
use crate::rows::{now, ApiKey, AuditEntry, Layout, Room, RoomMode, Tile, User};

const POOL_SIZE: u32 = 50;
/// Timeout запроса (BACKEND_RULES §5.1).
#[allow(dead_code)] // sqlite pool: acquire_timeout (см. postgres)
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// SQLite-хранилище (single-node).
pub struct SqliteStorage {
    pool: SqlitePool,
    core_participants: std::sync::RwLock<
        std::collections::HashMap<vks_core::participant::ParticipantId, vks_core::participant::Participant>,
    >,
    core_layouts: std::sync::RwLock<std::collections::HashMap<String, vks_core::ports::storage::Layout>>,
    core_api_keys: std::sync::RwLock<std::collections::HashMap<String, vks_core::ports::storage::ApiKey>>,
}

/// Строка users (raw, для `FromRow`).
#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    login: String,
    email: String,
    created_at: Uuid,
}

/// Строка rooms (raw, для `FromRow`).
#[derive(sqlx::FromRow)]
struct RoomRow {
    id: Uuid,
    name: String,
    mode: String,
    created_at: Uuid,
}

/// Строка room_mode_override (raw, для `FromRow`).
#[derive(sqlx::FromRow)]
#[allow(dead_code)] // room_id приходит из DB, но читается только mode
struct RoomModeOverrideRow {
    room_id: Uuid,
    mode: String,
}

/// Строка layouts (raw, для `FromRow`).
#[derive(sqlx::FromRow)]
struct LayoutRow {
    id: Uuid,
    room_id: Uuid,
    width: i32,
    height: i32,
    tiles: String,
}

/// Строка api_keys (raw, для `FromRow`).
#[derive(sqlx::FromRow)]
struct ApiKeyRow {
    id: Uuid,
    user_id: Uuid,
    hashed_value: String,
    prefix: String,
    created_at: Uuid,
}

/// Строка audit_log (raw, для `FromRow`).
#[derive(sqlx::FromRow)]
struct AuditRow {
    id: Uuid,
    created_at: Uuid,
    event: String,
    subject_id: Option<Uuid>,
    payload: Option<String>,
}

impl SqliteStorage {
    pub(crate) fn participants_map(
        &self,
    ) -> &std::sync::RwLock<
        std::collections::HashMap<vks_core::participant::ParticipantId, vks_core::participant::Participant>,
    > {
        &self.core_participants
    }

    pub(crate) fn layouts_map(
        &self,
    ) -> &std::sync::RwLock<std::collections::HashMap<String, vks_core::ports::storage::Layout>> {
        &self.core_layouts
    }

    pub(crate) fn api_keys_map(
        &self,
    ) -> &std::sync::RwLock<std::collections::HashMap<String, vks_core::ports::storage::ApiKey>> {
        &self.core_api_keys
    }

    /// Создаёт пул по `SqliteConnectOptions` и применяет migrations.
    pub async fn init_options(options: SqliteConnectOptions) -> Result<Self> {
        let pool = SqlitePoolOptions::new().max_connections(POOL_SIZE).connect_with(options).await?;
        let storage = Self {
            pool,
            core_participants: std::sync::RwLock::new(std::collections::HashMap::new()),
            core_layouts: std::sync::RwLock::new(std::collections::HashMap::new()),
            core_api_keys: std::sync::RwLock::new(std::collections::HashMap::new()),
        };
        storage.migrate().await?;
        Ok(storage)
    }

    /// Создаёт пул по URL (напр. `sqlite::memory:?cache=shared`) + migrations.
    pub async fn init(url: &str) -> Result<Self> {
        Self::init_options(SqliteConnectOptions::from_str(url)?).await
    }

    /// Применяет migrations из `migrations/`.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!().run(&self.pool).await?;
        Ok(())
    }

    /// Пул (для тестов/инспекции).
    #[must_use]
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // --- users ---

    /// Вставляет или обновляет пользователя (`ON CONFLICT` → update).
    pub async fn insert_user(&self, user: &User) -> Result<()> {
        sqlx::query(
            "INSERT INTO users (id, login, email, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET login = $2, email = $3, created_at = $4",
        )
        .bind(user.id)
        .bind(&user.login)
        .bind(&user.email)
        .bind(user.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Возвращает пользователя по `id`.
    pub async fn get_user(&self, id: Uuid) -> Result<User> {
        let row: Option<UserRow> = sqlx::query_as("SELECT id, login, email, created_at FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.map(|r| User { id: r.id, login: r.login, email: r.email, created_at: r.created_at })
            .ok_or_else(|| crate::Error::NotFound(format!("user {id}")))
    }

    /// Список всех пользователей.
    pub async fn list_users(&self) -> Result<Vec<User>> {
        let rows: Vec<UserRow> =
            sqlx::query_as("SELECT id, login, email, created_at FROM users").fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| User { id: r.id, login: r.login, email: r.email, created_at: r.created_at })
            .collect())
    }

    /// Удаляет пользователя.
    pub async fn delete_user(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!("user {id}")));
        }
        Ok(())
    }

    // --- rooms ---

    /// Создаёт комнату (UNIQUE по `name` → Conflict при дубликате).
    pub async fn insert_room(&self, id: Uuid, name: &str, mode: RoomMode) -> Result<Room> {
        let created_at = now();
        sqlx::query("INSERT INTO rooms (id, name, mode, created_at) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(name)
            .bind(mode.as_str())
            .bind(created_at)
            .execute(&self.pool)
            .await?;
        Ok(Room { id, name: name.to_owned(), mode, created_at })
    }

    /// Возвращает комнату по `id`.
    pub async fn get_room(&self, id: Uuid) -> Result<Room> {
        let row: Option<RoomRow> = sqlx::query_as("SELECT id, name, mode, created_at FROM rooms WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        let row = row.ok_or_else(|| crate::Error::NotFound(format!("room {id}")))?;
        Ok(Room { id: row.id, name: row.name, mode: RoomMode::from_str(&row.mode)?, created_at: row.created_at })
    }

    /// Список всех комнат.
    pub async fn list_rooms(&self) -> Result<Vec<Room>> {
        let rows: Vec<RoomRow> =
            sqlx::query_as("SELECT id, name, mode, created_at FROM rooms").fetch_all(&self.pool).await?;
        let mut rooms = Vec::with_capacity(rows.len());
        for r in rows {
            rooms.push(Room { id: r.id, name: r.name, mode: RoomMode::from_str(&r.mode)?, created_at: r.created_at });
        }
        Ok(rooms)
    }

    /// Переопределяет режим комнаты (`room_mode_override`, upsert).
    pub async fn set_room_mode_override(&self, room_id: Uuid, mode: RoomMode) -> Result<()> {
        sqlx::query(
            "INSERT INTO room_mode_override (room_id, mode) VALUES ($1, $2)
             ON CONFLICT (room_id) DO UPDATE SET mode = $2",
        )
        .bind(room_id)
        .bind(mode.as_str())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Возвращает override-режим комнаты.
    pub async fn effective_room_mode(&self, room_id: Uuid) -> Result<RoomMode> {
        let row: Option<RoomModeOverrideRow> =
            sqlx::query_as("SELECT room_id, mode FROM room_mode_override WHERE room_id = $1")
                .bind(room_id)
                .fetch_optional(&self.pool)
                .await?;
        let row = row.ok_or_else(|| crate::Error::NotFound(format!("room {room_id}")))?;
        RoomMode::from_str(&row.mode)
    }

    /// Удаляет комнату.
    pub async fn delete_room(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM rooms WHERE id = $1").bind(id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!("room {id}")));
        }
        Ok(())
    }

    // --- layouts ---

    /// Вставляет или обновляет layout.
    pub async fn insert_layout(&self, layout: &Layout) -> Result<()> {
        sqlx::query(
            "INSERT INTO layouts (id, room_id, width, height, tiles)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET room_id = $2, width = $3, height = $4, tiles = $5",
        )
        .bind(layout.id)
        .bind(layout.room_id)
        .bind(layout.width)
        .bind(layout.height)
        .bind(serde_json::to_string(&layout.tiles)?)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Возвращает layout по `id`.
    pub async fn get_layout(&self, id: Uuid) -> Result<Layout> {
        let row: Option<LayoutRow> =
            sqlx::query_as("SELECT id, room_id, width, height, tiles FROM layouts WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let row = row.ok_or_else(|| crate::Error::NotFound(format!("layout {id}")))?;
        Ok(Self::layout_from_row(row))
    }

    /// Layouts для комнаты.
    pub async fn list_layouts(&self, room_id: Uuid) -> Result<Vec<Layout>> {
        let rows: Vec<LayoutRow> =
            sqlx::query_as("SELECT id, room_id, width, height, tiles FROM layouts WHERE room_id = $1")
                .bind(room_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().map(Self::layout_from_row).collect())
    }

    fn layout_from_row(row: LayoutRow) -> Layout {
        let tiles: Vec<Tile> = serde_json::from_str(&row.tiles).unwrap_or_default();
        Layout { id: row.id, room_id: row.room_id, width: row.width, height: row.height, tiles }
    }

    // --- api_keys ---

    /// Вставляет или обновляет API-ключ.
    pub async fn insert_api_key(&self, key: &ApiKey) -> Result<()> {
        sqlx::query(
            "INSERT INTO api_keys (id, user_id, hashed_value, prefix, created_at)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET user_id = $2, hashed_value = $3, prefix = $4, created_at = $5",
        )
        .bind(key.id)
        .bind(key.user_id)
        .bind(&key.hashed_value)
        .bind(&key.prefix)
        .bind(key.created_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Возвращает API-ключ по `id`.
    pub async fn get_api_key(&self, id: Uuid) -> Result<ApiKey> {
        let row: Option<ApiKeyRow> =
            sqlx::query_as("SELECT id, user_id, hashed_value, prefix, created_at FROM api_keys WHERE id = $1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?;
        let row = row.ok_or_else(|| crate::Error::NotFound(format!("api_key {id}")))?;
        Ok(ApiKey {
            id: row.id,
            user_id: row.user_id,
            hashed_value: row.hashed_value,
            prefix: row.prefix,
            created_at: row.created_at,
        })
    }

    /// API-ключи пользователя.
    pub async fn list_api_keys(&self, user_id: Uuid) -> Result<Vec<ApiKey>> {
        let rows: Vec<ApiKeyRow> =
            sqlx::query_as("SELECT id, user_id, hashed_value, prefix, created_at FROM api_keys WHERE user_id = $1")
                .bind(user_id)
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|r| ApiKey {
                id: r.id,
                user_id: r.user_id,
                hashed_value: r.hashed_value,
                prefix: r.prefix,
                created_at: r.created_at,
            })
            .collect())
    }

    /// Удаляет API-ключ.
    pub async fn delete_api_key(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM api_keys WHERE id = $1").bind(id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!("api_key {id}")));
        }
        Ok(())
    }

    // --- audit (append-only) ---

    /// Добавляет запись аудита (append-only).
    pub async fn append_audit(&self, entry: &AuditEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO audit_log (id, created_at, event, subject_id, payload)
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(entry.id)
        .bind(entry.created_at)
        .bind(&entry.event)
        .bind(entry.subject_id)
        .bind(&entry.payload)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Весь журнал аудита (по времени).
    pub async fn list_audit(&self) -> Result<Vec<AuditEntry>> {
        let rows: Vec<AuditRow> =
            sqlx::query_as("SELECT id, created_at, event, subject_id, payload FROM audit_log ORDER BY created_at")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows
            .into_iter()
            .map(|r| AuditEntry {
                id: r.id,
                created_at: r.created_at,
                event: r.event,
                subject_id: r.subject_id,
                payload: r.payload,
            })
            .collect())
    }

    /// Записывает системное событие аудита.
    pub async fn log_audit(&self, event: &str, subject_id: Option<Uuid>, payload: Option<String>) -> Result<()> {
        let id = now();
        self.append_audit(&AuditEntry { id, created_at: id, event: event.to_owned(), subject_id, payload }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_storage() -> SqliteStorage {
        // Shared in-memory sqlite (одна БД на процесс) — migrations идемпотентны.
        SqliteStorage::init("sqlite::memory:?cache=shared").await.unwrap()
    }

    fn tile() -> Tile {
        Tile { x: 1, y: 2, width: 3, height: 4 }
    }

    #[tokio::test]
    async fn user_roundtrip() {
        let storage = test_storage().await;
        let user = User {
            id: Uuid::new_v4(),
            login: format!("user{}", Uuid::new_v4().simple()),
            email: "x@example.com".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_user(&user).await.unwrap();
        let got = storage.get_user(user.id).await.unwrap();
        assert_eq!(got, user);
        assert!(storage.list_users().await.unwrap().iter().any(|u| u.id == user.id));
        storage.delete_user(user.id).await.unwrap();
        assert!(storage.get_user(user.id).await.is_err());
    }

    #[tokio::test]
    async fn room_roundtrip_and_conflict() {
        let storage = test_storage().await;
        let id = Uuid::new_v4();
        let name = format!("room{}", Uuid::new_v4().simple());
        let room = storage.insert_room(id, &name, RoomMode::Sfu).await.unwrap();
        assert_eq!(room, storage.get_room(id).await.unwrap());
        // Duplicate name → conflict.
        assert!(storage.insert_room(Uuid::new_v4(), &name, RoomMode::Mcu).await.is_err());
        storage.set_room_mode_override(id, RoomMode::Mcu).await.unwrap();
        assert_eq!(storage.effective_room_mode(id).await.unwrap(), RoomMode::Mcu);
        assert!(storage.effective_room_mode(Uuid::new_v4()).await.is_err());
        assert!(storage.list_rooms().await.unwrap().iter().any(|r| r.id == id));
        storage.delete_room(id).await.unwrap();
        assert!(storage.get_room(id).await.is_err());
    }

    #[tokio::test]
    async fn layout_roundtrip() {
        let storage = test_storage().await;
        let room_id = Uuid::new_v4();
        storage.insert_room(room_id, &format!("l{}", Uuid::new_v4().simple()), RoomMode::Sfu).await.unwrap();
        let layout = Layout { id: Uuid::new_v4(), room_id, width: 1920, height: 1080, tiles: vec![tile()] };
        storage.insert_layout(&layout).await.unwrap();
        assert_eq!(storage.get_layout(layout.id).await.unwrap(), layout);
        let listed = storage.list_layouts(room_id).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0], layout);
    }

    #[tokio::test]
    async fn api_key_roundtrip() {
        let storage = test_storage().await;
        let user = User {
            id: Uuid::new_v4(),
            login: format!("k{}", Uuid::new_v4().simple()),
            email: "k@example.com".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_user(&user).await.unwrap();
        let key = ApiKey {
            id: Uuid::new_v4(),
            user_id: user.id,
            hashed_value: "sha256:abc".to_owned(),
            prefix: "vks_".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_api_key(&key).await.unwrap();
        assert_eq!(storage.get_api_key(key.id).await.unwrap(), key);
        assert_eq!(storage.list_api_keys(user.id).await.unwrap().len(), 1);
        storage.delete_api_key(key.id).await.unwrap();
        assert!(storage.get_api_key(key.id).await.is_err());
    }

    #[tokio::test]
    async fn audit_append_only() {
        let storage = test_storage().await;
        let before = storage.list_audit().await.unwrap().len();
        storage.log_audit("room.create", Some(Uuid::new_v4()), Some("{}".to_owned())).await.unwrap();
        let after = storage.list_audit().await.unwrap();
        assert_eq!(after.len(), before + 1);
    }

    #[tokio::test]
    async fn file_based_init() {
        // init() по URL с файлом + migrations.
        let dir = std::env::temp_dir().join(format!("vks-storage-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let url = format!("sqlite://{}?mode=rwc", dir.join("test.db").to_str().unwrap());
        let storage = SqliteStorage::init(&url).await.unwrap();
        let user = User {
            id: Uuid::new_v4(),
            login: format!("f{}", Uuid::new_v4().simple()),
            email: "f@example.com".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_user(&user).await.unwrap();
        assert_eq!(storage.get_user(user.id).await.unwrap(), user);
        // Cleanup.
        storage.pool().close().await;
        let _ = std::fs::remove_dir_all(&dir);
    }
}

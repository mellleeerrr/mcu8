//! PostgreSQL backend (cluster/prod, ADR-0008) на `sqlx::PostgresPool` (runtime API).
//!
//! Зеркало SQLite-бэкенда. Настройки пула — BACKEND_RULES §5.1:
//! pool 50, idle 10min, statement_timeout 30s. Credentials — env vars
//! (например `VKS_STORAGE_PG_URL` / `PGUSER`/`PGPASSWORD`).

use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::{PgConnectOptions, PgPool, PgPoolOptions};
use sqlx::Executor;
use uuid::Uuid;

use crate::error::Result;
use crate::rows::{now, ApiKey, AuditEntry, Layout, Room, RoomMode, Tile, User};

const POOL_SIZE: u32 = 50;
/// Idle timeout (BACKEND_RULES §5.1: 10min).
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Statement timeout (BACKEND_RULES §5.1: 30s).
const STATEMENT_TIMEOUT: Duration = Duration::from_secs(30);

/// PostgreSQL-хранилище (cluster/prod).
pub struct PostgresStorage {
    pool: PgPool,
    core_participants: std::sync::RwLock<
        std::collections::HashMap<vks_core::participant::ParticipantId, vks_core::participant::Participant>,
    >,
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

impl PostgresStorage {
    pub(crate) fn participants_map(
        &self,
    ) -> &std::sync::RwLock<
        std::collections::HashMap<vks_core::participant::ParticipantId, vks_core::participant::Participant>,
    > {
        &self.core_participants
    }

    /// Создаёт пул по `PgConnectOptions` и применяет migrations.
    pub async fn init_options(options: PgConnectOptions) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(POOL_SIZE)
            .idle_timeout(Some(IDLE_TIMEOUT))
            .acquire_timeout(STATEMENT_TIMEOUT)
            .connect_with(options)
            .await?;
        // statement_timeout по BACKEND_RULES §5.1 (30s).
        pool.execute("SET statement_timeout = 30000").await?;
        let storage = Self {
            pool,
            core_participants: std::sync::RwLock::new(std::collections::HashMap::new()),
        };
        storage.migrate().await?;
        Ok(storage)
    }

    /// Создаёт пул по URL + migrations (credentials — из URL/env).
    pub async fn init(url: &str) -> Result<Self> {
        Self::init_options(PgConnectOptions::from_str(url)?).await
    }

    /// Применяет migrations из `migrations/`.
    pub async fn migrate(&self) -> Result<()> {
        sqlx::migrate!().run(&self.pool).await?;
        Ok(())
    }

    /// Пул (для тестов/инспекции).
    #[must_use]
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    // --- users ---

    /// Вставляет или обновляет пользователя.
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

    /// Удаляет пользователя (GDPR: erasure).
    pub async fn delete_user(&self, id: Uuid) -> Result<()> {
        let result = sqlx::query("DELETE FROM users WHERE id = $1").bind(id).execute(&self.pool).await?;
        if result.rows_affected() == 0 {
            return Err(crate::Error::NotFound(format!("user {id}")));
        }
        Ok(())
    }

    // --- rooms ---

    /// Создаёт комнату (UNIQUE по `name`).
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

    /// Переопределяет режим комнаты (upsert).
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
        Ok(RoomMode::from_str(&row.mode)?)
    }

    /// Возвращает все per-room overrides режима.
    pub async fn list_room_mode_overrides(&self) -> Result<Vec<(Uuid, RoomMode)>> {
        let rows: Vec<RoomModeOverrideRow> =
            sqlx::query_as("SELECT room_id, mode FROM room_mode_override").fetch_all(&self.pool).await?;
        let mut res = Vec::with_capacity(rows.len());
        for r in rows {
            if let Ok(m) = RoomMode::from_str(&r.mode) {
                res.push((r.room_id, m));
            }
        }
        Ok(res)
    }

    /// Удаляет per-room override режима комнаты.
    pub async fn delete_room_mode_override(&self, room_id: Uuid) -> Result<()> {
        sqlx::query("DELETE FROM room_mode_override WHERE room_id = $1")
            .bind(room_id)
            .execute(&self.pool)
            .await?;
        Ok(())
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

    /// Удаляет API-ключ (GDPR: erasure).
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

    /// Без live-БД: проверяем парсинг URL и конфигурацию пула (compile-only).
    #[tokio::test]
    #[ignore]
    async fn init_roundtrip_live_db() {
        // Запускается только при наличии `VKS_STORAGE_PG_URL` (CI).
        let Some(url) = std::env::var("VKS_STORAGE_PG_URL").ok() else {
            return;
        };
        let storage = PostgresStorage::init(&url).await.unwrap();
        let user = User {
            id: Uuid::new_v4(),
            login: format!("pg{}", Uuid::new_v4().simple()),
            email: "pg@example.com".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_user(&user).await.unwrap();
        assert_eq!(storage.get_user(user.id).await.unwrap(), user);
        let room = storage
            .insert_room(Uuid::new_v4(), &format!("pg{}", Uuid::new_v4().simple()), RoomMode::Mcu)
            .await
            .unwrap();
        assert_eq!(storage.get_room(room.id).await.unwrap(), room);
    }

    #[test]
    fn compile_only_url_parse() {
        // compile-only: URL-парсинг без соединения.
        let options = PgConnectOptions::from_str("postgres://user:pass@localhost:5432/vks");
        assert!(options.is_ok());
    }
}

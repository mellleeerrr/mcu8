//! Адаптер реализаций хранилища под порт `vks_core::ports::Storage`.
//!
//! Связывает конкретные backend-хранилища (`MemoryStorage`, `SqliteStorage`)
//! с доменным трейтом ядра `vks_core::ports::Storage`.

use async_trait::async_trait;

use vks_core::participant::{Participant, ParticipantId};
use vks_core::ports::storage::{ApiKey, Layout, Storage, StorageError};
use vks_core::room::{Room, RoomId, RoomMode as CoreRoomMode};

use crate::memory::MemoryStorage;
use crate::postgres::PostgresStorage;
use crate::sqlite::SqliteStorage;

fn to_storage_err(err: impl std::fmt::Display) -> StorageError {
    StorageError::Other(err.to_string())
}

#[async_trait]
impl Storage for MemoryStorage {
    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError> {
        let id = *room.id.as_uuid();
        let mode = match room.mode {
            CoreRoomMode::Sfu => crate::rows::RoomMode::Sfu,
            CoreRoomMode::Mcu => crate::rows::RoomMode::Mcu,
        };
        let mut rooms = self.rooms_map().write().unwrap();
        let created_at = if let Some(existing) = rooms.get(&id) { existing.created_at } else { crate::rows::now() };
        let r = crate::rows::Room { id, name: room.name.clone(), mode, created_at };
        rooms.insert(id, r);
        Ok(())
    }

    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError> {
        let rooms = self.rooms_map().read().unwrap();
        if let Some(r) = rooms.get(id.as_uuid()) {
            let mode = match r.mode {
                crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
            };
            let created_ms = (r.created_at.as_u128() >> 80) as u64;
            let mut room = Room::new(r.name.clone(), mode);
            room.id = id;
            room.created_ms = created_ms;
            Ok(Some(room))
        } else {
            Ok(None)
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let mut rooms = self.rooms_map().write().unwrap();
        rooms.remove(id.as_uuid());
        self.room_mode_overrides_map().write().unwrap().remove(id.as_uuid());
        Ok(())
    }

    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError> {
        let rooms = self.rooms_map().read().unwrap();
        let res = rooms
            .values()
            .map(|r| {
                let mode = match r.mode {
                    crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                    crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
                };
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name.clone(), mode);
                room.id = RoomId::from_uuid(r.id);
                room.created_ms = created_ms;
                room
            })
            .collect();
        Ok(res)
    }

    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, CoreRoomMode>, StorageError> {
        let overrides = self.room_mode_overrides_map().read().unwrap();
        let mut res = std::collections::HashMap::new();
        for (&uuid, &m) in overrides.iter() {
            let core_m = match m {
                crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
            };
            res.insert(RoomId::from_uuid(uuid), core_m);
        }
        Ok(res)
    }

    async fn set_room_mode_override(&self, room_id: RoomId, mode: CoreRoomMode) -> Result<(), StorageError> {
        let row_m = match mode {
            CoreRoomMode::Sfu => crate::rows::RoomMode::Sfu,
            CoreRoomMode::Mcu => crate::rows::RoomMode::Mcu,
        };
        self.room_mode_overrides_map().write().unwrap().insert(*room_id.as_uuid(), row_m);
        Ok(())
    }

    async fn delete_room_mode_override(&self, room_id: RoomId) -> Result<(), StorageError> {
        self.room_mode_overrides_map().write().unwrap().remove(room_id.as_uuid());
        Ok(())
    }

    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().insert(p.id, p.clone());
        Ok(())
    }

    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError> {
        self.layouts_map().write().unwrap().insert(layout.id.clone(), layout.clone());
        Ok(())
    }

    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError> {
        Ok(self.layouts_map().read().unwrap().get(id).cloned())
    }

    async fn delete_layout(&self, id: &str) -> Result<(), StorageError> {
        self.layouts_map().write().unwrap().remove(id);
        Ok(())
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError> {
        Ok(self.layouts_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError> {
        self.api_keys_map().write().unwrap().insert(key.id.clone(), key.clone());
        Ok(())
    }

    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError> {
        Ok(self.api_keys_map().read().unwrap().get(id).cloned())
    }

    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError> {
        self.api_keys_map().write().unwrap().remove(id);
        Ok(())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError> {
        Ok(self.api_keys_map().read().unwrap().values().cloned().collect())
    }

    async fn reset(&self) -> Result<(), StorageError> {
        self.rooms_map().write().unwrap().clear();
        self.room_mode_overrides_map().write().unwrap().clear();
        self.participants_map().write().unwrap().clear();
        self.layouts_map().write().unwrap().clear();
        self.api_keys_map().write().unwrap().clear();
        Ok(())
    }
}

#[async_trait]
impl Storage for SqliteStorage {
    async fn ping(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(self.pool()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError> {
        let id = *room.id.as_uuid();
        let name = &room.name;
        let mode = match room.mode {
            CoreRoomMode::Sfu => "sfu",
            CoreRoomMode::Mcu => "mcu",
        };
        let created_at = crate::rows::now();

        sqlx::query(
            "INSERT INTO rooms (id, name, mode, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET name = $2, mode = $3",
        )
        .bind(id)
        .bind(name)
        .bind(mode)
        .bind(created_at)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;

        Ok(())
    }

    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError> {
        let uuid = *id.as_uuid();
        match self.get_room(uuid).await {
            Ok(r) => {
                let mode = match r.mode {
                    crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                    crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
                };
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, mode);
                room.id = id;
                room.created_ms = created_ms;
                Ok(Some(room))
            }
            Err(crate::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let uuid = *id.as_uuid();
        let _ = self.delete_room_mode_override(uuid).await;
        match self.delete_room(uuid).await {
            Ok(()) | Err(crate::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError> {
        let rooms = self.list_rooms().await.map_err(to_storage_err)?;
        Ok(rooms
            .into_iter()
            .map(|r| {
                let mode = match r.mode {
                    crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                    crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
                };
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, mode);
                room.id = RoomId::from_uuid(r.id);
                room.created_ms = created_ms;
                room
            })
            .collect())
    }

    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, CoreRoomMode>, StorageError> {
        let list = self.list_room_mode_overrides().await.map_err(to_storage_err)?;
        let mut res = std::collections::HashMap::new();
        for (uuid, m) in list {
            let core_m = match m {
                crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
            };
            res.insert(RoomId::from_uuid(uuid), core_m);
        }
        Ok(res)
    }

    async fn set_room_mode_override(&self, room_id: RoomId, mode: CoreRoomMode) -> Result<(), StorageError> {
        let row_m = match mode {
            CoreRoomMode::Sfu => crate::rows::RoomMode::Sfu,
            CoreRoomMode::Mcu => crate::rows::RoomMode::Mcu,
        };
        self.set_room_mode_override(*room_id.as_uuid(), row_m).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn delete_room_mode_override(&self, room_id: RoomId) -> Result<(), StorageError> {
        self.delete_room_mode_override(*room_id.as_uuid()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().insert(p.id, p.clone());
        Ok(())
    }

    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError> {
        let assigned_rooms_json = serde_json::to_string(&layout.assigned_rooms).map_err(to_storage_err)?;
        sqlx::query(
            "INSERT INTO core_layouts (id, name, version, assigned_rooms, is_default)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET name = $2, version = $3, assigned_rooms = $4, is_default = $5",
        )
        .bind(&layout.id)
        .bind(&layout.name)
        .bind(layout.version as i32)
        .bind(&assigned_rooms_json)
        .bind(layout.is_default)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError> {
        let row: Option<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, name, version, assigned_rooms, is_default)) => {
                let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
                Ok(Some(Layout { id, name, version: version as u32, assigned_rooms, is_default }))
            }
            None => Ok(None),
        }
    }

    async fn delete_layout(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_layouts WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError> {
        let rows: Vec<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, name, version, assigned_rooms, is_default) in rows {
            let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
            res.push(Layout { id, name, version: version as u32, assigned_rooms, is_default });
        }
        Ok(res)
    }

    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError> {
        let scopes_json = serde_json::to_string(&key.scopes).map_err(to_storage_err)?;
        let owner_str = key.owner.to_string();
        sqlx::query(
            "INSERT INTO core_api_keys (id, scopes, owner, revoked)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET scopes = $2, owner = $3, revoked = $4",
        )
        .bind(&key.id)
        .bind(&scopes_json)
        .bind(&owner_str)
        .bind(key.revoked)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError> {
        let row: Option<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, scopes, owner, revoked)) => {
                let scopes = serde_json::from_str(&scopes).unwrap_or_default();
                let owner = match owner.parse::<vks_core::participant::UserId>() {
                    Ok(u) => u,
                    Err(_) => return Ok(None),
                };
                Ok(Some(ApiKey { id, scopes, owner, revoked }))
            }
            None => Ok(None),
        }
    }

    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_api_keys WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError> {
        let rows: Vec<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, scopes, owner, revoked) in rows {
            let scopes = serde_json::from_str(&scopes).unwrap_or_default();
            if let Ok(owner) = owner.parse::<vks_core::participant::UserId>() {
                res.push(ApiKey { id, scopes, owner, revoked });
            }
        }
        Ok(res)
    }

    async fn reset(&self) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM room_mode_override").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM server_settings").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM rooms").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        self.participants_map().write().unwrap().clear();
        Ok(())
    }
}

#[async_trait]
impl Storage for PostgresStorage {
    async fn ping(&self) -> Result<(), StorageError> {
        sqlx::query("SELECT 1").execute(self.pool()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_room(&self, room: &Room) -> Result<(), StorageError> {
        let id = *room.id.as_uuid();
        let name = &room.name;
        let mode = match room.mode {
            CoreRoomMode::Sfu => "sfu",
            CoreRoomMode::Mcu => "mcu",
        };
        let created_at = crate::rows::now();

        sqlx::query(
            "INSERT INTO rooms (id, name, mode, created_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET name = $2, mode = $3",
        )
        .bind(id)
        .bind(name)
        .bind(mode)
        .bind(created_at)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;

        Ok(())
    }

    async fn get_room(&self, id: RoomId) -> Result<Option<Room>, StorageError> {
        let uuid = *id.as_uuid();
        match self.get_room(uuid).await {
            Ok(r) => {
                let mode = match r.mode {
                    crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                    crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
                };
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, mode);
                room.id = id;
                room.created_ms = created_ms;
                Ok(Some(room))
            }
            Err(crate::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let uuid = *id.as_uuid();
        let _ = self.delete_room_mode_override(uuid).await;
        match self.delete_room(uuid).await {
            Ok(()) | Err(crate::Error::NotFound(_)) => Ok(()),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn list_rooms(&self) -> Result<Vec<Room>, StorageError> {
        let rooms = self.list_rooms().await.map_err(to_storage_err)?;
        Ok(rooms
            .into_iter()
            .map(|r| {
                let mode = match r.mode {
                    crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                    crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
                };
                let created_ms = (r.created_at.as_u128() >> 80) as u64;
                let mut room = Room::new(r.name, mode);
                room.id = RoomId::from_uuid(r.id);
                room.created_ms = created_ms;
                room
            })
            .collect())
    }

    async fn list_room_mode_overrides(&self) -> Result<std::collections::HashMap<RoomId, CoreRoomMode>, StorageError> {
        let list = self.list_room_mode_overrides().await.map_err(to_storage_err)?;
        let mut res = std::collections::HashMap::new();
        for (uuid, m) in list {
            let core_m = match m {
                crate::rows::RoomMode::Sfu => CoreRoomMode::Sfu,
                crate::rows::RoomMode::Mcu => CoreRoomMode::Mcu,
            };
            res.insert(RoomId::from_uuid(uuid), core_m);
        }
        Ok(res)
    }

    async fn set_room_mode_override(&self, room_id: RoomId, mode: CoreRoomMode) -> Result<(), StorageError> {
        let row_m = match mode {
            CoreRoomMode::Sfu => crate::rows::RoomMode::Sfu,
            CoreRoomMode::Mcu => crate::rows::RoomMode::Mcu,
        };
        self.set_room_mode_override(*room_id.as_uuid(), row_m).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn delete_room_mode_override(&self, room_id: RoomId) -> Result<(), StorageError> {
        self.delete_room_mode_override(*room_id.as_uuid()).await.map_err(to_storage_err)?;
        Ok(())
    }

    async fn upsert_participant(&self, p: &Participant) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().insert(p.id, p.clone());
        Ok(())
    }

    async fn get_participant(&self, id: ParticipantId) -> Result<Option<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().get(&id).cloned())
    }

    async fn delete_participant(&self, id: ParticipantId) -> Result<(), StorageError> {
        self.participants_map().write().unwrap().remove(&id);
        Ok(())
    }

    async fn list_participants(&self) -> Result<Vec<Participant>, StorageError> {
        Ok(self.participants_map().read().unwrap().values().cloned().collect())
    }

    async fn upsert_layout(&self, layout: &Layout) -> Result<(), StorageError> {
        let assigned_rooms_json = serde_json::to_string(&layout.assigned_rooms).map_err(to_storage_err)?;
        sqlx::query(
            "INSERT INTO core_layouts (id, name, version, assigned_rooms, is_default)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (id) DO UPDATE SET name = $2, version = $3, assigned_rooms = $4, is_default = $5",
        )
        .bind(&layout.id)
        .bind(&layout.name)
        .bind(layout.version as i32)
        .bind(&assigned_rooms_json)
        .bind(layout.is_default)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_layout(&self, id: &str) -> Result<Option<Layout>, StorageError> {
        let row: Option<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, name, version, assigned_rooms, is_default)) => {
                let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
                Ok(Some(Layout { id, name, version: version as u32, assigned_rooms, is_default }))
            }
            None => Ok(None),
        }
    }

    async fn delete_layout(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_layouts WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_layouts(&self) -> Result<Vec<Layout>, StorageError> {
        let rows: Vec<(String, String, i32, String, bool)> =
            sqlx::query_as("SELECT id, name, version, assigned_rooms, is_default FROM core_layouts")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, name, version, assigned_rooms, is_default) in rows {
            let assigned_rooms = serde_json::from_str(&assigned_rooms).unwrap_or_default();
            res.push(Layout { id, name, version: version as u32, assigned_rooms, is_default });
        }
        Ok(res)
    }

    async fn upsert_api_key(&self, key: &ApiKey) -> Result<(), StorageError> {
        let scopes_json = serde_json::to_string(&key.scopes).map_err(to_storage_err)?;
        let owner_str = key.owner.to_string();
        sqlx::query(
            "INSERT INTO core_api_keys (id, scopes, owner, revoked)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (id) DO UPDATE SET scopes = $2, owner = $3, revoked = $4",
        )
        .bind(&key.id)
        .bind(&scopes_json)
        .bind(&owner_str)
        .bind(key.revoked)
        .execute(self.pool())
        .await
        .map_err(to_storage_err)?;
        Ok(())
    }

    async fn get_api_key(&self, id: &str) -> Result<Option<ApiKey>, StorageError> {
        let row: Option<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys WHERE id = $1")
                .bind(id)
                .fetch_optional(self.pool())
                .await
                .map_err(to_storage_err)?;

        match row {
            Some((id, scopes, owner, revoked)) => {
                let scopes = serde_json::from_str(&scopes).unwrap_or_default();
                let owner = match owner.parse::<vks_core::participant::UserId>() {
                    Ok(u) => u,
                    Err(_) => return Ok(None),
                };
                Ok(Some(ApiKey { id, scopes, owner, revoked }))
            }
            None => Ok(None),
        }
    }

    async fn delete_api_key(&self, id: &str) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM core_api_keys WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await
            .map_err(to_storage_err)?;
        Ok(())
    }

    async fn list_api_keys(&self) -> Result<Vec<ApiKey>, StorageError> {
        let rows: Vec<(String, String, String, bool)> =
            sqlx::query_as("SELECT id, scopes, owner, revoked FROM core_api_keys")
                .fetch_all(self.pool())
                .await
                .map_err(to_storage_err)?;

        let mut res = Vec::with_capacity(rows.len());
        for (id, scopes, owner, revoked) in rows {
            let scopes = serde_json::from_str(&scopes).unwrap_or_default();
            if let Ok(owner) = owner.parse::<vks_core::participant::UserId>() {
                res.push(ApiKey { id, scopes, owner, revoked });
            }
        }
        Ok(res)
    }

    async fn reset(&self) -> Result<(), StorageError> {
        sqlx::query("DELETE FROM room_mode_override").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM core_api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM server_settings").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM rooms").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        self.participants_map().write().unwrap().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    async fn test_storage_contract(storage: Arc<dyn Storage>) {
        // Rooms
        let room = Room::new("core-room".to_string(), CoreRoomMode::Mcu);
        let room_id = room.id;
        storage.upsert_room(&room).await.expect("upsert_room");

        let loaded = storage.get_room(room_id).await.expect("get_room").expect("exists");
        assert_eq!(loaded.name, "core-room");
        assert_eq!(loaded.mode, CoreRoomMode::Mcu);
        assert!(loaded.created_ms > 0, "created_ms must be extracted from UUIDv7 timestamp");

        let rooms = storage.list_rooms().await.expect("list_rooms");
        assert!(rooms.iter().any(|r| r.id == room_id));

        // Room mode override
        storage.set_room_mode_override(room_id, CoreRoomMode::Sfu).await.expect("set override");
        let overrides = storage.list_room_mode_overrides().await.expect("list overrides");
        assert_eq!(overrides.get(&room_id), Some(&CoreRoomMode::Sfu));
        storage.delete_room_mode_override(room_id).await.expect("delete override");
        let overrides_after = storage.list_room_mode_overrides().await.expect("list overrides");
        assert_eq!(overrides_after.get(&room_id), None);

        // Participants
        let participant =
            Participant::new(vks_core::participant::UserId::new(), "Alice", vks_core::participant::Role::Moderator);
        let p_id = participant.id;
        storage.upsert_participant(&participant).await.expect("upsert_participant");
        let loaded_p = storage.get_participant(p_id).await.expect("get_participant").expect("exists");
        assert_eq!(loaded_p.display_name, "Alice");

        // Layouts
        let layout = Layout {
            id: "layout-1".to_string(),
            name: "Grid 2x2".to_string(),
            version: 1,
            assigned_rooms: std::collections::BTreeMap::new(),
            is_default: false,
        };
        storage.upsert_layout(&layout).await.expect("upsert_layout");
        let loaded_l = storage.get_layout("layout-1").await.expect("get_layout").expect("exists");
        assert_eq!(loaded_l.name, "Grid 2x2");
        let all_layouts = storage.list_layouts().await.expect("list_layouts");
        assert!(all_layouts.iter().any(|l| l.id == "layout-1"));
        storage.delete_layout("layout-1").await.expect("delete_layout");
        assert!(storage.get_layout("layout-1").await.expect("get_layout").is_none());

        // ApiKeys
        let key = ApiKey {
            id: "key-123".to_string(),
            scopes: vec!["read".to_string(), "write".to_string()],
            owner: vks_core::participant::UserId::new(),
            revoked: false,
        };
        storage.upsert_api_key(&key).await.expect("upsert_api_key");
        let loaded_k = storage.get_api_key("key-123").await.expect("get_api_key").expect("exists");
        assert_eq!(loaded_k.scopes, vec!["read", "write"]);
        assert!(!loaded_k.revoked);
        let all_keys = storage.list_api_keys().await.expect("list_api_keys");
        assert!(all_keys.iter().any(|k| k.id == "key-123"));
        storage.delete_api_key("key-123").await.expect("delete_api_key");
        assert!(storage.get_api_key("key-123").await.expect("get_api_key").is_none());

        // Health probe (`/readyz`): реальный ping (SQL `SELECT 1`).
        storage.ping().await.expect("ping");

        // Cleanup
        storage.delete_room(room_id).await.expect("delete_room");
        assert!(storage.get_room(room_id).await.expect("get_room").is_none());
    }

    #[tokio::test]
    async fn memory_storage_satisfies_core_port() {
        let mem = Arc::new(MemoryStorage::new());
        test_storage_contract(mem).await;
    }

    #[tokio::test]
    async fn sqlite_storage_satisfies_core_port() {
        let sqlite = Arc::new(SqliteStorage::init("sqlite::memory:").await.expect("init sqlite"));
        test_storage_contract(sqlite).await;
    }
}

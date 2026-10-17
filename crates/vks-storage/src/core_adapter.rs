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
        // Вставка или обновление в in-memory таблице комнат
        let mut rooms = self.rooms_map().write().unwrap();
        let r = crate::rows::Room { id, name: room.name.clone(), mode, created_at: crate::rows::now() };
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
            let mut room = Room::new(r.name.clone(), mode);
            room.id = id;
            Ok(Some(room))
        } else {
            Ok(None)
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let mut rooms = self.rooms_map().write().unwrap();
        rooms.remove(id.as_uuid());
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
                let mut room = Room::new(r.name.clone(), mode);
                room.id = RoomId::from_uuid(r.id);
                room
            })
            .collect();
        Ok(res)
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
        self.participants_map().write().unwrap().clear();
        self.layouts_map().write().unwrap().clear();
        self.api_keys_map().write().unwrap().clear();
        Ok(())
    }
}

#[async_trait]
impl Storage for SqliteStorage {
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
                let mut room = Room::new(r.name, mode);
                room.id = id;
                Ok(Some(room))
            }
            Err(crate::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let uuid = *id.as_uuid();
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
                let mut room = Room::new(r.name, mode);
                room.id = RoomId::from_uuid(r.id);
                room
            })
            .collect())
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
        sqlx::query("DELETE FROM room_mode_override").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM rooms").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        self.participants_map().write().unwrap().clear();
        self.layouts_map().write().unwrap().clear();
        self.api_keys_map().write().unwrap().clear();
        Ok(())
    }
}

#[async_trait]
impl Storage for PostgresStorage {
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
                let mut room = Room::new(r.name, mode);
                room.id = id;
                Ok(Some(room))
            }
            Err(crate::Error::NotFound(_)) => Ok(None),
            Err(e) => Err(to_storage_err(e)),
        }
    }

    async fn delete_room(&self, id: RoomId) -> Result<(), StorageError> {
        let uuid = *id.as_uuid();
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
                let mut room = Room::new(r.name, mode);
                room.id = RoomId::from_uuid(r.id);
                room
            })
            .collect())
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
        sqlx::query("DELETE FROM room_mode_override").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM layouts").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM api_keys").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM rooms").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM users").execute(self.pool()).await.map_err(to_storage_err)?;
        sqlx::query("DELETE FROM audit_log").execute(self.pool()).await.map_err(to_storage_err)?;
        self.participants_map().write().unwrap().clear();
        self.layouts_map().write().unwrap().clear();
        self.api_keys_map().write().unwrap().clear();
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

        let rooms = storage.list_rooms().await.expect("list_rooms");
        assert!(rooms.iter().any(|r| r.id == room_id));

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

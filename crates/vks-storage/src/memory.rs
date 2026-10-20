//! In-memory backend `Storage` (для тестов, ADR-0008).
//!
//! Thread-safe: `RwLock<HashMap>` на сущность. CRUD: insert/get/list +
//! update (rooms) + append-only audit.

use std::collections::HashMap;
use std::sync::RwLock;

use uuid::Uuid;

use crate::error::{Error, Result};
use crate::rows::{now, ApiKey, AuditEntry, Layout, Room, RoomMode, User};

/// In-memory хранилище: пользователи, комнаты, layouts, api-ключи, audit.
#[derive(Debug, Default)]
pub struct MemoryStorage {
    users: RwLock<HashMap<Uuid, User>>,
    rooms: RwLock<HashMap<Uuid, Room>>,
    room_mode_override: RwLock<HashMap<Uuid, RoomMode>>,
    layouts: RwLock<HashMap<Uuid, Layout>>,
    api_keys: RwLock<HashMap<Uuid, ApiKey>>,
    audit: RwLock<Vec<AuditEntry>>,
    core_participants: RwLock<HashMap<vks_core::participant::ParticipantId, vks_core::participant::Participant>>,
    core_layouts: RwLock<HashMap<String, vks_core::ports::storage::Layout>>,
    core_api_keys: RwLock<HashMap<String, vks_core::ports::storage::ApiKey>>,
    core_users: RwLock<HashMap<vks_core::UserId, vks_core::ports::storage::StoredUser>>,
    core_credentials: RwLock<HashMap<vks_core::UserId, vks_core::ports::storage::UserCredentials>>,
    core_audit: RwLock<Vec<vks_core::ports::storage::AuditEntry>>,
    core_records: RwLock<HashMap<String, vks_core::ports::storage::RecordEntry>>,
    server_settings: RwLock<Option<String>>,
    configuration_documents: RwLock<HashMap<String, String>>,
}

impl MemoryStorage {
    pub(crate) fn configuration_documents(&self) -> &RwLock<HashMap<String, String>> {
        &self.configuration_documents
    }
    pub(crate) fn records_map(&self) -> &RwLock<HashMap<String, vks_core::ports::storage::RecordEntry>> {
        &self.core_records
    }
    pub(crate) fn rooms_map(&self) -> &RwLock<HashMap<Uuid, Room>> {
        &self.rooms
    }

    pub(crate) fn participants_map(
        &self,
    ) -> &RwLock<HashMap<vks_core::participant::ParticipantId, vks_core::participant::Participant>> {
        &self.core_participants
    }

    pub(crate) fn layouts_map(&self) -> &RwLock<HashMap<String, vks_core::ports::storage::Layout>> {
        &self.core_layouts
    }

    pub(crate) fn api_keys_map(&self) -> &RwLock<HashMap<String, vks_core::ports::storage::ApiKey>> {
        &self.core_api_keys
    }

    pub(crate) fn users_map(&self) -> &RwLock<HashMap<vks_core::UserId, vks_core::ports::storage::StoredUser>> {
        &self.core_users
    }

    pub(crate) fn credentials_map(
        &self,
    ) -> &RwLock<HashMap<vks_core::UserId, vks_core::ports::storage::UserCredentials>> {
        &self.core_credentials
    }

    pub(crate) fn server_settings_slot(&self) -> &RwLock<Option<String>> {
        &self.server_settings
    }

    pub(crate) fn audit_log(&self) -> &RwLock<Vec<vks_core::ports::storage::AuditEntry>> {
        &self.core_audit
    }

    pub(crate) fn room_mode_overrides_map(&self) -> &RwLock<HashMap<Uuid, RoomMode>> {
        &self.room_mode_override
    }
    /// Создаёт пустое in-memory хранилище.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // --- users ---

    /// Сохраняет пользователя (insert или replace по `id`).
    pub fn insert_user(&self, user: User) -> Result<()> {
        self.users.write().unwrap().insert(user.id, user);
        Ok(())
    }

    /// Возвращает пользователя по `id`.
    pub fn get_user(&self, id: Uuid) -> Result<User> {
        self.users.read().unwrap().get(&id).cloned().ok_or_else(|| Error::NotFound(format!("user {id}")))
    }

    /// Список всех пользователей.
    pub fn list_users(&self) -> Vec<User> {
        self.users.read().unwrap().values().cloned().collect()
    }

    /// Удаляет пользователя.
    pub fn delete_user(&self, id: Uuid) -> Result<()> {
        self.users.write().unwrap().remove(&id).map(|_| ()).ok_or_else(|| Error::NotFound(format!("user {id}")))
    }

    // --- rooms ---

    /// Создаёт комнату; имя уникально (Conflict при дубликате).
    pub fn insert_room(&self, id: Uuid, name: &str, mode: RoomMode) -> Result<Room> {
        let mut rooms = self.rooms.write().unwrap();
        if rooms.values().any(|r| r.name == name) {
            return Err(Error::Conflict(format!("room name '{name}' already exists")));
        }
        let room = Room { id, name: name.to_owned(), mode, created_at: now() };
        rooms.insert(id, room.clone());
        Ok(room)
    }

    /// Возвращает комнату по `id`.
    pub fn get_room(&self, id: Uuid) -> Result<Room> {
        self.rooms.read().unwrap().get(&id).cloned().ok_or_else(|| Error::NotFound(format!("room {id}")))
    }

    /// Список всех комнат.
    pub fn list_rooms(&self) -> Vec<Room> {
        self.rooms.read().unwrap().values().cloned().collect()
    }

    /// Переопределяет режим комнаты (`room_mode_override`).
    pub fn set_room_mode_override(&self, room_id: Uuid, mode: RoomMode) -> Result<()> {
        self.room_mode_override.write().unwrap().insert(room_id, mode);
        Ok(())
    }

    /// Возвращает режим с учётом override.
    pub fn effective_room_mode(&self, room_id: Uuid) -> Result<RoomMode> {
        if let Some(&override_) = self.room_mode_override.read().unwrap().get(&room_id) {
            return Ok(override_);
        }
        let room = self.get_room(room_id)?;
        Ok(room.mode)
    }

    /// Удаляет комнату.
    pub fn delete_room(&self, id: Uuid) -> Result<()> {
        self.rooms.write().unwrap().remove(&id).map(|_| ()).ok_or_else(|| Error::NotFound(format!("room {id}")))
    }

    // --- layouts ---

    /// Сохраняет layout комнаты (insert или replace по `id`).
    pub fn insert_layout(&self, layout: Layout) -> Result<()> {
        self.layouts.write().unwrap().insert(layout.id, layout);
        Ok(())
    }

    /// Возвращает layout по `id`.
    pub fn get_layout(&self, id: Uuid) -> Result<Layout> {
        self.layouts.read().unwrap().get(&id).cloned().ok_or_else(|| Error::NotFound(format!("layout {id}")))
    }

    /// Layouts для комнаты.
    pub fn list_layouts(&self, room_id: Uuid) -> Vec<Layout> {
        self.layouts.read().unwrap().values().filter(|l| l.room_id == room_id).cloned().collect()
    }

    // --- api_keys ---

    /// Сохраняет API-ключ (insert или replace по `id`).
    pub fn insert_api_key(&self, key: ApiKey) -> Result<()> {
        self.api_keys.write().unwrap().insert(key.id, key);
        Ok(())
    }

    /// Возвращает API-ключ по `id`.
    pub fn get_api_key(&self, id: Uuid) -> Result<ApiKey> {
        self.api_keys.read().unwrap().get(&id).cloned().ok_or_else(|| Error::NotFound(format!("api_key {id}")))
    }

    /// API-ключи пользователя.
    pub fn list_api_keys(&self, user_id: Uuid) -> Vec<ApiKey> {
        self.api_keys.read().unwrap().values().filter(|k| k.user_id == user_id).cloned().collect()
    }

    /// Удаляет API-ключ (GDPR: erasure).
    pub fn delete_api_key(&self, id: Uuid) -> Result<()> {
        self.api_keys.write().unwrap().remove(&id).map(|_| ()).ok_or_else(|| Error::NotFound(format!("api_key {id}")))
    }

    // --- audit (append-only) ---

    /// Добавляет запись аудита.
    pub fn append_audit(&self, entry: AuditEntry) -> Result<()> {
        self.audit.write().unwrap().push(entry);
        Ok(())
    }

    /// Весь журнал аудита (chronological order).
    pub fn list_audit(&self) -> Vec<AuditEntry> {
        self.audit.read().unwrap().clone()
    }

    /// Записывает системное событие аудита.
    pub fn log_audit(&self, event: &str, subject_id: Option<Uuid>, payload: Option<String>) -> Result<()> {
        self.append_audit(AuditEntry { id: now(), created_at: now(), event: event.to_owned(), subject_id, payload })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::Tile;

    fn test_tile() -> Tile {
        Tile { x: 0, y: 0, width: 100, height: 100 }
    }

    #[test]
    fn user_crud_roundtrip() {
        let storage = MemoryStorage::new();
        let user = User {
            id: Uuid::new_v4(),
            login: "alice".to_owned(),
            email: "alice@example.com".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_user(user.clone()).unwrap();
        assert_eq!(storage.get_user(user.id).unwrap(), user);
        assert_eq!(storage.list_users().len(), 1);
        assert!(storage.get_user(Uuid::new_v4()).is_err());
        storage.delete_user(user.id).unwrap();
        assert!(storage.get_user(user.id).is_err());
        assert!(storage.delete_user(user.id).is_err());
    }

    #[test]
    fn room_crud_roundtrip_and_name_conflict() {
        let storage = MemoryStorage::new();
        let id = Uuid::new_v4();
        let room = storage.insert_room(id, "ops", RoomMode::Sfu).unwrap();
        assert_eq!(room.id, id);
        assert_eq!(room.mode, RoomMode::Sfu);
        assert_eq!(storage.get_room(id).unwrap(), room);
        assert_eq!(storage.list_rooms().len(), 1);
        // Conflict on duplicate name.
        assert!(storage.insert_room(Uuid::new_v4(), "ops", RoomMode::Mcu).is_err());
        // Different name ok.
        storage.insert_room(Uuid::new_v4(), "prod", RoomMode::Mcu).unwrap();
        assert_eq!(storage.list_rooms().len(), 2);
        // Mode override.
        storage.set_room_mode_override(id, RoomMode::Mcu).unwrap();
        assert_eq!(storage.effective_room_mode(id).unwrap(), RoomMode::Mcu);
        assert!(storage.effective_room_mode(Uuid::new_v4()).is_err());
        // Delete.
        storage.delete_room(id).unwrap();
        assert!(storage.get_room(id).is_err());
    }

    #[test]
    fn layout_crud_roundtrip() {
        let storage = MemoryStorage::new();
        let room_id = Uuid::new_v4();
        let layout = Layout { id: Uuid::new_v4(), room_id, width: 1920, height: 1080, tiles: vec![test_tile()] };
        storage.insert_layout(layout.clone()).unwrap();
        assert_eq!(storage.get_layout(layout.id).unwrap(), layout);
        assert_eq!(storage.list_layouts(room_id).len(), 1);
        assert_eq!(storage.list_layouts(Uuid::new_v4()).len(), 0);
        assert!(storage.get_layout(Uuid::new_v4()).is_err());
    }

    #[test]
    fn api_key_crud_roundtrip() {
        let storage = MemoryStorage::new();
        let user_id = Uuid::new_v4();
        let key = ApiKey {
            id: Uuid::new_v4(),
            user_id,
            hashed_value: "sha256:abc".to_owned(),
            prefix: "vks_".to_owned(),
            created_at: Uuid::now_v7(),
        };
        storage.insert_api_key(key.clone()).unwrap();
        assert_eq!(storage.get_api_key(key.id).unwrap(), key);
        assert_eq!(storage.list_api_keys(user_id).len(), 1);
        assert_eq!(storage.list_api_keys(Uuid::new_v4()).len(), 0);
        storage.delete_api_key(key.id).unwrap();
        assert!(storage.get_api_key(key.id).is_err());
    }

    #[test]
    fn audit_append_only_and_list() {
        let storage = MemoryStorage::new();
        let subject = Uuid::new_v4();
        storage.log_audit("room.create", Some(subject), Some("{\"id\":\"x\"}".to_owned())).unwrap();
        storage
            .append_audit(AuditEntry {
                id: Uuid::new_v4(),
                created_at: Uuid::now_v7(),
                event: "user.update".to_owned(),
                subject_id: None,
                payload: None,
            })
            .unwrap();
        assert_eq!(storage.list_audit().len(), 2);
    }
}

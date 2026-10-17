// Rooms — CRUD комнат + live status + Observer (live view).
//
// RBAC (ADR-0022): create/delete — Operator/Mediator; live view —
// Viewer (Observer mode, read-only). Live status: participants +
// bitrate (monitor, фаза 5).

use std::sync::Arc;
use vks_core::{RoomId, RoomMode};

use crate::state::{AppState, RoomRecord};

/// Операции над комнатами (тонкая обёртка над [`AppState`]).
pub struct Rooms;

impl Rooms {
    /// Создаёт комнату (per-room режим = `None`, т.е. глобальный).
    pub fn create(state: &AppState, name: impl Into<String>) -> RoomRecord {
        let id = RoomId::new();
        let record = RoomRecord::new(id, name, None);
        {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            rooms.insert(id, record.clone());
        }
        if let Some(storage) = &state.storage {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let storage = Arc::clone(storage);
                let mut cr = vks_core::Room::new(record.name.clone(), record.mode.unwrap_or(RoomMode::Sfu));
                cr.id = id;
                drop(handle.spawn(async move {
                    let _ = storage.upsert_room(&cr).await;
                }));
            }
        }
        record
    }

    /// Возвращает запись комнаты.
    pub fn get(state: &AppState, id: &RoomId) -> Option<RoomRecord> {
        state.rooms.lock().expect("rooms store poisoned").get(id).cloned()
    }

    /// Список всех комнат (упорядоченный по id, BTreeMap).
    pub fn list(state: &AppState) -> Vec<RoomRecord> {
        state.rooms.lock().expect("rooms store poisoned").values().cloned().collect()
    }

    /// Обновляет запись (name, per-room mode, live metrics).
    ///
    /// `None`-поле в patch = без изменения.
    pub fn update(
        state: &AppState,
        id: &RoomId,
        name: Option<String>,
        mode: Option<RoomMode>,
        participants: Option<usize>,
        bitrate_kbps: Option<u32>,
    ) -> Option<RoomRecord> {
        let mut rooms = state.rooms.lock().expect("rooms store poisoned");
        let record = rooms.get_mut(id)?;
        if let Some(n) = name {
            record.name = n;
        }
        if let Some(m) = mode {
            record.mode = Some(m);
            state.mode.lock().expect("mode store poisoned").room_mode_overrides.insert(*id, m);
        }
        if let Some(p) = participants {
            record.participants = p;
        }
        if let Some(b) = bitrate_kbps {
            record.bitrate_kbps = b;
        }
        let updated = record.clone();
        if let Some(storage) = &state.storage {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let storage = Arc::clone(storage);
                let rid = *id;
                let room_name = updated.name.clone();
                let effective_mode = updated.mode.unwrap_or(RoomMode::Sfu);
                drop(handle.spawn(async move {
                    let mut cr = vks_core::Room::new(room_name, effective_mode);
                    cr.id = rid;
                    let _ = storage.upsert_room(&cr).await;
                }));
            }
        }
        Some(updated)
    }

    /// Удаляет комнату (и per-room override).
    ///
    /// Возвращает `true`, если запись существовала.
    pub fn delete(state: &AppState, id: &RoomId) -> bool {
        {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            if rooms.remove(id).is_none() {
                return false;
            }
        }
        state.mode.lock().expect("mode store poisoned").room_mode_overrides.remove(id);
        if let Some(storage) = &state.storage {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let storage = Arc::clone(storage);
                let rid = *id;
                drop(handle.spawn(async move {
                    let _ = storage.delete_room(rid).await;
                }));
            }
        }
        true
    }

    /// Live view (Observer, read-only): запись + эффективный режим
    /// (ARCHITECTURE §2: `ServerHello.room_mode`).
    pub fn live_view(state: &AppState, id: &RoomId) -> Option<LiveView> {
        let record = state.rooms.lock().expect("rooms store poisoned").get(id)?.clone();
        let mode = state.mode.lock().expect("mode store poisoned").effective_mode(id);
        Some(LiveView {
            id: record.id,
            name: record.name,
            participants: record.participants,
            bitrate_kbps: record.bitrate_kbps,
            mode,
        })
    }
}

/// Live view комнаты (Observer, read-only).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct LiveView {
    /// Идентикатор комнаты.
    pub id: RoomId,
    /// Название.
    pub name: String,
    /// Число участников.
    pub participants: usize,
    /// Битрейт, kbps.
    pub bitrate_kbps: u32,
    /// Эффективный режим MCU/SFU (override иначе глобальный).
    pub mode: RoomMode,
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[test]
    fn create_get_roundtrip() {
        let state = AppState::new();
        let created = Rooms::create(&state, "standup");
        let fetched = Rooms::get(&state, &created.id).expect("room exists");
        assert_eq!(fetched.name, "standup");
        assert_eq!(fetched.mode, None);
        assert_eq!(fetched.id, created.id);
    }

    #[test]
    fn list_is_sorted() {
        let state = AppState::new();
        Rooms::create(&state, "a");
        Rooms::create(&state, "b");
        let list = Rooms::list(&state);
        assert_eq!(list.len(), 2);
        let ids: Vec<RoomId> = list.iter().map(|r| r.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted);
        let names: Vec<&str> = list.iter().map(|r| r.name.as_str()).collect();
        assert!(names.contains(&"a") && names.contains(&"b"));
    }

    #[test]
    fn update_only_changes_given_fields() {
        let state = AppState::new();
        let created = Rooms::create(&state, "conf");
        let updated = Rooms::update(&state, &created.id, None, Some(RoomMode::Mcu), Some(3), None).expect("update");
        assert_eq!(updated.name, "conf");
        assert_eq!(updated.mode, Some(RoomMode::Mcu));
        assert_eq!(updated.participants, 3);
        assert_eq!(updated.bitrate_kbps, 0);
        // Override записан.
        assert_eq!(state.mode.lock().unwrap().room_mode_overrides.get(&created.id), Some(&RoomMode::Mcu));
    }

    #[test]
    fn update_missing_room_returns_none() {
        let state = AppState::new();
        assert!(Rooms::update(&state, &RoomId::new(), Some("x".into()), None, None, None).is_none());
    }

    #[test]
    fn delete_removes_room_and_override() {
        let state = AppState::new();
        let created = Rooms::create(&state, "conf");
        Rooms::update(&state, &created.id, None, Some(RoomMode::Mcu), None, None);
        assert!(Rooms::delete(&state, &created.id));
        assert!(!Rooms::delete(&state, &created.id));
        assert!(state.mode.lock().unwrap().room_mode_overrides.is_empty());
        assert!(state.rooms.lock().unwrap().is_empty());
    }

    #[test]
    fn live_view_reflects_effective_mode_and_metrics() {
        let state = AppState::new();
        let created = Rooms::create(&state, "conf");
        // Глобальный режим SFU, без override.
        let view = Rooms::live_view(&state, &created.id).unwrap();
        assert_eq!(view.mode, RoomMode::Sfu);
        assert_eq!(view.participants, 0);

        // Per-room override + live metrics.
        Rooms::update(&state, &created.id, None, Some(RoomMode::Mcu), Some(5), Some(1200));
        let view = Rooms::live_view(&state, &created.id).unwrap();
        assert_eq!(view.mode, RoomMode::Mcu);
        assert_eq!(view.participants, 5);
        assert_eq!(view.bitrate_kbps, 1200);
    }

    #[test]
    fn live_view_missing_room_returns_none() {
        let state = AppState::new();
        assert!(Rooms::live_view(&state, &RoomId::new()).is_none());
    }

    #[tokio::test]
    async fn rooms_syncs_with_storage() {
        struct MockStorage {
            rooms: std::sync::Mutex<std::collections::HashMap<RoomId, vks_core::Room>>,
        }

        #[async_trait]
        impl vks_core::ports::Storage for MockStorage {
            async fn upsert_room(&self, room: &vks_core::Room) -> Result<(), vks_core::ports::StorageError> {
                self.rooms.lock().unwrap().insert(room.id, room.clone());
                Ok(())
            }
            async fn get_room(&self, id: RoomId) -> Result<Option<vks_core::Room>, vks_core::ports::StorageError> {
                Ok(self.rooms.lock().unwrap().get(&id).cloned())
            }
            async fn delete_room(&self, id: RoomId) -> Result<(), vks_core::ports::StorageError> {
                self.rooms.lock().unwrap().remove(&id);
                Ok(())
            }
            async fn list_rooms(&self) -> Result<Vec<vks_core::Room>, vks_core::ports::StorageError> {
                Ok(self.rooms.lock().unwrap().values().cloned().collect())
            }
            async fn upsert_participant(&self, _: &vks_core::Participant) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn get_participant(
                &self,
                _: vks_core::ParticipantId,
            ) -> Result<Option<vks_core::Participant>, vks_core::ports::StorageError> {
                Ok(None)
            }
            async fn delete_participant(
                &self,
                _: vks_core::ParticipantId,
            ) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn list_participants(&self) -> Result<Vec<vks_core::Participant>, vks_core::ports::StorageError> {
                Ok(vec![])
            }
            async fn upsert_layout(
                &self,
                _: &vks_core::ports::storage::Layout,
            ) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn get_layout(
                &self,
                _: &str,
            ) -> Result<Option<vks_core::ports::storage::Layout>, vks_core::ports::StorageError> {
                Ok(None)
            }
            async fn delete_layout(&self, _: &str) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn list_layouts(
                &self,
            ) -> Result<Vec<vks_core::ports::storage::Layout>, vks_core::ports::StorageError> {
                Ok(vec![])
            }
            async fn upsert_api_key(
                &self,
                _: &vks_core::ports::storage::ApiKey,
            ) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn get_api_key(
                &self,
                _: &str,
            ) -> Result<Option<vks_core::ports::storage::ApiKey>, vks_core::ports::StorageError> {
                Ok(None)
            }
            async fn delete_api_key(&self, _: &str) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn list_api_keys(
                &self,
            ) -> Result<Vec<vks_core::ports::storage::ApiKey>, vks_core::ports::StorageError> {
                Ok(vec![])
            }
            async fn reset(&self) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
        }

        let mock = Arc::new(MockStorage { rooms: std::sync::Mutex::new(std::collections::HashMap::new()) });
        let state = AppState::with_storage(mock.clone());
        let created = Rooms::create(&state, "persisted-room");

        // Wait a moment for background task
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(mock.rooms.lock().unwrap().contains_key(&created.id));

        Rooms::delete(&state, &created.id);
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!mock.rooms.lock().unwrap().contains_key(&created.id));
    }
}

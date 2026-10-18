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
    /// Вычисляет эффективный режим комнаты (явный override иначе глобальный режим).
    fn resolve_effective_mode(state: &AppState, override_mode: Option<RoomMode>) -> RoomMode {
        override_mode.unwrap_or_else(|| {
            state.mode.lock().expect("mode store poisoned").global_mode
        })
    }

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
                let effective_mode = Self::resolve_effective_mode(state, record.mode);
                let mut cr = vks_core::Room::new(record.name.clone(), effective_mode);
                cr.id = id;
                cr.created_ms = record.created_ms;
                drop(handle.spawn(async move {
                    let _ = storage.upsert_room(&cr).await;
                }));
            }
        }
        record
    }

    /// Создаёт комнату с подтвержденной синхронной записью в Storage до ответа (M0-M1).
    pub async fn create_async(state: &AppState, name: impl Into<String>) -> Result<RoomRecord, vks_core::ports::storage::StorageError> {
        let id = RoomId::new();
        let record = RoomRecord::new(id, name, None);
        if let Some(storage) = &state.storage {
            let effective_mode = Self::resolve_effective_mode(state, record.mode);
            let mut cr = vks_core::Room::new(record.name.clone(), effective_mode);
            cr.id = id;
            cr.created_ms = record.created_ms;
            storage.upsert_room(&cr).await?;
        }
        {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            rooms.insert(id, record.clone());
        }
        Ok(record)
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
        let mode_changed = if let Some(m) = mode {
            let old_effective = Self::resolve_effective_mode(state, record.mode);
            record.mode = Some(m);
            let mut mode_state = state.mode.lock().expect("mode store poisoned");
            mode_state.room_mode_overrides.insert(*id, m);
            if old_effective != m {
                mode_state.switch_epoch += 1;
                state.event_queue.publish(crate::mode::ModeChangedEvent {
                    room_id: Some(*id),
                    new_mode: m,
                    switch_epoch: mode_state.switch_epoch,
                });
                state.notify_admin("room:mode_changed", serde_json::json!({
                    "room_id": id.to_string(),
                    "mode": m,
                    "switch_epoch": mode_state.switch_epoch,
                }));
                true
            } else {
                false
            }
        } else {
            false
        };

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
                let effective_mode = Self::resolve_effective_mode(state, updated.mode);
                let created_ms = updated.created_ms;
                let opt_override = updated.mode;
                drop(handle.spawn(async move {
                    let mut cr = vks_core::Room::new(room_name, effective_mode);
                    cr.id = rid;
                    cr.created_ms = created_ms;
                    let _ = storage.upsert_room(&cr).await;
                    if mode_changed {
                        if let Some(m) = opt_override {
                            let _ = storage.set_room_mode_override(rid, m).await;
                        }
                    }
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
                    let _ = storage.delete_room_mode_override(rid).await;
                    let _ = storage.delete_room(rid).await;
                }));
            }
        }
        true
    }

    /// Обновляет запись с подтвержденной синхронной записью в Storage до ответа (M0-M1).
    pub async fn update_async(
        state: &AppState,
        id: &RoomId,
        name: Option<String>,
        mode: Option<RoomMode>,
        participants: Option<usize>,
        bitrate_kbps: Option<u32>,
    ) -> Result<Option<RoomRecord>, vks_core::ports::storage::StorageError> {
        let (current_name, current_created_ms, effective_mode, mode_changed, opt_override) = {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            let rec = match rooms.get(id) {
                Some(r) => r.clone(),
                None => return Ok(None),
            };
            let new_name = name.clone().unwrap_or(rec.name);
            let mode_changed = if let Some(m) = mode {
                let old_effective = Self::resolve_effective_mode(state, rec.mode);
                old_effective != m
            } else {
                false
            };
            let effective_mode = Self::resolve_effective_mode(state, mode.or(rec.mode));
            (new_name, rec.created_ms, effective_mode, mode_changed, mode.or(rec.mode))
        };

        if let Some(storage) = &state.storage {
            let mut cr = vks_core::Room::new(current_name, effective_mode);
            cr.id = *id;
            cr.created_ms = current_created_ms;
            storage.upsert_room(&cr).await?;
            if mode_changed {
                if let Some(m) = opt_override {
                    storage.set_room_mode_override(*id, m).await?;
                }
            }
        }

        let updated = Self::update(state, id, name, mode, participants, bitrate_kbps);
        Ok(updated)
    }

    /// Удаляет комнату с подтвержденным синхронным удалением из Storage до ответа (M0-M1).
    pub async fn delete_async(state: &AppState, id: &RoomId) -> Result<bool, vks_core::ports::storage::StorageError> {
        let exists = {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            rooms.contains_key(id)
        };
        if !exists {
            return Ok(false);
        }
        if let Some(storage) = &state.storage {
            let _ = storage.delete_room_mode_override(*id).await;
            storage.delete_room(*id).await?;
        }
        Ok(Self::delete(state, id))
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

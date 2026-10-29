// Rooms — CRUD комнат + live status + Observer (live view).
//
// RBAC (ADR-0022): create/delete — Operator/Mediator; live view —
// Viewer (Observer mode, read-only). Live status: participants +
// bitrate (monitor, фаза 5).

use vks_core::{RoomId, RoomMode};

use crate::state::{AppState, RoomRecord};

/// Операции над комнатами (тонкая обёртка над [`AppState`]).
pub struct Rooms;

impl Rooms {
    /// Вычисляет эффективный режим комнаты (явный override иначе глобальный режим).
    fn resolve_effective_mode(state: &AppState, override_mode: Option<RoomMode>) -> RoomMode {
        override_mode.unwrap_or_else(|| state.mode.lock().expect("mode store poisoned").global_mode)
    }

    /// Создаёт комнату (per-room режим = `None`, т.е. глобальный).
    pub fn create(state: &AppState, name: impl Into<String>) -> RoomRecord {
        let id = RoomId::new();
        let record = RoomRecord::new(id, name, None);
        {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            rooms.insert(id, record.clone());
        }
        record
    }

    /// Создаёт комнату с подтвержденной синхронной записью в Storage до ответа (M0-M1).
    pub async fn create_async(
        state: &AppState,
        name: impl Into<String>,
    ) -> Result<RoomRecord, vks_core::ports::storage::StorageError> {
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

    /// Занято ли имя комнаты (регистронезависимо, как UI/lookup). `except` —
    /// сама переименовываемая комната: её собственное имя не конфликт.
    pub fn name_taken(state: &AppState, name: &str, except: Option<&RoomId>) -> bool {
        let target = name.trim().to_lowercase();
        state
            .rooms
            .lock()
            .expect("rooms store poisoned")
            .values()
            .any(|room| room.name.trim().to_lowercase() == target && except != Some(&room.id))
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
        let (updated, mode) = {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            let record = rooms.get_mut(id)?;
            if let Some(n) = name {
                record.name = n;
            }
            if let Some(m) = mode {
                record.mode = Some(m);
            }
            if let Some(p) = participants {
                record.participants = p;
            }
            if let Some(b) = bitrate_kbps {
                record.bitrate_kbps = b;
            }
            (record.clone(), mode)
        };
        // Never hold the rooms lock across the admission/eviction hook: gateway
        // announce takes admission then reads rooms, so the reverse order would
        // deadlock and also let an offer slip between the override and eviction.
        if let Some(m) = mode {
            let epoch = {
                let mut mode_state = state.mode.lock().expect("mode store poisoned");
                // Same lock acquisition for read+insert: a concurrent global
                // switch cannot make this event stale between the two.
                let old_effective = mode_state.effective_mode(id);
                mode_state.room_mode_overrides.insert(*id, m);
                if old_effective == m {
                    None
                } else {
                    mode_state.switch_epoch += 1;
                    state.event_queue.publish(crate::mode::ModeChangedEvent {
                        room_id: Some(*id),
                        new_mode: m,
                        switch_epoch: mode_state.switch_epoch,
                    });
                    Some(mode_state.switch_epoch)
                }
            };
            if let Some(epoch) = epoch {
                state.notify_admin(
                    "room:mode_changed",
                    serde_json::json!({
                        "room_id": id.to_string(),
                        "mode": m,
                        "switch_epoch": epoch,
                    }),
                );
                crate::mode::ModeService::broadcast_room_mode(state, id, m, epoch);
                if m == RoomMode::Mcu {
                    if let Some(evict) = &state.webrtc_evict {
                        // WebRTC media cannot enter the MCU mixer; drop real
                        // peers rather than leave them streaming silently.
                        evict(Some(*id));
                    }
                }
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
        // Snapshot the record under rooms, then resolve modes after releasing it:
        // lock order is always rooms -> (released) -> mode, never nested.
        let rec = {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            match rooms.get(id) {
                Some(record) => record.clone(),
                None => return Ok(None),
            }
        };
        let new_name = name.clone().unwrap_or(rec.name);
        let override_changed = mode.is_some_and(|m| rec.mode != Some(m));
        let effective_mode = Self::resolve_effective_mode(state, mode.or(rec.mode));
        let (current_name, current_created_ms, opt_override) = (new_name, rec.created_ms, mode.or(rec.mode));

        if let Some(storage) = &state.storage {
            let mut cr = vks_core::Room::new(current_name, effective_mode);
            cr.id = *id;
            cr.created_ms = current_created_ms;
            storage.upsert_room(&cr).await?;
            if override_changed {
                if let Some(m) = opt_override {
                    storage.set_room_mode_override(*id, m).await?;
                }
            }
        }

        let updated = Self::update(state, id, name, mode, participants, bitrate_kbps);
        Ok(updated)
    }

    /// Сбрасывает `created_ms` (время старта сессии) при повторном запуске
    /// остановленной конференции: история получает новый `started_at`.
    pub async fn reset_created_ms(
        state: &AppState,
        id: &RoomId,
    ) -> Result<Option<RoomRecord>, vks_core::ports::storage::StorageError> {
        let now = crate::state::now_unix_ms();
        let updated = {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            match rooms.get_mut(id) {
                Some(record) => {
                    record.created_ms = now;
                    Some(record.clone())
                }
                None => None,
            }
        };
        if let (Some(storage), Some(record)) = (&state.storage, &updated) {
            let mut cr = vks_core::Room::new(record.name.clone(), record.mode.unwrap_or(vks_core::RoomMode::Sfu));
            cr.id = *id;
            cr.created_ms = record.created_ms;
            storage.upsert_room(&cr).await?;
        }
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

    /// Kicks a participant from a room: broadcasts ParticipantLeft with reason 2 (kicked),
    /// removes from RoomHub roster, updates participant count in storage, and notifies admin WS.
    pub fn kick_participant(state: &AppState, room_id: &RoomId, participant_id: &str) -> bool {
        super::api_v3::call_strings::note_participant_left(state, room_id, participant_id, "Ended By Remote");
        let remaining = state.room_hub.unregister_participant(room_id, participant_id);

        if let Some(dialer) = state.outbound_dialer() {
            dialer.hangup(participant_id);
        }

        let left_msg = vks_proto::signaling::SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(vks_proto::signaling::signaling_message::Payload::ParticipantLeft(
                vks_proto::signaling::ParticipantLeft {
                    participant_id: participant_id.to_string(),
                    reason: 2, // 2 = kicked
                },
            )),
        };
        use prost::Message;
        let payload_bytes = left_msg.encode_to_vec();
        state.room_hub.broadcast(room_id, None, payload_bytes.into());

        if let Some(count) = remaining {
            Self::update(state, room_id, None, None, Some(count), None);
        }
        state.notify_admin(
            &format!("session:{}:abonents", room_id),
            serde_json::json!({
                "action": "kicked",
                "participant_id": participant_id,
            }),
        );
        true
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
            async fn upsert_record(
                &self,
                _: &vks_core::ports::storage::RecordEntry,
            ) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn get_record(
                &self,
                _: &str,
            ) -> Result<Option<vks_core::ports::storage::RecordEntry>, vks_core::ports::StorageError> {
                Ok(None)
            }
            async fn list_records(
                &self,
                _: Option<RoomId>,
            ) -> Result<Vec<vks_core::ports::storage::RecordEntry>, vks_core::ports::StorageError> {
                Ok(vec![])
            }
            async fn delete_record(&self, _: &str) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
            async fn reset(&self) -> Result<(), vks_core::ports::StorageError> {
                Ok(())
            }
        }

        let mock = std::sync::Arc::new(MockStorage { rooms: std::sync::Mutex::new(std::collections::HashMap::new()) });
        let state = AppState::with_storage(mock.clone());
        let created = Rooms::create_async(&state, "persisted-room".to_string()).await.expect("create_async failed");

        assert!(mock.rooms.lock().unwrap().contains_key(&created.id));

        let deleted = Rooms::delete_async(&state, &created.id).await.expect("delete_async failed");
        assert!(deleted);
        assert!(!mock.rooms.lock().unwrap().contains_key(&created.id));
    }

    #[tokio::test]
    async fn update_mode_broadcasts_room_mode_changed() {
        use prost::Message;
        let state = AppState::new();
        let created = Rooms::create(&state, "mode-test-room");
        let mut rx = state.room_hub.subscribe(&created.id);

        // Switch to Mcu
        let updated = Rooms::update(&state, &created.id, None, Some(RoomMode::Mcu), None, None).expect("room updated");
        assert_eq!(updated.mode, Some(RoomMode::Mcu));

        // Receive broadcast event
        let event = rx.try_recv().expect("broadcast event received");
        assert_eq!(event.sender_participant_id, None);

        let msg = vks_proto::signaling::SignalingMessage::decode(&event.payload[..]).expect("decode signaling message");
        match msg.payload {
            Some(vks_proto::signaling::signaling_message::Payload::RoomModeChanged(rmc)) => {
                assert_eq!(rmc.new_mode, vks_proto::media::RoomMode::Mcu as i32);
                assert_eq!(rmc.switch_epoch, 1);
                assert!(rmc.graceful);
            }
            other => panic!("expected RoomModeChanged, got {:?}", other),
        }
    }

    #[test]
    fn update_mode_calls_eviction_without_holding_rooms_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::mpsc;
        let mut state = AppState::new();
        let created = Rooms::create(&state, "evict-lock-room");
        let started = std::sync::Arc::new(AtomicBool::new(false));
        let flag = started.clone();
        state.webrtc_evict = Some(std::sync::Arc::new(move |_: Option<vks_core::RoomId>| {
            flag.store(true, Ordering::SeqCst);
            std::thread::sleep(std::time::Duration::from_millis(1500));
        }));
        let state = std::sync::Arc::new(state);
        let writer = state.clone();
        let id = created.id;
        let update = std::thread::spawn(move || Rooms::update(&writer, &id, None, Some(RoomMode::Mcu), None, None));
        while !started.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        // While the eviction hook sleeps, the rooms lock must already be free.
        let (get_tx, get_rx) = mpsc::channel();
        let reader = state.clone();
        std::thread::spawn(move || get_tx.send(Rooms::get(&reader, &id).is_some()).unwrap());
        assert!(
            get_rx.recv_timeout(std::time::Duration::from_millis(500)).expect("rooms lock held during eviction"),
            "room must still exist"
        );
        update.join().unwrap().expect("updated");
    }
}

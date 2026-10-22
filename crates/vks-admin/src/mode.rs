// ModeService — переключение MCU/SFU (ADR-0026, Q7: глобально + per-room).
//
// Это admin-вход для `RoomServerEvent` (ARCHITECTURE §2): смена режима
// публикуется в очередь (NATS в production), клиенты получают событие и
// делают graceful renegotiation. Комната остаётся (in-place switch).
//
// In-memory очередь [`ModeEventQueue`] — детерминированная замена NATS
// для этой волны; production-адаптер (vks-cluster) обёрнёт её.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use vks_core::{RoomId, RoomMode};

use crate::state::AppState;

/// Событие смены режима (административный вход, ADR-0026).
///
/// `room_id` — `None` для глобального переключения (влияет на все
/// комнаты без per-room override).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModeChangedEvent {
    /// Комната (None = глобальное переключение).
    pub room_id: Option<RoomId>,
    /// Новый режим.
    pub new_mode: RoomMode,
    /// Монотонная эпоха переключений (idempotence-кэш клиента).
    pub switch_epoch: u64,
}

/// In-memory очередь событий (production → NATS subject
/// `vks.cluster.room.mode_changed`).
#[derive(Debug, Default, Clone)]
pub struct ModeEventQueue {
    events: Arc<Mutex<VecDeque<ModeChangedEvent>>>,
}

impl ModeEventQueue {
    /// Создаёт пустую очередь.
    pub fn new() -> Self {
        Self::default()
    }

    /// Публикует событие в очередь.
    pub fn publish(&self, event: ModeChangedEvent) {
        self.events.lock().expect("mode event queue poisoned").push_back(event);
    }

    /// Достаёт и удаляет следующие `n` событий (FIFO, `None` = пусто).
    pub fn drain(&self, n: usize) -> Vec<ModeChangedEvent> {
        let mut q = self.events.lock().expect("mode event queue poisoned");
        let take = n.min(q.len());
        q.drain(..take).collect()
    }

    /// Число накопленных событий.
    pub fn len(&self) -> usize {
        self.events.lock().expect("mode event queue poisoned").len()
    }

    /// Очередь пуста.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Ошибка ModeService.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ModeError {
    /// Комната не найдена.
    #[error("room {0} not found")]
    RoomNotFound(RoomId),
    /// Ошибка персистентного хранилища (M1.4 / M1.5).
    #[error("storage error: {0}")]
    Storage(String),
}

/// ModeService — переключение MCU/SFU (глобально + per-room).
///
/// Тонкая обёртка над [`AppState`]: мутирует [`crate::state::ModeState`],
/// инкрементит `switch_epoch`, публикует [`ModeChangedEvent`] в очередь.
pub struct ModeService;

impl ModeService {
    /// Переключает ГЛОБАЛЬНЫЙ режим (RBAC: Operator only, ADR-0022).
    ///
    /// Возвращает `(old, new)`. Комнаты с per-room override не
    /// затрагиваются; остальные переходят в новый режим.
    pub fn set_global_mode(state: &AppState, new_mode: RoomMode) -> (RoomMode, RoomMode) {
        let old = {
            let mut m = state.mode.lock().expect("mode store poisoned");
            let old = m.global_mode;
            m.global_mode = new_mode;
            old
        };
        Self::announce_global_mode(state, old, new_mode);
        (old, new_mode)
    }

    /// Publishes a successful global mode change and evicts real peers only in
    /// rooms whose effective mode actually becomes MCU (per-room SFU
    /// overrides must keep their calls).
    pub(crate) fn announce_global_mode(state: &AppState, old: RoomMode, new_mode: RoomMode) {
        if old == new_mode {
            return;
        }
        {
            let mut m = state.mode.lock().expect("mode store poisoned");
            m.switch_epoch += 1;
            state.event_queue.publish(ModeChangedEvent { room_id: None, new_mode, switch_epoch: m.switch_epoch });
        }
        if new_mode == RoomMode::Mcu {
            Self::evict_mcu_rooms(state);
        }
    }

    pub(crate) fn evict_mcu_rooms(state: &AppState) {
        let targets: Vec<RoomId> = {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            let mode = state.mode.lock().expect("mode store poisoned");
            rooms.keys().filter(|id| mode.effective_mode(id) == RoomMode::Mcu).copied().collect()
        };
        if let Some(evict) = &state.webrtc_evict {
            for id in targets {
                evict(Some(id));
            }
        }
    }

    /// Переключает режим КОНКРЕТНОЙ комнаты (per-room override, ADR-0026, Q7).
    ///
    /// Комната должна существовать. RBAC: Operator/Mediator.
    pub fn set_room_mode(
        state: &AppState,
        room_id: &RoomId,
        new_mode: RoomMode,
    ) -> Result<(RoomMode, RoomMode), ModeError> {
        let (_room_name, _created_ms) = {
            let mut rooms = state.rooms.lock().expect("rooms store poisoned");
            let record = rooms.get_mut(room_id).ok_or(ModeError::RoomNotFound(*room_id))?;
            record.mode = Some(new_mode);
            (record.name.clone(), record.created_ms)
        };
        let mut m = state.mode.lock().expect("mode store poisoned");
        let old = m.room_mode_overrides.get(room_id).copied().unwrap_or(m.global_mode);
        m.room_mode_overrides.insert(*room_id, new_mode);
        if old != new_mode {
            m.switch_epoch += 1;
            state.event_queue.publish(ModeChangedEvent {
                room_id: Some(*room_id),
                new_mode,
                switch_epoch: m.switch_epoch,
            });
            state.notify_admin(
                "room:mode_changed",
                serde_json::json!({
                    "room_id": room_id.to_string(),
                    "mode": new_mode,
                    "switch_epoch": m.switch_epoch,
                }),
            );
            let proto_mode = match new_mode {
                RoomMode::Sfu => vks_proto::media::RoomMode::Sfu,
                RoomMode::Mcu => vks_proto::media::RoomMode::Mcu,
            };
            let mode_msg = vks_proto::signaling::SignalingMessage {
                msg_id: 0,
                reply_to: 0,
                payload: Some(vks_proto::signaling::signaling_message::Payload::RoomModeChanged(
                    vks_proto::signaling::RoomModeChanged {
                        new_mode: proto_mode as i32,
                        caps: None,
                        switch_epoch: m.switch_epoch,
                        graceful: true,
                    },
                )),
            };
            let payload_bytes = prost::Message::encode_to_vec(&mode_msg);
            state.room_hub.broadcast(room_id, None, payload_bytes.into());
            if new_mode == RoomMode::Mcu {
                drop(m);
                if let Some(evict) = &state.webrtc_evict {
                    evict(Some(*room_id));
                }
            }
        }
        Ok((old, new_mode))
    }

    /// Переключает режим КОНКРЕТНОЙ комнаты с подтвержденной записью в Storage (M1.4 / M1.5).
    pub async fn set_room_mode_async(
        state: &AppState,
        room_id: &RoomId,
        new_mode: RoomMode,
    ) -> Result<(RoomMode, RoomMode), ModeError> {
        let (room_name, created_ms) = {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            let record = rooms.get(room_id).ok_or(ModeError::RoomNotFound(*room_id))?;
            (record.name.clone(), record.created_ms)
        };
        if let Some(storage) = &state.storage {
            let mut cr = vks_core::Room::new(room_name.clone(), new_mode);
            cr.id = *room_id;
            cr.created_ms = created_ms;
            storage.upsert_room(&cr).await.map_err(|e| ModeError::Storage(e.to_string()))?;
            storage.set_room_mode_override(*room_id, new_mode).await.map_err(|e| ModeError::Storage(e.to_string()))?;
        }
        Self::set_room_mode(state, room_id, new_mode)
    }

    /// Эффективный режим комнаты (override иначе глобальный).
    pub fn effective_mode(state: &AppState, room_id: &RoomId) -> Result<RoomMode, ModeError> {
        {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            if !rooms.contains_key(room_id) {
                return Err(ModeError::RoomNotFound(*room_id));
            }
        }
        Ok(state.mode.lock().expect("mode store poisoned").effective_mode(room_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::RoomRecord;

    fn state_with_room() -> (AppState, RoomId) {
        let state = AppState::new();
        let id = RoomId::new();
        {
            let mut rooms = state.rooms.lock().unwrap();
            rooms.insert(id, RoomRecord::new(id, "conf", None));
        }
        (state, id)
    }

    #[test]
    fn set_global_mode_returns_old_new() {
        let state = AppState::new();
        let (old, new) = ModeService::set_global_mode(&state, RoomMode::Mcu);
        assert_eq!(old, RoomMode::Sfu);
        assert_eq!(new, RoomMode::Mcu);
        assert_eq!(state.mode.lock().unwrap().global_mode, RoomMode::Mcu);
    }

    #[test]
    fn set_global_mode_publishes_event() {
        let state = AppState::new();
        ModeService::set_global_mode(&state, RoomMode::Mcu);
        let events = state.event_queue.drain(10);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].room_id, None);
        assert_eq!(events[0].new_mode, RoomMode::Mcu);
        assert_eq!(events[0].switch_epoch, 1);
    }

    #[test]
    fn no_event_when_same_mode() {
        let state = AppState::new();
        ModeService::set_global_mode(&state, RoomMode::Sfu);
        assert!(state.event_queue.is_empty());
        assert_eq!(state.mode.lock().unwrap().switch_epoch, 0);
    }

    #[test]
    fn set_room_mode_sets_override() {
        let (state, id) = state_with_room();
        let (old, new) = ModeService::set_room_mode(&state, &id, RoomMode::Mcu).unwrap();
        assert_eq!(old, RoomMode::Sfu);
        assert_eq!(new, RoomMode::Mcu);
        assert_eq!(ModeService::effective_mode(&state, &id).unwrap(), RoomMode::Mcu);
    }

    #[test]
    fn set_room_mode_unknown_room_errors() {
        let state = AppState::new();
        let unknown = RoomId::new();
        assert_eq!(ModeService::set_room_mode(&state, &unknown, RoomMode::Mcu), Err(ModeError::RoomNotFound(unknown)));
    }

    #[test]
    fn room_override_survives_global_switch() {
        let (state, id) = state_with_room();
        ModeService::set_room_mode(&state, &id, RoomMode::Mcu).unwrap();
        // Глобальный режим по умолчанию Sfu — переключим на Mcu (реальное изменение),
        // per-room override должен выжить.
        ModeService::set_global_mode(&state, RoomMode::Mcu);
        assert_eq!(ModeService::effective_mode(&state, &id).unwrap(), RoomMode::Mcu);
        let events = state.event_queue.drain(10);
        let global = events.iter().find(|e| e.room_id.is_none()).expect("global event");
        assert_eq!(global.new_mode, RoomMode::Mcu);
    }

    #[test]
    fn mode_switch_evicts_only_actually_mcu_rooms() {
        let (mut state, mcu_room) = state_with_room();
        let sfu_room = RoomId::new();
        {
            let mut rooms = state.rooms.lock().unwrap();
            rooms.insert(sfu_room, RoomRecord::new(sfu_room, "sfu", None));
        }
        ModeService::set_room_mode(&state, &sfu_room, RoomMode::Sfu).unwrap();
        let evicted = Arc::new(Mutex::new(Vec::new()));
        let sink = evicted.clone();
        state.webrtc_evict = Some(Arc::new(move |room: Option<RoomId>| {
            sink.lock().unwrap().push(room);
        }));
        // Global MCU: only the room without an explicit SFU override loses peers.
        ModeService::set_global_mode(&state, RoomMode::Mcu);
        assert_eq!(*evicted.lock().unwrap(), vec![Some(mcu_room)]);

        // Canonical per-room endpoint also evicts when it becomes MCU.
        evicted.lock().unwrap().clear();
        ModeService::set_room_mode(&state, &sfu_room, RoomMode::Mcu).unwrap();
        assert_eq!(*evicted.lock().unwrap(), vec![Some(sfu_room)]);
    }

    #[test]
    fn epoch_increments_per_switch() {
        let (state, id) = state_with_room();
        ModeService::set_global_mode(&state, RoomMode::Mcu);
        ModeService::set_room_mode(&state, &id, RoomMode::Sfu).unwrap();
        assert_eq!(state.mode.lock().unwrap().switch_epoch, 2);
        let events = state.event_queue.drain(10);
        assert_eq!(events[0].switch_epoch, 1);
        assert_eq!(events[1].switch_epoch, 2);
    }
}

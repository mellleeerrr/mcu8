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
        let mut m = state.mode.lock().expect("mode store poisoned");
        let old = m.global_mode;
        m.global_mode = new_mode;
        if old != new_mode {
            m.switch_epoch += 1;
            state.event_queue.publish(ModeChangedEvent { room_id: None, new_mode, switch_epoch: m.switch_epoch });
        }
        (old, new_mode)
    }

    /// Переключает режим КОНКРЕТНОЙ комнаты (per-room override, ADR-0026, Q7).
    ///
    /// Комната должна существовать. RBAC: Operator/Mediator.
    pub fn set_room_mode(
        state: &AppState,
        room_id: &RoomId,
        new_mode: RoomMode,
    ) -> Result<(RoomMode, RoomMode), ModeError> {
        {
            let rooms = state.rooms.lock().expect("rooms store poisoned");
            if !rooms.contains_key(room_id) {
                return Err(ModeError::RoomNotFound(*room_id));
            }
        }
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
        }
        Ok((old, new_mode))
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

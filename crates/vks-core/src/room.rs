// Комната (room).
//
// `Room` — сущность видеоконференции. `RoomId` — идентификатор.
// `RoomState` — жизненный цикл (Idle/Active/Destroyed) с правилами
// переходов. `RoomMode` — режим MCU/SFU (прозрачность, ARCHITECTURE §2).
//
// Инвариант прозрачности MCU/SFU (ARCHITECTURE §2): режим хранится в
// комнате, а не в клиенте. `set_mode` реализует in-place переключение
// (ADR-0026): комната остаётся, режим меняется, клиенты renegotiate.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use crate::participant::Participant;

/// Идентикатор комнаты (UUIDv4/v7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RoomId(Uuid);

impl PartialOrd for RoomId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for RoomId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl RoomId {
    /// Новый глобально уникальный идентификатор комнаты.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Создаёт идентификатор из существующего [`Uuid`].
    pub fn from_uuid(uuid: Uuid) -> Self {
        Self(uuid)
    }

    /// Возвращает внутренний [`Uuid`].
    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Default for RoomId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for RoomId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for RoomId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Режим работы комнаты (ARCHITECTURE §2, ADR-0002/0026).
///
/// Клиент не знает о режиме (прозрачность): единый API, единый
/// сигнальный протокол. `ServerHello.room_mode` — единственный
/// источник истины.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomMode {
    /// SFU: клиент получает N треков, микширует сам (WebGL/Canvas).
    Sfu,
    /// MCU: сервер композитит, клиент получает 1 трек.
    Mcu,
}

impl RoomMode {
    /// Имя режима (для логов и wire-форматов).
    pub fn as_str(&self) -> &'static str {
        match self {
            RoomMode::Sfu => "sfu",
            RoomMode::Mcu => "mcu",
        }
    }
}

impl std::fmt::Display for RoomMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Состояние комнаты (жизненный цикл).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoomState {
    /// Создана, без активных участников.
    #[default]
    Idle,
    /// В конференции есть участники, идёт трансляция.
    Active,
    /// Уничтожена (неактивна, данные очищены).
    Destroyed,
}

impl RoomState {
    /// Имя состояния (для логов и wire-форматов).
    pub fn as_str(&self) -> &'static str {
        match self {
            RoomState::Idle => "idle",
            RoomState::Active => "active",
            RoomState::Destroyed => "destroyed",
        }
    }
}

impl std::fmt::Display for RoomState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Ошибка нарушения правил переходов состояния комнаты.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum RoomTransitionError {
    /// Переход невозможен из текущего состояния.
    #[error("illegal room transition: from {from} on {action}")]
    IllegalTransition { from: RoomState, action: &'static str },
}

/// Правила переходов состояния комнаты (state machine):
///
/// ```text
///   Idle ──activate──▶ Active ──deactivate──▶ Idle
///    │                            │
///    └──────destroy───────────────┘
///
///   Destroyed — terminal (no transitions).
/// ```
impl RoomState {
    /// Переход в активное состояние (первые участники вошли).
    pub fn activate(self) -> Result<RoomState, RoomTransitionError> {
        match self {
            RoomState::Idle => Ok(RoomState::Active),
            RoomState::Active => Err(RoomTransitionError::IllegalTransition { from: self, action: "activate" }),
            RoomState::Destroyed => Err(RoomTransitionError::IllegalTransition { from: self, action: "activate" }),
        }
    }

    /// Переход в простое состояние (все участники вышли).
    pub fn deactivate(self) -> Result<RoomState, RoomTransitionError> {
        match self {
            RoomState::Active => Ok(RoomState::Idle),
            RoomState::Idle => Err(RoomTransitionError::IllegalTransition { from: self, action: "deactivate" }),
            RoomState::Destroyed => Err(RoomTransitionError::IllegalTransition { from: self, action: "deactivate" }),
        }
    }

    /// Уничтожение комнаты (terminal).
    pub fn destroy(self) -> Result<RoomState, RoomTransitionError> {
        match self {
            RoomState::Idle | RoomState::Active => Ok(RoomState::Destroyed),
            RoomState::Destroyed => Err(RoomTransitionError::IllegalTransition { from: self, action: "destroy" }),
        }
    }

    /// Можно ли из этого состояния перейти в активное.
    pub fn can_activate(self) -> bool {
        matches!(self, RoomState::Idle)
    }

    /// Можно ли из этого состояния перейти в простое.
    pub fn can_deactivate(self) -> bool {
        matches!(self, RoomState::Active)
    }

    /// Можно ли из этого состояния уничтожить комнату.
    pub fn can_destroy(self) -> bool {
        matches!(self, RoomState::Idle | RoomState::Active)
    }

    /// Комната находится в простом состоянии (нет активных участников).
    pub fn is_idle(self) -> bool {
        matches!(self, RoomState::Idle)
    }

    /// Комната активна (идёт трансляция).
    pub fn is_active(self) -> bool {
        matches!(self, RoomState::Active)
    }

    /// Комната уничтожена (terminal).
    pub fn is_destroyed(self) -> bool {
        matches!(self, RoomState::Destroyed)
    }
}

/// Комната видеоконференции.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Room {
    /// Идентикатор комнаты.
    pub id: RoomId,
    /// Название комнаты.
    pub name: String,
    /// Текущее состояние (жизненный цикл).
    pub state: RoomState,
    /// Режим MCU/SFU (прозрачность, ARCHITECTURE §2).
    pub mode: RoomMode,
    /// Текущие участники.
    pub participants: Vec<Participant>,
}

impl Room {
    /// Создаёт новую комнату в состоянии [`RoomState::Idle`].
    pub fn new(name: impl Into<String>, mode: RoomMode) -> Self {
        Self { id: RoomId::new(), name: name.into(), state: RoomState::Idle, mode, participants: Vec::new() }
    }

    /// Возвращает количество участников.
    pub fn participant_count(&self) -> usize {
        self.participants.len()
    }

    /// Добавляет участника и активирует комнату, если она была idle.
    ///
    /// Возвращает [`RoomTransitionError`] если комната уже уничтожена.
    pub fn add_participant(&mut self, participant: Participant) -> Result<(), RoomTransitionError> {
        if self.state == RoomState::Destroyed {
            return Err(RoomTransitionError::IllegalTransition { from: self.state, action: "activate" });
        }
        if self.state == RoomState::Idle {
            self.state = self.state.activate()?;
        }
        self.participants.push(participant);
        Ok(())
    }

    /// Удаляет участника по id; деактивирует комнату, если стала пуста.
    ///
    /// Возвращает `true`, если участник был найден и удалён.
    pub fn remove_participant(&mut self, id: crate::participant::ParticipantId) -> Result<bool, RoomTransitionError> {
        let before = self.participants.len();
        self.participants.retain(|p| p.id != id);
        if self.participants.len() == before {
            return Ok(false);
        }
        if self.participants.is_empty() {
            self.state = self.state.deactivate()?;
        }
        Ok(true)
    }

    /// Переключает режим MCU/SFU in-place (ADR-0026, ARCHITECTURE §2.1).
    ///
    /// Комната остаётся (те же id, participants, state); меняется только
    /// [`RoomMode`]. Клиенты получают `RoomModeChanged` и делают graceful
    /// renegotiation.
    ///
    /// Возвращает [`RoomTransitionError`], если комната уничтожена.
    pub fn switch_mode(&mut self, new_mode: RoomMode) -> Result<RoomMode, RoomTransitionError> {
        if self.state == RoomState::Destroyed {
            return Err(RoomTransitionError::IllegalTransition { from: self.state, action: "switch_mode" });
        }
        self.mode = new_mode;
        Ok(new_mode)
    }

    /// Возвращает режим комнаты (для `ServerHello.room_mode`).
    pub fn mode(&self) -> RoomMode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::participant::{ParticipantId, Role};

    // --- RoomId ---
    #[test]
    fn room_id_roundtrip() {
        let id = RoomId::new();
        let s = id.to_string();
        let parsed: RoomId = s.parse().expect("parse");
        assert_eq!(id, parsed);
    }

    #[test]
    fn room_id_from_uuid() {
        let u = Uuid::new_v4();
        assert_eq!(RoomId::from_uuid(u).as_uuid(), &u);
    }

    // --- RoomMode ---
    #[test]
    fn room_mode_display() {
        assert_eq!(RoomMode::Sfu.to_string(), "sfu");
        assert_eq!(RoomMode::Mcu.to_string(), "mcu");
    }

    // --- RoomState transitions ---
    #[test]
    fn idle_can_activate() {
        assert_eq!(RoomState::Idle.activate(), Ok(RoomState::Active));
    }

    #[test]
    fn active_cannot_reactivate() {
        assert_eq!(
            RoomState::Active.activate(),
            Err(RoomTransitionError::IllegalTransition { from: RoomState::Active, action: "activate" })
        );
    }

    #[test]
    fn active_can_deactivate() {
        assert_eq!(RoomState::Active.deactivate(), Ok(RoomState::Idle));
    }

    #[test]
    fn idle_cannot_deactivate() {
        assert_eq!(
            RoomState::Idle.deactivate(),
            Err(RoomTransitionError::IllegalTransition { from: RoomState::Idle, action: "deactivate" })
        );
    }

    #[test]
    fn idle_can_destroy() {
        assert_eq!(RoomState::Idle.destroy(), Ok(RoomState::Destroyed));
    }

    #[test]
    fn active_can_destroy() {
        assert_eq!(RoomState::Active.destroy(), Ok(RoomState::Destroyed));
    }

    #[test]
    fn destroyed_is_terminal() {
        assert_eq!(
            RoomState::Destroyed.activate(),
            Err(RoomTransitionError::IllegalTransition { from: RoomState::Destroyed, action: "activate" })
        );
        assert_eq!(
            RoomState::Destroyed.deactivate(),
            Err(RoomTransitionError::IllegalTransition { from: RoomState::Destroyed, action: "deactivate" })
        );
        assert_eq!(
            RoomState::Destroyed.destroy(),
            Err(RoomTransitionError::IllegalTransition { from: RoomState::Destroyed, action: "destroy" })
        );
    }

    #[test]
    fn state_predicates() {
        assert!(RoomState::Idle.is_idle());
        assert!(RoomState::Active.is_active());
        assert!(RoomState::Destroyed.is_destroyed());
        assert!(RoomState::Idle.can_activate());
        assert!(!RoomState::Active.can_activate());
        assert!(RoomState::Active.can_deactivate());
        assert!(!RoomState::Idle.can_deactivate());
        assert!(RoomState::Idle.can_destroy());
        assert!(RoomState::Active.can_destroy());
        assert!(!RoomState::Destroyed.can_destroy());
    }

    #[test]
    fn state_roundtrip() {
        assert_eq!(RoomState::Idle.to_string(), "idle");
        assert_eq!(RoomState::Active.to_string(), "active");
        assert_eq!(RoomState::Destroyed.to_string(), "destroyed");
    }

    // --- Room ---
    #[test]
    fn new_room_is_idle() {
        let room = Room::new("conf", RoomMode::Sfu);
        assert_eq!(room.state, RoomState::Idle);
        assert_eq!(room.mode, RoomMode::Sfu);
        assert_eq!(room.participant_count(), 0);
    }

    #[test]
    fn add_participant_activates_room() {
        let mut room = Room::new("conf", RoomMode::Sfu);
        let p = Participant::new(crate::participant::UserId::new(), "a", Role::Participant);
        assert_eq!(room.add_participant(p), Ok(()));
        assert_eq!(room.state, RoomState::Active);
        assert_eq!(room.participant_count(), 1);
    }

    #[test]
    fn remove_last_participant_deactivates_room() {
        let mut room = Room::new("conf", RoomMode::Sfu);
        let p = Participant::new(crate::participant::UserId::new(), "a", Role::Participant);
        let pid = p.id;
        room.add_participant(p).unwrap();
        assert_eq!(room.state, RoomState::Active);

        assert_eq!(room.remove_participant(pid), Ok(true));
        assert_eq!(room.state, RoomState::Idle);
        assert_eq!(room.participant_count(), 0);
    }

    #[test]
    fn remove_absent_participant_returns_false() {
        let mut room = Room::new("conf", RoomMode::Sfu);
        let p = Participant::new(crate::participant::UserId::new(), "a", Role::Participant);
        let pid = p.id;
        room.add_participant(p).unwrap();
        let absent = ParticipantId::new();
        assert_ne!(absent, pid);
        assert_eq!(room.remove_participant(absent), Ok(false));
        assert_eq!(room.participant_count(), 1);
    }

    #[test]
    fn switch_mode_in_place() {
        let mut room = Room::new("conf", RoomMode::Sfu);
        let before_id = room.id;
        let before_state = room.state;
        let new = room.switch_mode(RoomMode::Mcu);
        assert_eq!(new, Ok(RoomMode::Mcu));
        assert_eq!(room.mode, RoomMode::Mcu);
        // id и state не меняются — in-place (прозрачность)
        assert_eq!(room.id, before_id);
        assert_eq!(room.state, before_state);
    }

    #[test]
    fn switch_mode_on_destroyed_fails() {
        let mut room = Room::new("conf", RoomMode::Sfu);
        let p = Participant::new(crate::participant::UserId::new(), "a", Role::Participant);
        room.add_participant(p).unwrap();
        room.state = RoomState::Destroyed;
        assert_eq!(
            room.switch_mode(RoomMode::Mcu),
            Err(RoomTransitionError::IllegalTransition { from: RoomState::Destroyed, action: "switch_mode" })
        );
    }

    #[test]
    fn switch_mode_between_same_modes_ok() {
        let mut room = Room::new("conf", RoomMode::Sfu);
        assert_eq!(room.switch_mode(RoomMode::Sfu), Ok(RoomMode::Sfu));
    }

    #[test]
    fn multiple_participants_can_join_active_room() {
        let mut room = Room::new("multi-conf", RoomMode::Sfu);
        assert_eq!(room.state, RoomState::Idle);

        for i in 0..10 {
            let p = Participant::new(crate::participant::UserId::new(), format!("user_{i}"), Role::Participant);
            room.add_participant(p).expect("subsequent participants must be able to join an active room");
            assert_eq!(room.state, RoomState::Active);
            assert_eq!(room.participant_count(), i + 1);
        }
    }
}

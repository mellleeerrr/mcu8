// Сессия (session lifecycle FSM).
//
// Автомат жизненного цикла одной сигнальной сессии. Соответствует
// фазе 2 DEVELOPMENT_PLAN:
//
//   Disconnected → Authenticating → Connected → Joining → InRoom
//   → Negotiating → MediaActive
//
// с поддержкой reconnect (resume token, TTL 60s) и graceful leave.
//
// Ядро определяет только **закономерные** переходы: метод
// [`Session::transition`] принимает текущее состояние + событие и
// возвращает новое состояние или [`SessionError`]. Все переходы,
// включая незаконные, покрыты unit-тестами.

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

/// Идентикатор сессии (UUID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(Uuid);

impl PartialOrd for SessionId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for SessionId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl SessionId {
    /// Новый глобально уникальный идентификатор сессии.
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

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::str::FromStr for SessionId {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// Состояния жизненного цикла сессии.
///
/// См. ARCHITECTURE §2 и Phase 2 (DEVELOPMENT_PLAN line 117):
/// `Disconnected → Authenticating → Connected → Joining → InRoom →
/// Negotiating → MediaActive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    /// Сессия не установлена (начальное/конечное состояние).
    Disconnected,
    /// Аутентификация в процессе (credentials проверяются).
    Authenticating,
    /// Аутентифицирована, соединение установлено.
    Connected,
    /// Запрос join отправлен, ожидание подтверждения комнаты.
    Joining,
    /// В комнате, но медиа ещё не активированы.
    InRoom,
    /// Переговоры медиа (SDP offer/answer).
    Negotiating,
    /// Медиа активно (RTP/RTCP течёт).
    MediaActive,
}

impl SessionState {
    /// Имя состояния (для логов и wire-форматов).
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionState::Disconnected => "disconnected",
            SessionState::Authenticating => "authenticating",
            SessionState::Connected => "connected",
            SessionState::Joining => "joining",
            SessionState::InRoom => "in_room",
            SessionState::Negotiating => "negotiating",
            SessionState::MediaActive => "media_active",
        }
    }
}

impl std::fmt::Display for SessionState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// События, которые вызывают переходы сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionEvent {
    /// Инициализировать соединение (client opens transport).
    Connect,
    /// Начать аутентификацию (credentials sent).
    Authenticate,
    /// Аутентификация успешна.
    AuthOk,
    /// Аутентификация не удалась.
    AuthFailed,
    /// Начать join (JoinRoom sent).
    Join,
    /// Подтверждение join (JoinRoomResult received).
    Joined,
    /// Ошибка join.
    JoinFailed,
    /// Начать медиа-переговоры (SDP offer sent).
    StartNegotiation,
    /// Переговоры завершены (SDP answer received).
    Negotiated,
    /// Ошибка переговоров.
    NegotiationFailed,
    /// Медиа активно (first RTP).
    MediaStarted,
    /// Медиа остановлено.
    MediaStopped,
    /// Клиент выходит из комнаты (LeaveRoom).
    LeaveRoom,
    /// Клиент обрывает соединение (transport closed).
    Disconnect,
    /// Resume (reconnect with resume token, TTL 60s).
    Resume,
    /// Resume отклонён (token expired/invalid).
    ResumeFailed,
}

impl std::fmt::Display for SessionEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            SessionEvent::Connect => "connect",
            SessionEvent::Authenticate => "authenticate",
            SessionEvent::AuthOk => "auth_ok",
            SessionEvent::AuthFailed => "auth_failed",
            SessionEvent::Join => "join",
            SessionEvent::Joined => "joined",
            SessionEvent::JoinFailed => "join_failed",
            SessionEvent::StartNegotiation => "start_negotiation",
            SessionEvent::Negotiated => "negotiated",
            SessionEvent::NegotiationFailed => "negotiation_failed",
            SessionEvent::MediaStarted => "media_started",
            SessionEvent::MediaStopped => "media_stopped",
            SessionEvent::LeaveRoom => "leave_room",
            SessionEvent::Disconnect => "disconnect",
            SessionEvent::Resume => "resume",
            SessionEvent::ResumeFailed => "resume_failed",
        };
        f.write_str(s)
    }
}

/// Ошибка перехода сессии.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum SessionError {
    /// Переход невозможен из текущего состояния.
    #[error("illegal session transition: from {from} on {event}")]
    IllegalTransition { from: SessionState, event: SessionEvent },
    /// Resume отклонён (token expired/invalid).
    #[error("resume rejected: {0}")]
    ResumeRejected(String),
}

/// Сессия: автомат жизненного цикла (FSM).
///
/// Ядро не хранит транспорт (Sans-I/O, ADR-0016): сессия — чистое
/// состояние + правила переходов. Транспорт (WS/WebTransport) —
/// адаптер, который вызывает [`Session::transition`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    /// Идентикатор сессии.
    pub id: SessionId,
    /// Текущее состояние.
    pub state: SessionState,
}

impl Session {
    /// Создаёт новую сессию в состоянии [`SessionState::Disconnected`].
    pub fn new() -> Self {
        Self { id: SessionId::new(), state: SessionState::Disconnected }
    }

    /// Возвращает текущее состояние.
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Проверяет, является ли состояние terminal (не принимает события).
    pub fn is_terminal(&self) -> bool {
        self.state == SessionState::Disconnected
    }

    /// Применяет событие к текущему состоянию.
    ///
    /// Возвращает новое состояние или [`SessionError`]. Если событие
    /// законное, `self.state` **не меняется** — возвращаемое состояние
    /// должно быть сохранено вызывающим (pure FSM, easy to test).
    ///
    /// Правила переходов (см. таблица в docs):
    ///
    /// | from           | event             | to             |
    /// |----------------|-------------------|----------------|
    /// | Disconnected   | Connect           | Authenticating |
    /// | Disconnected   | Resume            | Authenticating*|
    /// | Authenticating | AuthOk            | Connected      |
    /// | Authenticating | AuthFailed        | Disconnected   |
    /// | Authenticating | Resume            | Authenticating |
    /// | Connected      | Join              | Joining        |
    /// | Connected      | Disconnect        | Disconnected   |
    /// | Joining        | Joined            | InRoom         |
    /// | Joining        | JoinFailed        | Connected      |
    /// | InRoom         | StartNegotiation  | Negotiating    |
    /// | InRoom         | LeaveRoom         | Connected      |
    /// | InRoom         | Disconnect        | Disconnected   |
    /// | Negotiating    | Negotiated        | MediaActive    |
    /// | Negotiating    | NegotiationFailed | InRoom         |
    /// | Negotiating    | Disconnect        | Disconnected   |
    /// | MediaActive    | MediaStopped      | InRoom         |
    /// | MediaActive    | LeaveRoom         | Connected      |
    /// | MediaActive    | Disconnect        | Disconnected   |
    ///
    /// * `Resume` из `Disconnected` — reconnect path (resume token).
    ///   Отклонение (TTL expired) обрабатывается вызывающим через
    ///   [`SessionError::ResumeRejected`].
    pub fn transition(&self, event: SessionEvent) -> Result<SessionState, SessionError> {
        let next = match (self.state, event) {
            // Disconnected: только connect или resume (reconnect path)
            (SessionState::Disconnected, SessionEvent::Connect) => SessionState::Authenticating,
            (SessionState::Disconnected, SessionEvent::Resume) => SessionState::Authenticating,

            // Authenticating: auth ok/failed, resume (retry)
            (SessionState::Authenticating, SessionEvent::AuthOk) => SessionState::Connected,
            (SessionState::Authenticating, SessionEvent::AuthFailed) => SessionState::Disconnected,
            (SessionState::Authenticating, SessionEvent::Resume) => SessionState::Authenticating,

            // Connected: join or disconnect
            (SessionState::Connected, SessionEvent::Join) => SessionState::Joining,
            (SessionState::Connected, SessionEvent::Disconnect) => SessionState::Disconnected,

            // Joining: joined or failed
            (SessionState::Joining, SessionEvent::Joined) => SessionState::InRoom,
            (SessionState::Joining, SessionEvent::JoinFailed) => SessionState::Connected,
            (SessionState::Joining, SessionEvent::Disconnect) => SessionState::Disconnected,

            // InRoom: start negotiation, leave, disconnect
            (SessionState::InRoom, SessionEvent::StartNegotiation) => SessionState::Negotiating,
            (SessionState::InRoom, SessionEvent::LeaveRoom) => SessionState::Connected,
            (SessionState::InRoom, SessionEvent::Disconnect) => SessionState::Disconnected,

            // Negotiating: negotiated, failed, disconnect
            (SessionState::Negotiating, SessionEvent::Negotiated) => SessionState::MediaActive,
            (SessionState::Negotiating, SessionEvent::NegotiationFailed) => SessionState::InRoom,
            (SessionState::Negotiating, SessionEvent::Disconnect) => SessionState::Disconnected,

            // MediaActive: renegotiation, media stopped, leave, disconnect
            (SessionState::MediaActive, SessionEvent::StartNegotiation) => SessionState::Negotiating,
            (SessionState::MediaActive, SessionEvent::MediaStopped) => SessionState::InRoom,
            (SessionState::MediaActive, SessionEvent::LeaveRoom) => SessionState::Connected,
            (SessionState::MediaActive, SessionEvent::Disconnect) => SessionState::Disconnected,

            // Everything else is illegal.
            (from, event) => {
                return Err(SessionError::IllegalTransition { from, event });
            }
        };
        Ok(next)
    }

    /// Применяет событие и обновляет `self.state` на месте.
    ///
    /// Convenience wrapper вокруг [`Session::transition`].
    pub fn transition_in_place(&mut self, event: SessionEvent) -> Result<(), SessionError> {
        let next = self.transition(event)?;
        self.state = next;
        Ok(())
    }
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- SessionId ---
    #[test]
    fn session_id_roundtrip() {
        let id = SessionId::new();
        let s = id.to_string();
        let parsed: SessionId = s.parse().expect("parse");
        assert_eq!(id, parsed);
    }

    // --- SessionState display ---
    #[test]
    fn state_display() {
        assert_eq!(SessionState::Disconnected.to_string(), "disconnected");
        assert_eq!(SessionState::Authenticating.to_string(), "authenticating");
        assert_eq!(SessionState::Connected.to_string(), "connected");
        assert_eq!(SessionState::Joining.to_string(), "joining");
        assert_eq!(SessionState::InRoom.to_string(), "in_room");
        assert_eq!(SessionState::Negotiating.to_string(), "negotiating");
        assert_eq!(SessionState::MediaActive.to_string(), "media_active");
    }

    // --- Legal transitions: happy path ---
    #[test]
    fn full_happy_path() {
        // Full lifecycle: Disconnected → Authenticating → Connected →
        // Joining → InRoom → Negotiating → MediaActive → (leave) Connected
        // → Disconnected.
        let mut s = Session::new();
        assert_eq!(s.state(), SessionState::Disconnected);

        assert_eq!(s.transition_in_place(SessionEvent::Connect), Ok(()));
        assert_eq!(s.state(), SessionState::Authenticating);

        assert_eq!(s.transition_in_place(SessionEvent::AuthOk), Ok(()));
        assert_eq!(s.state(), SessionState::Connected);

        assert_eq!(s.transition_in_place(SessionEvent::Join), Ok(()));
        assert_eq!(s.state(), SessionState::Joining);

        assert_eq!(s.transition_in_place(SessionEvent::Joined), Ok(()));
        assert_eq!(s.state(), SessionState::InRoom);

        assert_eq!(s.transition_in_place(SessionEvent::StartNegotiation), Ok(()));
        assert_eq!(s.state(), SessionState::Negotiating);

        assert_eq!(s.transition_in_place(SessionEvent::Negotiated), Ok(()));
        assert_eq!(s.state(), SessionState::MediaActive);

        // Leave room: back to Connected
        assert_eq!(s.transition_in_place(SessionEvent::LeaveRoom), Ok(()));
        assert_eq!(s.state(), SessionState::Connected);

        // Disconnect: back to Disconnected (terminal)
        assert_eq!(s.transition_in_place(SessionEvent::Disconnect), Ok(()));
        assert_eq!(s.state(), SessionState::Disconnected);
        assert!(s.is_terminal());
    }

    fn illegal(from: SessionState, event: SessionEvent) -> SessionError {
        SessionError::IllegalTransition { from, event }
    }

    #[test]
    fn connect_from_disconnected() {
        let s = Session::new();
        assert_eq!(s.transition(SessionEvent::Connect), Ok(SessionState::Authenticating));
    }

    #[test]
    fn auth_ok_from_authenticating() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        assert_eq!(s.state(), SessionState::Authenticating);
        assert_eq!(s.transition(SessionEvent::AuthOk), Ok(SessionState::Connected));
    }

    #[test]
    fn auth_failed_returns_disconnected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        assert_eq!(s.transition(SessionEvent::AuthFailed), Ok(SessionState::Disconnected));
    }

    #[test]
    fn resume_retry_in_authenticating() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        // Resume while authenticating: stay authenticating
        assert_eq!(s.transition(SessionEvent::Resume), Ok(SessionState::Authenticating));
    }

    #[test]
    fn join_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(s.state(), SessionState::Connected);
        assert_eq!(s.transition(SessionEvent::Join), Ok(SessionState::Joining));
    }

    #[test]
    fn joined_from_joining() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        assert_eq!(s.state(), SessionState::Joining);
        assert_eq!(s.transition(SessionEvent::Joined), Ok(SessionState::InRoom));
    }

    #[test]
    fn join_failed_returns_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        assert_eq!(s.transition(SessionEvent::JoinFailed), Ok(SessionState::Connected));
    }

    #[test]
    fn disconnect_from_joining() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        assert_eq!(s.transition(SessionEvent::Disconnect), Ok(SessionState::Disconnected));
    }

    #[test]
    fn start_negotiation_from_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        assert_eq!(s.state(), SessionState::InRoom);
        assert_eq!(s.transition(SessionEvent::StartNegotiation), Ok(SessionState::Negotiating));
    }

    #[test]
    fn leave_room_from_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        assert_eq!(s.transition(SessionEvent::LeaveRoom), Ok(SessionState::Connected));
    }

    #[test]
    fn disconnect_from_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        assert_eq!(s.transition(SessionEvent::Disconnect), Ok(SessionState::Disconnected));
    }

    #[test]
    fn negotiated_to_media_active() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        s.transition_in_place(SessionEvent::StartNegotiation).unwrap();
        assert_eq!(s.state(), SessionState::Negotiating);
        assert_eq!(s.transition(SessionEvent::Negotiated), Ok(SessionState::MediaActive));
    }

    #[test]
    fn negotiation_failed_back_to_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        s.transition_in_place(SessionEvent::StartNegotiation).unwrap();
        assert_eq!(s.transition(SessionEvent::NegotiationFailed), Ok(SessionState::InRoom));
    }

    #[test]
    fn disconnect_from_negotiating() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        s.transition_in_place(SessionEvent::StartNegotiation).unwrap();
        assert_eq!(s.transition(SessionEvent::Disconnect), Ok(SessionState::Disconnected));
    }

    #[test]
    fn media_stopped_back_to_in_room() {
        let mut s = Session::new();
        // Fast-forward to MediaActive
        for ev in [
            SessionEvent::Connect,
            SessionEvent::AuthOk,
            SessionEvent::Join,
            SessionEvent::Joined,
            SessionEvent::StartNegotiation,
            SessionEvent::Negotiated,
        ] {
            s.transition_in_place(ev).unwrap();
        }
        assert_eq!(s.state(), SessionState::MediaActive);
        assert_eq!(s.transition(SessionEvent::MediaStopped), Ok(SessionState::InRoom));
    }

    #[test]
    fn leave_room_from_media_active() {
        let mut s = Session::new();
        for ev in [
            SessionEvent::Connect,
            SessionEvent::AuthOk,
            SessionEvent::Join,
            SessionEvent::Joined,
            SessionEvent::StartNegotiation,
            SessionEvent::Negotiated,
        ] {
            s.transition_in_place(ev).unwrap();
        }
        assert_eq!(s.transition(SessionEvent::LeaveRoom), Ok(SessionState::Connected));
    }

    #[test]
    fn disconnect_from_media_active() {
        let mut s = Session::new();
        for ev in [
            SessionEvent::Connect,
            SessionEvent::AuthOk,
            SessionEvent::Join,
            SessionEvent::Joined,
            SessionEvent::StartNegotiation,
            SessionEvent::Negotiated,
        ] {
            s.transition_in_place(ev).unwrap();
        }
        assert_eq!(s.transition(SessionEvent::Disconnect), Ok(SessionState::Disconnected));
    }

    // --- Reconnect path ---
    #[test]
    fn resume_from_disconnected() {
        let s = Session::new();
        assert_eq!(s.state(), SessionState::Disconnected);
        assert_eq!(s.transition(SessionEvent::Resume), Ok(SessionState::Authenticating));
    }

    // --- Illegal transitions (exhaustive) ---
    // We test every (from, event) pair that is NOT in the legal table.

    #[test]
    fn illegal_connect_from_authenticating() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        assert_eq!(
            s.transition(SessionEvent::Connect),
            Err(illegal(SessionState::Authenticating, SessionEvent::Connect))
        );
    }

    #[test]
    fn illegal_connect_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(s.transition(SessionEvent::Connect), Err(illegal(SessionState::Connected, SessionEvent::Connect)));
    }

    #[test]
    fn illegal_authenticate_from_disconnected() {
        let s = Session::new();
        assert_eq!(
            s.transition(SessionEvent::Authenticate),
            Err(illegal(SessionState::Disconnected, SessionEvent::Authenticate))
        );
    }

    #[test]
    fn illegal_auth_ok_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(s.transition(SessionEvent::AuthOk), Err(illegal(SessionState::Connected, SessionEvent::AuthOk)));
    }

    #[test]
    fn illegal_auth_failed_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(
            s.transition(SessionEvent::AuthFailed),
            Err(illegal(SessionState::Connected, SessionEvent::AuthFailed))
        );
    }

    #[test]
    fn illegal_join_from_disconnected() {
        let s = Session::new();
        assert_eq!(s.transition(SessionEvent::Join), Err(illegal(SessionState::Disconnected, SessionEvent::Join)));
    }

    #[test]
    fn illegal_joined_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(s.transition(SessionEvent::Joined), Err(illegal(SessionState::Connected, SessionEvent::Joined)));
    }

    #[test]
    fn illegal_start_negotiation_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(
            s.transition(SessionEvent::StartNegotiation),
            Err(illegal(SessionState::Connected, SessionEvent::StartNegotiation))
        );
    }

    #[test]
    fn illegal_negotiated_from_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        assert_eq!(
            s.transition(SessionEvent::Negotiated),
            Err(illegal(SessionState::InRoom, SessionEvent::Negotiated))
        );
    }

    #[test]
    fn illegal_media_started_from_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        assert_eq!(
            s.transition(SessionEvent::MediaStarted),
            Err(illegal(SessionState::InRoom, SessionEvent::MediaStarted))
        );
    }

    #[test]
    fn illegal_media_stopped_from_in_room() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        s.transition_in_place(SessionEvent::Join).unwrap();
        s.transition_in_place(SessionEvent::Joined).unwrap();
        assert_eq!(
            s.transition(SessionEvent::MediaStopped),
            Err(illegal(SessionState::InRoom, SessionEvent::MediaStopped))
        );
    }

    #[test]
    fn illegal_disconnect_from_disconnected() {
        let s = Session::new();
        assert_eq!(
            s.transition(SessionEvent::Disconnect),
            Err(illegal(SessionState::Disconnected, SessionEvent::Disconnect))
        );
    }

    #[test]
    fn illegal_leave_room_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(
            s.transition(SessionEvent::LeaveRoom),
            Err(illegal(SessionState::Connected, SessionEvent::LeaveRoom))
        );
    }

    #[test]
    fn illegal_resume_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(s.transition(SessionEvent::Resume), Err(illegal(SessionState::Connected, SessionEvent::Resume)));
    }

    #[test]
    fn illegal_authenticate_from_connected() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(
            s.transition(SessionEvent::Authenticate),
            Err(illegal(SessionState::Connected, SessionEvent::Authenticate))
        );
    }

    // --- is_terminal / in-place ---
    #[test]
    fn initial_session_is_terminal() {
        let s = Session::new();
        assert!(s.is_terminal());
    }

    #[test]
    fn transition_in_place_updates_state() {
        let mut s = Session::new();
        s.transition_in_place(SessionEvent::Connect).unwrap();
        assert_eq!(s.state(), SessionState::Authenticating);
        assert!(!s.is_terminal());
        s.transition_in_place(SessionEvent::AuthOk).unwrap();
        assert_eq!(s.state(), SessionState::Connected);
    }

    #[test]
    fn transition_pure_does_not_mutate() {
        let s = Session::new();
        let _ = s.transition(SessionEvent::Connect);
        // pure transition: state unchanged
        assert_eq!(s.state(), SessionState::Disconnected);
    }

    #[test]
    fn resume_rejected_error_display() {
        let e = SessionError::ResumeRejected("ttl expired".into());
        assert_eq!(e.to_string(), "resume rejected: ttl expired");
    }
}

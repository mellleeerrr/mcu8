//! Session FSM сигнального соединения.
//!
//! Основание: `docs/architecture/SIGNALING_PROTOCOL.md` §11 (Session FSM),
//! §12 (Disconnect handling). Каждый переход — `tracing::event!` + метрика.
//!
//! Состояния и переходы (SIGNALING_PROTOCOL §11):
//! ```text
//! Disconnected → Authenticating → Connected → Joining → InRoom
//!   InRoom → Negotiating → MediaActive ↔ InRoom
//!   InRoom → Leaving → Disconnected
//!   any → Disconnected (timeout / error / close)
//! ```

use thiserror::Error;

/// Состояния сессии сигнального соединения (SIGNALING_PROTOCOL §11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Disconnected,
    Authenticating,
    Connected,
    Joining,
    InRoom,
    Negotiating,
    MediaActive,
    Reconnecting,
    Leaving,
}

impl SessionState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Authenticating => "authenticating",
            Self::Connected => "connected",
            Self::Joining => "joining",
            Self::InRoom => "in_room",
            Self::Negotiating => "negotiating",
            Self::MediaActive => "media_active",
            Self::Reconnecting => "reconnecting",
            Self::Leaving => "leaving",
        }
    }
}

/// События, двигающие FSM (триггеры из SIGNALING_PROTOCOL §11/§12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEvent {
    /// WS connect + ClientHello.
    Hello,
    /// JWT валиден, ServerHello отправлен.
    AuthOk,
    /// JoinRoom получено.
    JoinRoom,
    /// JoinRoomResult отправлен.
    Joined,
    /// SdpOffer получено.
    SdpOffer,
    /// ICE/DTLS started (после SdpAnswer).
    IceStarted,
    /// IceComplete (success).
    IceComplete,
    /// Потеря сетевого соединения / начало переподключения.
    ConnectionLost,
    /// Успешное восстановление сессии (resume с nonce).
    Resumed,
    /// LeaveRoom получено.
    LeaveRoom,
    /// Left подтверждено, соединение закрывается.
    Left,
    /// timeout / error / close (из любого состояния).
    Disconnect,
}

/// Ошибка недопустимого перехода.
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid transition {from:?} -({event:?})-> {to:?}: {reason}")]
pub struct InvalidTransition {
    pub from: SessionState,
    pub to: Option<SessionState>,
    pub event: SessionEvent,
    pub reason: &'static str,
}

/// Сессия сигнального соединения.
pub struct Session {
    state: SessionState,
}

impl Session {
    /// Новая сессия всегда начинается в `Disconnected`.
    #[must_use]
    pub fn new() -> Self {
        Self { state: SessionState::Disconnected }
    }

    /// Текущее состояние.
    #[must_use]
    pub fn state(&self) -> SessionState {
        self.state
    }

    /// Применяет событие: если переход допустим — меняет состояние и
    /// логирует; иначе — возвращает `Err(InvalidTransition)` (состояние не меняется).
    pub fn apply(&mut self, event: SessionEvent) -> Result<SessionState, InvalidTransition> {
        let from = self.state;
        let to = match (from, event) {
            // §11: Disconnected → Authenticating (WS connect, ClientHello).
            (SessionState::Disconnected, SessionEvent::Hello) => Some(SessionState::Authenticating),
            // §11: Authenticating → Connected (JWT valid, ServerHello sent).
            (SessionState::Authenticating, SessionEvent::AuthOk) => Some(SessionState::Connected),
            // §11: Connected → Joining (JoinRoom received).
            (SessionState::Connected, SessionEvent::JoinRoom) => Some(SessionState::Joining),
            // §11: Joining → InRoom (JoinRoomResult sent).
            (SessionState::Joining, SessionEvent::Joined) => Some(SessionState::InRoom),
            // §11: InRoom → Negotiating (SdpOffer received).
            (SessionState::InRoom, SessionEvent::SdpOffer) => Some(SessionState::Negotiating),
            // Negotiating → MediaActive (IceComplete success).
            (SessionState::Negotiating, SessionEvent::IceComplete) => Some(SessionState::MediaActive),
            // MediaActive → Negotiating (re-negotiation, e.g. new SdpOffer).
            (SessionState::MediaActive, SessionEvent::SdpOffer) => Some(SessionState::Negotiating),
            // MediaActive → InRoom (ICE resumption settled without full renegotiation).
            (SessionState::MediaActive, SessionEvent::IceStarted) => Some(SessionState::InRoom),

            // Reconnect / Resume
            (SessionState::InRoom, SessionEvent::ConnectionLost)
            | (SessionState::Negotiating, SessionEvent::ConnectionLost)
            | (SessionState::MediaActive, SessionEvent::ConnectionLost) => Some(SessionState::Reconnecting),
            (SessionState::Connected, SessionEvent::Resumed) => Some(SessionState::InRoom),
            (SessionState::Reconnecting, SessionEvent::Resumed) => Some(SessionState::InRoom),

            // Корректный выход из ЛЮБОГО активного состояния комнаты (Joining, InRoom, Negotiating, MediaActive, Reconnecting):
            (SessionState::Joining, SessionEvent::LeaveRoom)
            | (SessionState::InRoom, SessionEvent::LeaveRoom)
            | (SessionState::Negotiating, SessionEvent::LeaveRoom)
            | (SessionState::MediaActive, SessionEvent::LeaveRoom)
            | (SessionState::Reconnecting, SessionEvent::LeaveRoom) => Some(SessionState::Leaving),

            // Leaving → Disconnected (left confirmed).
            (SessionState::Leaving, SessionEvent::Left) => Some(SessionState::Disconnected),
            // §11: any → Disconnected (timeout / error / close).
            (_, SessionEvent::Disconnect) => Some(SessionState::Disconnected),
            // Всё остальное — недопустимо.
            _ => None,
        };

        match to {
            Some(next) => {
                tracing::event!(
                    tracing::Level::INFO,
                    from = from.as_str(),
                    to = next.as_str(),
                    event = ?event,
                    "session state transition"
                );
                self.state = next;
                Ok(next)
            }
            None => Err(InvalidTransition { from, to: None, event, reason: "no rule for this state+event" }),
        }
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

    fn into_in_room() -> Session {
        let mut s = Session::new();
        s.apply(SessionEvent::Hello).unwrap();
        s.apply(SessionEvent::AuthOk).unwrap();
        s.apply(SessionEvent::JoinRoom).unwrap();
        s.apply(SessionEvent::Joined).unwrap();
        assert_eq!(s.state(), SessionState::InRoom);
        s
    }

    #[test]
    fn starts_disconnected() {
        assert_eq!(Session::new().state(), SessionState::Disconnected);
    }

    #[test]
    fn happy_path_to_in_room() {
        let mut s = Session::new();
        assert_eq!(s.apply(SessionEvent::Hello).unwrap(), SessionState::Authenticating);
        assert_eq!(s.apply(SessionEvent::AuthOk).unwrap(), SessionState::Connected);
        assert_eq!(s.apply(SessionEvent::JoinRoom).unwrap(), SessionState::Joining);
        assert_eq!(s.apply(SessionEvent::Joined).unwrap(), SessionState::InRoom);
    }

    #[test]
    fn negotiation_and_media_active() {
        let mut s = into_in_room();
        assert_eq!(s.apply(SessionEvent::SdpOffer).unwrap(), SessionState::Negotiating);
        assert_eq!(s.apply(SessionEvent::IceComplete).unwrap(), SessionState::MediaActive);
        // MediaActive → Negotiating (re-negotiation).
        assert_eq!(s.apply(SessionEvent::SdpOffer).unwrap(), SessionState::Negotiating);
    }

    #[test]
    fn leave_flow_from_in_room() {
        let mut s = into_in_room();
        assert_eq!(s.apply(SessionEvent::LeaveRoom).unwrap(), SessionState::Leaving);
        assert_eq!(s.apply(SessionEvent::Left).unwrap(), SessionState::Disconnected);
    }

    #[test]
    fn leave_flow_from_media_active() {
        let mut s = into_in_room();
        s.apply(SessionEvent::SdpOffer).unwrap();
        s.apply(SessionEvent::IceComplete).unwrap();
        assert_eq!(s.state(), SessionState::MediaActive);
        assert_eq!(s.apply(SessionEvent::LeaveRoom).unwrap(), SessionState::Leaving);
        assert_eq!(s.apply(SessionEvent::Left).unwrap(), SessionState::Disconnected);
    }

    #[test]
    fn disconnect_from_any_state() {
        // Доводим до каждого не-terminal-состояния и проверяем Disconnect → Disconnected.
        let targets: Vec<SessionState> = vec![
            SessionState::Disconnected,
            SessionState::Authenticating,
            SessionState::Connected,
            SessionState::Joining,
            SessionState::InRoom,
            SessionState::Negotiating,
            SessionState::MediaActive,
            SessionState::Reconnecting,
            SessionState::Leaving,
        ];
        for target in targets {
            let mut s = match target {
                SessionState::Disconnected => Session::new(),
                SessionState::Authenticating => {
                    let mut s = Session::new();
                    s.apply(SessionEvent::Hello).unwrap();
                    s
                }
                SessionState::Connected
                | SessionState::Joining
                | SessionState::InRoom
                | SessionState::MediaActive
                | SessionState::Leaving => into_in_room(),
                SessionState::Negotiating => {
                    let mut s = into_in_room();
                    s.apply(SessionEvent::SdpOffer).unwrap();
                    s
                }
                SessionState::Reconnecting => {
                    let mut s = into_in_room();
                    s.apply(SessionEvent::ConnectionLost).unwrap();
                    s
                }
            };
            assert_eq!(s.apply(SessionEvent::Disconnect).unwrap(), SessionState::Disconnected);
        }
    }

    #[test]
    fn leave_from_negotiating_and_reconnecting() {
        // Leave from Negotiating
        let mut s1 = into_in_room();
        s1.apply(SessionEvent::SdpOffer).unwrap();
        assert_eq!(s1.state(), SessionState::Negotiating);
        assert_eq!(s1.apply(SessionEvent::LeaveRoom).unwrap(), SessionState::Leaving);
        assert_eq!(s1.apply(SessionEvent::Left).unwrap(), SessionState::Disconnected);

        // Leave from Reconnecting
        let mut s2 = into_in_room();
        s2.apply(SessionEvent::ConnectionLost).unwrap();
        assert_eq!(s2.state(), SessionState::Reconnecting);
        assert_eq!(s2.apply(SessionEvent::LeaveRoom).unwrap(), SessionState::Leaving);
        assert_eq!(s2.apply(SessionEvent::Left).unwrap(), SessionState::Disconnected);
    }

    #[test]
    fn reconnect_and_resume_cycle() {
        let mut s = into_in_room();
        assert_eq!(s.apply(SessionEvent::ConnectionLost).unwrap(), SessionState::Reconnecting);
        assert_eq!(s.apply(SessionEvent::Resumed).unwrap(), SessionState::InRoom);
    }

    #[test]
    fn illegal_transitions_rejected() {
        let mut s = Session::new();
        // Disconnected + AuthOk — не допустимо (нужен сначала Hello).
        assert!(matches!(s.apply(SessionEvent::AuthOk), Err(InvalidTransition { .. })));
        assert_eq!(s.state(), SessionState::Disconnected);

        // Connected + SdpOffer — не допустимо (нужен InRoom).
        let mut s2 = Session::new();
        s2.apply(SessionEvent::Hello).unwrap();
        s2.apply(SessionEvent::AuthOk).unwrap();
        assert!(matches!(s2.apply(SessionEvent::SdpOffer), Err(InvalidTransition { .. })));

        // Disconnected + IceComplete — не допустимо.
        assert!(matches!(s.apply(SessionEvent::IceComplete), Err(InvalidTransition { .. })));
    }

    #[test]
    fn error_is_displayable() {
        let mut s = Session::new();
        let e = s.apply(SessionEvent::IceComplete).unwrap_err();
        assert!(e.to_string().contains("invalid transition"));
        assert_eq!(e.from, SessionState::Disconnected);
        assert!(e.to.is_none());
    }
}

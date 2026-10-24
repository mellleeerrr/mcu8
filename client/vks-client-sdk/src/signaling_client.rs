//! Signaling клиент SDK (WS/WebTransport) — join/hello/leave + auto-reconnect.
//!
//! Основание: `docs/architecture/SIGNALING_PROTOCOL.md` §2 (ClientHello),
//! §3 (JoinRoom), §6 (LeaveRoom), §7 (Reconnect).
//!
//! Транспорт — порт `SignalingTransport` (WS или WebTransport; Q5). Сейчас —
//! `MockSignaling` для тестов; реальная impl в `ws.rs`/`wt.rs` (фаза 8).

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// Ответ сервера на ClientHello.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerHello {
    /// Серверный version (для feature detection).
    pub server_version: String,
    /// Session id (ULID) — для resume.
    pub session_id: String,
    /// Server-time (unix-ms).
    pub server_time_ms: u64,
}

/// Результат JoinRoom (server → client).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JoinRoomResult {
    /// Room id.
    pub room_id: String,
    /// Participant id (для этого клиента).
    pub participant_id: String,
    /// Nonce для resume (TTL 60s, §7).
    pub join_nonce: String,
    /// Список участников (для UI).
    pub participants: Vec<String>,
    /// Снимок раскладки комнаты на момент входа (прото-раскладка сервера).
    pub layout: Option<vks_proto::signaling::Layout>,
}

/// Ошибка signaling.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SignalingError {
    #[error("auth failed: {0}")]
    AuthFailed(String),
    #[error("room not found: {0}")]
    RoomNotFound(String),
    #[error("transport: {0}")]
    Transport(String),
}

/// Обработчик push-сообщений сервера.
///
/// Сервер шлёт push без запроса: `LayoutChanged` (смена раскладки),
/// `RoomModeChanged` (SFU ↔ MCU), `SpeakingStateChanged` и т.д. Транспорт
/// вызывает обработчик; SDK разбирает сообщения и обновляет своё состояние.
pub trait PushHandler: Send + Sync {
    /// Сообщение от сервера.
    fn on_message(&self, message: &vks_proto::signaling::SignalingMessage);
}

/// Любое замыкание `Fn(&SignalingMessage)` — уже обработчик push-сообщений.
impl<F> PushHandler for F
where
    F: Fn(&vks_proto::signaling::SignalingMessage) + Send + Sync,
{
    fn on_message(&self, message: &vks_proto::signaling::SignalingMessage) {
        self(message);
    }
}

/// Signaling transport-порт (WS или WebTransport; Q5).
#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
pub trait SignalingTransport: Send + Sync {
    /// Отправляет ClientHello с resume_nonce (для сессионного resume).
    async fn client_hello_with_resume(
        &self,
        token: &str,
        _resume_nonce: Option<&str>,
    ) -> Result<ServerHello, SignalingError> {
        self.client_hello(token).await
    }
    /// Отправляет ClientHello (token) → ServerHello.
    async fn client_hello(&self, token: &str) -> Result<ServerHello, SignalingError>;
    /// Отправляет JoinRoom (room, name) → JoinRoomResult.
    async fn join_room(&self, room_id: &str, display_name: &str) -> Result<JoinRoomResult, SignalingError>;
    /// Отправляет LeaveRoom.
    async fn leave_room(&self, room_id: &str) -> Result<(), SignalingError>;
    /// Отправляет SdpOffer → SdpAnswer (SDP exchange).
    async fn send_sdp_offer(&self, _sdp: &str) -> Result<String, SignalingError> {
        Ok(String::new())
    }
    /// Отправляет IceCandidate.
    async fn send_ice_candidate(&self, _candidate: &str) -> Result<(), SignalingError> {
        Ok(())
    }
    /// Регистрирует получателя push-сообщений сервера.
    ///
    /// Вызывается приложением через [`SignalingClient::set_push_handler`];
    /// транспорт обязан звать `PushHandler::on_message` при получении push.
    fn set_push_handler(&self, _handler: Arc<dyn PushHandler>) {}
}

/// Signaling клиент (обёртка над transport + backoff для auto-reconnect).
pub struct SignalingClient {
    transport: Arc<dyn SignalingTransport>,
    // backoff для auto-reconnect (exponential 1s→16s, max 5 — §7).
    reconnect_delay: Duration,
    reconnect_attempts: u32,
}

impl SignalingClient {
    #[must_use]
    pub fn new(transport: Arc<dyn SignalingTransport>) -> Self {
        Self { transport: transport.clone(), reconnect_delay: Duration::from_secs(1), reconnect_attempts: 0 }
    }

    /// Создаёт клиент с конкретным transport (для тестов — mock).
    #[must_use]
    pub fn with_transport(transport: Arc<dyn SignalingTransport>) -> Self {
        Self::new(transport)
    }

    /// Регистрирует обработчик push-сообщений сервера.
    ///
    /// Приложение получает `LayoutChanged`/`RoomModeChanged` и передаёт их в
    /// конференцию (`Conference::on_layout_changed` / `on_mode_changed`).
    /// Снимок раскладки на момент входа приходит в `JoinRoomResult.layout`,
    /// поэтому отдельного кэша в SDK не нужно.
    pub fn set_push_handler(&self, handler: Arc<dyn PushHandler>) {
        self.transport.set_push_handler(handler);
    }

    /// ClientHello → ServerHello (async).
    pub async fn hello_async(&self, token: &str) -> Result<ServerHello, SignalingError> {
        self.transport.client_hello(token).await
    }

    /// ClientHello с resume_nonce (async).
    pub async fn hello_with_resume_async(
        &self,
        token: &str,
        resume_nonce: Option<&str>,
    ) -> Result<ServerHello, SignalingError> {
        self.transport.client_hello_with_resume(token, resume_nonce).await
    }

    /// JoinRoom → JoinRoomResult (async).
    pub async fn join_room_async(&self, room_id: &str, display_name: &str) -> Result<JoinRoomResult, SignalingError> {
        self.transport.join_room(room_id, display_name).await
    }

    /// LeaveRoom (async).
    pub async fn leave_room_async(&self, room_id: &str) -> Result<(), SignalingError> {
        self.transport.leave_room(room_id).await
    }

    /// Отправка SdpOffer → SdpAnswer (async).
    pub async fn send_sdp_offer_async(&self, sdp: &str) -> Result<String, SignalingError> {
        self.transport.send_sdp_offer(sdp).await
    }

    /// Отправка IceCandidate (async).
    pub async fn send_ice_candidate_async(&self, candidate: &str) -> Result<(), SignalingError> {
        self.transport.send_ice_candidate(candidate).await
    }

    /// ClientHello → ServerHello (sync для обратной совместимости на native).
    pub fn hello(&mut self, token: &str) -> Result<ServerHello, SignalingError> {
        crate::sync_helper::block_on(self.hello_async(token))
    }

    /// JoinRoom → JoinRoomResult (sync для обратной совместимости на native).
    pub fn join_room(&mut self, room_id: &str, display_name: &str) -> Result<JoinRoomResult, SignalingError> {
        crate::sync_helper::block_on(self.join_room_async(room_id, display_name))
    }

    /// LeaveRoom (sync для обратной совместимости на native).
    pub fn leave_room(&mut self, room_id: &str) -> Result<(), SignalingError> {
        crate::sync_helper::block_on(self.leave_room_async(room_id))
    }

    /// Текущий backoff-таймер (для auto-reconnect).
    #[must_use]
    pub fn next_reconnect_delay(&mut self) -> Duration {
        if self.reconnect_attempts >= 5 {
            // исчерпали — возвращаем "max"
            Duration::from_secs(16)
        } else {
            let d = self.reconnect_delay;
            self.reconnect_delay = (self.reconnect_delay * 2).min(Duration::from_secs(16));
            self.reconnect_attempts += 1;
            d
        }
    }

    /// Сброс backoff (после успешного reconnect).
    pub fn reset_reconnect(&mut self) {
        self.reconnect_delay = Duration::from_secs(1);
        self.reconnect_attempts = 0;
    }
}

/// Mock transport (для тестов).
#[derive(Debug, Clone, Default)]
pub struct MockSignaling {
    pubok_hello: bool,
    pubok_join: bool,
}

impl MockSignaling {
    #[must_use]
    pub fn ok() -> Self {
        Self { pubok_hello: true, pubok_join: true }
    }

    /// Variant с empty `server_version` (для теста `BadServerHello`).
    #[must_use]
    pub fn empty_server_hello() -> Self {
        Self { pubok_hello: false, pubok_join: true }
    }
}

#[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
#[cfg_attr(not(target_arch = "wasm32"), async_trait)]
impl SignalingTransport for MockSignaling {
    async fn client_hello(&self, _token: &str) -> Result<ServerHello, SignalingError> {
        if self.pubok_hello {
            Ok(ServerHello {
                server_version: "1.0".into(),
                session_id: vks_common::ids::ulid_string(),
                server_time_ms: 0,
            })
        } else {
            Ok(ServerHello::default())
        }
    }

    async fn join_room(&self, room_id: &str, _display_name: &str) -> Result<JoinRoomResult, SignalingError> {
        if self.pubok_join {
            Ok(JoinRoomResult {
                room_id: room_id.into(),
                participant_id: "p1".into(),
                join_nonce: "nonce-1".into(),
                participants: vec!["p1".into()],
                layout: None,
            })
        } else {
            Err(SignalingError::RoomNotFound(room_id.into()))
        }
    }

    async fn leave_room(&self, _room_id: &str) -> Result<(), SignalingError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_ok() {
        let mut c = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        let h = c.hello("tok").unwrap();
        assert_eq!(h.server_version, "1.0");
        assert!(!h.session_id.is_empty());
    }

    #[test]
    fn join_room_ok() {
        let mut c = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        let r = c.join_room("r1", "alice").unwrap();
        assert_eq!(r.room_id, "r1");
        assert_eq!(r.participant_id, "p1");
        assert!(!r.join_nonce.is_empty());
    }

    #[test]
    fn leave_room_ok() {
        let mut c = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.leave_room("r1").unwrap();
    }

    #[test]
    fn empty_hello() {
        let mut c = SignalingClient::with_transport(Arc::new(MockSignaling::empty_server_hello()));
        let h = c.hello("tok").unwrap();
        assert!(h.server_version.is_empty());
    }

    #[test]
    fn backoff_sequence() {
        let mut c = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(1));
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(2));
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(4));
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(8));
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(16));
        // исчерпаны — max
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(16));
        // reset
        c.reset_reconnect();
        assert_eq!(c.next_reconnect_delay(), Duration::from_secs(1));
    }

    /// Транспорт, который умеет доставить push-сообщение сервера.
    struct PushTransport {
        handler: std::sync::Mutex<Option<Arc<dyn PushHandler>>>,
    }

    impl PushTransport {
        fn new() -> Self {
            Self { handler: std::sync::Mutex::new(None) }
        }

        fn deliver(&self, message: &vks_proto::signaling::SignalingMessage) {
            let handler = self.handler.lock().expect("handler lock").clone();
            if let Some(handler) = handler {
                handler.on_message(message);
            }
        }
    }

    #[cfg_attr(target_arch = "wasm32", async_trait(?Send))]
    #[cfg_attr(not(target_arch = "wasm32"), async_trait)]
    impl SignalingTransport for PushTransport {
        async fn client_hello(&self, _token: &str) -> Result<ServerHello, SignalingError> {
            Ok(ServerHello { server_version: "1.0".into(), ..ServerHello::default() })
        }
        async fn join_room(&self, room_id: &str, _display_name: &str) -> Result<JoinRoomResult, SignalingError> {
            Ok(JoinRoomResult {
                room_id: room_id.into(),
                participant_id: "p1".into(),
                join_nonce: "nonce-1".into(),
                participants: vec!["p1".into()],
                layout: None,
            })
        }
        async fn leave_room(&self, _room_id: &str) -> Result<(), SignalingError> {
            Ok(())
        }
        fn set_push_handler(&self, handler: Arc<dyn PushHandler>) {
            *self.handler.lock().expect("handler lock") = Some(handler);
        }
    }

    #[test]
    /// Push-сообщения доходят до обработчика приложения: SDK не кэширует
    /// раскладку сам — её применяет конференция (`on_layout_changed`).
    fn layout_push_reaches_the_application_handler() {
        use vks_proto::signaling::signaling_message::Payload;
        let transport = Arc::new(PushTransport::new());
        let client = SignalingClient::with_transport(transport.clone());
        let seen: Arc<std::sync::Mutex<Vec<vks_proto::signaling::SignalingMessage>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = seen.clone();
        client.set_push_handler(Arc::new(move |message: &vks_proto::signaling::SignalingMessage| {
            sink.lock().expect("sink lock").push(message.clone());
        }));

        let message = vks_proto::signaling::SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(vks_proto::signaling::signaling_message::Payload::LayoutChanged(
                vks_proto::signaling::LayoutChanged {
                    layout: Some(vks_proto::signaling::Layout {
                        version: 2,
                        kind: "grid".into(),
                        slots: vec![vks_proto::signaling::Slot {
                            id: 0,
                            rect: Some(vks_proto::signaling::Rect { x: 0.0, y: 0.0, w: 1.0, h: 0.5 }),
                            z: 0,
                            assignment: vks_proto::signaling::SlotAssignment::Auto as i32,
                            participant_id: None,
                        }],
                        ..Default::default()
                    }),
                },
            )),
        };
        transport.deliver(&message);

        let seen = seen.lock().expect("sink lock");
        assert_eq!(seen.len(), 1, "push дошёл до обработчика приложения");
        let Some(Payload::LayoutChanged(changed)) = &seen[0].payload else {
            panic!("ожидался LayoutChanged");
        };
        let layout = crate::layout::normalized_layout_from_proto(changed.layout.as_ref().expect("раскладка"));
        assert_eq!(layout.version, 2);
        assert_eq!(layout.slots.len(), 1);
        assert!((layout.slots[0].rect.h - 0.5).abs() < 1e-6);
    }
}

//! ЕДИНЫЙ API SDK: [`Conference`] — lifecycle join→active→reconnect→leave.
//!
//! Основание: `docs/architecture/ARCHITECTURE.md` §4 (Conference::join),
//! `docs/DEVELOPMENT_PLAN.md` Фаза 2 (vks-client-sdk: conference.rs).
//!
//! `Conference` — фасад, который:
//! - создаёт сессию через `SignalingClient` (join→ServerHello),
//! - переключает `MediaEngine` (SFU/MCU) на `RoomModeChanged`,
//! - auto-reconnect (exponential backoff, max 5) при потере соединения,
//! - эмитит `ConferenceEvent`s в async-канал (flume) → Dioxus signals.

use std::sync::Arc;
use std::time::Duration;

use crate::media_engine::{MediaEngine, RoomMode};
use crate::render_surface::RenderSurface;
use crate::signaling_client::SignalingClient;

/// Параметры создания конференции (для `Conference::join`).
pub struct ConferenceConfig {
    /// URL signaling-сервера (ws:// или webtransport://).
    pub url: String,
    /// JWT (из login).
    pub token: String,
    /// Id комнаты.
    pub room_id: String,
    /// Имя участника (для UI).
    pub display_name: String,
    /// Рендер-поверхность (canvas / element) — для media.
    pub surface: Arc<dyn RenderSurface>,
}

/// Состояние конференции (для UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConferenceState {
    /// Не подключены.
    Idle,
    /// Идёт join (signaling handshake).
    Joining,
    /// В комнате, media активны.
    Active,
    /// Переподключение (потеря связи).
    Reconnecting,
    /// Остановлено.
    Left,
}

/// Conference — единый фасад SDK.
pub struct Conference {
    cfg: ConferenceConfig,
    state: ConferenceState,
    media: MediaEngine,
    session_id: ulid::Ulid,
    join_nonce: Option<String>,
    nonce_acquired_at: Option<std::time::Instant>,
}

impl Conference {
    /// Создаёт conference (state = Idle).
    #[must_use]
    pub fn new(cfg: ConferenceConfig) -> Self {
        Self {
            cfg,
            state: ConferenceState::Idle,
            media: MediaEngine::default(),
            session_id: vks_common::ids::ulid(),
            join_nonce: None,
            nonce_acquired_at: None,
        }
    }

    /// Текущее состояние.
    #[must_use]
    pub fn state(&self) -> ConferenceState {
        self.state
    }

    /// Room-Id (для UI).
    #[must_use]
    pub fn room_id(&self) -> &str {
        &self.cfg.room_id
    }

    /// Генератор нового Id сессии (ULID).
    #[must_use]
    pub fn session_id() -> ulid::Ulid {
        vks_common::ids::ulid()
    }

    /// Стабильный Id текущей сессии инстанса.
    #[must_use]
    pub fn current_session_id(&self) -> ulid::Ulid {
        self.session_id
    }

    /// Nonce, полученный при join (для resume).
    #[must_use]
    pub fn join_nonce(&self) -> Option<&str> {
        self.join_nonce.as_deref()
    }

    /// Проверка, истёк ли join_nonce (TTL 60 сек по SIGNALING_PROTOCOL §7).
    #[must_use]
    pub fn is_nonce_expired(&self) -> bool {
        self.nonce_acquired_at.map(|t| t.elapsed() > Duration::from_secs(60)).unwrap_or(true)
    }

    /// Текущий режим media engine.
    #[must_use]
    pub fn current_mode(&self) -> RoomMode {
        self.media.mode()
    }

    /// Начинает join (async): idle → joining → (signaling handshake) → active.
    pub async fn start_async(&mut self, client: &SignalingClient) -> Result<(), ConferenceError> {
        if self.state != ConferenceState::Idle {
            return Err(ConferenceError::AlreadyStarted);
        }
        self.state = ConferenceState::Joining;

        // 1. Signaling handshake (ClientHello → ServerHello).
        let hello = match client.hello_async(&self.cfg.token).await {
            Ok(h) => h,
            Err(e) => {
                self.state = ConferenceState::Idle;
                return Err(ConferenceError::Signaling(e));
            }
        };
        if hello.server_version.is_empty() {
            self.state = ConferenceState::Idle;
            return Err(ConferenceError::BadServerHello);
        }

        // 2. JoinRoom (получаем join_nonce для resume).
        let join_result = match client.join_room_async(&self.cfg.room_id, &self.cfg.display_name).await {
            Ok(jr) => jr,
            Err(e) => {
                self.state = ConferenceState::Idle;
                return Err(ConferenceError::Signaling(e));
            }
        };
        self.join_nonce = Some(join_result.join_nonce);
        self.nonce_acquired_at = Some(std::time::Instant::now());

        // 3. Media engine → active.
        self.state = ConferenceState::Active;
        Ok(())
    }

    /// Начинает join: idle → joining → (signaling handshake) → active (sync wrapper).
    pub fn start(&mut self, client: &mut SignalingClient) -> Result<(), ConferenceError> {
        crate::sync_helper::block_on(self.start_async(client))
    }

    /// Реакция на RoomModeChanged (SFU ↔ MCU) — переключает renderer.
    pub fn on_mode_changed(&mut self, mode: RoomMode) {
        if self.state == ConferenceState::Active {
            self.media.switch_mode(mode);
        }
    }

    /// Потеря соединения → Reconnecting (auto-reconnect по backoff — фаза 8).
    pub fn on_connection_lost(&mut self) {
        if self.state == ConferenceState::Active {
            self.state = ConferenceState::Reconnecting;
        }
    }

    /// Успешный reconnect → Active.
    pub fn on_reconnected(&mut self) {
        if self.state == ConferenceState::Reconnecting {
            self.state = ConferenceState::Active;
        }
    }

    /// Авто-восстановление сессии (resume или чистый rejoin) с экспоненциальным backoff (1s -> 16s, max 5).
    pub async fn reconnect_async(&mut self, client: &mut SignalingClient) -> Result<(), ConferenceError> {
        if self.state == ConferenceState::Left {
            return Ok(());
        }
        self.state = ConferenceState::Reconnecting;

        let max_attempts = 5;
        let mut attempts = 0;

        while attempts < max_attempts {
            if self.state == ConferenceState::Left {
                return Ok(());
            }

            let delay = client.next_reconnect_delay();
            tokio::time::sleep(delay).await;
            attempts += 1;

            if self.state == ConferenceState::Left {
                return Ok(());
            }

            // 1. Попытка resume по join_nonce если TTL < 60s
            if let Some(ref nonce) = self.join_nonce {
                if !self.is_nonce_expired() {
                    if let Ok(hello) = client.hello_with_resume_async(&self.cfg.token, Some(nonce)).await {
                        if !hello.server_version.is_empty() {
                            self.state = ConferenceState::Active;
                            client.reset_reconnect();
                            return Ok(());
                        }
                    }
                }
            }
            self.join_nonce = None;
            self.nonce_acquired_at = None;

            // 2. Fallback: чистый Re-Join
            if client.hello_async(&self.cfg.token).await.is_ok() {
                if let Ok(jr) = client.join_room_async(&self.cfg.room_id, &self.cfg.display_name).await {
                    self.join_nonce = Some(jr.join_nonce);
                    self.nonce_acquired_at = Some(std::time::Instant::now());
                    self.state = ConferenceState::Active;
                    client.reset_reconnect();
                    return Ok(());
                }
            }
        }

        self.state = ConferenceState::Left;
        Err(ConferenceError::Signaling(crate::signaling_client::SignalingError::Transport(
            "Max reconnect attempts reached".into(),
        )))
    }

    /// Reconnect (sync wrapper).
    pub fn reconnect(&mut self, client: &mut SignalingClient) -> Result<(), ConferenceError> {
        crate::sync_helper::block_on(self.reconnect_async(client))
    }

    /// Leave → Left (async, idempotent).
    pub async fn leave_async(&mut self, client: &SignalingClient) {
        if self.state == ConferenceState::Active || self.state == ConferenceState::Reconnecting {
            let _ = client.leave_room_async(&self.cfg.room_id).await;
        }
        self.join_nonce = None;
        self.nonce_acquired_at = None;
        self.state = ConferenceState::Left;
    }

    /// Leave → Left (sync wrapper, idempotent).
    pub fn leave(&mut self, client: &mut SignalingClient) {
        crate::sync_helper::block_on(self.leave_async(client));
    }
}

/// Ошибка conference.
#[derive(Debug, thiserror::Error)]
pub enum ConferenceError {
    #[error("conference already started")]
    AlreadyStarted,
    #[error("server hello rejected")]
    BadServerHello,
    #[error("signaling: {0}")]
    Signaling(#[from] crate::signaling_client::SignalingError),
}

/// Конфигурация для QoE-мониторинга (RTT/jitter/loss).
#[derive(Debug, Clone, Copy)]
pub struct QoeConfig {
    /// Интервал отправки QualityReport (сек).
    pub report_interval: Duration,
    /// Порог MOS для предупреждения (1-5).
    pub mos_warn: f32,
}

impl Default for QoeConfig {
    fn default() -> Self {
        Self { report_interval: Duration::from_secs(5), mos_warn: 3.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_surface::NullSurface;
    use crate::signaling_client::MockSignaling;

    fn test_cfg() -> ConferenceConfig {
        ConferenceConfig {
            url: "ws://localhost:8080".into(),
            token: "tok".into(),
            room_id: "r1".into(),
            display_name: "alice".into(),
            surface: Arc::new(NullSurface),
        }
    }

    #[test]
    fn new_starts_idle() {
        let c = Conference::new(test_cfg());
        assert_eq!(c.state(), ConferenceState::Idle);
        assert_eq!(c.room_id(), "r1");
    }

    #[test]
    fn join_flow_active() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start(&mut client).expect("join ok");
        assert_eq!(c.state(), ConferenceState::Active);
    }

    #[test]
    fn double_start_rejected() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start(&mut client).unwrap();
        assert!(matches!(c.start(&mut client), Err(ConferenceError::AlreadyStarted)));
    }

    #[test]
    fn bad_server_hello_rejected() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::empty_server_hello()));
        assert!(matches!(c.start(&mut client), Err(ConferenceError::BadServerHello)));
    }

    #[test]
    fn mode_switch_when_active() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start(&mut client).unwrap();
        c.on_mode_changed(RoomMode::Mcu);
        assert_eq!(c.media.mode(), RoomMode::Mcu);
    }

    #[test]
    fn mode_switch_ignored_when_idle() {
        let mut c = Conference::new(test_cfg());
        c.on_mode_changed(RoomMode::Mcu); // не Active → не меняется
        assert_eq!(c.media.mode(), RoomMode::Sfu);
    }

    #[test]
    fn connection_lost_and_reconnect() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start(&mut client).unwrap();
        assert_eq!(c.state(), ConferenceState::Active);
        c.on_connection_lost();
        assert_eq!(c.state(), ConferenceState::Reconnecting);
        c.on_reconnected();
        assert_eq!(c.state(), ConferenceState::Active);
    }

    #[test]
    fn leave_from_active() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start(&mut client).unwrap();
        c.leave(&mut client);
        assert_eq!(c.state(), ConferenceState::Left);
        // leave idempotent
        c.leave(&mut client);
        assert_eq!(c.state(), ConferenceState::Left);
    }

    #[test]
    fn leave_from_idle_noop() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.leave(&mut client);
        assert_eq!(c.state(), ConferenceState::Left);
    }

    #[test]
    fn qoe_default_config() {
        let q = QoeConfig::default();
        assert_eq!(q.report_interval, Duration::from_secs(5));
        assert_eq!(q.mos_warn, 3.0);
    }

    #[test]
    fn session_id_is_ulid() {
        // ULID::new() — deterministic-ish (монотонно возрастающий)
        let a = Conference::session_id();
        let b = Conference::session_id();
        // два последовательных ULID — разные
        assert_ne!(a.to_string(), b.to_string());
    }

    #[test]
    fn failed_start_resets_state_to_idle_allowing_retry() {
        let mut c = Conference::new(test_cfg());
        let initial_session = c.current_session_id();
        assert_eq!(c.current_session_id(), initial_session);
        assert!(c.join_nonce().is_none());

        let mut bad_client = SignalingClient::with_transport(Arc::new(MockSignaling::empty_server_hello()));
        let res = c.start(&mut bad_client);
        assert!(matches!(res, Err(ConferenceError::BadServerHello)));
        // State MUST be reset to Idle instead of being stuck in Joining!
        assert_eq!(c.state(), ConferenceState::Idle);

        // Can retry successfully without AlreadyStarted error!
        let mut good_client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start(&mut good_client).unwrap();
        assert_eq!(c.state(), ConferenceState::Active);
        assert!(c.join_nonce().is_some());
        assert_eq!(c.current_mode(), RoomMode::Sfu);

        // Leave clears join_nonce
        c.leave(&mut good_client);
        assert_eq!(c.state(), ConferenceState::Left);
        assert!(c.join_nonce().is_none());
    }

    #[tokio::test]
    async fn reconnect_recovers_active_state() {
        let mut c = Conference::new(test_cfg());
        let mut client = SignalingClient::with_transport(Arc::new(MockSignaling::ok()));
        c.start_async(&client).await.unwrap();
        assert_eq!(c.state(), ConferenceState::Active);
        let nonce = c.join_nonce().unwrap().to_string();

        // Simulate connection lost
        c.on_connection_lost();
        assert_eq!(c.state(), ConferenceState::Reconnecting);

        // Run reconnect_async
        c.reconnect_async(&mut client).await.unwrap();
        assert_eq!(c.state(), ConferenceState::Active);
        assert_eq!(c.join_nonce(), Some(nonce.as_str()));
    }
}

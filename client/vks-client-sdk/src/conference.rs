//! ЕДИНЫЙ API SDK: [`Conference`] — lifecycle join→active→reconnect→leave.
//!
//! Основание: `docs/architecture/ARCHITECTURE.md` §4 (Conference::join),
//! `docs/DEVELOPMENT_PLAN.md` Фаза 2 (vks-client-sdk: conference.rs).
//!
//! `Conference` — фасад, который:
//! - создаёт сессию через `SignalingClient` (join→ServerHello),
//! - переключает `MediaEngine` (SFU/MCU) на `RoomModeChanged`,
//! - auto-reconnect (exponential backoff, max 5) при потере соединения,
//! - складывает `ConferenceEvent`s в очередь; приложение забирает её через
//!   [`Conference::drain_events`].

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
    /// Очередь «живых» событий сервера (push-сообщения).
    ///
    /// Единственный владелец состояния раскладки и режима — `Conference`:
    /// push-хендлер транспорта только кладёт событие в очередь, приложение
    /// забирает очередь через [`Conference::drain_events`].
    events: Arc<std::sync::Mutex<std::collections::VecDeque<crate::types::ConferenceEvent>>>,
    /// Метаданные последней применённой раскладки (T10c): raw `preset` и
    /// `mode` админки. `""` = сервер не сообщил; не подставлять `auto`.
    layout_preset: String,
    layout_mode: String,
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
            events: Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new())),
            layout_preset: String::new(),
            layout_mode: String::new(),
        }
    }

    /// Media engine конференции — источник кадров для отрисовки.
    ///
    /// Раскладка и режим живут внутри конференции, поэтому приложение
    /// берёт кадры именно отсюда: отдельный экземпляр движка видел бы
    /// авто-сетку, а не раскладку сервера.
    #[must_use]
    pub fn media(&self) -> &MediaEngine {
        &self.media
    }

    /// Забирает события, накопленные push-сообщениями сервера, и применяет
    /// их к media engine (режим, раскладка).
    ///
    /// Устаревшие кадры раскладки (`version <=` уже применённой) не
    /// применяются и не отдаются в UI. Метаданные снимка `JoinRoomResult`/rejoin
    /// событием не дублируются — их отдают [`Self::layout_preset`] и
    /// [`Self::layout_mode`].
    pub fn drain_events(&mut self) -> Vec<crate::types::ConferenceEvent> {
        let mut events: Vec<crate::types::ConferenceEvent> = {
            let mut queue = self.events.lock().expect("events lock");
            queue.drain(..).collect()
        };
        // T10c/ADR-0032: кадр с version <= применённой — устаревший
        // (VAD-пересадка и admin-apply конкурируют). Не применяем его и не
        // отдаём в UI, иначе подпись preset/mode откатится на старую.
        events.retain_mut(|event| match event {
            crate::types::ConferenceEvent::ModeChanged { mode } => {
                if let Some(mode) = RoomMode::parse(mode) {
                    self.media.switch_mode(mode);
                }
                true
            }
            crate::types::ConferenceEvent::LayoutChanged { layout, preset, mode } => {
                if self.media.layout().is_some_and(|current| layout.version <= current.version) {
                    return false;
                }
                self.media.set_layout(layout.clone());
                self.layout_preset.clone_from(preset);
                self.layout_mode.clone_from(mode);
                true
            }
            _ => true,
        });
        events
    }

    /// Raw-id схемы последней применённой раскладки (`""` = unknown).
    #[must_use]
    pub fn layout_preset(&self) -> &str {
        &self.layout_preset
    }

    /// Режим редактора последней применённой раскладки (`""` = unknown).
    #[must_use]
    pub fn layout_mode(&self) -> &str {
        &self.layout_mode
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

    /// Применяет снимок раскладки из `JoinRoomResult` (join и rejoin):
    /// геометрию — в media engine, raw-метаданные — в поля конференции.
    fn apply_layout_snapshot(&mut self, layout: Option<&vks_proto::signaling::Layout>) {
        match layout {
            Some(layout) => {
                self.media.set_layout(crate::layout::normalized_layout_from_proto(layout));
                self.layout_preset.clone_from(&layout.preset);
                self.layout_mode.clone_from(&layout.mode);
            }
            None => {
                // Rejoin/resume попал в перезапущенный движок без раскладки:
                // старую версию хранить нельзя.
                self.media.clear_layout();
                self.layout_preset.clear();
                self.layout_mode.clear();
            }
        }
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

        // 3. Раскладка комнаты приходит снимком в JoinRoomResult — без гонки
        // с push-сообщением (поздний участник иначе рисовал бы авто-сетку до
        // следующего изменения раскладки).
        if let Some(layout) = join_result.layout.as_ref() {
            self.apply_layout_snapshot(Some(layout));
        }

        // 4. Media engine → active.
        self.state = ConferenceState::Active;

        // 5. Живые push-сообщения сервера (смена режима, смена раскладки)
        // складываются в очередь конференции: приложение забирает её через
        // `drain_events`. Конференция — единственный владелец состояния
        // раскладки, поэтому SDK не держит вторую копию.
        let events = self.events.clone();
        client.set_push_handler(Arc::new(move |message: &vks_proto::signaling::SignalingMessage| {
            let event = match &message.payload {
                Some(vks_proto::signaling::signaling_message::Payload::LayoutChanged(changed)) => {
                    changed.layout.as_ref().map(|layout| crate::types::ConferenceEvent::LayoutChanged {
                        layout: crate::layout::normalized_layout_from_proto(layout),
                        preset: layout.preset.clone(),
                        mode: layout.mode.clone(),
                    })
                }
                Some(vks_proto::signaling::signaling_message::Payload::RoomModeChanged(changed)) => {
                    vks_proto::media::RoomMode::try_from(changed.new_mode).ok().and_then(|mode| {
                        // as_str_name() даёт ROOM_MODE_MCU — для события нужен
                        // "mcu"/"sfu" (иначе RoomMode::parse не разбирает).
                        let mode = match mode {
                            vks_proto::media::RoomMode::Sfu => "sfu",
                            vks_proto::media::RoomMode::Mcu => "mcu",
                            vks_proto::media::RoomMode::Unspecified => return None,
                        };
                        Some(crate::types::ConferenceEvent::ModeChanged { mode: mode.into() })
                    })
                }
                _ => None,
            };
            if let Some(event) = event {
                events.lock().expect("events lock").push_back(event);
            }
        }));
        Ok(())
    }

    /// Начинает join: idle → joining → (signaling handshake) → active (sync wrapper).
    pub fn start(&mut self, client: &mut SignalingClient) -> Result<(), ConferenceError> {
        crate::sync_helper::block_on(self.start_async(client))
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
            backoff_sleep(delay).await;
            attempts += 1;

            if self.state == ConferenceState::Left {
                return Ok(());
            }

            // 1. Попытка resume по join_nonce если TTL < 60s
            if let Some(ref nonce) = self.join_nonce {
                if !self.is_nonce_expired() {
                    if let Ok(hello) = client.hello_with_resume_async(&self.cfg.token, Some(nonce)).await {
                        if !hello.server_version.is_empty() {
                            // Resume не несёт снимка раскладки (сервер пока
                            // игнорирует nonce; §7-снимок не реализован), а
                            // эпоха версий неизвестна: старую раскладку и
                            // накопленные layout-кадры прежней эпохи хранить
                            // нельзя, иначе guard заморозит рендер. Не-layout
                            // события (например RoomModeChanged) сохраняем.
                            self.apply_layout_snapshot(None);
                            self.events
                                .lock()
                                .expect("events lock")
                                .retain(|event| !matches!(event, crate::types::ConferenceEvent::LayoutChanged { .. }));
                            self.state = ConferenceState::Active;
                            client.reset_reconnect();
                            return Ok(());
                        }
                    }
                }
            }
            self.join_nonce = None;
            self.nonce_acquired_at = None;

            // 2. Fallback: чистый Re-Join. Очередь чистим только здесь:
            // кадры до обрыва относятся к прежней эпохе версий, а rejoin
            // снимок её сбрасывает.
            self.events.lock().expect("events lock").clear();
            if client.hello_async(&self.cfg.token).await.is_ok() {
                if let Ok(jr) = client.join_room_async(&self.cfg.room_id, &self.cfg.display_name).await {
                    self.join_nonce = Some(jr.join_nonce);
                    self.nonce_acquired_at = Some(std::time::Instant::now());
                    self.apply_layout_snapshot(jr.layout.as_ref());
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

/// Пауза backoff: tokio на native, `setTimeout` на wasm (tokio там недоступен).
#[cfg(not(target_arch = "wasm32"))]
async fn backoff_sleep(delay: std::time::Duration) {
    tokio::time::sleep(delay).await;
}

#[cfg(target_arch = "wasm32")]
async fn backoff_sleep(delay: std::time::Duration) {
    let millis = delay.as_millis().min(i32::MAX as u128) as i32;
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, millis);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render_surface::NullSurface;
    use crate::signaling_client::MockSignaling;
    #[cfg(not(target_arch = "wasm32"))]
    use async_trait::async_trait;

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

    /// Транспорт, умеющий доставить push-сообщение сервера и вернуть
    /// заданный снимок раскладки в `JoinRoomResult`.
    struct PushTransport {
        handler: std::sync::Mutex<Option<Arc<dyn crate::signaling_client::PushHandler>>>,
        join_layout: std::sync::Mutex<Option<vks_proto::signaling::Layout>>,
        resume_ok: std::sync::Mutex<bool>,
    }

    impl PushTransport {
        fn new() -> Self {
            Self {
                handler: std::sync::Mutex::new(None),
                join_layout: std::sync::Mutex::new(None),
                resume_ok: std::sync::Mutex::new(false),
            }
        }

        fn with_join_layout(layout: vks_proto::signaling::Layout) -> Self {
            Self { join_layout: std::sync::Mutex::new(Some(layout)), ..Self::new() }
        }

        fn set_join_layout(&self, layout: Option<vks_proto::signaling::Layout>) {
            *self.join_layout.lock().expect("join layout lock") = layout;
        }

        fn allow_resume(&self) {
            *self.resume_ok.lock().expect("resume flag lock") = true;
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
    impl crate::signaling_client::SignalingTransport for PushTransport {
        async fn client_hello(
            &self,
            _token: &str,
        ) -> Result<crate::signaling_client::ServerHello, crate::signaling_client::SignalingError> {
            Ok(crate::signaling_client::ServerHello {
                server_version: "1.0".into(),
                session_id: vks_common::ids::ulid_string(),
                server_time_ms: 0,
            })
        }

        async fn client_hello_with_resume(
            &self,
            token: &str,
            _resume_nonce: Option<&str>,
        ) -> Result<crate::signaling_client::ServerHello, crate::signaling_client::SignalingError> {
            if *self.resume_ok.lock().expect("resume flag lock") {
                crate::signaling_client::SignalingTransport::client_hello(self, token).await
            } else {
                // По умолчанию reconnect идёт через rejoin (снимок эпохи).
                Err(crate::signaling_client::SignalingError::Transport("resume disabled".into()))
            }
        }

        async fn join_room(
            &self,
            room_id: &str,
            _display_name: &str,
        ) -> Result<crate::signaling_client::JoinRoomResult, crate::signaling_client::SignalingError> {
            Ok(crate::signaling_client::JoinRoomResult {
                room_id: room_id.into(),
                participant_id: "p1".into(),
                join_nonce: "nonce-1".into(),
                participants: vec!["p1".into()],
                layout: self.join_layout.lock().expect("join layout lock").clone(),
            })
        }

        async fn leave_room(&self, _room_id: &str) -> Result<(), crate::signaling_client::SignalingError> {
            Ok(())
        }

        fn set_push_handler(&self, handler: Arc<dyn crate::signaling_client::PushHandler>) {
            *self.handler.lock().expect("handler lock") = Some(handler);
        }
    }

    /// S9: живой push раскладки доходит до отрисовки: сервер → push-хендлер →
    /// очередь конференции → media engine → плитки на экране.
    #[test]
    fn live_layout_push_changes_the_rendered_tiles() {
        use crate::render_surface::RecordingSurface;
        use vks_proto::signaling::signaling_message::Payload;
        use vks_proto::signaling::{Layout, LayoutChanged, Rect, Slot, SlotAssignment};

        let transport = Arc::new(PushTransport::new());
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");

        let message = vks_proto::signaling::SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::LayoutChanged(LayoutChanged {
                layout: Some(Layout {
                    version: 5,
                    kind: "grid".into(),
                    slots: vec![Slot {
                        id: 0,
                        rect: Some(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
                        z: 0,
                        assignment: SlotAssignment::Fixed as i32,
                        participant_id: Some("00000000-0000-0000-0000-0000000000aa".into()),
                    }],
                    ..Default::default()
                }),
            })),
        };
        transport.deliver(&message);

        let events = c.drain_events();
        assert_eq!(events.len(), 1, "push попал в очередь конференции");
        let surface = RecordingSurface::new();
        c.media().render(&["a".into(), "b".into()], &surface);
        let cmds = surface.take();
        assert_eq!(cmds.len(), 1, "плитки берутся из раскладки сервера, а не из авто-сетки");
        assert_eq!(cmds[0].participant_id, "00000000-0000-0000-0000-0000000000aa");
    }

    #[test]
    fn live_mode_push_switches_renderer() {
        let transport = Arc::new(PushTransport::new());
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).unwrap();
        transport.deliver(&mode_push(vks_proto::media::RoomMode::Mcu));
        let events = c.drain_events();
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], crate::types::ConferenceEvent::ModeChanged { mode } if mode == "mcu"),
            "wire-имя режима должно разбираться RoomMode::parse"
        );
        assert_eq!(c.media().mode(), RoomMode::Mcu);
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

    /// T10c: join-снимок применяет и геометрию, и метаданные (поздний
    /// участник не получит push до следующего изменения раскладки).
    #[test]
    fn join_snapshot_applies_layout_and_metadata() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(7, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");
        assert_eq!(c.media().layout().expect("layout applied").version, 7);
        assert_eq!(c.layout_preset(), "codian_07");
        assert_eq!(c.layout_mode(), "manual");
        assert!(c.drain_events().is_empty(), "снимок не дублируется событием");
    }

    /// T10c: снимок version=0 (proto-default/legacy) применяется и не
    /// блокирует следующий кадр (guard только на `<=` при наличии layout).
    #[test]
    fn v0_snapshot_does_not_freeze_updates() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(0, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");
        assert_eq!(c.media().layout().expect("v0 snapshot").version, 0);
        transport.deliver(&layout_push(layout_proto(1, "b", "vks_grid_4", "manual")));
        assert_eq!(c.drain_events().len(), 1);
        assert_eq!(c.media().layout().expect("v1 applied").version, 1);
    }

    /// T10c: живой push доносит raw-строки без нормализации/подмены.
    #[test]
    fn live_push_carries_raw_metadata() {
        let transport = Arc::new(PushTransport::new());
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");
        transport.deliver(&layout_push(layout_proto(5, "a", "preset_equal", "dynamic")));
        let events = c.drain_events();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            &events[0],
            crate::types::ConferenceEvent::LayoutChanged { preset, mode, .. }
                if preset == "preset_equal" && mode == "dynamic"
        ));
        assert_eq!(c.layout_preset(), "preset_equal");
        assert_eq!(c.layout_mode(), "dynamic");
    }

    /// T10c: устаревший кадр не применяется и не отдаётся в UI; следующий
    /// монотонный кадр снова применяется.
    #[test]
    fn stale_layout_push_is_ignored_end_to_end() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(5, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");

        transport.deliver(&layout_push(layout_proto(5, "b", "vks_grid_9", "manual")));
        assert!(c.drain_events().is_empty(), "равная версия — retransmit, не применяем");
        transport.deliver(&layout_push(layout_proto(4, "b", "vks_grid_9", "manual")));
        assert!(c.drain_events().is_empty(), "меньшая версия — stale");
        assert_eq!(c.media().layout().expect("layout").version, 5);
        assert_eq!(c.layout_preset(), "codian_07", "метаданные не откатились");
        let surface = crate::render_surface::RecordingSurface::new();
        c.media().render(&["a".into(), "b".into()], &surface);
        let cmds = surface.take();
        assert_eq!(cmds[0].participant_id, participant_uuid("a"), "рендер остался на применённой раскладке");

        transport.deliver(&layout_push(layout_proto(6, "b", "vks_grid_9", "manual")));
        assert_eq!(c.drain_events().len(), 1, "монотонный кадр применяется");
        assert_eq!(c.media().layout().expect("layout").version, 6);
        assert_eq!(c.layout_preset(), "vks_grid_9");
    }

    /// T10c/ADR-0032: пустые метаданные = unknown, не `auto`.
    #[test]
    fn empty_metadata_means_unknown() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(5, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");
        assert_eq!(c.layout_preset(), "codian_07");
        transport.deliver(&layout_push(layout_proto(6, "a", "", "")));
        assert_eq!(c.drain_events().len(), 1);
        assert_eq!(c.layout_preset(), "");
        assert_eq!(c.layout_mode(), "", "unknown не подменяется на auto и не оставляет старое");
    }

    /// T10c: rejoin в перезапущенный движок без раскладки очищает снимок,
    /// и следующая (новая) эпоха версий снова принимается.
    #[test]
    fn rejoin_without_snapshot_clears_layout() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(5, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");
        transport.set_join_layout(None);
        c.on_connection_lost();
        c.reconnect(&mut client).expect("rejoin ok");
        assert!(c.media().layout().is_none(), "старая эпоха не переживает rejoin");
        assert_eq!(c.layout_preset(), "");
        assert_eq!(c.layout_mode(), "");
        transport.deliver(&layout_push(layout_proto(1, "b", "vks_grid_4", "manual")));
        assert_eq!(c.drain_events().len(), 1, "version=1 принимается после сброса");
        assert_eq!(c.media().layout().expect("layout").version, 1);
    }

    /// T10c: успешный resume не несёт снимка и не сохраняет эпоху версий —
    /// layout-кадры прежней эпохи отбрасываются, не-layout события остаются.
    #[test]
    fn resume_success_drops_stale_layout_frames_but_keeps_mode_events() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(5, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start(&mut client).expect("join ok");
        transport.deliver(&layout_push(layout_proto(7, "b", "vks_grid_9", "manual")));
        transport.deliver(&mode_push(vks_proto::media::RoomMode::Mcu));
        transport.allow_resume();
        c.on_connection_lost();
        c.reconnect(&mut client).expect("resume ok");
        assert!(c.media().layout().is_none(), "resume без снимка не сохраняет старую версию");
        assert_eq!(c.layout_preset(), "");
        let events = c.drain_events();
        assert_eq!(events.len(), 1, "layout-кадр прежней эпохи отброшен, mode — сохранён");
        assert!(matches!(&events[0], crate::types::ConferenceEvent::ModeChanged { .. }));
        assert_eq!(c.media().mode(), RoomMode::Mcu);
        transport.deliver(&layout_push(layout_proto(1, "e", "preset_equal", "dynamic")));
        assert_eq!(c.drain_events().len(), 1, "после resume принимается любой layout-кадр");
        assert_eq!(c.media().layout().expect("layout").version, 1);
    }

    /// T10c: rejoin после рестарта движка сбрасывает эпоху версий —
    /// иначе version-guard заморозил бы рендер навсегда.
    #[tokio::test]
    async fn reconnect_rejoin_resets_version_epoch() {
        let transport = Arc::new(PushTransport::with_join_layout(layout_proto(5, "a", "codian_07", "manual")));
        let mut client = SignalingClient::with_transport(transport.clone());
        let mut c = Conference::new(test_cfg());
        c.start_async(&client).await.expect("join ok");
        // Кадр до обрыва остаётся в очереди и не должен пережить rejoin.
        transport.deliver(&layout_push(layout_proto(7, "b", "vks_grid_9", "manual")));
        transport.set_join_layout(Some(layout_proto(2, "c", "preset_equal", "dynamic")));
        c.on_connection_lost();
        c.reconnect_async(&mut client).await.expect("rejoin ok");
        assert_eq!(c.state(), ConferenceState::Active);
        assert_eq!(c.media().layout().expect("snapshot applied").version, 2);
        assert_eq!(c.layout_preset(), "preset_equal");
        assert!(c.drain_events().is_empty(), "очередь очищена на reconnect");
        transport.deliver(&layout_push(layout_proto(3, "d", "codian_01", "manual")));
        assert_eq!(c.drain_events().len(), 1, "новая эпоха версий принимается");
        assert_eq!(c.media().layout().expect("layout").version, 3);
    }

    fn layout_proto(version: u32, participant: &str, preset: &str, mode: &str) -> vks_proto::signaling::Layout {
        use vks_proto::signaling::{Layout, Rect, Slot, SlotAssignment};
        Layout {
            version,
            slots: vec![Slot {
                id: 0,
                rect: Some(Rect { x: 0.0, y: 0.0, w: 1.0, h: 1.0 }),
                z: 0,
                assignment: SlotAssignment::Fixed as i32,
                participant_id: Some(participant_uuid(participant)),
            }],
            kind: "custom".into(),
            preset: preset.into(),
            mode: mode.into(),
        }
    }

    fn participant_uuid(name: &str) -> String {
        let byte = name.as_bytes().first().copied().unwrap_or(0);
        format!("00000000-0000-0000-0000-0000000000{byte:02x}")
    }

    fn mode_push(mode: vks_proto::media::RoomMode) -> vks_proto::signaling::SignalingMessage {
        use vks_proto::signaling::signaling_message::Payload;
        vks_proto::signaling::SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::RoomModeChanged(vks_proto::signaling::RoomModeChanged {
                new_mode: mode as i32,
                caps: None,
                switch_epoch: 0,
                graceful: true,
            })),
        }
    }

    fn layout_push(layout: vks_proto::signaling::Layout) -> vks_proto::signaling::SignalingMessage {
        use vks_proto::signaling::signaling_message::Payload;
        vks_proto::signaling::SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::LayoutChanged(vks_proto::signaling::LayoutChanged { layout: Some(layout) })),
        }
    }
}

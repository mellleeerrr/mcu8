//! H.323-вызов и H.323-шлюз (H.225/H.245 signaling).
//!
//! Фаза 6: H.323 → WebRTC bridge. H.323-терминал звонит в комнату; вызов
//! «приземляется» как participant (gateway). FFI-граница — H323Plus
//! (ADR-0007), см. [`crate::ffi`].
//!
//! Автомат сигнализации H.225 (H.323, Q.931):
//!
//!   Idle ──Setup──► Alerting ──Proceeding──► Connect (Connected)
//!   ──Release──► Terminated
//!
//! Детерминированный (без реального H.225-транспорта в этой волне).

use std::collections::HashMap;

use vks_common::Error;

use crate::codec::{H323Codec, H323CodecMap};

/// Состояния H.323-вызова (H.225, Q.931).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum H323CallState {
    /// Вызов не установлен.
    Idle,
    /// Setup получен, отправлен Alerting (аналог 180).
    Alerting,
    /// Отправлен Proceeding (H.245 capability exchange).
    Proceeding,
    /// Connect получен — медиа течёт (Connected).
    Connect,
    /// Завершён (Release/ReleaseComplete).
    Terminated,
}

impl H323CallState {
    /// Человекочитаемое имя (для логов).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Alerting => "alerting",
            Self::Proceeding => "proceeding",
            Self::Connect => "connect",
            Self::Terminated => "terminated",
        }
    }
}

/// H.323-вызов (одна H.225-сессия).
#[derive(Debug, Clone)]
pub struct H323Call {
    /// Уникальный идентификатор вызова (callSignalIdentifier).
    pub call_id: String,
    /// Аноним от зовущего (H.323 alias).
    pub from: String,
    /// Сессионный идентификатор участника в шлюзе.
    pub participant_id: String,
    /// Имя текущей привязанной комнаты (если вызов приземлён).
    pub room_id: Option<String>,
    /// H.323-кодек, согласованный из capability advertisement.
    pub h323_codec: H323Codec,
    /// WebRTC-кодек, которым комната принимает поток.
    pub web_codec: String,
    /// Требуется ли транскодирование на шлюзе.
    pub needs_transcode: bool,
    /// Текущее состояние.
    pub state: H323CallState,
}

impl H323Call {
    /// Создаёт новый вызов в состоянии [`H323CallState::Idle`].
    ///
    /// Кодек комнаты (webrtc) вычисляется из H.323 capability
    /// ([`H323CodecMap::negotiate`]).
    pub fn new(call_id: impl Into<String>, from: impl Into<String>, h323_offer: &[H323Codec]) -> Self {
        let (web, transcode) = H323CodecMap::negotiate(h323_offer);
        let cid = call_id.into();
        let frm = from.into();
        let pid = format!("{frm}:{cid}@h323-gateway");
        Self {
            call_id: cid,
            from: frm,
            participant_id: pid,
            room_id: None,
            h323_codec: h323_offer.first().cloned().unwrap_or_else(|| H323Codec::new("PCMU")),
            web_codec: web.name,
            needs_transcode: transcode,
            state: H323CallState::Idle,
        }
    }

    /// Setup → Alerting (шлюз отвечает Alerting, аналог 180).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Idle`.
    pub fn setup(&mut self) -> Result<(), Error> {
        if self.state != H323CallState::Idle {
            return Err(Error::Conflict(format!("Setup in state {}", self.state.as_str())));
        }
        self.state = H323CallState::Alerting;
        Ok(())
    }

    /// Alerting → Proceeding (H.245 capability exchange).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Alerting`.
    pub fn proceeding(&mut self) -> Result<(), Error> {
        if self.state != H323CallState::Alerting {
            return Err(Error::Conflict(format!("Proceeding in state {}", self.state.as_str())));
        }
        self.state = H323CallState::Proceeding;
        Ok(())
    }

    /// Proceeding → Connect (Connect получен, медиа активна).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Proceeding`.
    pub fn connect(&mut self) -> Result<(), Error> {
        if self.state != H323CallState::Proceeding {
            return Err(Error::Conflict(format!("Connect in state {}", self.state.as_str())));
        }
        self.state = H323CallState::Connect;
        Ok(())
    }

    /// Connect → Terminated (Release/ReleaseComplete).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Connect`.
    pub fn release(&mut self) -> Result<(), Error> {
        if self.state != H323CallState::Connect {
            return Err(Error::Conflict(format!("Release in state {}", self.state.as_str())));
        }
        self.state = H323CallState::Terminated;
        Ok(())
    }
}

/// H.323-шлюз (в-memory) с mock-реестром WebRTC-комнат.
///
/// FFI-граница (H323Plus) — см. [`crate::ffi::H323PlusBridge`]; в этой
/// волне шлюз детерминированный и без реального H.225-транспорта.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RoomCallSlot {
    call_id: String,
    participant_name: String,
}

/// H.323-шлюз (в-memory) с mock-реестром WebRTC-комнат.
///
/// FFI-граница (H323Plus) — см. [`crate::ffi::H323PlusBridge`]; в этой
/// волне шлюз детерминированный и без реального H.225-транспорта.
/// Хранит привязку вызовов к комнатам по Call-ID, исключая
/// ложное выталкивание участника при параллельных звонках (M5.1).
#[derive(Debug)]
pub struct H323Gateway {
    /// Зарегистрирован ли H.323 шлюз.
    registered: bool,
    /// Вызовы по callSignalIdentifier.
    calls: HashMap<String, H323Call>,
    /// Mock-реестр комнат: room_id → слоты вызовов (call_id, имя).
    room_calls: HashMap<String, Vec<RoomCallSlot>>,
    /// Кэш участников комнат для быстрого возврата &[String].
    room_participants_cache: HashMap<String, Vec<String>>,
}

impl Default for H323Gateway {
    fn default() -> Self {
        Self {
            registered: true,
            calls: HashMap::new(),
            room_calls: HashMap::new(),
            room_participants_cache: HashMap::new(),
        }
    }
}

impl H323Gateway {
    /// Создаёт новый шлюз.
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует H.323 шлюз (открывает сигнальный транспорт Q.931).
    pub fn register(&mut self) -> Result<(), Error> {
        if self.registered {
            return Err(Error::Conflict("H.323 gateway already registered".into()));
        }
        self.registered = true;
        tracing::info!("H.323 gateway registered");
        Ok(())
    }

    /// Зарегистрирован ли H.323 шлюз.
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.registered
    }

    /// Отменяет регистрацию H.323 шлюза.
    pub fn unregister(&mut self) {
        self.registered = false;
        tracing::info!("H.323 gateway unregistered");
    }

    /// Количество активных звонков.
    pub fn active_calls_count(&self) -> usize {
        self.calls.values().filter(|c| c.state == H323CallState::Connect).count()
    }

    /// Принимает входящий Setup: создаёт вызов в `Alerting`,
    /// согласовывает кодеки (H.323 capability → WebRTC SDP).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если шлюз не зарегистрирован или call-id уже существует.
    /// [`Error::InvalidInput`] — если оффер пуст или не содержит поддерживаемых кодеков.
    pub fn on_setup(
        &mut self,
        call_id: impl Into<String>,
        from: impl Into<String>,
        h323_offer: &[H323Codec],
    ) -> Result<H323Call, Error> {
        if !self.registered {
            return Err(Error::Conflict("H.323 gateway is not registered".into()));
        }
        let call_id = call_id.into();
        if self.calls.contains_key(&call_id) {
            return Err(Error::Conflict(format!("duplicate call id {call_id}")));
        }

        // Строгая валидация кодеков
        let selection = H323CodecMap::try_negotiate(h323_offer)
            .map_err(|e| Error::InvalidInput(format!("unsupported codec offer: {e}")))?;

        let mut call = H323Call::new(call_id.clone(), from.into(), h323_offer);
        call.h323_codec = selection.offered_codec;
        call.web_codec = selection.negotiated.web.name;
        call.needs_transcode = selection.negotiated.needs_transcode;

        call.setup().map_err(|_| Error::Conflict("Setup transition failed".into()))?;
        self.calls.insert(call.call_id.clone(), call.clone());
        tracing::info!(call_id = %call.call_id, web_codec = %call.web_codec, transcode = call.needs_transcode, "H.323 Setup accepted, alerting");
        Ok(call)
    }

    /// Подвигает вызов Alerting → Proceeding.
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден; [`Error::Conflict`] —
    /// если не в `Alerting`.
    pub fn proceeding(&mut self, call_id: &str) -> Result<H323Call, Error> {
        {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            call.proceeding()?;
        }
        Ok(self.calls.get(call_id).expect("just transitioned").clone())
    }

    /// Подвигает вызов Proceeding → Connect (Connected).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден; [`Error::Conflict`] —
    /// если не в `Proceeding`.
    pub fn connect(&mut self, call_id: &str) -> Result<H323Call, Error> {
        {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            call.connect()?;
        }
        tracing::info!(call_id = %call_id, "H.323 call connected");
        Ok(self.calls.get(call_id).expect("just transitioned").clone())
    }

    /// Release: Connect → Terminated; изолированно выводит участника только из привязанной комнаты,
    /// не затрагивая параллельные вызовы того же абонента в других комнатах или той же комнате.
    /// Идемпотентен при повторном вызове (late Release).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден.
    pub fn release(&mut self, call_id: &str) -> Result<H323Call, Error> {
        let bound_room = {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            if call.state == H323CallState::Terminated {
                return Ok(call.clone());
            }
            if call.state == H323CallState::Alerting
                || call.state == H323CallState::Proceeding
                || call.state == H323CallState::Idle
            {
                call.state = H323CallState::Terminated;
            } else {
                call.release()?;
            }
            call.room_id.take()
        };

        if let Some(room) = bound_room {
            if let Some(slots) = self.room_calls.get_mut(&room) {
                slots.retain(|s| s.call_id != call_id);
                self.room_participants_cache
                    .insert(room.clone(), slots.iter().map(|s| s.participant_name.clone()).collect());
            }
        }

        let call = self.calls.get(call_id).expect("just transitioned").clone();
        tracing::info!(call_id = %call_id, "H.323 call terminated");
        Ok(call)
    }

    /// Отменяет вызов до установления и освобождает ресурсы.
    pub fn cancel(&mut self, call_id: &str) -> Result<H323Call, Error> {
        self.release(call_id)
    }

    /// Завершает вызов по таймауту.
    pub fn timeout(&mut self, call_id: &str) -> Result<H323Call, Error> {
        self.release(call_id)
    }

    /// Удаляет завершенные вызовы из реестра для ограничения памяти.
    pub fn prune_terminated(&mut self) -> usize {
        let before = self.calls.len();
        self.calls.retain(|_, call| call.state != H323CallState::Terminated);
        before - self.calls.len()
    }

    /// «Приземляет» connected-вызов в WebRTC-комнату.
    /// Идемпотентен при повторном вызове в ту же комнату.
    /// При указании другой комнаты корректно переносит участника (Move).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден.
    /// [`Error::Conflict`] — если вызов не в `Connect`.
    pub fn land_in_room(&mut self, room_id: impl Into<String>, call_id: &str) -> Result<H323Call, Error> {
        let target_room = room_id.into();
        let (from, prev_room) = {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            if call.state != H323CallState::Connect {
                return Err(Error::Conflict(format!("land_in_room requires Connect, got {}", call.state.as_str())));
            }

            // 1. Идемпотентность
            if call.room_id.as_deref() == Some(&target_room) {
                return Ok(call.clone());
            }

            let prev = call.room_id.take();
            call.room_id = Some(target_room.clone());
            (call.from.clone(), prev)
        };

        let name = Self::participant_name_for(&from);

        // 2. Перенос: если вызов был в другой комнате — удаляем из старой
        if let Some(prev) = prev_room {
            if let Some(slots) = self.room_calls.get_mut(&prev) {
                slots.retain(|s| s.call_id != call_id);
                self.room_participants_cache
                    .insert(prev.clone(), slots.iter().map(|s| s.participant_name.clone()).collect());
            }
        }

        // 3. Приземление в новую комнату строго по call_id
        let slots = self.room_calls.entry(target_room.clone()).or_default();
        if !slots.iter().any(|s| s.call_id == call_id) {
            slots.push(RoomCallSlot { call_id: call_id.to_string(), participant_name: name.clone() });
        }
        self.room_participants_cache
            .insert(target_room.clone(), slots.iter().map(|s| s.participant_name.clone()).collect());

        tracing::info!(room_id = %target_room, call_id = %call_id, participant = %name, "H.323 call landed in room");
        Ok(self.calls.get(call_id).cloned().expect("exists"))
    }

    fn participant_name_for(from: &str) -> String {
        format!("{from}@h323-gateway")
    }

    /// Имена участников mock-реестра комнаты.
    #[must_use]
    pub fn room_participants(&self, room_id: &str) -> &[String] {
        self.room_participants_cache.get(room_id).map_or(&[], Vec::as_slice)
    }

    /// Имя participant'а в комнате для данного вызова (если существует).
    #[must_use]
    pub fn participant_name(&self, call_id: &str) -> Option<String> {
        self.calls.get(call_id).map(|c| Self::participant_name_for(&c.from))
    }

    /// Подключён ли вызов (Connect) в реестре.
    #[must_use]
    pub fn has_active_call(&self, call_id: &str) -> bool {
        self.calls.get(call_id).is_some_and(|c| c.state == H323CallState::Connect)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_proceeding_connect_lands_participant() {
        let mut g = H323Gateway::new();
        let offer = [H323Codec::new("PCMU"), H323Codec::new("H263")];
        let call = g.on_setup("call-1", "alice", &offer).expect("Setup accepted");
        assert_eq!(call.state, H323CallState::Alerting);
        // Audio priority → Opus transcode.
        assert_eq!(call.web_codec, "OPUS");
        assert!(call.needs_transcode);

        let call = g.proceeding("call-1").expect("proceeding");
        assert_eq!(call.state, H323CallState::Proceeding);

        let call = g.connect("call-1").expect("connect");
        assert_eq!(call.state, H323CallState::Connect);

        g.land_in_room("room-1", "call-1").expect("landed");
        assert_eq!(g.room_participants("room-1"), &vec!["alice@h323-gateway".to_owned()]);
        assert!(g.has_active_call("call-1"));
    }

    #[test]
    fn release_terminates_and_removes_participant() {
        let mut g = H323Gateway::new();
        g.on_setup("call-1", "alice", &[H323Codec::new("PCMU")]).unwrap();
        g.proceeding("call-1").unwrap();
        g.connect("call-1").unwrap();
        g.land_in_room("room-1", "call-1").unwrap();

        let call = g.release("call-1").expect("release");
        assert_eq!(call.state, H323CallState::Terminated);
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);
        assert!(!g.has_active_call("call-1"));
    }

    #[test]
    fn codec_negotiation_per_call() {
        let mut g = H323Gateway::new();
        let call = g.on_setup("call-2", "bob", &[H323Codec::new("H264")]).unwrap();
        assert_eq!(call.web_codec, "VP9");
        assert!(call.needs_transcode);

        let call = g.on_setup("call-3", "carol", &[H323Codec::new("G729")]).unwrap();
        assert_eq!(call.web_codec, "OPUS");
        assert!(call.needs_transcode);

        let call = g.on_setup("call-4", "dave", &[H323Codec::new("OPUS")]).unwrap();
        assert_eq!(call.web_codec, "OPUS");
        assert!(!call.needs_transcode);
    }

    #[test]
    fn setup_requires_idle() {
        let mut call = H323Call::new("x", "y", &[H323Codec::new("PCMU")]);
        call.setup().unwrap();
        let err = call.setup().expect_err("second setup rejected");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn illegal_transition_rejected() {
        let mut call = H323Call::new("x", "y", &[H323Codec::new("PCMU")]);
        // Skip setup → proceeding must fail.
        let err = call.proceeding().expect_err("proceeding before setup");
        assert!(matches!(err, Error::Conflict(_)));
        call.setup().unwrap();
        // connect before proceeding must fail.
        let err = call.connect().expect_err("connect before proceeding");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn duplicate_setup_rejected() {
        let mut g = H323Gateway::new();
        g.on_setup("call-1", "alice", &[H323Codec::new("PCMU")]).unwrap();
        let err = g.on_setup("call-1", "alice", &[H323Codec::new("PCMU")]).expect_err("duplicate");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn land_requires_connect() {
        let mut g = H323Gateway::new();
        g.on_setup("call-1", "alice", &[H323Codec::new("PCMU")]).unwrap();
        let err = g.land_in_room("room-1", "call-1").expect_err("not connected yet");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn two_calls_same_from_in_different_rooms_isolated() {
        let mut g = H323Gateway::new();
        let offer = [H323Codec::new("PCMU")];

        // Абонент alice совершает два разных звонка в две разные комнаты
        g.on_setup("call-1", "alice", &offer).unwrap();
        g.proceeding("call-1").unwrap();
        g.connect("call-1").unwrap();
        g.land_in_room("room-1", "call-1").unwrap();

        g.on_setup("call-2", "alice", &offer).unwrap();
        g.proceeding("call-2").unwrap();
        g.connect("call-2").unwrap();
        g.land_in_room("room-2", "call-2").unwrap();

        assert_eq!(g.room_participants("room-1"), &["alice@h323-gateway"]);
        assert_eq!(g.room_participants("room-2"), &["alice@h323-gateway"]);

        // Завершаем call-1: он должен удалиться ТОЛЬКО из room-1
        g.release("call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);
        assert_eq!(g.room_participants("room-2"), &["alice@h323-gateway"]);
        assert!(!g.has_active_call("call-1"));
        assert!(g.has_active_call("call-2"));

        // Теперь завершаем call-2: он удаляется из room-2
        g.release("call-2").unwrap();
        assert_eq!(g.room_participants("room-2"), &[] as &[String]);
        assert!(!g.has_active_call("call-2"));
    }

    #[test]
    fn land_in_room_idempotent_and_move() {
        let mut g = H323Gateway::new();
        let offer = [H323Codec::new("PCMU")];
        g.on_setup("call-1", "alice", &offer).unwrap();
        g.proceeding("call-1").unwrap();
        g.connect("call-1").unwrap();

        // Первое приземление
        g.land_in_room("room-1", "call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &["alice@h323-gateway"]);

        // Повторное приземление в ту же комнату — идемпотентно, не дублирует
        g.land_in_room("room-1", "call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &["alice@h323-gateway"]);

        // Перенос в другую комнату
        g.land_in_room("room-2", "call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);
        assert_eq!(g.room_participants("room-2"), &["alice@h323-gateway"]);
    }

    #[test]
    fn unsupported_codec_offer_rejected() {
        let mut g = H323Gateway::new();
        // Пустой оффер
        let err = g.on_setup("call-empty", "alice", &[]).expect_err("empty offer rejected");
        assert!(matches!(err, Error::InvalidInput(_)));

        // Несовместимый оффер
        let err = g.on_setup("call-bad", "alice", &[H323Codec::new("AMR")]).expect_err("AMR rejected");
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn two_calls_same_from_in_same_room_no_ejection() {
        let mut g = H323Gateway::new();
        let offer = [H323Codec::new("PCMU")];

        // Абонент alice совершает два звонка в ОДНУ комнату
        g.on_setup("call-1", "alice", &offer).unwrap();
        g.proceeding("call-1").unwrap();
        g.connect("call-1").unwrap();
        g.land_in_room("room-1", "call-1").unwrap();

        g.on_setup("call-2", "alice", &offer).unwrap();
        g.proceeding("call-2").unwrap();
        g.connect("call-2").unwrap();
        g.land_in_room("room-1", "call-2").unwrap();

        assert_eq!(g.room_participants("room-1"), &["alice@h323-gateway", "alice@h323-gateway"]);

        // Завершаем call-1: call-2 обязан остаться в room-1!
        g.release("call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &["alice@h323-gateway"]);
        assert!(!g.has_active_call("call-1"));
        assert!(g.has_active_call("call-2"));

        // Late Release на call-1 идемпотентен
        let late = g.release("call-1").expect("late release is idempotent");
        assert_eq!(late.state, H323CallState::Terminated);

        // Завершаем call-2
        g.release("call-2").unwrap();
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);

        let pruned = g.prune_terminated();
        assert_eq!(pruned, 2);
    }

    #[test]
    fn cancel_and_timeout_during_alerting() {
        let mut g = H323Gateway::new();
        let offer = [H323Codec::new("PCMU")];
        g.on_setup("call-cancel", "alice", &offer).unwrap();
        let call = g.cancel("call-cancel").expect("cancel succeeds");
        assert_eq!(call.state, H323CallState::Terminated);

        g.on_setup("call-timeout", "bob", &offer).unwrap();
        let call = g.timeout("call-timeout").expect("timeout succeeds");
        assert_eq!(call.state, H323CallState::Terminated);
    }
}

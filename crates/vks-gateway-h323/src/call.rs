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
        Self {
            call_id: call_id.into(),
            from: from.into(),
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
#[derive(Debug, Default)]
pub struct H323Gateway {
    /// Вызовы по callSignalIdentifier.
    calls: HashMap<String, H323Call>,
    /// Mock-реестр комнат: room_id → имена участников.
    rooms: HashMap<String, Vec<String>>,
}

impl H323Gateway {
    /// Создаёт новый шлюз.
    pub fn new() -> Self {
        Self::default()
    }

    /// Принимает входящий Setup: создаёт вызов в `Alerting`,
    /// согласовывает кодеки (H.323 capability → WebRTC SDP).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если call-id уже существует.
    pub fn on_setup(
        &mut self,
        call_id: impl Into<String>,
        from: impl Into<String>,
        h323_offer: &[H323Codec],
    ) -> Result<H323Call, Error> {
        let call_id = call_id.into();
        if self.calls.contains_key(&call_id) {
            return Err(Error::Conflict(format!("duplicate call id {call_id}")));
        }
        let mut call = H323Call::new(call_id.clone(), from.into(), h323_offer);
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

    /// Release: Connect → Terminated; выводит участника из комнаты.
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден; [`Error::Conflict`] —
    /// если не в `Connect`.
    pub fn release(&mut self, call_id: &str) -> Result<H323Call, Error> {
        let participant_name = self.participant_name(call_id);
        {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            call.release()?;
        }
        for participants in self.rooms.values_mut() {
            participants.retain(|name| name != &participant_name);
        }
        tracing::info!(call_id = %call_id, "H.323 call terminated");
        Ok(self.calls.get(call_id).expect("just transitioned").clone())
    }

    /// «Приземляет» connected-вызов в WebRTC-комнату: вызов становится
    /// participant'ом с именем `{from}@h323-gateway` (ARCHITECTURE §2).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден.
    /// [`Error::Conflict`] — если вызов не в `Connect`.
    pub fn land_in_room(&mut self, room_id: impl Into<String>, call_id: &str) -> Result<H323Call, Error> {
        let room_id = room_id.into();
        let state = self.calls.get(call_id).map_or("unknown", |c| c.state.as_str());
        if state != H323CallState::Connect.as_str() {
            return Err(Error::Conflict(format!("land_in_room requires Connect, got {state}")));
        }
        let name = self.participant_name(call_id);
        self.rooms.entry(room_id.clone()).or_default().push(name.clone());
        tracing::info!(room_id = %room_id, call_id = %call_id, participant = %name, "H.323 call landed in room");
        Ok(self.calls.get(call_id).expect("checked above").clone())
    }

    /// Имена участников mock-реестра комнаты.
    #[must_use]
    pub fn room_participants(&self, room_id: &str) -> &[String] {
        self.rooms.get(room_id).map_or(&[], Vec::as_slice)
    }

    /// Имя participant'а в комнате для данного вызова.
    fn participant_name(&self, call_id: &str) -> String {
        format!("{}@h323-gateway", self.calls.get(call_id).map_or(call_id, |c| c.from.as_str()))
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
}

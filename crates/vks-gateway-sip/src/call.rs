//! SIP-вызов и в-memory SIP-шлюз (UAS).
//!
//! Фаза 6: приём SIP-вызова, «приземление» в WebRTC-комнату — вызов
//! становится participant'ом комнаты с ролью gateway.
//!
//! Автомат состояния (SIP UAS, RFC 3261):
//!
//!   Idle ──INVITE──► Ringing ──accept──► Active ──BYE──► Terminated
//!
//! Реальный SIP-транспорт — rvoip (feature `rvoip`); по умолчанию
//! шлюз полностью in-memory и детерминированный (без I/O), что
//! позволяет покрывать сценарии unit-тестами.

use std::collections::HashMap;

use vks_common::Error;

use crate::codec::{CodecMap, SipCodec};

/// Состояния SIP-вызова (UAS-сторона).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallState {
    /// Вызов не установлен.
    Idle,
    /// Трел (180 Ringing отправлен).
    Ringing,
    /// Активный (200 OK отправлен, медиа течёт).
    Active,
    /// Завершён (BYE или отклонение).
    Terminated,
}

impl CallState {
    /// Человекочитаемое имя (для логов, без SDP-деталей).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Ringing => "ringing",
            Self::Active => "active",
            Self::Terminated => "terminated",
        }
    }
}

/// SIP-вызов (одна UAS-сессия).
#[derive(Debug, Clone)]
pub struct SipCall {
    /// Уникальный идентификатор вызова (Call-ID).
    pub call_id: String,
    /// Аноним от зовущего (From).
    pub from: String,
    /// Кодек, согласованный с SIP-стороны.
    pub sip_codec: SipCodec,
    /// WebRTC-кодек, которым комната принимает поток.
    pub web_codec: String,
    /// Требуется ли транскодирование на шлюзе.
    pub needs_transcode: bool,
    /// Текущее состояние.
    pub state: CallState,
}

impl SipCall {
    /// Создаёт новый вызов в состоянии [`CallState::Idle`].
    ///
    /// Кодек комнаты (web_codec) вычисляется из SIP SDP-оффера
    /// ([`CodecMap::negotiate`]).
    pub fn new(call_id: impl Into<String>, from: impl Into<String>, sip_offer: &[SipCodec]) -> Self {
        let (web, transcode) = CodecMap::negotiate(sip_offer);
        Self {
            call_id: call_id.into(),
            from: from.into(),
            sip_codec: sip_offer.first().cloned().unwrap_or_else(|| SipCodec::new("PCMU")),
            web_codec: web.name,
            needs_transcode: transcode,
            state: CallState::Idle,
        }
    }

    /// INVITE: `Idle` → `Ringing` (шлюз отвечает 180 Ringing).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Idle`.
    pub fn invite(&mut self) -> Result<(), Error> {
        if self.state != CallState::Idle {
            return Err(Error::Conflict(format!("INVITE in state {}", self.state.as_str())));
        }
        self.state = CallState::Ringing;
        Ok(())
    }

    /// 200 OK: `Ringing` → `Active`.
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Ringing`.
    pub fn accept(&mut self) -> Result<(), Error> {
        if self.state != CallState::Ringing {
            return Err(Error::Conflict(format!("accept in state {}", self.state.as_str())));
        }
        self.state = CallState::Active;
        Ok(())
    }

    /// BYE: `Active` → `Terminated`.
    ///
    /// # Errors
    /// [`Error::Conflict`] — если вызов не в `Active`.
    pub fn bye(&mut self) -> Result<(), Error> {
        if self.state != CallState::Active {
            return Err(Error::Conflict(format!("BYE in state {}", self.state.as_str())));
        }
        self.state = CallState::Terminated;
        Ok(())
    }
}

/// В-memory SIP-шлюз (UAS) с mock-реестром WebRTC-комнат.
///
/// Реестр комнат — замена реального room-реестра (vks-core/vks-storage):
/// хранит только список participant-имён, чтобы проверять «приземление»
/// вызова в комнату (ARCHITECTURE §2: gateway = participant, для
/// клиентов прозрачно).
#[derive(Debug, Default)]
pub struct SipGateway {
    /// UAS зарегистрирован (порт SIP открыт).
    uas_registered: bool,
    /// Активные вызовы по Call-ID.
    calls: HashMap<String, SipCall>,
    /// Mock-реестр комнат: room_id → имена участников.
    rooms: HashMap<String, Vec<String>>,
}

impl SipGateway {
    /// Создаёт новый шлюз без зарегистрированного UAS.
    pub fn new() -> Self {
        Self::default()
    }

    /// Регистрирует SIP UAS (открывает SIP-транспорт).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если UAS уже зарегистрирован.
    pub fn register_uas(&mut self) -> Result<(), Error> {
        if self.uas_registered {
            return Err(Error::Conflict("SIP UAS already registered".into()));
        }
        self.uas_registered = true;
        tracing::info!("SIP UAS registered");
        Ok(())
    }

    /// Принимает входящий INVITE: создаёт вызов в `Ringing` и
    /// согласовывает кодеки (SIP SDP → WebRTC SDP).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если UAS не зарегистрирован.
    /// [`Error::Conflict`] — если Call-ID уже существует.
    pub fn on_invite(
        &mut self,
        call_id: impl Into<String>,
        from: impl Into<String>,
        sdp_offer: &[SipCodec],
    ) -> Result<SipCall, Error> {
        if !self.uas_registered {
            return Err(Error::Conflict("SIP UAS is not registered".into()));
        }
        let call_id = call_id.into();
        if self.calls.contains_key(&call_id) {
            return Err(Error::Conflict(format!("duplicate call id {call_id}")));
        }
        let mut call = SipCall::new(call_id.clone(), from.into(), sdp_offer);
        // INVITE от зовущего: Idle → Ringing (180).
        call.invite().map_err(|_| Error::Conflict("INVITE transition failed".into()))?;
        self.calls.insert(call.call_id.clone(), call.clone());
        tracing::info!(call_id = %call.call_id, web_codec = %call.web_codec, transcode = call.needs_transcode, "SIP INVITE accepted, ringing");
        Ok(call)
    }

    /// Активирует вызов (200 OK).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден; [`Error::Conflict`] —
    /// если не в `Ringing`.
    pub fn accept(&mut self, call_id: &str) -> Result<SipCall, Error> {
        {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            call.accept()?;
        }
        let call = self.calls.get(call_id).expect("just transitioned").clone();
        tracing::info!(call_id = %call_id, "SIP call active");
        Ok(call)
    }

    /// Завёршивает вызов (BYE) и выводит его из комнаты, если был
    /// «приземлён».
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден; [`Error::Conflict`] —
    /// если не в `Active`.
    pub fn bye(&mut self, call_id: &str) -> Result<SipCall, Error> {
        // Имя участника до transition (чтобы вычесть из комнаты).
        let participant_name = self.participant_name(call_id);
        {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            call.bye()?;
        }
        // Удаляем участника из комнаты (mock-реестр).
        for participants in self.rooms.values_mut() {
            participants.retain(|name| name != &participant_name);
        }
        let call = self.calls.get(call_id).expect("just transitioned").clone();
        tracing::info!(call_id = %call_id, "SIP call terminated");
        Ok(call)
    }

    /// «Приземляет» активный вызов в WebRTC-комнату: вызов регистрируется
    /// как participant с именем `{from}@sip-gateway` (ARCHITECTURE §2 —
    /// для WebRTC-клиентов прозрачно, это обычный участник).
    ///
    /// Комната создаётся в mock-реестре при первом обращении.
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден.
    /// [`Error::Conflict`] — если вызов не в `Active`.
    pub fn land_in_room(&mut self, room_id: impl Into<String>, call_id: &str) -> Result<SipCall, Error> {
        let room_id = room_id.into();
        let name = self.participant_name(call_id);
        let is_active = self.calls.get(call_id).is_some_and(|c| c.state == CallState::Active);
        if !is_active {
            let state = self.calls.get(call_id).map_or("unknown", |c| c.state.as_str());
            return Err(Error::Conflict(format!("land_in_room requires Active, got {state}")));
        }
        self.rooms.entry(room_id.clone()).or_default().push(name.clone());
        tracing::info!(room_id = %room_id, call_id = %call_id, participant = %name, "SIP call landed in room");
        Ok(self.calls.get(call_id).expect("checked above").clone())
    }

    /// Имена участников mock-реестра комнаты.
    #[must_use]
    pub fn room_participants(&self, room_id: &str) -> &[String] {
        self.rooms.get(room_id).map_or(&[], Vec::as_slice)
    }

    /// Имя participant'а в комнате для данного вызова.
    fn participant_name(&self, call_id: &str) -> String {
        format!("{}@sip-gateway", self.calls.get(call_id).map_or(call_id, |c| c.from.as_str()))
    }

    /// Активный ли вызов в реестре.
    #[must_use]
    pub fn has_call(&self, call_id: &str) -> bool {
        self.calls.get(call_id).is_some_and(|c| c.state == CallState::Active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gateway_with_uas() -> SipGateway {
        let mut g = SipGateway::new();
        g.register_uas().expect("UAS registers");
        g
    }

    #[test]
    fn invite_rings_then_200_lands_participant() {
        let mut g = gateway_with_uas();
        let offer = [SipCodec::new("PCMU")];
        let call = g.on_invite("call-1", "alice", &offer).expect("INVITE accepted");
        assert_eq!(call.state, CallState::Ringing);
        assert_eq!(call.web_codec, "OPUS");
        assert!(call.needs_transcode);

        let call = g.accept("call-1").expect("200 OK");
        assert_eq!(call.state, CallState::Active);

        g.land_in_room("room-1", "call-1").expect("landed");
        let participants = g.room_participants("room-1");
        assert_eq!(participants, &vec!["alice@sip-gateway".to_owned()]);
        assert!(g.has_call("call-1"));
    }

    #[test]
    fn bye_terminates_and_removes_participant() {
        let mut g = gateway_with_uas();
        let offer = [SipCodec::new("PCMU")];
        g.on_invite("call-1", "alice", &offer).unwrap();
        g.accept("call-1").unwrap();
        g.land_in_room("room-1", "call-1").unwrap();

        let call = g.bye("call-1").expect("BYE");
        assert_eq!(call.state, CallState::Terminated);
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);
        assert!(!g.has_call("call-1"));
    }

    #[test]
    fn codec_negotiation_applied_per_call() {
        let mut g = gateway_with_uas();
        let call = g.on_invite("call-2", "bob", &[SipCodec::new("H264")]).unwrap();
        assert_eq!(call.web_codec, "VP8");
        assert!(call.needs_transcode);

        let call = g.on_invite("call-3", "carol", &[SipCodec::new("OPUS")]).unwrap();
        assert_eq!(call.web_codec, "OPUS");
        assert!(!call.needs_transcode);
    }

    #[test]
    fn on_invite_requires_uas() {
        let mut g = SipGateway::new();
        let err = g.on_invite("call-1", "alice", &[SipCodec::new("PCMU")]).expect_err("must fail without UAS");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn duplicate_call_id_rejected() {
        let mut g = gateway_with_uas();
        g.on_invite("call-1", "alice", &[SipCodec::new("PCMU")]).unwrap();
        let err = g.on_invite("call-1", "alice", &[SipCodec::new("PCMU")]).expect_err("duplicate");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn accept_without_invite_rejected() {
        let mut g = gateway_with_uas();
        let err = g.accept("missing").expect_err("not found");
        assert!(matches!(err, Error::NotFound(_)));
    }

    #[test]
    fn land_requires_active() {
        let mut g = gateway_with_uas();
        g.on_invite("call-1", "alice", &[SipCodec::new("PCMU")]).unwrap();
        let err = g.land_in_room("room-1", "call-1").expect_err("not active yet");
        assert!(matches!(err, Error::Conflict(_)));
    }

    #[test]
    fn double_uas_registration_rejected() {
        let mut g = gateway_with_uas();
        let err = g.register_uas().expect_err("already registered");
        assert!(matches!(err, Error::Conflict(_)));
    }
}

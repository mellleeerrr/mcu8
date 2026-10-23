//! SIP-вызов и в-memory SIP-шлюз (UAS).
//!
//! Фаза 6: приём SIP-вызова, «приземление» в WebRTC-комнату — вызов
//! становится participant'ом комнаты с ролью gateway.
//!
//! Автомат состояния (SIP UAS, RFC 3261):
//!
//!   Idle ──INVITE──► Ringing ──accept──► Active ──BYE──► Terminated
//!
//! Реальный SIP-транспорт ещё не подключён (ADR-0006); сейчас
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
    /// Сессионный идентификатор участника в шлюзе.
    pub participant_id: String,
    /// Имя текущей привязанной комнаты (если вызов приземлён).
    pub room_id: Option<String>,
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
        let cid = call_id.into();
        let frm = from.into();
        let pid = format!("{frm}:{cid}@sip-gateway");
        Self {
            call_id: cid,
            from: frm,
            participant_id: pid,
            room_id: None,
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
#[derive(Debug, Clone, PartialEq, Eq)]
struct RoomCallSlot {
    call_id: String,
    participant_name: String,
}

/// В-memory SIP-шлюз (UAS) с mock-реестром WebRTC-комнат.
///
/// Реестр комнат — замена реального room-реестра (vks-core/vks-storage):
/// хранит привязку вызовов к комнатам по Call-ID, предотвращая
/// ложное выталкивание участника при нескольких звонках одного абонента (M5.1).
#[derive(Debug, Default)]
pub struct SipGateway {
    /// UAS зарегистрирован (порт SIP открыт).
    uas_registered: bool,
    /// Активные вызовы по Call-ID.
    calls: HashMap<String, SipCall>,
    /// Mock-реестр комнат: room_id → слоты вызовов (call_id, имя).
    room_calls: HashMap<String, Vec<RoomCallSlot>>,
    /// Кэш участников комнат для эффективного возврата &[String].
    room_participants_cache: HashMap<String, Vec<String>>,
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

    /// Зарегистрирован ли SIP UAS (открыт ли порт/транспорт).
    #[must_use]
    pub fn is_registered(&self) -> bool {
        self.uas_registered
    }

    /// Отменяет регистрацию SIP UAS (закрывает транспорт).
    pub fn unregister(&mut self) {
        self.uas_registered = false;
        tracing::info!("SIP UAS unregistered");
    }

    /// Количество активных звонков.
    pub fn active_calls_count(&self) -> usize {
        self.calls.values().filter(|c| c.state == CallState::Active).count()
    }

    /// Принимает входящий INVITE: создаёт вызов в `Ringing` и
    /// согласовывает кодеки (SIP SDP → WebRTC SDP).
    ///
    /// # Errors
    /// [`Error::Conflict`] — если UAS не зарегистрирован или Call-ID уже существует.
    /// [`Error::InvalidInput`] — если оффер пуст или не содержит поддерживаемых кодеков.
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

        // Строгая валидация кодеков
        let selection = CodecMap::try_negotiate(sdp_offer)
            .map_err(|e| Error::InvalidInput(format!("unsupported codec offer: {e}")))?;

        let mut call = SipCall::new(call_id.clone(), from.into(), sdp_offer);
        call.sip_codec = selection.offered_codec;
        call.web_codec = selection.negotiated.web.name;
        call.needs_transcode = selection.negotiated.needs_transcode;

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

    /// Завершает вызов (BYE) и изолированно выводит его только из привязанной комнаты,
    /// не затрагивая параллельные вызовы того же абонента в других комнатах или той же комнате.
    /// Идемпотентен при повторном вызове (late BYE).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден.
    pub fn bye(&mut self, call_id: &str) -> Result<SipCall, Error> {
        let bound_room = {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            if call.state == CallState::Terminated {
                return Ok(call.clone());
            }
            if call.state == CallState::Ringing || call.state == CallState::Idle {
                call.state = CallState::Terminated;
            } else {
                call.bye()?;
            }
            call.room_id.take()
        };

        // Удаляем вызов строго по call_id
        if let Some(room) = bound_room {
            if let Some(slots) = self.room_calls.get_mut(&room) {
                slots.retain(|s| s.call_id != call_id);
                self.room_participants_cache
                    .insert(room.clone(), slots.iter().map(|s| s.participant_name.clone()).collect());
            }
        }

        let call = self.calls.get(call_id).expect("just transitioned").clone();
        tracing::info!(call_id = %call_id, "SIP call terminated");
        Ok(call)
    }

    /// Отменяет вызов до установления (CANCEL) и освобождает ресурсы.
    pub fn cancel(&mut self, call_id: &str) -> Result<SipCall, Error> {
        self.bye(call_id)
    }

    /// Завершает вызов по таймауту.
    pub fn timeout(&mut self, call_id: &str) -> Result<SipCall, Error> {
        self.bye(call_id)
    }

    /// Удаляет завершенные вызовы из реестра для ограничения памяти.
    pub fn prune_terminated(&mut self) -> usize {
        let before = self.calls.len();
        self.calls.retain(|_, call| call.state != CallState::Terminated);
        before - self.calls.len()
    }

    /// «Приземляет» активный вызов в WebRTC-комнату.
    /// Идемпотентен при повторном вызове в ту же комнату.
    /// При указании другой комнаты корректно переносит участника (Move).
    ///
    /// # Errors
    /// [`Error::NotFound`] — если вызов не найден.
    /// [`Error::Conflict`] — если вызов не в `Active`.
    pub fn land_in_room(&mut self, room_id: impl Into<String>, call_id: &str) -> Result<SipCall, Error> {
        let target_room = room_id.into();
        let (from, prev_room) = {
            let call = self.calls.get_mut(call_id).ok_or_else(|| Error::NotFound(call_id.to_owned()))?;
            if call.state != CallState::Active {
                return Err(Error::Conflict(format!("land_in_room requires Active, got {}", call.state.as_str())));
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

        // 2. Перенос: если вызов был в другой комнате — удаляем этот вызов из старой комнаты
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

        tracing::info!(room_id = %target_room, call_id = %call_id, participant = %name, "SIP call landed in room");
        Ok(self.calls.get(call_id).cloned().expect("exists"))
    }

    fn participant_name_for(from: &str) -> String {
        format!("{from}@sip-gateway")
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

    #[test]
    fn two_calls_same_from_in_different_rooms_isolated() {
        let mut g = gateway_with_uas();
        let offer = [SipCodec::new("PCMU")];

        // Абонент alice совершает два разных звонка в две разные комнаты
        g.on_invite("call-1", "alice", &offer).unwrap();
        g.accept("call-1").unwrap();
        g.land_in_room("room-1", "call-1").unwrap();

        g.on_invite("call-2", "alice", &offer).unwrap();
        g.accept("call-2").unwrap();
        g.land_in_room("room-2", "call-2").unwrap();

        assert_eq!(g.room_participants("room-1"), &["alice@sip-gateway"]);
        assert_eq!(g.room_participants("room-2"), &["alice@sip-gateway"]);

        // Завершаем call-1: он должен удалиться ТОЛЬКО из room-1
        g.bye("call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);
        assert_eq!(g.room_participants("room-2"), &["alice@sip-gateway"]);
        assert!(!g.has_call("call-1"));
        assert!(g.has_call("call-2"));

        // Теперь завершаем call-2: он удаляется из room-2
        g.bye("call-2").unwrap();
        assert_eq!(g.room_participants("room-2"), &[] as &[String]);
        assert!(!g.has_call("call-2"));
    }

    #[test]
    fn land_in_room_idempotent_and_move() {
        let mut g = gateway_with_uas();
        let offer = [SipCodec::new("PCMU")];
        g.on_invite("call-1", "alice", &offer).unwrap();
        g.accept("call-1").unwrap();

        // Первое приземление
        g.land_in_room("room-1", "call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &["alice@sip-gateway"]);

        // Повторное приземление в ту же комнату — идемпотентно, не дублирует
        g.land_in_room("room-1", "call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &["alice@sip-gateway"]);

        // Перенос в другую комнату
        g.land_in_room("room-2", "call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);
        assert_eq!(g.room_participants("room-2"), &["alice@sip-gateway"]);
    }

    #[test]
    fn unsupported_codec_offer_rejected() {
        let mut g = gateway_with_uas();
        // Пустой оффер
        let err = g.on_invite("call-empty", "alice", &[]).expect_err("empty offer rejected");
        assert!(matches!(err, Error::InvalidInput(_)));

        // Несовместимый оффер
        let err = g.on_invite("call-bad", "alice", &[SipCodec::new("AMR")]).expect_err("AMR rejected");
        assert!(matches!(err, Error::InvalidInput(_)));
    }

    #[test]
    fn two_calls_same_from_in_same_room_no_ejection() {
        let mut g = gateway_with_uas();
        let offer = [SipCodec::new("PCMU")];

        // Абонент alice совершает два звонка в ОДНУ комнату (например, аудио и видео)
        g.on_invite("call-1", "alice", &offer).unwrap();
        g.accept("call-1").unwrap();
        g.land_in_room("room-1", "call-1").unwrap();

        g.on_invite("call-2", "alice", &offer).unwrap();
        g.accept("call-2").unwrap();
        g.land_in_room("room-1", "call-2").unwrap();

        assert_eq!(g.room_participants("room-1"), &["alice@sip-gateway", "alice@sip-gateway"]);

        // Завершаем call-1: call-2 обязан остаться в room-1!
        g.bye("call-1").unwrap();
        assert_eq!(g.room_participants("room-1"), &["alice@sip-gateway"]);
        assert!(!g.has_call("call-1"));
        assert!(g.has_call("call-2"));

        // Late BYE на call-1 идемпотентен и не возвращает ошибку
        let late = g.bye("call-1").expect("late BYE is idempotent");
        assert_eq!(late.state, CallState::Terminated);

        // Завершаем call-2
        g.bye("call-2").unwrap();
        assert_eq!(g.room_participants("room-1"), &[] as &[String]);

        // Прунинг завершенных вызовов
        let pruned = g.prune_terminated();
        assert_eq!(pruned, 2);
    }

    #[test]
    fn cancel_and_timeout_during_ringing() {
        let mut g = gateway_with_uas();
        let offer = [SipCodec::new("PCMU")];
        g.on_invite("call-cancel", "alice", &offer).unwrap();
        let call = g.cancel("call-cancel").expect("cancel succeeds");
        assert_eq!(call.state, CallState::Terminated);

        g.on_invite("call-timeout", "bob", &offer).unwrap();
        let call = g.timeout("call-timeout").expect("timeout succeeds");
        assert_eq!(call.state, CallState::Terminated);
    }
}

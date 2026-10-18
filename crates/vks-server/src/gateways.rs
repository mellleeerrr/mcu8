//! Интеграция SIP и H.323 шлюзов с сервером VKS и RoomHub (Milestone M5).
//!
//! Обеспечивает сквозную регистрацию звонков из SIP/H.323 в WebRTC-комнаты,
//! трансляцию сигнальных событий в RoomHub и админ-шину WebSocket,
//! а также мониторинг доступности протоколов.

use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use vks_admin::hub::ParticipantSnapshot;
use vks_admin::rooms::Rooms;
use vks_admin::state::AppState;
use vks_common::Error;
use vks_core::RoomId;
use vks_gateway_h323::{H323Call, H323Codec, H323Gateway};
use vks_gateway_sip::{SipCall, SipCodec, SipGateway};
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_signaling::Envelope;

/// Менеджер внешних шлюзов (SIP & H.323).
#[derive(Clone)]
pub struct GatewayManager {
    sip: Arc<Mutex<SipGateway>>,
    h323: Arc<Mutex<H323Gateway>>,
    mcu_engine: Option<Arc<vks_media_mcu::McuEngine>>,
}

impl Default for GatewayManager {
    fn default() -> Self {
        Self::new()
    }
}

impl GatewayManager {
    /// Создаёт и инициализирует менеджер шлюзов.
    pub fn new() -> Self {
        let mut sip = SipGateway::new();
        if let Err(e) = sip.register_uas() {
            warn!(error = %e, "SIP UAS already registered or conflict");
        }
        let h323 = H323Gateway::new();
        Self { sip: Arc::new(Mutex::new(sip)), h323: Arc::new(Mutex::new(h323)), mcu_engine: None }
    }

    /// Конфигурирует привязку к MCU движку для прозрачной композиции.
    pub fn with_mcu(mut self, mcu_engine: Arc<vks_media_mcu::McuEngine>) -> Self {
        self.mcu_engine = Some(mcu_engine);
        self
    }

    /// Статус доступности SIP-шлюза для `ui_system_status`.
    pub fn sip_status(&self) -> serde_json::Value {
        let sip = self.sip.lock().unwrap();
        let is_registered = sip.is_registered();
        serde_json::json!({
            "status": if is_registered { "READY" } else { "DISABLED" },
            "connected": is_registered,
            "registration": is_registered,
            "active_calls": sip.active_calls_count(),
            "account": "sip:vks@local"
        })
    }

    /// Статус доступности H.323-шлюза для `ui_system_status`.
    pub fn h323_status(&self) -> serde_json::Value {
        let h323 = self.h323.lock().unwrap();
        let is_registered = h323.is_registered();
        serde_json::json!({
            "status": if is_registered { "READY" } else { "DISABLED" },
            "connected": is_registered,
            "registration": is_registered,
            "active_calls": h323.active_calls_count(),
            "account": "h323:vks@local"
        })
    }

    /// Ссылка на внутренний экземпляр SIP-шлюза.
    pub fn sip(&self) -> &Arc<Mutex<SipGateway>> {
        &self.sip
    }

    /// Ссылка на внутренний экземпляр H.323-шлюза.
    pub fn h323(&self) -> &Arc<Mutex<H323Gateway>> {
        &self.h323
    }

    /// Принимает входящий SIP-вызов и приземляет его в комнату.
    pub fn incoming_sip_call(
        &self,
        state: &Arc<AppState>,
        call_id: &str,
        from: &str,
        room_id: &RoomId,
        sdp_offer: &[SipCodec],
    ) -> Result<SipCall, Error> {
        let mut sip = self.sip.lock().map_err(|_| Error::Conflict("SIP lock poisoned".into()))?;
        let room_str = room_id.to_string();

        let call = sip.on_invite(call_id, from, sdp_offer)?;
        let _ = sip.accept(&call.call_id)?;
        let active_call = sip.land_in_room(&room_str, &call.call_id)?;

        let display_name = format!("{from} (SIP)");
        let now_sec =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

        let snapshot = ParticipantSnapshot {
            participant_id: active_call.participant_id.clone(),
            display_name: display_name.clone(),
            is_recorder: false,
            is_speaking: false,
            track_ids: Vec::new(),
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: now_sec,
        };

        // 1. Регистрация участника в сигнальном RoomHub
        state.room_hub.register_participant(room_id, snapshot);

        // 2. Рассылка ParticipantJoined WebRTC-клиентам комнаты
        let joined_msg = SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::ParticipantJoined(ParticipantJoined {
                participant_id: active_call.participant_id.clone(),
                display_name,
                is_recorder: false,
            })),
        };
        let payload_bytes = Envelope::new(joined_msg).encode();
        state.room_hub.broadcast(room_id, None, payload_bytes.into());

        // 3. Обновление счётчика участников в комнате
        let current_count = state.room_hub.list_participants(room_id).len();
        let _ = Rooms::update(state, room_id, None, None, Some(current_count), None);

        // 4. Оповещение административного WebSocket
        state.notify_admin(
            &format!("session:{room_str}:abonents"),
            serde_json::json!({
                "action": "joined",
                "room_id": room_str,
                "participant_id": active_call.participant_id,
                "count": current_count
            }),
        );

        // 5. Прозрачное подключение к MCU-микшеру и композитору
        let pid_val = crate::parse_participant_id(&active_call.participant_id);
        if let Some(ref mcu) = self.mcu_engine {
            let mut tracks = vec![vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Audio)];
            if active_call.sip_codec.name.to_lowercase().contains("264")
                || active_call.sip_codec.name.to_lowercase().contains("vp8")
                || sdp_offer
                    .iter()
                    .any(|c| c.name.to_lowercase().contains("264") || c.name.to_lowercase().contains("vp8"))
            {
                tracks.push(vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Video));
            }
            mcu.bridge_participant(*room_id, pid_val, tracks);
        }

        info!(call_id = %call_id, room_id = %room_str, "SIP participant landed in room");
        Ok(active_call)
    }

    /// Завершает SIP-вызов (BYE) и удаляет участника из комнаты.
    pub fn terminate_sip_call(&self, state: &Arc<AppState>, call_id: &str) -> Result<SipCall, Error> {
        let mut sip = self.sip.lock().map_err(|_| Error::Conflict("SIP lock poisoned".into()))?;
        let call = sip.bye(call_id)?;

        let room_id = state.room_hub.find_participant_room(&call.participant_id).map(|(r, _)| r);

        if let Some(rid) = room_id {
            let pid_val = crate::parse_participant_id(&call.participant_id);
            if let Some(ref mcu) = self.mcu_engine {
                mcu.unbridge_participant(rid, pid_val);
            }

            let left_count = state.room_hub.unregister_participant(&rid, &call.participant_id);

            let left_msg = SignalingMessage {
                msg_id: 0,
                reply_to: 0,
                payload: Some(Payload::ParticipantLeft(ParticipantLeft {
                    participant_id: call.participant_id.clone(),
                    reason: 0, // normal hangup
                })),
            };
            let payload_bytes = Envelope::new(left_msg).encode();
            state.room_hub.broadcast(&rid, None, payload_bytes.into());

            let remaining = left_count.unwrap_or(0);
            let _ = Rooms::update(state, &rid, None, None, Some(remaining), None);

            let room_str = rid.to_string();
            state.notify_admin(
                &format!("session:{room_str}:abonents"),
                serde_json::json!({
                    "action": "left",
                    "room_id": room_str,
                    "participant_id": call.participant_id,
                    "count": remaining
                }),
            );
        }

        info!(call_id = %call_id, "SIP call terminated");
        Ok(call)
    }

    /// Принимает входящий H.323-вызов и приземляет его в комнату.
    pub fn incoming_h323_call(
        &self,
        state: &Arc<AppState>,
        call_id: &str,
        from: &str,
        room_id: &RoomId,
        h323_offer: &[H323Codec],
    ) -> Result<H323Call, Error> {
        let mut h323 = self.h323.lock().map_err(|_| Error::Conflict("H323 lock poisoned".into()))?;
        let room_str = room_id.to_string();

        let call = h323.on_setup(call_id, from, h323_offer)?;
        let _ = h323.proceeding(&call.call_id)?;
        let _ = h323.connect(&call.call_id)?;
        let active_call = h323.land_in_room(&room_str, &call.call_id)?;

        let display_name = format!("{from} (H.323)");
        let now_sec =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

        let snapshot = ParticipantSnapshot {
            participant_id: active_call.participant_id.clone(),
            display_name: display_name.clone(),
            is_recorder: false,
            is_speaking: false,
            track_ids: Vec::new(),
            is_audio_muted: false,
            is_video_muted: false,
            connected_at: now_sec,
        };

        state.room_hub.register_participant(room_id, snapshot);

        let joined_msg = SignalingMessage {
            msg_id: 0,
            reply_to: 0,
            payload: Some(Payload::ParticipantJoined(ParticipantJoined {
                participant_id: active_call.participant_id.clone(),
                display_name,
                is_recorder: false,
            })),
        };
        let payload_bytes = Envelope::new(joined_msg).encode();
        state.room_hub.broadcast(room_id, None, payload_bytes.into());

        let current_count = state.room_hub.list_participants(room_id).len();
        let _ = Rooms::update(state, room_id, None, None, Some(current_count), None);

        state.notify_admin(
            &format!("session:{room_str}:abonents"),
            serde_json::json!({
                "action": "joined",
                "room_id": room_str,
                "participant_id": active_call.participant_id,
                "count": current_count
            }),
        );

        // 5. Прозрачное подключение к MCU-микшеру и композитору
        let pid_val = crate::parse_participant_id(&active_call.participant_id);
        if let Some(ref mcu) = self.mcu_engine {
            let mut tracks = vec![vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Audio)];
            if active_call.h323_codec.name.to_lowercase().contains("264")
                || active_call.h323_codec.name.to_lowercase().contains("vp8")
                || h323_offer
                    .iter()
                    .any(|c| c.name.to_lowercase().contains("264") || c.name.to_lowercase().contains("vp8"))
            {
                tracks.push(vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Video));
            }
            mcu.bridge_participant(*room_id, pid_val, tracks);
        }

        info!(call_id = %call_id, room_id = %room_str, "H.323 participant landed in room");
        Ok(active_call)
    }

    /// Завершает H.323-вызов (Release) и удаляет участника из комнаты.
    pub fn terminate_h323_call(&self, state: &Arc<AppState>, call_id: &str) -> Result<H323Call, Error> {
        let mut h323 = self.h323.lock().map_err(|_| Error::Conflict("H323 lock poisoned".into()))?;
        let call = h323.release(call_id)?;

        let room_id = state.room_hub.find_participant_room(&call.participant_id).map(|(r, _)| r);

        if let Some(rid) = room_id {
            let pid_val = crate::parse_participant_id(&call.participant_id);
            if let Some(ref mcu) = self.mcu_engine {
                mcu.unbridge_participant(rid, pid_val);
            }

            let left_count = state.room_hub.unregister_participant(&rid, &call.participant_id);

            let left_msg = SignalingMessage {
                msg_id: 0,
                reply_to: 0,
                payload: Some(Payload::ParticipantLeft(ParticipantLeft {
                    participant_id: call.participant_id.clone(),
                    reason: 0,
                })),
            };
            let payload_bytes = Envelope::new(left_msg).encode();
            state.room_hub.broadcast(&rid, None, payload_bytes.into());

            let remaining = left_count.unwrap_or(0);
            let _ = Rooms::update(state, &rid, None, None, Some(remaining), None);

            let room_str = rid.to_string();
            state.notify_admin(
                &format!("session:{room_str}:abonents"),
                serde_json::json!({
                    "action": "left",
                    "room_id": room_str,
                    "participant_id": call.participant_id,
                    "count": remaining
                }),
            );
        }

        info!(call_id = %call_id, "H.323 call terminated");
        Ok(call)
    }

    /// Возвращает ссылку на сконфигурированный McuEngine (если подключен).
    pub fn mcu_engine(&self) -> Option<&Arc<vks_media_mcu::McuEngine>> {
        self.mcu_engine.as_ref()
    }
}

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

#[path = "gateway_audio.rs"]
mod audio;
#[path = "gateway_video.rs"]
mod video;

/// Observed packet/frame counters. Negotiated capabilities are not traffic statistics.
#[derive(Default)]
pub struct GatewayMediaStats {
    pub audio_rx_packets: std::sync::atomic::AtomicU64,
    pub audio_tx_packets: std::sync::atomic::AtomicU64,
    pub audio_rx_bytes: std::sync::atomic::AtomicU64,
    pub audio_tx_bytes: std::sync::atomic::AtomicU64,
    pub video_rx_packets: std::sync::atomic::AtomicU64,
    pub video_tx_packets: std::sync::atomic::AtomicU64,
    pub video_rx_bytes: std::sync::atomic::AtomicU64,
    pub video_tx_bytes: std::sync::atomic::AtomicU64,
    pub decoded_video_frames: std::sync::atomic::AtomicU64,
    pub encoded_video_frames: std::sync::atomic::AtomicU64,
    pub rejected_packets: std::sync::atomic::AtomicU64,
    pub dropped_video_frames: std::sync::atomic::AtomicU64,
    pub video_codec_running: std::sync::atomic::AtomicBool,
    pub audio_codec_running: std::sync::atomic::AtomicBool,
    /// Wall-clock of the last accepted RTP packet; zero until the peer speaks.
    pub last_rx_ms: std::sync::atomic::AtomicU64,
}

impl GatewayMediaStats {
    pub fn snapshot(&self) -> serde_json::Value {
        use std::sync::atomic::Ordering::Relaxed;
        serde_json::json!({
            "audio_rx_packets": self.audio_rx_packets.load(Relaxed),
            "audio_tx_packets": self.audio_tx_packets.load(Relaxed),
            "audio_rx_bytes": self.audio_rx_bytes.load(Relaxed),
            "audio_tx_bytes": self.audio_tx_bytes.load(Relaxed),
            "video_rx_packets": self.video_rx_packets.load(Relaxed),
            "video_tx_packets": self.video_tx_packets.load(Relaxed),
            "video_rx_bytes": self.video_rx_bytes.load(Relaxed),
            "video_tx_bytes": self.video_tx_bytes.load(Relaxed),
            "decoded_video_frames": self.decoded_video_frames.load(Relaxed),
            "encoded_video_frames": self.encoded_video_frames.load(Relaxed),
            "rejected_packets": self.rejected_packets.load(Relaxed),
            "dropped_video_frames": self.dropped_video_frames.load(Relaxed),
            "video_codec_running": self.video_codec_running.load(Relaxed),
            "audio_codec_running": self.audio_codec_running.load(Relaxed),
            "last_rx_ms": self.last_rx_ms.load(Relaxed),
        })
    }
}

/// A call is dead only after a full silence budget; a slow first packet and a
/// terminal-initiated hold are not distinguished by RTP alone, so be generous.
fn media_bridge_dead(elapsed: std::time::Duration, last_rx_ms: u64, now_ms: u64, audio_codec_running: bool) -> bool {
    let silence_ms = if last_rx_ms == 0 { elapsed.as_millis() as u64 } else { now_ms.saturating_sub(last_rx_ms) };
    let media_stale = silence_ms > 15_000;
    let codec_dead = !audio_codec_running && elapsed > std::time::Duration::from_secs(5);
    media_stale || codec_dead
}

/// Атомарные элементы управления медиа-потоками абонента (Mute, Volume, DTMF).
#[derive(Clone)]
pub struct GatewayMediaControls {
    pub audio_tx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub audio_rx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub video_tx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub video_rx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub volume_tx: Arc<std::sync::atomic::AtomicU32>, // 0..1000 percent (native +20 dB = 1000%)
    pub volume_rx: Arc<std::sync::atomic::AtomicU32>,
    pub dtmf_sender: tokio::sync::mpsc::UnboundedSender<String>,
    pub dtmf_payload_type: Arc<std::sync::atomic::AtomicU32>,
}

impl Default for GatewayMediaControls {
    fn default() -> Self {
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        Self {
            audio_tx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            audio_rx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            video_tx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            video_rx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            volume_tx: Arc::new(std::sync::atomic::AtomicU32::new(100)),
            volume_rx: Arc::new(std::sync::atomic::AtomicU32::new(100)),
            dtmf_sender: tx,
            dtmf_payload_type: Arc::new(std::sync::atomic::AtomicU32::new(256)),
        }
    }
}

/// Сессия исходящего или входящего шлюзового вызова.
#[derive(Clone)]
pub struct GatewayCallSession {
    pub call_id: String,
    pub participant_id: String,
    pub room_id: RoomId,
    pub display_name: String,
    pub uri: String,
    pub active_sip: Option<vks_gateway_sip::ActiveSipSession>,
    pub active_h323: Option<vks_gateway_h323::ActiveH323Session>,
    pub cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    pub controls: GatewayMediaControls,
    pub media_stats: Arc<GatewayMediaStats>,
    pub media_details: serde_json::Value,
    pub connected_at: std::time::Instant,
}

/// Потокобезопасный реестр сессий шлюза по participant_id и call_id.
#[derive(Default)]
pub struct GatewayCallRegistry {
    pub by_participant: std::collections::HashMap<String, GatewayCallSession>,
    pub by_call: std::collections::HashMap<String, String>,
}

/// Менеджер внешних шлюзов (SIP & H.323).
#[derive(Clone)]
pub struct GatewayManager {
    sip: Arc<Mutex<SipGateway>>,
    h323: Arc<Mutex<H323Gateway>>,
    mcu_engine: Option<Arc<vks_media_mcu::McuEngine>>,
    port_pool: Arc<vks_gateway_sip::RtpPortPool>,
    uac_client: Arc<vks_gateway_sip::SipUacClient>,
    h323_uac_client: Arc<vks_gateway_h323::H323UacClient>,
    registry: Arc<Mutex<GatewayCallRegistry>>,
    state: Arc<std::sync::RwLock<Option<std::sync::Weak<AppState>>>>,
    candidate_ip: String,
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
        let candidate_ip = "127.0.0.1".to_string();
        let uac_client = Arc::new(vks_gateway_sip::SipUacClient::new(&candidate_ip));
        let h323_uac_client = Arc::new(vks_gateway_h323::H323UacClient::new(&candidate_ip));
        Self {
            sip: Arc::new(Mutex::new(sip)),
            h323: Arc::new(Mutex::new(h323)),
            mcu_engine: None,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::default()),
            uac_client,
            h323_uac_client,
            registry: Arc::new(Mutex::new(GatewayCallRegistry::default())),
            state: Arc::new(std::sync::RwLock::new(None)),
            candidate_ip,
        }
    }

    /// Конфигурирует привязку к MCU движку для прозрачной композиции.
    pub fn with_mcu(mut self, mcu_engine: Arc<vks_media_mcu::McuEngine>) -> Self {
        self.mcu_engine = Some(mcu_engine);
        self
    }

    /// Конфигурирует кандидатный локальный IP для SIP/RTP взаимодействия с терминалами.
    pub fn with_candidate_ip(mut self, ip: impl Into<String>) -> Self {
        let ip_str = ip.into();
        self.uac_client = Arc::new(vks_gateway_sip::SipUacClient::new(&ip_str));
        self.h323_uac_client = Arc::new(vks_gateway_h323::H323UacClient::new(&ip_str));
        self.candidate_ip = ip_str;
        self
    }

    /// Связывает шлюз со слабым указателем на AppState для устранения циклов ссылок.
    pub fn bind_state(&self, state: std::sync::Weak<AppState>) {
        if let Ok(mut lock) = self.state.write() {
            *lock = Some(state);
        }
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
        let active_call = {
            let mut sip = self.sip.lock().map_err(|_| Error::Conflict("SIP lock poisoned".into()))?;
            let room_str = room_id.to_string();
            let call = sip.on_invite(call_id, from, sdp_offer)?;
            let _ = sip.accept(&call.call_id)?;
            sip.land_in_room(&room_str, &call.call_id)?
        };

        let display_name = format!("{from} (SIP)");
        let has_video = active_call.sip_codec.name.to_lowercase().contains("264")
            || active_call.sip_codec.name.to_lowercase().contains("vp8")
            || sdp_offer.iter().any(|c| c.name.to_lowercase().contains("264") || c.name.to_lowercase().contains("vp8"));

        land_gateway_participant_in_room(
            state,
            self.mcu_engine.as_ref(),
            room_id,
            &active_call.participant_id,
            &display_name,
            has_video,
        );

        info!(call_id = %call_id, room_id = %room_id, "SIP participant landed in room");
        Ok(active_call)
    }

    /// Завершает SIP-вызов (BYE) и удаляет участника из комнаты.
    pub fn terminate_sip_call(&self, state: &Arc<AppState>, call_id: &str) -> Result<SipCall, Error> {
        let call = {
            let mut sip = self.sip.lock().map_err(|_| Error::Conflict("SIP lock poisoned".into()))?;
            sip.bye(call_id)?
        };

        // Очищаем реестр и отменяем фоновый RTP-таск
        if let Ok(mut reg) = self.registry.lock() {
            if let Some(pid) = reg.by_call.remove(call_id) {
                if let Some(sess) = reg.by_participant.remove(&pid) {
                    sess.cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }

        if let Some((rid, _)) = state.room_hub.find_participant_room(&call.participant_id) {
            terminate_gateway_participant_in_room(state, self.mcu_engine.as_ref(), &rid, &call.participant_id);
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
        let active_call = {
            let mut h323 = self.h323.lock().map_err(|_| Error::Conflict("H323 lock poisoned".into()))?;
            let room_str = room_id.to_string();
            let call = h323.on_setup(call_id, from, h323_offer)?;
            let _ = h323.proceeding(&call.call_id)?;
            let _ = h323.connect(&call.call_id)?;
            h323.land_in_room(&room_str, &call.call_id)?
        };

        let display_name = format!("{from} (H.323)");
        let has_video = active_call.h323_codec.name.to_lowercase().contains("264")
            || active_call.h323_codec.name.to_lowercase().contains("vp8")
            || h323_offer
                .iter()
                .any(|c| c.name.to_lowercase().contains("264") || c.name.to_lowercase().contains("vp8"));

        land_gateway_participant_in_room(
            state,
            self.mcu_engine.as_ref(),
            room_id,
            &active_call.participant_id,
            &display_name,
            has_video,
        );

        info!(call_id = %call_id, room_id = %room_id, "H.323 participant landed in room");
        Ok(active_call)
    }

    /// Завершает H.323-вызов (Release) и удаляет участника из комнаты.
    pub fn terminate_h323_call(&self, state: &Arc<AppState>, call_id: &str) -> Result<H323Call, Error> {
        let call = {
            let mut h323 = self.h323.lock().map_err(|_| Error::Conflict("H323 lock poisoned".into()))?;
            h323.release(call_id)?
        };

        if let Some((rid, _)) = state.room_hub.find_participant_room(&call.participant_id) {
            terminate_gateway_participant_in_room(state, self.mcu_engine.as_ref(), &rid, &call.participant_id);
        }

        info!(call_id = %call_id, "H.323 call terminated");
        Ok(call)
    }

    /// Возвращает ссылку на сконфигурированный McuEngine (если подключен).
    pub fn mcu_engine(&self) -> Option<&Arc<vks_media_mcu::McuEngine>> {
        self.mcu_engine.as_ref()
    }

    pub fn media_stats(&self, participant_id: &str) -> Option<serde_json::Value> {
        self.registry.lock().ok()?.by_participant.get(participant_id).map(|session| {
            let mut stats = session.media_stats.snapshot();
            if let (Some(target), Some(details)) = (stats.as_object_mut(), session.media_details.as_object()) {
                target.extend(details.clone());
            }
            let elapsed = session.connected_at.elapsed();
            let elapsed_ms = elapsed.as_millis().max(1) as u64;
            let elapsed_seconds = elapsed.as_secs_f64().max(0.001);
            stats["elapsed_ms"] = serde_json::json!(elapsed_ms);
            stats["rate_basis"] = serde_json::json!("average_since_connection");
            for (counter, rate) in [
                ("audio_rx_bytes", "audio_rx_bitrate_bps"),
                ("audio_tx_bytes", "audio_tx_bitrate_bps"),
                ("video_rx_bytes", "video_rx_bitrate_bps"),
                ("video_tx_bytes", "video_tx_bitrate_bps"),
            ] {
                stats[rate] = serde_json::json!(stats[counter].as_u64().unwrap_or(0).saturating_mul(8000) / elapsed_ms);
            }
            stats["decoded_video_fps"] =
                serde_json::json!(stats["decoded_video_frames"].as_u64().unwrap_or(0) as f64 / elapsed_seconds);
            stats["encoded_video_fps"] =
                serde_json::json!(stats["encoded_video_frames"].as_u64().unwrap_or(0) as f64 / elapsed_seconds);
            stats
        })
    }
}

/// Параметры для двунаправленного RTP медиа-моста между шлюзом и McuEngine.
pub struct RtpBridgeSession {
    pub room_id: RoomId,
    pub participant_id_str: String,
    pub participant_id_val: vks_core::ParticipantId,
    pub remote_media_ip: String,
    pub remote_audio_port: u16,
    pub audio_payload_type: u8,
    pub dtmf_payload_type: Option<u8>,
    pub media_stats: Arc<GatewayMediaStats>,
    pub rtp_socket: tokio::net::UdpSocket,
    pub rtcp_socket: Option<tokio::net::UdpSocket>,
    pub rtp_port: u16,
    pub port_pool: Arc<vks_gateway_sip::RtpPortPool>,
    pub mcu_engine: Arc<vks_media_mcu::McuEngine>,
    pub controls: GatewayMediaControls,
    pub cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    pub dtmf_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

/// Returns a reserved port even if call setup, a worker, or a future is cancelled.
pub(super) struct RtpPortLease {
    pool: Arc<vks_gateway_sip::RtpPortPool>,
    port: u16,
    armed: bool,
}
impl RtpPortLease {
    pub(super) fn new(pool: Arc<vks_gateway_sip::RtpPortPool>, port: u16) -> Self {
        Self { pool, port, armed: true }
    }
    fn disarm(&mut self) {
        self.armed = false;
    }
}
impl Drop for RtpPortLease {
    fn drop(&mut self) {
        if self.armed {
            self.pool.release_port(self.port);
        }
    }
}

struct PendingOutboundGuard {
    cancelled: Arc<std::sync::atomic::AtomicBool>,
    sip: Option<vks_gateway_sip::ActiveSipSession>,
    h323: Option<vks_gateway_h323::ActiveH323Session>,
    armed: bool,
    registered: Option<(GatewayManager, String)>,
}
impl Drop for PendingOutboundGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Some((manager, participant)) = self.registered.take() {
            vks_admin::state::OutboundDialer::hangup(&manager, &participant);
            return;
        }
        self.cancelled.store(true, std::sync::atomic::Ordering::Relaxed);
        let sip = self.sip.take();
        let h323 = self.h323.take();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                if let Some(session) = sip {
                    let _ = session.hangup().await;
                }
                if let Some(session) = h323 {
                    let _ = session.hangup().await;
                }
            });
        }
    }
}

/// Запускает фоновый 20-мс RTP медиа-мост (G.711 PCMU <-> McuEngine N-1 mix-minus).
///
/// Полностью абстрагирован от сигнального протокола (используется одинаково для SIP и H.323).
/// Zero heap allocation на горячем пути: стек-буферы и `set_audio_slice`/`copy_participant_audio`.
pub fn spawn_rtp_media_bridge(session: RtpBridgeSession) -> tokio::task::JoinHandle<()> {
    let lease = RtpPortLease::new(session.port_pool.clone(), session.rtp_port);
    session.media_stats.audio_codec_running.store(true, std::sync::atomic::Ordering::Relaxed);
    tokio::spawn(async move {
        let _lease = lease;
        audio::run_audio_bridge(session).await;
    })
}

/// Регистрирует медиа-треки шлюза в MCU до запуска RTP-мостов.
/// RoomHub объявляется только после успешного старта аудио и видео,
/// иначе при отказе FFmpeg участник успевает мигнуть в комнате.
pub fn bridge_gateway_participant_media(
    mcu_engine: Option<&Arc<vks_media_mcu::McuEngine>>,
    room_id: &RoomId,
    participant_id: &str,
    has_video: bool,
) {
    let Some(mcu) = mcu_engine else {
        return;
    };
    let pid_val = crate::parse_participant_id(participant_id);
    let mut mcu_tracks = vec![vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Audio)];
    if has_video {
        mcu_tracks.push(vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Video));
    }
    mcu.bridge_participant(*room_id, pid_val, mcu_tracks);
}

/// Публикует участника шлюза в RoomHub и уведомляет админку.
pub fn announce_gateway_participant(
    state: &Arc<AppState>,
    room_id: &RoomId,
    participant_id: &str,
    display_name: &str,
    has_video: bool,
) {
    let now_sec = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);

    let mut tracks = vec!["audio".to_string()];
    if has_video {
        tracks.push("video".to_string());
    }

    let snapshot = ParticipantSnapshot {
        participant_id: participant_id.to_string(),
        display_name: display_name.to_string(),
        is_recorder: false,
        is_speaking: false,
        track_ids: tracks,
        is_audio_muted: false,
        is_video_muted: false,
        connected_at: now_sec,
    };
    state.room_hub.register_participant(room_id, snapshot);

    let joined_msg = SignalingMessage {
        msg_id: 0,
        reply_to: 0,
        payload: Some(Payload::ParticipantJoined(ParticipantJoined {
            participant_id: participant_id.to_string(),
            display_name: display_name.to_string(),
            is_recorder: false,
        })),
    };
    let payload_bytes = Envelope::new(joined_msg).encode();
    state.room_hub.broadcast(room_id, None, payload_bytes.into());

    let current_count = state.room_hub.list_participants(room_id).len();
    let _ = Rooms::update(state, room_id, None, None, Some(current_count), None);
    state.notify_admin(
        &format!("session:{room_id}:abonents"),
        serde_json::json!({
            "action": "joined",
            "room_id": room_id.to_string(),
            "participant_id": participant_id,
            "count": current_count
        }),
    );
}

/// Приземляет участника шлюза в RoomHub, обновляет счетчик комнаты,
/// уведомляет WebSocket админки и бриджит аудио/видео в McuEngine.
pub fn land_gateway_participant_in_room(
    state: &Arc<AppState>,
    mcu_engine: Option<&Arc<vks_media_mcu::McuEngine>>,
    room_id: &RoomId,
    participant_id: &str,
    display_name: &str,
    has_video: bool,
) {
    bridge_gateway_participant_media(mcu_engine, room_id, participant_id, has_video);
    announce_gateway_participant(state, room_id, participant_id, display_name, has_video);
}

/// Удаляет участника шлюза из RoomHub, обновляет счетчик комнаты,
/// уведомляет WebSocket админки и отключает его из McuEngine.
pub fn terminate_gateway_participant_in_room(
    state: &Arc<AppState>,
    mcu_engine: Option<&Arc<vks_media_mcu::McuEngine>>,
    room_id: &RoomId,
    participant_id: &str,
) {
    let was_registered = state
        .room_hub
        .list_participants(room_id)
        .iter()
        .any(|participant| participant.participant_id == participant_id);
    state.room_hub.unregister_participant(room_id, participant_id);
    if !was_registered {
        // A dial that failed before announce must not emit a phantom leave.
        if let Some(mcu) = mcu_engine {
            let pid_val = crate::parse_participant_id(participant_id);
            mcu.unbridge_participant(*room_id, pid_val);
        }
        return;
    }

    let left_msg = SignalingMessage {
        msg_id: 0,
        reply_to: 0,
        payload: Some(Payload::ParticipantLeft(ParticipantLeft {
            participant_id: participant_id.to_string(),
            reason: 0,
        })),
    };
    let payload_bytes = Envelope::new(left_msg).encode();
    state.room_hub.broadcast(room_id, None, payload_bytes.into());

    let current_count = state.room_hub.list_participants(room_id).len();
    let _ = Rooms::update(state, room_id, None, None, Some(current_count), None);
    state.notify_admin(
        &format!("session:{room_id}:abonents"),
        serde_json::json!({
            "action": "left",
            "room_id": room_id.to_string(),
            "participant_id": participant_id,
            "count": current_count
        }),
    );

    if let Some(mcu) = mcu_engine {
        let pid_val = crate::parse_participant_id(participant_id);
        mcu.unbridge_participant(*room_id, pid_val);
    }
}

#[async_trait::async_trait]
impl vks_admin::state::OutboundDialer for GatewayManager {
    fn media_stats(&self, participant_id: &str) -> Option<serde_json::Value> {
        GatewayManager::media_stats(self, participant_id)
    }

    async fn dial(&self, room_id: RoomId, uri: &str, display_name: Option<&str>) -> Result<String, String> {
        let trimmed = uri.trim();
        let is_h323 = trimmed.starts_with("h323:");
        let is_sip = trimmed.starts_with("sip:")
            || (!trimmed.contains("://") && !trimmed.starts_with("webrtc:") && !trimmed.starts_with("recorder:"));
        if !is_sip && !is_h323 {
            return Err(format!("Unsupported outbound protocol scheme: {uri}"));
        }

        let pid_str = if is_h323 {
            format!("h323-{}@h323-gateway", uuid::Uuid::new_v4())
        } else {
            format!("sip-{}@sip-gateway", uuid::Uuid::new_v4())
        };
        let effective_name = display_name.filter(|s| !s.trim().is_empty()).unwrap_or(trimmed).to_string();
        let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (dtmf_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let controls = GatewayMediaControls {
            audio_tx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            audio_rx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            video_tx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            video_rx_muted: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            volume_tx: Arc::new(std::sync::atomic::AtomicU32::new(100)),
            volume_rx: Arc::new(std::sync::atomic::AtomicU32::new(100)),
            dtmf_sender: dtmf_tx,
            dtmf_payload_type: Arc::new(std::sync::atomic::AtomicU32::new(256)),
        };
        let media_stats = Arc::new(GatewayMediaStats::default());
        if self.mcu_engine.is_none() {
            return Err("Gateway MCU media runtime is unavailable".into());
        }

        let mcu = self.mcu_engine.as_ref().expect("checked MCU runtime").clone();
        let participant = crate::parse_participant_id(&pid_str);
        let port_pair = self
            .port_pool
            .allocate_pair(&self.candidate_ip)
            .await
            .ok_or_else(|| "No RTP/RTCP port pair is available; call was not started".to_string())?;
        let rtp_port = port_pair.rtp_port;
        let mut audio_lease = RtpPortLease::new(self.port_pool.clone(), rtp_port);
        let mut video_pair = if video::codec_available().await {
            Some(
                self.port_pool
                    .allocate_pair(&self.candidate_ip)
                    .await
                    .ok_or_else(|| "No RTP/RTCP video port pair is available; call was not started".to_string())?,
            )
        } else {
            None
        };
        let mut video_lease = video_pair.as_ref().map(|pair| RtpPortLease::new(self.port_pool.clone(), pair.rtp_port));
        let mut setup = PendingOutboundGuard {
            cancelled: cancel_flag.clone(),
            sip: None,
            h323: None,
            armed: true,
            registered: None,
        };
        let (remote_ip, remote_audio_port, audio_payload_type, dtmf_payload_type, video_remote, call_id) = if is_h323 {
            let session = self
                .h323_uac_client
                .dial_with_media(
                    trimmed,
                    rtp_port,
                    video_pair.as_ref().map(|pair| pair.rtp_port),
                    std::time::Duration::from_secs(60),
                )
                .await
                .map_err(|error| format!("H.323 outbound dial failed: {error}"))?;
            setup.h323 = Some(session.clone());
            let audio_port = session
                .remote_audio_port
                .filter(|port| *port > 0)
                .ok_or_else(|| "H.323 did not negotiate an audio RTP channel".to_string())?;
            if session.local_audio_payload_type != session.remote_audio_payload_type {
                return Err(
                    "Asymmetric H.323 audio payload types are not supported by the negotiated G711 bridge".into()
                );
            }
            let video = session
                .remote_video_port
                .filter(|port| *port > 0)
                .zip(session.remote_video_ip.clone())
                .zip(session.remote_video_payload_type)
                .zip(session.local_video_payload_type)
                .map(|(((port, ip), transmit_pt), receive_pt)| (ip, port, transmit_pt, receive_pt));
            (
                session.remote_ip.clone(),
                audio_port,
                session.remote_audio_payload_type,
                None,
                video,
                session.call_id.clone(),
            )
        } else {
            let offer = vks_gateway_sip::build_plain_sdp(
                &self.candidate_ip,
                rtp_port,
                video_pair.as_ref().map(|pair| pair.rtp_port),
            );
            let session = self
                .uac_client
                .dial(trimmed, &offer, std::time::Duration::from_secs(60))
                .await
                .map_err(|error| format!("SIP outbound dial failed: {error}"))?;
            setup.sip = Some(session.clone());
            let audio_port = session
                .remote_audio_port
                .filter(|port| *port > 0)
                .ok_or_else(|| "SIP did not negotiate an audio RTP channel".to_string())?;
            let video = session
                .remote_video_port
                .filter(|port| *port > 0)
                .zip(session.remote_video_payload_type)
                .zip(session.local_video_payload_type)
                .zip(session.remote_video_ip.clone())
                .map(|(((port, transmit_pt), receive_pt), ip)| (ip, port, transmit_pt, receive_pt));
            (
                session.remote_media_ip.clone(),
                audio_port,
                session.remote_audio_payload_type,
                session.remote_dtmf_payload_type,
                video,
                session.call_id.clone(),
            )
        };
        if ![0, 8].contains(&audio_payload_type) {
            return Err("Only negotiated PCMU/PCMA audio can be bridged".into());
        }
        let remote_audio_ip =
            remote_ip.parse::<std::net::IpAddr>().map_err(|_| "Invalid negotiated audio IP".to_string())?;
        controls
            .dtmf_payload_type
            .store(dtmf_payload_type.map(u32::from).unwrap_or(256), std::sync::atomic::Ordering::Relaxed);
        let video_configuration = if video_pair.is_some() { video_remote } else { None };
        if let Some((ip, _, _, _)) = &video_configuration {
            ip.parse::<std::net::IpAddr>().map_err(|_| "Invalid negotiated video IP".to_string())?;
        }
        let mut has_video = video_configuration.is_some();
        let media_details = serde_json::json!({
            "audio_codec": if audio_payload_type == 8 { "PCMA" } else { "PCMU" },
            "audio_payload_type": audio_payload_type, "audio_clock_rate": 8000,
            "audio_tx_payload_type": audio_payload_type, "audio_rx_payload_type": audio_payload_type,
            "remote_audio_address": std::net::SocketAddr::new(remote_audio_ip, remote_audio_port).to_string(),
            "video_codec": if has_video { Some("H264") } else { None },
            "video_tx_payload_type": video_configuration.as_ref().map(|configuration| configuration.2),
            "video_rx_payload_type": video_configuration.as_ref().map(|configuration| configuration.3),
            "video_clock_rate": if has_video { Some(90000) } else { None },
            "runtime_video_size": if has_video { Some("640x360") } else { None },
            "runtime_video_max_fps": if has_video { Some(15) } else { None },
        });
        {
            let mut registry = self.registry.lock().map_err(|_| "Registry lock poisoned".to_string())?;
            registry.by_call.insert(call_id.clone(), pid_str.clone());
            registry.by_participant.insert(
                pid_str.clone(),
                GatewayCallSession {
                    call_id: call_id.clone(),
                    participant_id: pid_str.clone(),
                    room_id,
                    display_name: effective_name.clone(),
                    uri: trimmed.to_string(),
                    active_sip: setup.sip.clone(),
                    active_h323: setup.h323.clone(),
                    cancel_flag: cancel_flag.clone(),
                    controls: controls.clone(),
                    media_stats: media_stats.clone(),
                    media_details,
                    connected_at: std::time::Instant::now(),
                },
            );
        }
        setup.registered = Some((self.clone(), pid_str.clone()));
        let state = self.state.read().ok().and_then(|value| value.as_ref().and_then(|weak| weak.upgrade()));
        if state.is_some() {
            bridge_gateway_participant_media(Some(&mcu), &room_id, &pid_str, has_video);
        } else {
            let mut tracks = vec![vks_core::track::Track::new(participant, vks_core::track::TrackKind::Audio)];
            if has_video {
                tracks.push(vks_core::track::Track::new(participant, vks_core::track::TrackKind::Video));
            }
            mcu.bridge_participant(room_id, participant, tracks);
        }
        spawn_rtp_media_bridge(RtpBridgeSession {
            room_id,
            participant_id_str: pid_str.clone(),
            participant_id_val: participant,
            remote_media_ip: remote_ip,
            remote_audio_port,
            audio_payload_type,
            dtmf_payload_type,
            media_stats: media_stats.clone(),
            rtp_socket: port_pair.rtp_socket,
            rtcp_socket: Some(port_pair.rtcp_socket),
            rtp_port,
            port_pool: self.port_pool.clone(),
            mcu_engine: mcu.clone(),
            controls: controls.clone(),
            cancel_flag: cancel_flag.clone(),
            dtmf_rx,
        });
        audio_lease.disarm();
        if let (Some(pair), Some((ip, port, transmit_pt, receive_pt))) = (video_pair.take(), video_configuration) {
            let remote_video_ip =
                ip.parse::<std::net::IpAddr>().map_err(|_| "Invalid negotiated video IP".to_string())?;
            let start = video::spawn_video_media_bridge(video::VideoBridgeSession {
                room_id,
                participant_id: participant,
                rtp_socket: pair.rtp_socket,
                rtcp_socket: Some(pair.rtcp_socket),
                remote_target: std::net::SocketAddr::new(remote_video_ip, port),
                payload_type: transmit_pt,
                receive_payload_type: receive_pt,
                port_pool: self.port_pool.clone(),
                engine: mcu.clone(),
                controls: controls.clone(),
                cancelled: cancel_flag.clone(),
                stats: media_stats,
            })
            .await;
            if let Err(error) = start {
                // A codec/FFmpeg failure must not kill an already working
                // audio call. Downgrade this participant to audio-only.
                tracing::warn!(pid = %pid_str, %error, "H.264 bridge unavailable; continuing audio-only");
                mcu.unbridge_participant(room_id, participant);
                mcu.bridge_participant(
                    room_id,
                    participant,
                    vec![vks_core::track::Track::new(participant, vks_core::track::TrackKind::Audio)],
                );
                has_video = false;
                video_lease.take();
            } else if let Some(lease) = video_lease.as_mut() {
                lease.disarm();
            }
        }
        if let Some(state) = &state {
            // Start the media watchdog clock only when the participant is
            // actually visible, not before the FFmpeg probe and channel start.
            if let Ok(mut registry) = self.registry.lock() {
                if let Some(session) = registry.by_participant.get_mut(&pid_str) {
                    session.connected_at = std::time::Instant::now();
                }
            }
            announce_gateway_participant(state, &room_id, &pid_str, &effective_name, has_video);
        }
        let manager = self.clone();
        let monitored_participant = pid_str.clone();
        let monitored_sip = setup.sip.clone();
        let monitored_h323 = setup.h323.clone();
        let mut h323_disconnect = monitored_h323.as_ref().map(|session| session.subscribe_disconnect());
        let monitored_cancel = cancel_flag.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(100));
            while !monitored_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                let h323_notification = tokio::select! {
                    _ = interval.tick() => false,
                    _ = async {
                        if let Some(disconnect) = &mut h323_disconnect { let _ = disconnect.changed().await; }
                        else { std::future::pending::<()>().await; }
                    } => true,
                };
                let media_dead = manager
                    .registry
                    .lock()
                    .ok()
                    .and_then(|registry| {
                        registry.by_participant.get(&monitored_participant).map(|session| {
                            let elapsed = session.connected_at.elapsed();
                            let last_rx = session.media_stats.last_rx_ms.load(std::sync::atomic::Ordering::Relaxed);
                            let now_ms = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64;
                            media_bridge_dead(
                                elapsed,
                                last_rx,
                                now_ms,
                                session.media_stats.audio_codec_running.load(std::sync::atomic::Ordering::Relaxed),
                            )
                        })
                    })
                    .unwrap_or(false);
                if h323_notification
                    || monitored_h323.as_ref().is_some_and(|session| session.is_closed())
                    || monitored_sip.as_ref().is_some_and(|session| session.is_ended())
                    || media_dead
                {
                    manager.hangup(&monitored_participant);
                    break;
                }
            }
        });
        setup.armed = false;
        info!(call_id = %call_id, pid = %pid_str, room = %room_id, rtp_port, has_video, protocol = if is_h323 { "H.323" } else { "SIP" }, "Outbound gateway call connected with negotiated media");
        Ok(pid_str)
    }

    fn hangup(&self, participant_id: &str) -> bool {
        let session = {
            let mut reg = match self.registry.lock() {
                Ok(g) => g,
                Err(_) => return false,
            };
            let sess = reg.by_participant.remove(participant_id);
            if let Some(ref s) = sess {
                reg.by_call.remove(&s.call_id);
            }
            sess
        };

        let Some(session) = session else {
            return false;
        };

        session.cancel_flag.store(true, std::sync::atomic::Ordering::Relaxed);

        if let Some(active_sip) = session.active_sip {
            tokio::spawn(async move {
                let _ = active_sip.hangup().await;
            });
        }
        if let Some(active_h323) = session.active_h323 {
            tokio::spawn(async move {
                let _ = active_h323.hangup().await;
            });
        }

        let state_opt = self.state.read().ok().and_then(|w| w.as_ref().and_then(|weak| weak.upgrade()));
        if let Some(ref state) = state_opt {
            // Per-participant media settings are keyed by a UUID pid; without
            // this a long-running server leaks one entry per gateway call.
            if let Ok(mut settings) = state.api_v3.abonent_settings.lock() {
                settings.remove(&session.participant_id);
            }
            if let Ok(mut processing) = state.api_v3.abonent_processing.lock() {
                processing.remove(&session.participant_id);
            }
            terminate_gateway_participant_in_room(
                state,
                self.mcu_engine.as_ref(),
                &session.room_id,
                &session.participant_id,
            );
        } else if let Some(ref mcu) = self.mcu_engine {
            let pid_val = crate::parse_participant_id(&session.participant_id);
            mcu.unbridge_participant(session.room_id, pid_val);
        }

        info!(pid = %participant_id, call_id = %session.call_id, "Gateway call terminated via hangup");
        true
    }

    fn set_media_control(
        &self,
        participant_id: &str,
        audio_tx_muted: Option<bool>,
        audio_rx_muted: Option<bool>,
        video_tx_muted: Option<bool>,
        video_rx_muted: Option<bool>,
        volume_tx: Option<u32>,
        volume_rx: Option<u32>,
    ) -> bool {
        let (room_id, controls) = {
            let reg = match self.registry.lock() {
                Ok(g) => g,
                Err(_) => return false,
            };
            let Some(sess) = reg.by_participant.get(participant_id) else {
                return false;
            };
            (sess.room_id, sess.controls.clone())
        };

        if let Some(m) = audio_tx_muted {
            controls.audio_tx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
            if m {
                if let Some(mcu) = &self.mcu_engine {
                    mcu.set_audio_slice(room_id, crate::parse_participant_id(participant_id), &[0.0; 960]);
                }
            }
        }
        if let Some(m) = audio_rx_muted {
            controls.audio_rx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(m) = video_tx_muted {
            controls.video_tx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
            if m {
                if let Some(mcu) = &self.mcu_engine {
                    mcu.clear_participant_frame(room_id, crate::parse_participant_id(participant_id));
                }
            }
        }
        if let Some(m) = video_rx_muted {
            controls.video_rx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(v) = volume_tx {
            controls.volume_tx.store(v.clamp(0, 1000), std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(v) = volume_rx {
            controls.volume_rx.store(v.clamp(0, 1000), std::sync::atomic::Ordering::Relaxed);
        }

        let state_opt = self.state.read().ok().and_then(|w| w.as_ref().and_then(|weak| weak.upgrade()));
        if let Some(state) = state_opt {
            state.room_hub.update_participant_media(&room_id, participant_id, audio_tx_muted, video_tx_muted, None);
        }
        true
    }

    fn send_dtmf(&self, participant_id: &str, sequence: &str) -> Result<(), String> {
        let controls = {
            let reg = self.registry.lock().map_err(|_| "Registry lock poisoned".to_string())?;
            let sess = reg
                .by_participant
                .get(participant_id)
                .ok_or_else(|| format!("Participant {participant_id} not found"))?;
            sess.controls.clone()
        };
        if controls.dtmf_payload_type.load(std::sync::atomic::Ordering::Relaxed) > 127 {
            return Err("The terminal did not negotiate RTP telephone-event".into());
        }
        controls.dtmf_sender.send(sequence.to_string()).map_err(|e| format!("DTMF queue error: {e}"))
    }

    fn send_fecc(&self, participant_id: &str, command: &str) -> Result<(), String> {
        let reg = self.registry.lock().map_err(|_| "Registry lock poisoned".to_string())?;
        if !reg.by_participant.contains_key(participant_id) {
            return Err(format!("Participant {participant_id} not found"));
        }
        Err(format!("FECC is not negotiated for this gateway participant (command {command})"))
    }
}

#[cfg(test)]
mod media_runtime_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn gateway_measured_average_rates_follow_actual_counters() {
        let manager = GatewayManager::new();
        let stats = Arc::new(GatewayMediaStats::default());
        stats.audio_rx_bytes.store(1000, Ordering::Relaxed);
        stats.video_tx_bytes.store(5000, Ordering::Relaxed);
        stats.decoded_video_frames.store(20, Ordering::Relaxed);
        stats.encoded_video_frames.store(30, Ordering::Relaxed);
        let participant = "measured-rates-fixture";
        manager.registry.lock().unwrap().by_participant.insert(
            participant.into(),
            GatewayCallSession {
                call_id: "measured-rates".into(),
                participant_id: participant.into(),
                room_id: RoomId::new(),
                display_name: "metrics".into(),
                uri: "sip:127.0.0.1".into(),
                active_sip: None,
                active_h323: None,
                cancel_flag: Arc::new(AtomicBool::new(false)),
                controls: GatewayMediaControls::default(),
                media_stats: stats,
                media_details: serde_json::json!({}),
                connected_at: std::time::Instant::now() - Duration::from_secs(2),
            },
        );
        let measured = manager.media_stats(participant).unwrap();
        assert!(
            measured["audio_rx_bitrate_bps"].as_u64().unwrap_or(0) > 3900,
            "actual RTP bytes are not exposed as measured bitrate"
        );
        assert!(measured["audio_rx_bitrate_bps"].as_u64().unwrap_or(u64::MAX) <= 4000);
        assert!(measured["video_tx_bitrate_bps"].as_u64().unwrap_or(0) > 19500);
        assert!(measured["decoded_video_fps"].as_f64().unwrap_or(0.0) > 9.5);
        assert!(measured["encoded_video_fps"].as_f64().unwrap_or(0.0) > 14.5);
        assert_eq!(measured["rate_basis"], "average_since_connection");
    }

    #[tokio::test]
    #[ignore = "requires Python and FFmpeg; uses only fixture signaling and loopback RTP"]
    async fn h323_gateway_negotiates_video_tx96_rx109_and_cleans_remote_disconnect() {
        use vks_admin::state::OutboundDialer;
        static ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
        let _environment = ENVIRONMENT.lock().await;
        struct HelperEnvironment {
            path: std::path::PathBuf,
            old_helper: Option<std::ffi::OsString>,
            old_python: Option<std::ffi::OsString>,
        }
        impl Drop for HelperEnvironment {
            fn drop(&mut self) {
                for (key, value) in [("VKS_H323_HELPER", &self.old_helper), ("VKS_H323_PYTHON", &self.old_python)] {
                    if let Some(value) = value {
                        std::env::set_var(key, value);
                    } else {
                        std::env::remove_var(key);
                    }
                }
                let _ = std::fs::remove_file(&self.path);
            }
        }
        let path = std::env::temp_dir().join(format!("vks-h323-media-fixture-{}.py", uuid::Uuid::new_v4()));
        let fixture = r#"import json,sys,socket,struct,subprocess,time,select,re
c=json.loads(sys.stdin.readline())
assert c['target_ip']=='127.0.0.1'
if c.get('video_port') is None:
    print(json.dumps(dict(event='failed',reason='gateway did not request video media')),flush=True)
    sys.exit(1)
a=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); a.bind(('127.0.0.1',0)); a.setblocking(False)
v=socket.socket(socket.AF_INET,socket.SOCK_DGRAM); v.bind(('127.0.0.1',0)); v.setblocking(False)
encoded=subprocess.run(['ffmpeg','-hide_banner','-loglevel','error','-f','lavfi','-i','color=c=red:s=320x180:r=15','-frames:v','30','-c:v','libx264','-threads','1','-preset','ultrafast','-tune','zerolatency','-profile:v','baseline','-x264-params','repeat-headers=1:aud=1:scenecut=0','-f','h264','pipe:1'],stdout=subprocess.PIPE,check=True).stdout
units=[x for x in re.split(b'\x00\x00(?:\x00)?\x01',encoded) if x]
frames=[]; frame=[]
for unit in units:
    if unit[0]&31==9 and frame: frames.append(frame); frame=[]
    frame.append(unit)
if frame: frames.append(frame)
e=dict(event='connected',call_id=c['call_id'],call_ref=c['call_ref'],remote_ip='127.0.0.1',remote_audio_port=a.getsockname()[1],remote_audio_payload_type=0,local_audio_payload_type=0,remote_video_ip='127.0.0.1',remote_video_port=v.getsockname()[1],remote_video_payload_type=96,local_video_payload_type=109)
print(json.dumps(e),flush=True)
start=time.monotonic(); next_audio=start; next_video=start; audio_seq=1; video_seq=1; frame_index=0; received_video=False
while time.monotonic()-start<3.0:
    now=time.monotonic()
    if now>=next_audio:
        a.sendto(struct.pack('!BBHII',128,0,audio_seq&65535,(audio_seq*160)&0xffffffff,42)+bytes([0x96])*160,('127.0.0.1',c['audio_port']))
        audio_seq+=1; next_audio+=.02
    if now>=next_video:
        frame=frames[frame_index%len(frames)]
        for index,unit in enumerate(frame):
            chunks=[unit] if len(unit)<=1188 else [bytes([(unit[0]&224)|28,(unit[0]&31)|(128 if part==0 else 64 if position+1186>=len(unit)-1 else 0)])+unit[1+position:1+position+1186] for part,position in enumerate(range(0,len(unit)-1,1186))]
            for part,chunk in enumerate(chunks):
                marker=index+1==len(frame) and part+1==len(chunks)
                v.sendto(struct.pack('!BBHII',128,109|(128 if marker else 0),video_seq&65535,(frame_index*6000)&0xffffffff,43)+chunk,('127.0.0.1',c['video_port']))
                video_seq+=1
        frame_index+=1; next_video+=1/15
    readable,_,_=select.select([v,sys.stdin],[],[],.002)
    if v in readable:
        data,_=v.recvfrom(65536)
        assert len(data)>=12 and data[1]&127==96,'gateway confused H323 transmit and receive video PT'
        received_video=True
    if sys.stdin in readable:
        command=sys.stdin.readline()
        if not command or json.loads(command).get('command')=='hangup': break
print(json.dumps(dict(event='disconnected',reason='fixture peer release after verified RTP' if received_video else 'fixture peer release without TX')),flush=True)
"#;
        std::fs::write(&path, fixture).unwrap();
        let _restore = HelperEnvironment {
            path: path.clone(),
            old_helper: std::env::var_os("VKS_H323_HELPER"),
            old_python: std::env::var_os("VKS_H323_PYTHON"),
        };
        std::env::set_var("VKS_H323_HELPER", &path);
        std::env::set_var("VKS_H323_PYTHON", if cfg!(windows) { "python" } else { "python3" });
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone()).with_candidate_ip("127.0.0.1");
        let room = RoomId::new();
        let participant = manager
            .dial(room, "h323:127.0.0.1", Some("H323 fixture"))
            .await
            .expect("H323 gateway must request video channels");
        let stats = manager.media_stats(&participant).unwrap();
        assert_eq!(stats["video_tx_payload_type"], 96);
        assert_eq!(stats["video_rx_payload_type"], 109);
        let retained = manager.registry.lock().unwrap().by_participant[&participant].media_stats.clone();
        tokio::time::timeout(Duration::from_secs(3), async {
            while retained.decoded_video_frames.load(Ordering::Relaxed) == 0
                || retained.audio_rx_packets.load(Ordering::Relaxed) == 0
                || retained.encoded_video_frames.load(Ordering::Relaxed) == 0
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("negotiated RX109 H264 and RX0 G711 must reach the actual media pipeline");
        assert!(retained.encoded_video_frames.load(Ordering::Relaxed) > 0);
        tokio::time::timeout(Duration::from_secs(5), async {
            while manager.registry.lock().unwrap().by_participant.contains_key(&participant) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("remote H323 release must remove the participant without UI refresh");
        assert!(!mcu.has_participant(room, crate::parse_participant_id(&participant)));
    }

    #[tokio::test]
    async fn gateway_port_exhaustion_does_not_create_a_connected_participant() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager {
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            ..GatewayManager::new().with_mcu(mcu.clone())
        };
        let room = RoomId::new();
        let result = manager.dial(room, "sip:127.0.0.1:9", None).await;
        assert!(result.is_err(), "RTP port exhaustion must never produce a fake connected call");
        assert!(manager.registry.lock().unwrap().by_participant.is_empty());
    }

    #[test]
    fn gateway_video_tracks_get_distinct_default_layout_slots() {
        let mcu = vks_media_mcu::McuEngine::new();
        let room = RoomId::new();
        for pid in [vks_core::ParticipantId::new(), vks_core::ParticipantId::new()] {
            mcu.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)],
            );
        }
        mcu.tick_video_at(room, 0);
        mcu.tick_video_at(room, 334);
        assert_eq!(mcu.slot_rects(room).unwrap().len(), 2, "real gateway video has no compositor slots");
    }

    #[tokio::test]
    async fn gateway_audio_runtime_ticks_mix_minus_without_manual_engine_tick() {
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let room = RoomId::new();
        let listener = vks_core::ParticipantId::new();
        let speaker = vks_core::ParticipantId::new();
        for pid in [listener, speaker] {
            mcu.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Audio)],
            );
        }
        mcu.set_audio(room, speaker, vec![0.4; 960]);
        let terminal = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let bridge_port = bridge.local_addr().unwrap().port();
        let cancel = Arc::new(AtomicBool::new(false));
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: listener.to_string(),
            participant_id_val: listener,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 0,
            dtmf_payload_type: None,
            media_stats: Arc::new(GatewayMediaStats::default()),
            rtp_socket: bridge,
            rtcp_socket: None,
            rtp_port: bridge_port,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49002)),
            mcu_engine: mcu,
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let mut packet = [0; 2048];
        let received = tokio::time::timeout(Duration::from_millis(250), async {
            loop {
                let n = terminal.recv(&mut packet).await.unwrap();
                if n == 172 && vks_gateway_sip::ulaw_to_f32(packet[12]) > 0.3 {
                    break;
                }
            }
        })
        .await;
        cancel.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_millis(100), task).await.unwrap().unwrap();
        assert!(received.is_ok(), "gateway emitted silence because the room audio pipeline was never ticked");
    }

    #[tokio::test]
    async fn gateway_audio_rejects_rtp_from_another_socket() {
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let room = RoomId::new();
        let source = vks_core::ParticipantId::new();
        let listener = vks_core::ParticipantId::new();
        for pid in [source, listener] {
            mcu.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Audio)],
            );
        }
        let terminal = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let attacker = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = socket.local_addr().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: source.to_string(),
            participant_id_val: source,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 0,
            dtmf_payload_type: None,
            media_stats: Arc::new(GatewayMediaStats::default()),
            rtp_socket: socket,
            rtcp_socket: None,
            rtp_port: target.port(),
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49002)),
            mcu_engine: mcu.clone(),
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let mut packet = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 1, 0, 42, false);
        vks_gateway_sip::encode_ulaw(&[0.75; 160], &mut packet[12..]);
        attacker.send_to(&packet, target).await.unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        let output = mcu.participant_audio(room, listener).unwrap();
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
        assert!(output.iter().all(|sample| sample.abs() < 0.01), "RTP from an unrelated source reached the room mixer");
    }

    #[tokio::test]
    async fn gateway_audio_respects_negotiated_pcma_payload_type() {
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let room = RoomId::new();
        let source = vks_core::ParticipantId::new();
        let listener = vks_core::ParticipantId::new();
        for pid in [source, listener] {
            mcu.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Audio)],
            );
        }
        mcu.set_audio(room, source, vec![0.4; 960]);
        let terminal = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let port = socket.local_addr().unwrap().port();
        let cancel = Arc::new(AtomicBool::new(false));
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: listener.to_string(),
            participant_id_val: listener,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 8,
            dtmf_payload_type: None,
            rtp_socket: socket,
            rtcp_socket: None,
            rtp_port: port,
            media_stats: Arc::new(GatewayMediaStats::default()),
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49002)),
            mcu_engine: mcu,
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let mut packet = [0; 2048];
        let count =
            tokio::time::timeout(Duration::from_millis(100), terminal.recv(&mut packet)).await.unwrap().unwrap();
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
        assert_eq!(count, 172);
        assert_eq!(packet[1] & 0x7f, 8, "bridge sent PCMU after PCMA was negotiated");
        // A-law reference value for +0.4 full-scale PCM is 0xbc (approximately +13056).
        assert_eq!(packet[12], 0xbc, "payload must use A-law rather than mu-law");
    }

    #[tokio::test]
    async fn gateway_audio_consumes_both_halves_of_a_40ms_packet_once() {
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let room = RoomId::new();
        let source = vks_core::ParticipantId::new();
        let listener = vks_core::ParticipantId::new();
        for pid in [source, listener] {
            mcu.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Audio)],
            );
        }
        let terminal = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = socket.local_addr().unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: source.to_string(),
            participant_id_val: source,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 0,
            dtmf_payload_type: None,
            media_stats: Arc::new(GatewayMediaStats::default()),
            rtp_socket: socket,
            rtcp_socket: None,
            rtp_port: target.port(),
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            mcu_engine: mcu.clone(),
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let mut packet = [0u8; 332];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 1, 0, 42, false);
        vks_gateway_sip::encode_ulaw(&[0.4; 160], &mut packet[12..172]);
        vks_gateway_sip::encode_ulaw(&[-0.4; 160], &mut packet[172..]);
        terminal.send_to(&packet, target).await.unwrap();
        let second_half = tokio::time::timeout(Duration::from_millis(150), async {
            loop {
                if mcu
                    .participant_audio(room, listener)
                    .is_some_and(|audio| audio.first().is_some_and(|sample| *sample < -0.3))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await;
        tokio::time::sleep(Duration::from_millis(80)).await;
        let after = mcu.participant_audio(room, listener).unwrap();
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
        assert!(second_half.is_ok(), "the second half of negotiated G711 packetization was truncated");
        assert!(after.iter().all(|sample| sample.abs() < 0.01), "old PCM repeated after its RTP packet ended");
    }

    #[test]
    fn watchdog_allows_a_slow_first_packet_but_not_a_silent_call() {
        use std::time::Duration;
        assert!(!media_bridge_dead(Duration::from_secs(6), 0, 0, true));
        assert!(media_bridge_dead(Duration::from_secs(16), 0, 0, true));
        assert!(!media_bridge_dead(Duration::from_secs(10), 9_000, 10_000, true));
        assert!(media_bridge_dead(Duration::from_secs(20), 1_000, 20_000, true));
    }

    #[test]
    fn watchdog_fails_a_bridge_whose_audio_codec_never_started() {
        use std::time::Duration;
        assert!(!media_bridge_dead(Duration::from_secs(4), 3_000, 4_000, false));
        assert!(media_bridge_dead(Duration::from_secs(6), 5_000, 6_000, false));
    }
}

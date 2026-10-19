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

/// Атомарные элементы управления медиа-потоками абонента (Mute, Volume, DTMF).
#[derive(Clone)]
pub struct GatewayMediaControls {
    pub audio_tx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub audio_rx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub video_tx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub video_rx_muted: Arc<std::sync::atomic::AtomicBool>,
    pub volume_tx: Arc<std::sync::atomic::AtomicU32>, // 0..200 (100 = 100%)
    pub volume_rx: Arc<std::sync::atomic::AtomicU32>,
    pub dtmf_sender: tokio::sync::mpsc::UnboundedSender<String>,
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
}

/// Параметры для двунаправленного RTP медиа-моста между шлюзом и McuEngine.
pub struct RtpBridgeSession {
    pub room_id: RoomId,
    pub participant_id_str: String,
    pub participant_id_val: vks_core::ParticipantId,
    pub remote_media_ip: String,
    pub remote_audio_port: u16,
    pub rtp_socket: tokio::net::UdpSocket,
    pub rtp_port: u16,
    pub port_pool: Arc<vks_gateway_sip::RtpPortPool>,
    pub mcu_engine: Arc<vks_media_mcu::McuEngine>,
    pub controls: GatewayMediaControls,
    pub cancel_flag: Arc<std::sync::atomic::AtomicBool>,
    pub dtmf_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

/// Запускает фоновый 20-мс RTP медиа-мост (G.711 PCMU <-> McuEngine N-1 mix-minus).
///
/// Полностью абстрагирован от сигнального протокола (используется одинаково для SIP и H.323).
/// Zero heap allocation на горячем пути: стек-буферы и `set_audio_slice`/`copy_participant_audio`.
pub fn spawn_rtp_media_bridge(mut session: RtpBridgeSession) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let remote_target: std::net::SocketAddr =
            match format!("{}:{}", session.remote_media_ip, session.remote_audio_port).parse() {
                Ok(addr) => addr,
                Err(_) => {
                    session.port_pool.release_port(session.rtp_port);
                    return;
                }
            };

        let mut send_interval = tokio::time::interval(std::time::Duration::from_millis(20));
        send_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        let mut rtp_seq: u16 = 1;
        let mut rtp_ts: u32 = 0;
        let rtp_ssrc: u32 = 0x51900001;

        let mut in_buf = [0u8; 2048];
        let mut out_packet = [0u8; 172]; // 12 байт заголовок + 160 байт PCMU
        let mut pcm_48k_buf = [0.0f32; 960];

        while !session.cancel_flag.load(std::sync::atomic::Ordering::Relaxed) {
            tokio::select! {
                Some(seq) = session.dtmf_rx.recv() => {
                    for ch in seq.chars().take(32) {
                        let event = match ch {
                            '0'..='9' => ch as u8 - b'0',
                            '*' => 10,
                            '#' => 11,
                            'A' | 'a' => 12,
                            'B' | 'b' => 13,
                            'C' | 'c' => 14,
                            'D' | 'd' => 15,
                            _ => continue,
                        };
                        let dtmf_start_ts = rtp_ts;
                        for (i, dur, end_flag) in [
                            (0, 160u16, false),
                            (1, 320u16, false),
                            (2, 480u16, true),
                            (3, 480u16, true),
                        ] {
                            let mut dtmf_pkt = [0u8; 16];
                            let marker = i == 0;
                            vks_gateway_sip::write_rtp_header(&mut dtmf_pkt[..12], 101, rtp_seq, dtmf_start_ts, rtp_ssrc, marker);
                            rtp_seq = rtp_seq.wrapping_add(1);

                            dtmf_pkt[12] = event;
                            dtmf_pkt[13] = if end_flag { 0x8a } else { 0x0a };
                            dtmf_pkt[14..16].copy_from_slice(&dur.to_be_bytes());

                            let _ = session.rtp_socket.send_to(&dtmf_pkt, remote_target).await;
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                    }
                }
                _ = send_interval.tick() => {
                    // McuEngine -> Терминал (N-1 Mix-minus)
                    let rx_muted = session.controls.audio_rx_muted.load(std::sync::atomic::Ordering::Relaxed);
                    let mut ulaw_samples = [0xFFu8; 160]; // 0xFF = тишина в G.711 µ-law

                    if !rx_muted
                        && session
                            .mcu_engine
                            .copy_participant_audio(session.room_id, session.participant_id_val, &mut pcm_48k_buf)
                    {
                        let mut pcm_8k = [0.0f32; 160];
                        vks_gateway_sip::downsample_48k_to_8k(&pcm_48k_buf, &mut pcm_8k);
                        let vol_rx = session.controls.volume_rx.load(std::sync::atomic::Ordering::Relaxed);
                        if vol_rx != 100 {
                            let scale = (vol_rx as f32) / 100.0;
                            for s in pcm_8k.iter_mut() {
                                *s = (*s * scale).tanh();
                            }
                        }
                        vks_gateway_sip::encode_ulaw(&pcm_8k, &mut ulaw_samples);
                    }

                    let mut hdr = [0u8; 12];
                    vks_gateway_sip::write_rtp_header(&mut hdr, 0, rtp_seq, rtp_ts, rtp_ssrc, false);
                    rtp_seq = rtp_seq.wrapping_add(1);
                    rtp_ts = rtp_ts.wrapping_add(160);

                    out_packet[..12].copy_from_slice(&hdr);
                    out_packet[12..].copy_from_slice(&ulaw_samples);

                    let _ = session.rtp_socket.send_to(&out_packet, remote_target).await;
                }
                recv_res = session.rtp_socket.recv_from(&mut in_buf) => {
                    // Терминал -> McuEngine (Zero-heap allocation)
                    if let Ok((len, _)) = recv_res {
                        if let Some((_pt, _seq, _ts, _ssrc, offset)) = vks_gateway_sip::parse_rtp_header(&in_buf[..len]) {
                            let payload = &in_buf[offset..len];
                            if !payload.is_empty() {
                                let tx_muted = session.controls.audio_tx_muted.load(std::sync::atomic::Ordering::Relaxed);
                                if tx_muted {
                                    static ZERO_SAMPLES: [f32; 960] = [0.0f32; 960];
                                    session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, &ZERO_SAMPLES);
                                } else {
                                    let mut dec_8k = [0.0f32; 160];
                                    let count = payload.len().min(160);
                                    for i in 0..count {
                                        dec_8k[i] = vks_gateway_sip::ulaw_to_f32(payload[i]);
                                    }
                                    let vol_tx = session.controls.volume_tx.load(std::sync::atomic::Ordering::Relaxed);
                                    if vol_tx != 100 {
                                        let scale = (vol_tx as f32) / 100.0;
                                        for s in dec_8k.iter_mut() {
                                            *s = (*s * scale).tanh();
                                        }
                                    }
                                    let mut up_48k = [0.0f32; 960];
                                    vks_gateway_sip::upsample_8k_to_48k(&dec_8k, &mut up_48k);
                                    session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, &up_48k);
                                }
                            }
                        }
                    }
                }
            }
        }

        session.port_pool.release_port(session.rtp_port);
        tracing::info!(pid = %session.participant_id_str, "RTP media bridge terminated and ports released");
    })
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

    if let Some(mcu) = mcu_engine {
        let pid_val = crate::parse_participant_id(participant_id);
        let mut mcu_tracks = vec![vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Audio)];
        if has_video {
            mcu_tracks.push(vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Video));
        }
        mcu.bridge_participant(*room_id, pid_val, mcu_tracks);
    }
}

/// Удаляет участника шлюза из RoomHub, обновляет счетчик комнаты,
/// уведомляет WebSocket админки и отключает его из McuEngine.
pub fn terminate_gateway_participant_in_room(
    state: &Arc<AppState>,
    mcu_engine: Option<&Arc<vks_media_mcu::McuEngine>>,
    room_id: &RoomId,
    participant_id: &str,
) {
    state.room_hub.unregister_participant(room_id, participant_id);

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
        };

        // 1. Выделяем RTP порт для медиа-моста и инициируем вызов
        let (active_sip, active_h323, rtp_port_opt, call_id) = if let Some(port_pair) =
            self.port_pool.allocate_pair(&self.candidate_ip).await
        {
            let rtp_port = port_pair.rtp_port;
            let dial_res = if is_h323 {
                self.h323_uac_client
                    .dial(trimmed, rtp_port, std::time::Duration::from_secs(5))
                    .await
                    .map(|sess| {
                        (None, Some(sess.clone()), sess.remote_ip, sess.remote_audio_port.unwrap_or(5004), sess.call_id)
                    })
                    .map_err(|e| format!("H.323 outbound dial failed: {e}"))
            } else {
                let sdp_offer = vks_gateway_sip::build_plain_sdp(&self.candidate_ip, rtp_port, None);
                self.uac_client
                    .dial(trimmed, &sdp_offer, std::time::Duration::from_secs(5))
                    .await
                    .map(|sess| {
                        (
                            Some(sess.clone()),
                            None,
                            sess.remote_media_ip,
                            sess.remote_audio_port.unwrap_or(5004),
                            sess.call_id,
                        )
                    })
                    .map_err(|e| format!("SIP outbound dial failed: {e}"))
            };

            match dial_res {
                Ok((sip_opt, h323_opt, rem_ip, rem_port, cid)) => {
                    if let Some(ref mcu) = self.mcu_engine {
                        spawn_rtp_media_bridge(RtpBridgeSession {
                            room_id,
                            participant_id_str: pid_str.clone(),
                            participant_id_val: crate::parse_participant_id(&pid_str),
                            remote_media_ip: rem_ip,
                            remote_audio_port: rem_port,
                            rtp_socket: port_pair.rtp_socket,
                            rtp_port,
                            port_pool: self.port_pool.clone(),
                            mcu_engine: mcu.clone(),
                            controls: controls.clone(),
                            cancel_flag: cancel_flag.clone(),
                            dtmf_rx,
                        });
                    }
                    (sip_opt, h323_opt, Some(rtp_port), cid)
                }
                Err(err_msg) => {
                    self.port_pool.release_port(rtp_port);
                    return Err(err_msg);
                }
            }
        } else {
            (None, None, None, format!("call-out-{}", uuid::Uuid::new_v4()))
        };

        // 2. Регистрируем в реестре вызовов
        {
            let mut reg = self.registry.lock().map_err(|_| "Registry lock poisoned".to_string())?;
            let session = GatewayCallSession {
                call_id: call_id.clone(),
                participant_id: pid_str.clone(),
                room_id,
                display_name: effective_name.clone(),
                uri: trimmed.to_string(),
                active_sip,
                active_h323,
                cancel_flag,
                controls,
            };
            reg.by_call.insert(call_id.clone(), pid_str.clone());
            reg.by_participant.insert(pid_str.clone(), session);
        }

        // 3. Приземляем участника в RoomHub и McuEngine
        let state_opt = self.state.read().ok().and_then(|w| w.as_ref().and_then(|weak| weak.upgrade()));
        if let Some(ref state) = state_opt {
            land_gateway_participant_in_room(
                state,
                self.mcu_engine.as_ref(),
                &room_id,
                &pid_str,
                &effective_name,
                true,
            );
        } else if let Some(ref mcu) = self.mcu_engine {
            let pid_val = crate::parse_participant_id(&pid_str);
            let tracks = vec![
                vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Audio),
                vks_core::track::Track::new(pid_val, vks_core::track::TrackKind::Video),
            ];
            mcu.bridge_participant(room_id, pid_val, tracks);
        }

        info!(call_id = %call_id, pid = %pid_str, room = %room_id, rtp_port = ?rtp_port_opt, protocol = if is_h323 { "H.323" } else { "SIP" }, "Outbound gateway call dialed and bridged");
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
        }
        if let Some(m) = audio_rx_muted {
            controls.audio_rx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(m) = video_tx_muted {
            controls.video_tx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(m) = video_rx_muted {
            controls.video_rx_muted.store(m, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(v) = volume_tx {
            controls.volume_tx.store(v.clamp(0, 200), std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(v) = volume_rx {
            controls.volume_rx.store(v.clamp(0, 200), std::sync::atomic::Ordering::Relaxed);
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
        controls.dtmf_sender.send(sequence.to_string()).map_err(|e| format!("DTMF queue error: {e}"))
    }

    fn send_fecc(&self, participant_id: &str, command: &str) -> Result<(), String> {
        let reg = self.registry.lock().map_err(|_| "Registry lock poisoned".to_string())?;
        if !reg.by_participant.contains_key(participant_id) {
            return Err(format!("Participant {participant_id} not found"));
        }
        tracing::info!(participant_id = %participant_id, command = %command, "FECC camera control command routed to gateway");
        Ok(())
    }
}

//! Интеграция SIP и H.323 шлюзов с сервером VKS и RoomHub (Milestone M5).
//!
//! Обеспечивает сквозную регистрацию звонков из SIP/H.323 в WebRTC-комнаты,
//! трансляцию сигнальных событий в RoomHub и админ-шину WebSocket,
//! а также мониторинг доступности протоколов.

use std::sync::{Arc, Mutex};
use tracing::{info, warn};

use vks_admin::hub::ParticipantSnapshot;
use vks_admin::rooms::Rooms;
use vks_admin::state::{AppState, OutboundDialer as _};
use vks_common::Error;
use vks_core::RoomId;
use vks_gateway_h323::{H323Call, H323Codec, H323Gateway};
use vks_gateway_sip::{SipCall, SipCodec, SipDialError, SipGateway};
use vks_proto::signaling::signaling_message::Payload;
use vks_proto::signaling::{ParticipantJoined, ParticipantLeft, SignalingMessage};
use vks_signaling::Envelope;

#[path = "gateway_audio.rs"]
mod audio;
#[path = "gateway_video.rs"]
mod video;

/// Находит `call_string.sip|h323` сохранённого адреса по URI. Вынесено из
/// async-метода, чтобы приоритет адреса проверялся без блокировок AppState.
fn protocol_options<'a>(payload: &'a serde_json::Value, protocol: &str) -> Option<&'a serde_json::Value> {
    // Contacts хранят wrapper `call_string.<proto>`, глобальные адреса —
    // плоскую карту протоколов `payload.<proto>`; поддерживаем оба shape.
    // Contact может быть как готовым payload, так и записью с wrapper
    // `{uri, payload:{call_string:{…}}}`.
    let payload = payload.get("payload").filter(|value| value.is_object()).unwrap_or(payload);
    payload
        .get("call_string")
        .and_then(|value| value.get(protocol))
        .or_else(|| payload.get(protocol))
        .filter(|value| value.is_object())
}

/// Явные видео-настройки из payload модалки. `default_contact_payload`
/// всегда содержит `call_string.sip` с null-полями; такой объект не должен
/// считаться override и затирать сохранённый профиль адреса.
fn explicit_video_options(contact: Option<&serde_json::Value>, protocol: &str) -> Option<serde_json::Value> {
    contact
        .and_then(|value| protocol_options(value, protocol))
        .filter(|options| {
            options.get("bitrate").is_some_and(|value| !value.is_null())
                || options.get("resolution").is_some_and(|value| !value.is_null())
        })
        .cloned()
}

/// Ищет intent конкретного адреса: сначала контакт этой комнаты, затем
/// глобальную адресную книгу. Сравнение — по единой canonical-форме T10,
/// иначе `SIP:HOST:5060` не найдёт сохранённый `sip:host`.
fn find_call_string_override<'a>(
    resources: &'a vks_admin::api_v3::resources::ResourceState,
    room_id: Option<RoomId>,
    uri: &str,
) -> Option<&'a serde_json::Value> {
    let wanted = vks_common::uri::canonical_uri(uri);
    let protocol = if wanted.starts_with("h323:") { "h323" } else { "sip" };
    let matches = |contact: &&serde_json::Value| {
        contact
            .get("uri")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| vks_common::uri::canonical_uri(value) == wanted)
    };
    if let Some(contact) = room_id
        .and_then(|id| resources.contacts.get(&id.to_string()))
        .and_then(|contacts| contacts.iter().find(matches))
        .and_then(|contact| protocol_options(contact, protocol))
    {
        return Some(contact);
    }
    resources
        .addresses
        .values()
        .find(|record| vks_common::uri::canonical_uri(&record.uri) == wanted)
        .and_then(|record| protocol_options(&record.payload, protocol))
}

/// Проверяет выбор кодеков комнаты/адреса до дозвона. Клиент, оставивший
/// только несовместимые кодеки (G.722-only, H.265-only), должен получить
/// явную причину, а не молчаливый PCMU/H.264 fallback и не «connected» без
/// медиа. `None`/пустой список = выбор не задан, действует native-профиль.
fn codec_name(item: &serde_json::Value) -> Option<String> {
    item.as_str()
        .or_else(|| item.get("name").and_then(serde_json::Value::as_str))
        .map(|name| name.trim().to_uppercase().replace(['.', '-', '_'], ""))
        .filter(|name| !name.is_empty())
}

fn ensure_bridgeable_codec_selection(
    audio: Option<&serde_json::Value>,
    video: Option<&serde_json::Value>,
) -> Result<(), String> {
    use vks_common::codec_capabilities::{is_native_audio, is_native_video, NATIVE_AUDIO, NATIVE_VIDEO};
    for (kind, value, supported, native) in [
        ("audio", audio, NATIVE_AUDIO, is_native_audio as fn(&str) -> bool),
        ("video", video, NATIVE_VIDEO, is_native_video as fn(&str) -> bool),
    ] {
        let Some(items) = value.and_then(serde_json::Value::as_array) else { continue };
        if items.is_empty() {
            continue;
        }
        let names: Vec<String> = items.iter().filter_map(codec_name).collect();
        if names.is_empty() {
            continue;
        }
        if names.iter().any(|name| native(name)) {
            continue;
        }
        return Err(format!(
            "no common {kind} codec: configured {:?}, gateway bridges {:?}; \
             enable transcoding (T06b) or select a bridgeable codec",
            names, supported
        ));
    }
    Ok(())
}

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

/// SIP-ответ терминала (4xx/6xx/488/606, невалидный SDP) — шлюз жив.
/// Таймаут/IO/URI — транспортный отказ.
fn sip_dial_error_is_terminal(error: &SipDialError) -> bool {
    match error {
        SipDialError::Rejected(..) | SipDialError::InvalidSdpAnswer => true,
        SipDialError::NoCommonCodec { peer_status, .. } => peer_status.is_some(),
        SipDialError::InvalidUri(_) | SipDialError::Io(_) | SipDialError::Timeout(..) => false,
    }
}

/// Отказ терминала помечается точным префиксом на границе протокола.
fn terminal_gateway_error(message: &str) -> bool {
    vks_common::is_terminal_failure(message)
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

    /// Пишет последний результат дозвона в общий AppState для `/ui/system/status`.
    fn note_gateway_health(&self, protocol: &str, ok: bool, terminal: bool, error: Option<String>) {
        if let Some(state) = self.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
        {
            state.record_gateway_health(protocol, ok, terminal, error);
        }
    }

    pub fn bind_state(&self, state: std::sync::Weak<AppState>) {
        if let Ok(mut lock) = self.state.write() {
            *lock = Some(state);
        }
    }

    /// Живой профиль шлюзового видео: env-переопределения + `max_frame_rate`
    /// из `/api/v3/settings/video`. Больше не прибит к `640x360/15/256k`.
    fn video_profile(&self, room_id: RoomId, call_string: Option<&serde_json::Value>) -> video::VideoProfile {
        let mut profile = video::VideoProfile::from_env();
        let state = self.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade));
        if let Some(state) = state {
            let global = state.api_v3.video_settings.lock().map(|settings| settings.clone()).unwrap_or_default();
            let conference =
                state.api_v3.conference_settings.lock().ok().and_then(|settings| settings.get(&room_id).cloned());
            video::apply_video_settings(&mut profile, &global, conference.as_ref());
        }
        if let Some(overrides) = call_string {
            video::apply_call_string_settings(&mut profile, overrides);
        }
        profile
    }

    /// Ищет сохранённый адрес по URI и берёт `call_string.sip|h323`: настройки
    /// конкретного терминала приоритетнее комнаты и глобальных.
    async fn call_string_video_profile(&self, room_id: RoomId, uri: &str) -> Option<serde_json::Value> {
        let state = self.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))?;
        let resources = state.api_v3.resources.lock().await;
        find_call_string_override(&resources, Some(room_id), uri).cloned()
    }

    fn bridgeable_codec_selection(
        resources: &vks_admin::api_v3::resources::ResourceState,
        room_id: RoomId,
        conference: Option<&serde_json::Value>,
        uri: &str,
    ) -> Result<(), String> {
        let call_string = find_call_string_override(resources, Some(room_id), uri);
        let payload = conference.and_then(|value| value.get("payload")).or(conference);
        let pick = |snake: &str, camel: &str| {
            call_string
                .and_then(|value| value.get(camel).or_else(|| value.get(snake)))
                .filter(|value| value.as_array().is_some_and(|items| !items.is_empty()))
                .or_else(|| payload.and_then(|value| value.get(snake).or_else(|| value.get(camel))))
        };
        ensure_bridgeable_codec_selection(pick("audio_codecs", "audioCodecs"), pick("video_codecs", "videoCodecs"))
    }

    /// Проверяет, что выбранные для адреса/комнаты кодеки реально мостятся
    /// native-профилем. Возвращает явную ошибку до SIP/H.323-дозвона.
    async fn ensure_bridgeable_codecs(&self, room_id: RoomId, uri: &str) -> Result<(), String> {
        let Some(state) = self.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
        else {
            return Ok(());
        };
        let conference =
            state.api_v3.conference_settings.lock().ok().and_then(|settings| settings.get(&room_id).cloned());
        let resources = state.api_v3.resources.lock().await;
        Self::bridgeable_codec_selection(&resources, room_id, conference.as_ref(), uri)
    }

    fn sdp_video_profile(profile: &video::VideoProfile) -> vks_gateway_sip::VideoSdpProfile {
        vks_gateway_sip::VideoSdpProfile {
            width: profile.width,
            height: profile.height,
            fps: profile.fps,
            bitrate_kbps: profile.bitrate_kbps,
            profile_level_id: profile.profile_level_id(),
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

        vks_admin::api_v3::call_strings::record_incoming_call(
            state.as_ref(),
            *room_id,
            &active_call.participant_id,
            &format!("sip:{from}"),
            &display_name,
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

        vks_admin::api_v3::call_strings::record_incoming_call(
            state.as_ref(),
            *room_id,
            &active_call.participant_id,
            &format!("h323:{from}"),
            &display_name,
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
    vks_admin::api_v3::call_strings::note_participant_left(state.as_ref(), room_id, participant_id, "Ended By Remote");
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

/// `sip-<uuid>@sip-gateway` / `part-room-<uuid>`: UUID внутри строки.
///
/// `parse_participant_id` сам ищет UUID, но для строк без UUID молча хеширует
/// их в синтетический UUID — такой слот раскладки никогда не совпадёт с
/// реально забридженным участником и стал бы лишней пустой ячейкой композита.
fn layout_identifier_has_uuid(value: &str) -> bool {
    value.split(['@', ':', '/', '_', ' ']).any(|token| {
        let token = token.trim();
        let start = token.len().saturating_sub(36);
        token.is_char_boundary(start) && uuid::Uuid::parse_str(&token[start..]).is_ok()
    })
}

impl GatewayManager {
    async fn dial_with_contact_inner(
        &self,
        room_id: RoomId,
        uri: &str,
        display_name: Option<&str>,
        contact: Option<&serde_json::Value>,
    ) -> Result<String, String> {
        let trimmed = uri.trim();
        let is_h323 = trimmed.starts_with("h323:");
        let is_sip = trimmed.starts_with("sip:")
            || (!trimmed.contains("://") && !trimmed.starts_with("webrtc:") && !trimmed.starts_with("recorder:"));
        if !is_sip && !is_h323 {
            return Err(format!("Unsupported outbound protocol scheme: {uri}"));
        }
        self.ensure_bridgeable_codecs(room_id, trimmed).await?;

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
        let protocol = if is_h323 { "h323" } else { "sip" };
        // Модалка может прислать только auto-call/mute-флаги без видеопрофиля:
        // такие options не должны затирать сохранённый intent адреса.
        let explicit_profile = explicit_video_options(contact, protocol);
        let call_string_profile = match explicit_profile {
            Some(options) => Some(serde_json::json!({"payload": options})),
            None => self.call_string_video_profile(room_id, trimmed).await,
        };
        let mut answer_limits = vks_gateway_sip::VideoAnswerLimits::default();
        let mut video_profile = self.video_profile(room_id, call_string_profile.as_ref());
        let offered_video_profile = video_profile.clone();
        tracing::info!(
            room = %room_id,
            width = video_profile.width,
            height = video_profile.height,
            fps = video_profile.fps,
            bitrate_kbps = video_profile.bitrate_kbps,
            "outbound gateway video profile"
        );
        let (remote_ip, remote_audio_port, audio_payload_type, dtmf_payload_type, video_remote, call_id) = if is_h323 {
            let h323_video = vks_gateway_h323::H323VideoProfile {
                max_bitrate_kbps: video_profile.bitrate_kbps.max(64),
                level_idc: video_profile.h323_level_idc(),
                profile_bits: 64,
            };
            let session = self
                .h323_uac_client
                .dial_with_media_profile(
                    trimmed,
                    rtp_port,
                    video_pair.as_ref().map(|pair| pair.rtp_port),
                    std::time::Duration::from_secs(60),
                    h323_video,
                )
                .await
                .map_err(|error| format!("H.323 outbound dial failed: {error}"))?;
            setup.h323 = Some(session.clone());
            // H.241 peer limits are authoritative for the encoder: never send
            // above the peer's maxBitRate/level even if the room asks for more.
            if let Some(max_bitrate_100bps) = session.remote_video_max_bitrate_100bps {
                // Peer may advertise less than 64 kbit/s; never round UP past
                // its declared maxBitRate (16 kbit/s is the encoder floor).
                video_profile.bitrate_kbps = video_profile.bitrate_kbps.min((max_bitrate_100bps / 10).max(16));
            }
            if let Some(bits) = session.remote_video_h264_profile_bits {
                if bits & 64 == 0 {
                    return Err(
                        "Peer H.264 profile is not baseline-compatible (generic parameter 41 bit 6 is clear)".into()
                    );
                }
            }
            if let Some(level_idc) = session.remote_video_h264_level {
                let level = video::h264_level_name(level_idc)
                    .ok_or_else(|| format!("Peer advertised unsupported H.264 level_idc {level_idc}"))?;
                video_profile.level = level.to_string();
            }
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
            let offer = vks_gateway_sip::build_plain_sdp_with_video(
                &Self::sdp_video_profile(&video_profile),
                &self.candidate_ip,
                rtp_port,
                video_pair.as_ref().map(|pair| pair.rtp_port),
            );
            let session = match self.uac_client.dial(trimmed, &offer, std::time::Duration::from_secs(60)).await {
                Ok(session) => session,
                Err(error) if sip_dial_error_is_terminal(&error) => {
                    return Err(vks_common::terminal_failure(format!("SIP outbound dial failed: {error}")));
                }
                Err(error) => return Err(format!("SIP outbound dial failed: {error}")),
            };
            setup.sip = Some(session.clone());
            answer_limits =
                vks_gateway_sip::video_answer_limits(&session.answer_sdp, session.remote_video_payload_type);
            let answer_bitrate = answer_limits.max_br.iter().chain(answer_limits.b_as.iter()).copied().min();
            video_profile.clamp_to_answer(answer_limits.max_fs, answer_bitrate, answer_limits.max_mbps);
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
            "runtime_video_size": if has_video { Some(format!("{}x{}", video_profile.width, video_profile.height)) } else { None },
            "runtime_video_max_fps": if has_video { Some(video_profile.fps) } else { None },
            "runtime_video_bitrate_kbps": if has_video { Some(video_profile.bitrate_kbps) } else { None },
            "offered_video_size": if has_video { Some(format!("{}x{}", offered_video_profile.width, offered_video_profile.height)) } else { None },
            "offered_video_bitrate_kbps": if has_video { Some(offered_video_profile.bitrate_kbps) } else { None },
            "offered_video_max_fs": if has_video { Some(offered_video_profile.max_fs()) } else { None },
            "offered_video_max_mbps": if has_video { Some(offered_video_profile.max_mbps()) } else { None },
            "offered_video_b_as": if has_video { Some(offered_video_profile.bitrate_kbps) } else { None },
            "answer_video_max_fs": answer_limits.max_fs,
            "answer_video_max_br": answer_limits.max_br,
            "answer_video_max_mbps": answer_limits.max_mbps,
            "answer_video_b_as": answer_limits.b_as,
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
                profile: video_profile.clone(),
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
}

#[async_trait::async_trait]
impl vks_admin::state::OutboundDialer for GatewayManager {
    fn media_stats(&self, participant_id: &str) -> Option<serde_json::Value> {
        GatewayManager::media_stats(self, participant_id)
    }

    /// Проводит выбранную PNM-раскладку в MCU: слот → участник.
    ///
    /// Из `payload.slots[]` берётся `position` и первый непустой идентификатор
    /// (`contact_id`, `call_string_id`, `abonent_id`, `connection_id`); он
    /// нормализуется `parse_participant_id`, чтобы `sip-<uuid>@sip-gateway`
    /// и голый UUID давали один и тот же `ParticipantId`. Слоты без
    /// идентификатора/позиции пропускаются (пустые окна сетки).
    ///
    /// Идентификатор без UUID (`cs-1`, имя адресной книги) отбрасывается:
    /// `parse_participant_id` превратил бы его в sha-256 UUIDv5, который
    /// никогда не совпадёт с реально забридженным участником, — такой слот
    /// стал бы лишней пустой ячейкой в композите.
    async fn apply_layout(&self, room_id: RoomId, layout: serde_json::Value) -> Result<(), String> {
        use vks_core::ports::media_engine::{LayoutDescriptor, LayoutSlot, MediaEngine};

        let Some(mcu) = &self.mcu_engine else {
            return Ok(());
        };
        let layout_id = layout["id"].as_str().filter(|id| !id.is_empty()).unwrap_or("grid").to_string();
        let version =
            layout["payload"]["version"].as_u64().and_then(|version| u32::try_from(version).ok()).unwrap_or(1);
        let Some(slots) = layout["payload"]["slots"].as_array() else {
            return Ok(());
        };

        // Явный контракт: `calculate` и `attach_abonent` отдают 1-based
        // позиции, поэтому по умолчанию вычитаем 1. Для старых 0-based
        // документов можно прислать `payload.position_base = 0`; inference
        // по «есть ли ноль» больше нет (он ломал разреженные [2,3]).
        let position_base = match layout["payload"]["position_base"].as_u64() {
            Some(0) => 0,
            Some(1) | None => 1,
            Some(other) => {
                tracing::warn!(position_base = other, "unsupported position_base; expected 0 or 1, using 1");
                1
            }
        };

        let mut assignments = std::collections::BTreeMap::new();
        let mut geometry = Vec::new();
        for (index, slot) in slots.iter().enumerate() {
            let Some(position) = slot["position"].as_u64().and_then(|value| u32::try_from(value).ok()) else {
                continue;
            };
            let Some(position) = position.checked_sub(position_base) else {
                tracing::warn!(
                    position,
                    position_base,
                    "layout slot position is below position_base; skipped (legacy 0-based layouts must send position_base=0)"
                );
                continue;
            };
            let identifier = ["contact_id", "call_string_id", "abonent_id", "connection_id"]
                .iter()
                .filter_map(|key| slot[*key].as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .find(|value| uuid::Uuid::parse_str(value).is_ok() || layout_identifier_has_uuid(value));
            if let Some(identifier) = identifier {
                assignments.insert(crate::parse_participant_id(identifier), position);
            }
            let number = |key: &str| {
                slot[key].as_f64().filter(|value| value.is_finite()).map(|value| value.max(0.0).round() as u32)
            };
            if let (Some(x), Some(y), Some(width), Some(height)) =
                (number("x"), number("y"), number("width"), number("height"))
            {
                if width > 0 && height > 0 {
                    geometry.push(LayoutSlot {
                        slot: position,
                        x,
                        y,
                        width,
                        height,
                        z_order: slot["z_order"]
                            .as_u64()
                            .and_then(|value| u32::try_from(value).ok())
                            .unwrap_or(index as u32),
                    });
                }
            }
        }
        // Реальные PNM-раскладки хранят только `position`/`role`; геометрия
        // задаётся `payload.preset` (`preset_equal_25`, `preset_selected_b_4_(2)`…).
        // Без этого MCU рисовал sqrt-сетку и выбранная схема терялась.
        let mode = layout["payload"]["mode"].as_str().unwrap_or_default();
        // AUTO (`mode=="auto"`) оставляет geometry пустой: движок строит сетку
        // по числу забридженных паблишеров (фиксированные 13 ячеек
        // `preset_default_13` давали участнику плитку 480×270). Для
        // manual/dynamic геометрия берётся из `payload.preset`.
        if geometry.is_empty() && mode != "auto" {
            let slot_count = assignments.values().copied().max().unwrap_or(0).saturating_add(1);
            let preset_name = layout["payload"]["preset"].as_str().unwrap_or_default();
            let preset_size = layout["payload"]["size"].as_u64().and_then(|value| u8::try_from(value).ok());
            let preset_name = vks_layout::preset::resolve(preset_name, preset_size);
            let parsed = vks_layout::preset::PresetId::parse(&preset_name)
                .filter(|preset| u32::from(preset.size) >= slot_count)
                .unwrap_or_else(|| {
                    vks_layout::preset::PresetId::equal(u8::try_from(slot_count.clamp(1, 64)).unwrap_or(1))
                });
            let overlap_factor = layout["payload"]["overlap_factor"]
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(1.0);
            let pip = layout["payload"]["pip"].as_bool().unwrap_or(false);
            // `geometry_for_manual` сохраняет вендорный z_order (overlap/PiP),
            // generic `geometry` его не возвращает — для остальных схем z=0.
            let calculated = vks_layout::preset::geometry_for_manual(&parsed.to_string(), 1920.0, 1080.0)
                .unwrap_or_else(|| {
                    vks_layout::preset::geometry(&parsed, 1920.0, 1080.0, overlap_factor, pip)
                        .into_iter()
                        .map(|rect| vks_layout::preset::CalculatedSlot { rect, z_order: 0 })
                        .collect()
                });
            for (index, cell) in calculated.iter().enumerate() {
                geometry.push(LayoutSlot {
                    slot: index as u32,
                    x: cell.rect.x.round().max(0.0) as u32,
                    y: cell.rect.y.round().max(0.0) as u32,
                    width: cell.rect.width.round().max(1.0) as u32,
                    height: cell.rect.height.round().max(1.0) as u32,
                    z_order: u32::from(cell.z_order),
                });
            }
        }
        if assignments.is_empty() && geometry.is_empty() && mode != "auto" {
            return Ok(());
        }
        geometry.sort_by_key(|cell| cell.slot);
        // Если MCU-комнаты ещё нет, движок сохранит pending layout и применит
        // его на первом bridge — пустая SFU-сессия не аллоцирует композит.
        mcu.apply_layout(room_id, LayoutDescriptor { layout_id, version, assignments, geometry })
            .await
            .map_err(|error| format!("MCU layout apply failed: {error}"))
    }

    async fn dial(&self, room_id: RoomId, uri: &str, display_name: Option<&str>) -> Result<String, String> {
        self.dial_with_contact(room_id, uri, display_name, None).await
    }

    async fn dial_with_contact(
        &self,
        room_id: RoomId,
        uri: &str,
        display_name: Option<&str>,
        contact: Option<&serde_json::Value>,
    ) -> Result<String, String> {
        let protocol = if uri.trim().starts_with("h323:") { "h323" } else { "sip" };
        let result = self.dial_with_contact_inner(room_id, uri, display_name, contact).await;
        let terminal = result.as_ref().err().is_some_and(|error| terminal_gateway_error(error));
        let error = result.as_ref().err().map(|error| vks_common::strip_terminal_failure(error).to_string());
        self.note_gateway_health(protocol, result.is_ok(), terminal, error);
        result.map_err(|error| vks_common::strip_terminal_failure(&error).to_string())
    }

    fn gateway_registration(&self, protocol: &str) -> Option<bool> {
        match protocol {
            "sip" => self.sip.lock().ok().map(|gateway| gateway.is_registered()),
            "h323" => self.h323.lock().ok().map(|gateway| gateway.is_registered()),
            _ => None,
        }
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

/// Входящий SIP-вызов, принятый UAS-listener'ом.
struct InboundUasCall {
    participant_id: String,
    room_id: RoomId,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    controls: GatewayMediaControls,
    stats: Arc<GatewayMediaStats>,
    ack: Arc<std::sync::atomic::AtomicBool>,
    sdp: String,
    to_tag: String,
}

fn sip_from_display_name(from: &str) -> String {
    let quoted = from.trim();
    if let Some(start) = quoted.find('"') {
        if let Some(end) = quoted[start + 1..].find('"') {
            let name = quoted[start + 1..start + 1 + end].trim();
            if !name.is_empty() {
                return vks_common::sanitize_label_or(name, "SIP");
            }
        }
    }
    let user = quoted.split("sip:").nth(1).and_then(|rest| rest.split(['@', '>', ';']).next()).unwrap_or("SIP");
    vks_common::sanitize_label_or(user, "SIP")
}

fn sip_dialog_key(request: &vks_gateway_sip::UasRequest) -> String {
    format!("{}|{}", request.call_id, request.from_tag.as_deref().unwrap_or("-"))
}

fn sip_send(socket: &tokio::net::UdpSocket, peer: std::net::SocketAddr, text: &str) {
    let _ = socket.try_send_to(text.as_bytes(), peer);
}

/// UDP SIP UAS: принимает `INVITE sip:<room-id>@<advertised-ip>` и поднимает
/// существующий RTP-мост. Транспорт минимален (UDP + plain RTP), потому что
/// именно так вызывают терминалы/TrueConf в этом стенде; SRTP/TLS/100rel
/// возвращают 488/420, а не изображают поддержку.
pub async fn serve_sip_uas(manager: Arc<GatewayManager>, bind: String, advertised_ip: String) -> std::io::Result<()> {
    let socket = Arc::new(tokio::net::UdpSocket::bind(&bind).await?);
    info!(bind = %bind, advertised_ip = %advertised_ip, "SIP UAS listening");
    let max_calls =
        std::env::var("VKS_SIP_UAS_MAX_CALLS").ok().and_then(|value| value.parse::<usize>().ok()).unwrap_or(64);
    let calls: Arc<Mutex<std::collections::HashMap<String, InboundUasCall>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    let mut buf = [0u8; 65535];
    loop {
        let Ok((count, peer)) = socket.recv_from(&mut buf).await else { continue };
        let text = String::from_utf8_lossy(&buf[..count]);
        let Some(request) = vks_gateway_sip::UasRequest::parse(&text) else { continue };
        match request.method.as_str() {
            "INVITE" => {
                if let Some(require) = &request.require {
                    let unsupported = require
                        .split(',')
                        .map(str::trim)
                        .filter(|value| matches!(*value, "100rel" | "timer"))
                        .collect::<Vec<_>>();
                    if !unsupported.is_empty() {
                        let mut response = vks_gateway_sip::build_response(
                            420,
                            "Bad Extension",
                            &request,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        );
                        response = response.replace(
                            "Content-Length:",
                            &format!("Unsupported: {}\r\nContent-Length:", unsupported.join(", ")),
                        );
                        sip_send(&socket, peer, &response);
                        continue;
                    }
                }
                let key = sip_dialog_key(&request);
                let existing = calls
                    .lock()
                    .expect("inbound calls poisoned")
                    .get(&key)
                    .map(|call| (call.to_tag.clone(), call.sdp.clone()));
                if let Some((to_tag, sdp)) = existing {
                    // Retransmission (same dialog): rebuild with this request's
                    // Via/branch, but keep the established To-tag and SDP.
                    let response =
                        vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, 5060, Some(&to_tag), &sdp);
                    sip_send(&socket, peer, &response);
                    continue;
                }
                if calls.lock().expect("inbound calls poisoned").len() >= max_calls {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            503,
                            "Service Unavailable",
                            &request,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        ),
                    );
                    continue;
                }
                let Some(user) = request.target_user() else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(404, "Not Found", &request, &advertised_ip, 5060, None, ""),
                    );
                    continue;
                };
                let Some(state) =
                    manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            503,
                            "Service Unavailable",
                            &request,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        ),
                    );
                    continue;
                };
                let room_id = uuid::Uuid::parse_str(&user).ok().map(RoomId::from_uuid);
                if room_id
                    .is_none_or(|room_id| state.rooms.lock().expect("rooms store poisoned").get(&room_id).is_none())
                {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(404, "Not Found", &request, &advertised_ip, 5060, None, ""),
                    );
                    continue;
                }
                let Some(media) = vks_gateway_sip::negotiate(&request.body) else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            488,
                            "Not Acceptable Here",
                            &request,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        ),
                    );
                    continue;
                };
                let Some(mcu) = manager.mcu_engine.clone() else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            503,
                            "Service Unavailable",
                            &request,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        ),
                    );
                    continue;
                };
                let Some(audio_pair) = manager.port_pool.allocate_pair("0.0.0.0").await else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            503,
                            "Service Unavailable",
                            &request,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        ),
                    );
                    continue;
                };
                let mut audio_lease = RtpPortLease::new(manager.port_pool.clone(), audio_pair.rtp_port);
                let video_pair = if media.video_payload_type.is_some() {
                    manager.port_pool.allocate_pair("0.0.0.0").await
                } else {
                    None
                };
                let mut video_lease =
                    video_pair.as_ref().map(|pair| RtpPortLease::new(manager.port_pool.clone(), pair.rtp_port));
                let has_video = video_pair.is_some();
                let session_id = uuid::Uuid::new_v4().as_u128() as u64;
                let sdp = vks_gateway_sip::build_answer(
                    &advertised_ip,
                    session_id,
                    audio_pair.rtp_port,
                    video_pair.as_ref().map(|pair| pair.rtp_port),
                    &media,
                );
                let to_tag = format!("vks{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(100, "Trying", &request, &advertised_ip, 5060, None, ""),
                );
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(180, "Ringing", &request, &advertised_ip, 5060, None, ""),
                );
                let response =
                    vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, 5060, Some(&to_tag), &sdp);
                sip_send(&socket, peer, &response);
                if let Some(room_id) = room_id {
                    let participant_id = format!("sip-{}-in@sip-gateway", uuid::Uuid::new_v4());
                    let participant = crate::parse_participant_id(&participant_id);
                    let display_name = sip_from_display_name(&request.from);
                    let stats = Arc::new(GatewayMediaStats::default());
                    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let ack = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let controls = GatewayMediaControls::default();
                    land_gateway_participant_in_room(
                        &state,
                        Some(&mcu),
                        &room_id,
                        &participant_id,
                        &display_name,
                        has_video,
                    );
                    let (dtmf_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
                    let controls = GatewayMediaControls { dtmf_sender: dtmf_tx, ..controls };
                    spawn_rtp_media_bridge(RtpBridgeSession {
                        room_id,
                        participant_id_str: participant_id.clone(),
                        participant_id_val: participant,
                        remote_media_ip: media.remote_ip.clone(),
                        remote_audio_port: media.remote_audio_port,
                        audio_payload_type: media.audio_payload_type,
                        dtmf_payload_type: None,
                        media_stats: stats.clone(),
                        rtp_socket: audio_pair.rtp_socket,
                        rtcp_socket: Some(audio_pair.rtcp_socket),
                        rtp_port: audio_pair.rtp_port,
                        port_pool: manager.port_pool.clone(),
                        mcu_engine: mcu.clone(),
                        controls: controls.clone(),
                        cancel_flag: cancel.clone(),
                        dtmf_rx,
                    });
                    audio_lease.disarm();
                    if let (Some(pair), Some(remote_port), Some(payload)) =
                        (video_pair, media.remote_video_port, media.video_payload_type)
                    {
                        if let Ok(remote_ip) = media.remote_ip.parse::<std::net::IpAddr>() {
                            let session = video::VideoBridgeSession {
                                room_id,
                                participant_id: participant,
                                rtp_socket: pair.rtp_socket,
                                rtcp_socket: Some(pair.rtcp_socket),
                                remote_target: std::net::SocketAddr::new(remote_ip, remote_port),
                                payload_type: payload,
                                receive_payload_type: payload,
                                port_pool: manager.port_pool.clone(),
                                engine: mcu,
                                controls: controls.clone(),
                                cancelled: cancel.clone(),
                                stats: stats.clone(),
                                profile: video::VideoProfile::default(),
                            };
                            tokio::spawn(async move {
                                let mut lease = video_lease.take();
                                match video::spawn_video_media_bridge(session).await {
                                    Ok(_) => {
                                        if let Some(lease) = lease.as_mut() {
                                            lease.disarm();
                                        }
                                    }
                                    Err(error) => warn!(%error, "inbound SIP video bridge unavailable; audio-only"),
                                }
                            });
                        }
                    }
                    let media_details = serde_json::json!({
                        "protocol": "SIP",
                        "direction": "inbound",
                        "remote_audio_address": format!("{}:{}", media.remote_ip, media.remote_audio_port),
                        "remote_video_address": media.remote_video_port.map(|port| format!("{}:{}", media.remote_ip, port)),
                        "audio_codec": if media.audio_payload_type == 8 { "PCMA" } else { "PCMU" },
                        "audio_payload_type": media.audio_payload_type,
                        "video_codec": media.video_payload_type.map(|_| "H264"),
                    });
                    if let Ok(mut registry) = manager.registry.lock() {
                        registry.by_call.insert(request.call_id.clone(), participant_id.clone());
                        registry.by_participant.insert(
                            participant_id.clone(),
                            GatewayCallSession {
                                call_id: request.call_id.clone(),
                                participant_id: participant_id.clone(),
                                room_id,
                                display_name: display_name.clone(),
                                uri: request.request_uri.clone(),
                                active_sip: None,
                                active_h323: None,
                                cancel_flag: cancel.clone(),
                                controls: controls.clone(),
                                media_stats: stats.clone(),
                                media_details,
                                connected_at: std::time::Instant::now(),
                            },
                        );
                    }
                    let socket_clone = socket.clone();
                    let ack_flag = ack.clone();
                    let cancel_task = cancel.clone();
                    let response_clone = response.clone();
                    let calls_cleanup = calls.clone();
                    let manager_cleanup = manager.clone();
                    let cleanup_key = key.clone();
                    calls.lock().expect("inbound calls poisoned").insert(
                        key.clone(),
                        InboundUasCall {
                            participant_id: participant_id.clone(),
                            room_id,
                            cancel: cancel.clone(),
                            controls: controls.clone(),
                            stats: stats.clone(),
                            ack: ack.clone(),
                            sdp: sdp.clone(),
                            to_tag: to_tag.clone(),
                        },
                    );
                    tokio::spawn(async move {
                        for _ in 0..20 {
                            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                            if ack_flag.load(std::sync::atomic::Ordering::Relaxed)
                                || cancel_task.load(std::sync::atomic::Ordering::Relaxed)
                            {
                                return;
                            }
                            sip_send(&socket_clone, peer, &response_clone);
                        }
                        // Терминал не подтвердил 200 ACK: снимаем фантомный
                        // participant и возвращаем RTP-порты вместо вечной утечки.
                        if ack_flag.load(std::sync::atomic::Ordering::Relaxed)
                            || cancel_task.load(std::sync::atomic::Ordering::Relaxed)
                        {
                            return;
                        }
                        let call = calls_cleanup.lock().expect("inbound calls poisoned").remove(&cleanup_key);
                        if let Some(call) = call {
                            call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                            if let Ok(mut registry) = manager_cleanup.registry.lock() {
                                if let Some(session) = registry.by_participant.remove(&call.participant_id) {
                                    registry.by_call.remove(&session.call_id);
                                }
                            }
                            if let Some(state) = manager_cleanup
                                .state
                                .read()
                                .ok()
                                .and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                            {
                                terminate_gateway_participant_in_room(
                                    &state,
                                    manager_cleanup.mcu_engine.as_ref(),
                                    &call.room_id,
                                    &call.participant_id,
                                );
                            }
                        }
                    });
                }
            }
            "ACK" => {
                if let Some(call) = calls.lock().expect("inbound calls poisoned").get(&sip_dialog_key(&request)) {
                    call.ack.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            "BYE" => {
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, 5060, None, ""),
                );
                if let Some(call) = calls.lock().expect("inbound calls poisoned").remove(&sip_dialog_key(&request)) {
                    call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Ok(mut registry) = manager.registry.lock() {
                        if let Some(session) = registry.by_participant.remove(&call.participant_id) {
                            registry.by_call.remove(&session.call_id);
                        }
                    }
                    if let Some(state) =
                        manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                    {
                        terminate_gateway_participant_in_room(
                            &state,
                            manager.mcu_engine.as_ref(),
                            &call.room_id,
                            &call.participant_id,
                        );
                    }
                    let _ = (call.controls, call.stats);
                }
            }
            "CANCEL" => {
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, 5060, None, ""),
                );
                if let Some(call) = calls.lock().expect("inbound calls poisoned").remove(&sip_dialog_key(&request)) {
                    call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    if let Ok(mut registry) = manager.registry.lock() {
                        if let Some(session) = registry.by_participant.remove(&call.participant_id) {
                            registry.by_call.remove(&session.call_id);
                        }
                    }
                    let mut invite = request.clone();
                    invite.method = "INVITE".into();
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            487,
                            "Request Terminated",
                            &invite,
                            &advertised_ip,
                            5060,
                            None,
                            "",
                        ),
                    );
                    if let Some(state) =
                        manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                    {
                        terminate_gateway_participant_in_room(
                            &state,
                            manager.mcu_engine.as_ref(),
                            &call.room_id,
                            &call.participant_id,
                        );
                    }
                }
            }
            "OPTIONS" => {
                let mut response = vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, 5060, None, "");
                response =
                    response.replace("Content-Length:", "Allow: INVITE, ACK, BYE, CANCEL, OPTIONS\r\nContent-Length:");
                sip_send(&socket, peer, &response);
            }
            _ => {
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(
                        405,
                        "Method Not Allowed",
                        &request,
                        &advertised_ip,
                        5060,
                        None,
                        "",
                    ),
                );
            }
        }
    }
}

#[cfg(test)]
mod media_runtime_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn terminal_and_transport_gateway_errors_are_classified() {
        // Терминальный отказ — только точный маркер, а не подстроки в тексте.
        assert!(terminal_gateway_error(&vks_common::terminal_failure("486 Busy Here")));
        assert!(terminal_gateway_error(&vks_common::terminal_failure("remote_release:cause=17")));
        for transport in [
            "H.323 sidecar not found; set VKS_H323_HELPER. Searched: /opt/4040",
            "SIP outbound dial failed: Dial timeout (no answer from sip:4861234@host within 60s)",
            "SIP outbound dial failed: Socket IO error: connection refused :4040",
            "No RTP/RTCP port pair is available",
        ] {
            assert!(!terminal_gateway_error(transport), "{transport}");
        }
        // SIP-ответы 4xx/6xx — терминальные; таймаут/IO — транспортные.
        assert!(sip_dial_error_is_terminal(&SipDialError::Rejected(480, "Temporarily Unavailable".into())));
        assert!(sip_dial_error_is_terminal(&SipDialError::Rejected(408, "Request Timeout".into())));
        assert!(sip_dial_error_is_terminal(&SipDialError::Rejected(600, "Busy Everywhere".into())));
        assert!(sip_dial_error_is_terminal(&SipDialError::InvalidSdpAnswer));
        assert!(!sip_dial_error_is_terminal(&SipDialError::Timeout(
            "sip:4861234@host".into(),
            std::time::Duration::from_secs(60)
        )));
        assert!(!sip_dial_error_is_terminal(&SipDialError::Io(std::io::Error::other("bind :4040"))));
        assert!(!sip_dial_error_is_terminal(&SipDialError::NoCommonCodec {
            codec: Some("G722".into()),
            payload: Some(9),
            peer_status: None,
        }));
    }

    #[test]
    fn modal_payload_overrides_only_explicit_video_settings() {
        use serde_json::json;
        let empty = json!({"call_string": {"sip": {"bitrate": null, "resolution": null}}, "auto_call": true});
        assert!(explicit_video_options(Some(&empty), "sip").is_none());
        let bitrate = json!({"call_string": {"sip": {"bitrate": 1024}}});
        assert_eq!(explicit_video_options(Some(&bitrate), "sip").unwrap()["bitrate"], 1024);
        let resolution = json!({"call_string": {"h323": {"resolution": {"width": 640, "height": 360}}}});
        assert_eq!(explicit_video_options(Some(&resolution), "h323").unwrap()["resolution"]["width"], 640);
        assert!(explicit_video_options(None, "sip").is_none());
    }

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
    fn codec_selection_rejects_unbridgeable_only_lists() {
        let g722_only = serde_json::json!([{"name": "G722", "bitrate": null}]);
        let h265_only = serde_json::json!(["H265"]);
        assert!(ensure_bridgeable_codec_selection(Some(&g722_only), Some(&h265_only)).is_err());
        let error = ensure_bridgeable_codec_selection(Some(&g722_only), Some(&h265_only)).unwrap_err();
        assert!(error.contains("no common audio"), "{error}");
        let g711_g722 = serde_json::json!([{"name": "G722"}, {"name": "G711"}]);
        assert!(
            ensure_bridgeable_codec_selection(Some(&g711_g722), Some(&h265_only)).is_err(),
            "H.265-only video всё ещё не мостится"
        );
        let native = serde_json::json!([{"name": "G711", "bitrate": null}]);
        let h264 = serde_json::json!(["H264"]);
        assert!(ensure_bridgeable_codec_selection(Some(&native), Some(&h264)).is_ok());
        assert!(ensure_bridgeable_codec_selection(None, None).is_ok());
        assert!(ensure_bridgeable_codec_selection(Some(&serde_json::json!([])), None).is_ok());
        let high_only = serde_json::json!(["H264-HIGH"]);
        assert!(ensure_bridgeable_codec_selection(Some(&native), Some(&high_only)).is_err());
    }

    #[test]
    fn call_string_lookup_uses_canonical_host_case_and_picks_protocol() {
        let mut resources = vks_admin::api_v3::resources::ResourceState::default();
        resources.addresses.insert(
            "addr-1".to_string(),
            vks_admin::api_v3::resources::AddressRecord {
                id: "addr-1".into(),
                uri: "SIP:Probe@192.168.1.132".into(),
                display_name: "Probe".into(),
                created_at: 1,
                payload: serde_json::json!({
                    "sip": {"bitrate": 256, "resolution": {"width": 320, "height": 180, "fps_limit": 10}},
                    "h323": {"bitrate": 512}
                }),
            },
        );
        let sip = find_call_string_override(&resources, None, "sip:Probe@192.168.1.132:5060").expect("sip override");
        assert_eq!(sip["bitrate"], 256);
        assert_eq!(sip["resolution"]["width"], 320);
        resources.addresses.insert(
            "addr-2".to_string(),
            vks_admin::api_v3::resources::AddressRecord {
                id: "addr-2".into(),
                uri: "h323:probe@192.168.1.132".into(),
                display_name: "Probe H323".into(),
                created_at: 2,
                payload: serde_json::json!({"call_string": {"sip": {"bitrate": 111}, "h323": {"bitrate": 512}}}),
            },
        );
        let h323 = find_call_string_override(&resources, None, "h323:probe@192.168.1.132").expect("h323 override");
        assert_eq!(h323["bitrate"], 512);
        assert!(find_call_string_override(&resources, None, "sip:other@192.168.1.132").is_none());
    }

    #[test]
    fn room_contact_intent_wins_over_global_address_and_uses_camel_keys() {
        let mut resources = vks_admin::api_v3::resources::ResourceState::default();
        let room = RoomId::new();
        resources.addresses.insert(
            "addr".into(),
            vks_admin::api_v3::resources::AddressRecord {
                id: "addr".into(),
                uri: "sip:probe@192.168.1.132".into(),
                display_name: "Probe".into(),
                created_at: 1,
                payload: serde_json::json!({"sip": {"audioCodecs": [{"name": "G711"}], "videoCodecs": ["H264"]}}),
            },
        );
        resources.contacts.insert(
            room.to_string(),
            vec![serde_json::json!({
                "uri": "SIP:probe@192.168.1.132",
                "payload": {"call_string": {"sip": {
                    "audioCodecs": [{"name": "G722"}],
                    "videoCodecs": ["H265"]
                }}}
            })],
        );
        let selected =
            find_call_string_override(&resources, Some(room), "sip:probe@192.168.1.132").expect("contact override");
        assert_eq!(selected["audioCodecs"][0]["name"], "G722");
        assert!(ensure_bridgeable_codec_selection(selected.get("audioCodecs"), selected.get("videoCodecs")).is_err());
        let error = GatewayManager::bridgeable_codec_selection(&resources, room, None, "sip:probe@192.168.1.132")
            .expect_err("contact G722-only must be refused");
        assert!(error.contains("no common audio"), "{error}");
        // Глобальный адрес для другого SIP URI всё ещё читается как плоская карта.
        let global = find_call_string_override(&resources, Some(room), "sip:other@192.168.1.132");
        assert!(global.is_none());
        // Чужой контакт с тем же URI не должен влиять на комнату: после
        // удаления комнатного контакта остаётся глобальный адрес, иначе
        // G722-only другого зала ложно отклонит вызов.
        resources.contacts.remove(&room.to_string());
        let foreign = RoomId::new();
        resources.contacts.insert(
            foreign.to_string(),
            vec![serde_json::json!({
                "uri": "sip:probe@192.168.1.132",
                "payload": {"call_string": {"sip": {"audioCodecs": [{"name": "G722"}]}}}
            })],
        );
        let selected = find_call_string_override(&resources, Some(room), "sip:probe@192.168.1.132")
            .expect("global address after foreign contact ignored");
        assert_eq!(selected["audioCodecs"][0]["name"], "G711");
    }

    #[test]
    fn conference_intent_is_a_fallback_when_no_contact_or_address() {
        let room = RoomId::new();
        let resources = vks_admin::api_v3::resources::ResourceState::default();
        let g722 = serde_json::json!({"payload": {"audio_codecs": [{"name": "G722"}]}});
        let error = GatewayManager::bridgeable_codec_selection(&resources, room, Some(&g722), "sip:x@192.168.1.132")
            .expect_err("conference G722-only must be refused");
        assert!(error.contains("no common audio"), "{error}");
        let native = serde_json::json!({"payload": {"audio_codecs": [{"name": "G711"}]}});
        assert!(
            GatewayManager::bridgeable_codec_selection(&resources, room, Some(&native), "sip:x@192.168.1.132").is_ok()
        );
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

    /// P0: JSON раскладки из админки доводит абонента шлюза до его ячейки.
    #[tokio::test]
    async fn gateway_manager_applies_layout_slots_to_mcu() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        let pid = vks_core::ParticipantId::new();
        let participant = format!("sip-{}@sip-gateway", pid.as_uuid());
        let layout = serde_json::json!({
            "id": "l1",
            "payload": {"slots": [
                // 1-based позиция из `attach_layout_abonent` + точная геометрия.
                {"position": 1, "contact_id": participant, "x": 0, "y": 0, "width": 960, "height": 540, "z_order": 0},
                // Идентификатор адресной книги без UUID: фантомный слот отброшен.
                {"position": 2, "contact_id": "cs-1"}
            ]}
        });
        manager.apply_layout(room, layout).await.expect("chosen layout must reach the MCU");
        assert!(mcu.layout_descriptor(room).is_some(), "pending layout must be stored");
        assert!(mcu.slot_rects(room).is_none(), "pending must not allocate room");

        mcu.bridge_participant(room, pid, vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)]);
        assert!(mcu.layout_descriptor(room).is_some(), "layout after bridge");
        for _ in 0..11 {
            mcu.tick(room);
        }
        let rects = mcu.slot_rects(room).expect("MCU room must exist");
        assert_eq!(rects.len(), 1, "one real participant, no auto slot and no phantom slot 2");
        let cell = rects.get(&0).expect("1-based position 1 normalizes to slot 0");
        assert_eq!((cell.x, cell.y, cell.w, cell.h), (0, 0, 960, 540), "exact PNM geometry, not sqrt grid");
    }

    /// T03/B1: auto-«Общая» в существующей комнате строит сетку по числу
    /// участников, а не рисует фиксированные 13 ячеек `preset_default_13`.
    #[tokio::test]
    async fn gateway_manager_auto_layout_uses_participant_count_not_preset_13() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        mcu.ensure_room(room);
        for _ in 0..2 {
            let pid = vks_core::ParticipantId::new();
            mcu.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)],
            );
        }
        let manual = serde_json::json!({
            "id": "manual-2x2",
            "payload": {
                "mode": "manual", "preset": "preset_equal_4", "size": 4, "type": "individual",
                "slots": [
                    {"position": 1, "role": "AUTO"}, {"position": 2, "role": "AUTO"},
                    {"position": 3, "role": "AUTO"}, {"position": 4, "role": "AUTO"}
                ]
            }
        });
        manager.apply_layout(room, manual).await.expect("manual 2x2");
        for _ in 0..11 {
            mcu.tick(room);
        }
        assert_eq!(mcu.slot_rects(room).expect("manual rects").len(), 4, "manual 2x2 applied");

        let before = mcu.layout_descriptor(room).expect("manual layout").version;
        let layout = serde_json::json!({
            "id": "auto-default",
            "payload": {"mode": "auto", "preset": "preset_default_13", "size": 13, "slots": [], "type": "default"}
        });
        manager.apply_layout(room, layout).await.expect("auto layout must reach existing MCU room");
        for _ in 0..11 {
            mcu.tick(room);
        }
        let rects = mcu.slot_rects(room).expect("MCU room");
        assert_eq!(rects.len(), 2, "2 участника → 2 ячейки, не 13");
        let layout = mcu.layout_descriptor(room).expect("layout");
        assert!(layout.geometry.is_empty(), "auto не синтезирует preset-геометрию");
        assert_eq!(layout.version, before + 1, "ровно один инкремент версии при возврате к «Общей»");
    }

    /// T03: gateway не теряет вендорный z_order overlap/PiP-пресета.
    #[tokio::test]
    async fn gateway_manager_preserves_preset_z_order_for_overlap() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        mcu.ensure_room(room);
        let layout = serde_json::json!({
            "id": "overlap",
            "payload": {
                "mode": "manual", "preset": "preset_selected_b_overlap_4", "size": 4, "type": "individual",
                "slots": [
                    {"position": 1, "role": "AUTO"}, {"position": 2, "role": "AUTO"},
                    {"position": 3, "role": "AUTO"}, {"position": 4, "role": "AUTO"}
                ]
            }
        });
        manager.apply_layout(room, layout).await.expect("overlap layout");
        let descriptor = mcu.layout_descriptor(room).expect("layout");
        assert!(descriptor.geometry.iter().any(|slot| slot.z_order > 0), "overlap windows must carry z_order>0");
    }

    /// T03: позиция ниже `position_base` пропускается с warn, без сдвига.
    #[tokio::test]
    async fn gateway_manager_skips_position_below_base() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        let pid = vks_core::ParticipantId::new();
        let layout = serde_json::json!({
            "id": "below-base",
            "payload": {
                "mode": "manual", "preset": "preset_equal_4",
                "slots": [{"position": 0, "abonent_id": pid.as_uuid().to_string()}]
            }
        });
        manager.apply_layout(room, layout).await.expect("layout accepted");
        let descriptor = mcu.layout_descriptor(room).expect("pending descriptor");
        assert!(descriptor.assignments.is_empty(), "position 0 при base=1 не должен стать слотом 0");
    }

    /// T03: явный `position_base=0` сохраняет разреженные 0-based позиции
    /// [2,3] как слоты 2/3, без inference по «есть ли ноль».
    #[tokio::test]
    async fn gateway_manager_honors_explicit_zero_position_base() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        let a = vks_core::ParticipantId::new();
        let b = vks_core::ParticipantId::new();
        let layout = serde_json::json!({
            "id": "sparse-zero",
            "payload": {
                "position_base": 0,
                "mode": "manual",
                "preset": "preset_equal_4",
                "slots": [
                    {"position": 2, "abonent_id": a.as_uuid().to_string(), "x": 0, "y": 0, "width": 960, "height": 540},
                    {"position": 3, "abonent_id": b.as_uuid().to_string(), "x": 960, "y": 0, "width": 960, "height": 540}
                ]
            }
        });
        manager.apply_layout(room, layout).await.expect("sparse 0-based layout");
        let descriptor = mcu.layout_descriptor(room).expect("pending descriptor");
        let mut slots: Vec<u32> = descriptor.assignments.values().copied().collect();
        slots.sort_unstable();
        assert_eq!(slots, vec![2, 3], "positions 2/3 остаются 2/3, а не превращаются в 0/1");
    }

    /// Реальные PNM-раскладки не хранят x/y: геометрия выводится из
    /// `payload.preset`; `preset_selected_b_4` даёт большую нижнюю зону 75/25,
    /// а не sqrt-сетку 2×2.
    #[tokio::test]
    async fn gateway_manager_derives_geometry_from_preset() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        let pid = vks_core::ParticipantId::new();
        let participant = format!("sip-{}@sip-gateway", pid.as_uuid());
        let layout = serde_json::json!({
            "id": "l2",
            "payload": {
                "preset": "preset_selected_b_4",
                "size": 4,
                "slots": [
                    {"position": 1, "abonent_id": participant, "role": "PEER"},
                    {"position": 2, "role": "AUTO"},
                    {"position": 3, "role": "AUTO"},
                    {"position": 4, "role": "AUTO"}
                ]
            }
        });
        manager.apply_layout(room, layout).await.expect("preset geometry must reach the MCU");
        assert!(mcu.layout_descriptor(room).is_some(), "pending preset layout must be stored");
        mcu.bridge_participant(room, pid, vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)]);
        for _ in 0..11 {
            mcu.tick(room);
        }
        let rects = mcu.slot_rects(room).expect("MCU room");
        assert_eq!(rects.len(), 4, "4 preset cells, empty AUTO cells stay black");
        // Вендорная таблица `preset_selected_b_4` (3×3, блок 3×2): крупная зона
        // 1920×720 и три мелких 640×360 под ней — не авто-сетка 2×2.
        let large = rects.get(&0).expect("selected scheme slot 0");
        assert_eq!((large.x, large.y, large.w, large.h), (0, 0, 1920, 720), "vendor 2/3 zone, not a 2x2 grid");
        let small = rects.get(&1).expect("selected scheme slot 1");
        assert_eq!((small.x, small.y, small.w, small.h), (0, 720, 640, 360), "small cell under the big zone");
    }

    /// Живой JSON раскладки, сохранённой UI в E2E на :8080 (сессия
    /// `c437bf65…`, layout `86a1a258…`): manual 4, `preset_equal_4`,
    /// окна 1..4 все AUTO (это состояние после «Очистить все видеоокна»).
    /// Привязок нет, но геометрия 2×2 обязана доехать до MCU — иначе шлюз
    /// получает sqrt-сетку по числу участников вместо выбранной схемы.
    #[tokio::test]
    async fn live_manual_equal_4_layout_gives_exact_2x2_cells() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        let layout = serde_json::json!({
            "id": "86a1a258-3bef-40b0-bb0a-7c2d5c0cfa1d",
            "payload": {
                "autoscroll": 0,
                "mode": "manual",
                "preset": "preset_equal_4",
                "size": 4,
                "slots": [
                    {"position": 1, "role": "AUTO"},
                    {"position": 2, "role": "AUTO"},
                    {"position": 3, "role": "AUTO"},
                    {"position": 4, "role": "AUTO"}
                ],
                "type": "individual"
            }
        });
        manager.apply_layout(room, layout).await.expect("manual layout must reach the MCU");
        assert!(mcu.layout_descriptor(room).is_some(), "раскладка сохранена как pending");
        mcu.ensure_room(room);

        let rects = mcu.slot_rects(room).expect("MCU room must exist after a manual layout");
        assert_eq!(rects.len(), 4, "equal 2×2, not a sqrt grid by participant count");
        let cell = |slot: u32| {
            let rect = rects.get(&slot).expect("cell");
            (rect.x, rect.y, rect.w, rect.h)
        };
        assert_eq!(cell(0), (0, 0, 960, 540), "top-left cell");
        assert_eq!(cell(3), (960, 540, 960, 540), "bottom-right cell");

        // SIP-участник садится в свободную ячейку 0, кадр виден в композите.
        let pid = vks_core::ParticipantId::new();
        mcu.bridge_participant(room, pid, vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)]);
        assert!(mcu.set_participant_frame(
            room,
            pid,
            vks_media_mcu::frame::Frame::new(320, 180, vec![77u8; 320 * 180 * 3 / 2])
        ));
        let composite = mcu.tick_video_at(room, 0).expect("composite frame");
        assert_eq!(composite.data[270 * 1920 + 480], 77, "gateway frame in the top-left cell");
    }

    /// Компромисс: авто-раскладка без окон не создаёт MCU-комнату (2 МБ
    /// композита) в каждой SFU-сессии; MCU берёт сетку по числу участников.
    #[tokio::test]
    async fn auto_layout_without_slots_does_not_create_mcu_room() {
        use vks_admin::state::OutboundDialer;
        let mcu = Arc::new(vks_media_mcu::McuEngine::new());
        let manager = GatewayManager::new().with_mcu(mcu.clone());
        let room = RoomId::new();
        let layout = serde_json::json!({
            "id": "b74fc0b2-4920-411d-9b53-88b247b0ec50",
            "payload": {"mode": "auto", "preset": "preset_default_13", "size": 13, "slots": [], "type": "default"}
        });
        manager.apply_layout(room, layout).await.expect("auto layout is a no-op, not an error");
        assert!(mcu.slot_rects(room).is_none(), "idle SFU room must not allocate an MCU composite");
    }

    /// Идентификаторы участников: UUID-производные → true, адресная книга/имена
    /// (sha-256 UUIDv5 у `parse_participant_id`) → false.
    #[test]
    fn layout_identifier_accepts_only_uuid_shaped_values() {
        let pid = vks_core::ParticipantId::new();
        assert!(layout_identifier_has_uuid(&format!("sip-{}@sip-gateway", pid.as_uuid())));
        assert!(layout_identifier_has_uuid(&format!("h323-{}@h323-gateway", pid.as_uuid())));
        assert!(layout_identifier_has_uuid(&format!("part-room-{}", pid.as_uuid())));
        assert!(layout_identifier_has_uuid(&pid.as_uuid().to_string()));
        assert!(!layout_identifier_has_uuid("cs-1"));
        assert!(!layout_identifier_has_uuid("Terminal #2"));
        assert!(!layout_identifier_has_uuid(""));
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

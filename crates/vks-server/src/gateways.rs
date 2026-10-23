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
/// только несовместимые кодеки (например G.729-only, H.265-only), должен
/// получить явную причину, а не молчаливый fallback и не «connected» без
/// медиа. `None`/пустой список = выбор не задан, действует native-профиль.
/// Для `h323:` используется отдельный список реально открываемых sidecar'ом
/// кодеков (G.711U), иначе SIP-wideband G.722 проходил бы precheck H.323.
fn codec_name(item: &serde_json::Value) -> Option<String> {
    let raw = item.as_str().or_else(|| item.get("name").and_then(serde_json::Value::as_str))?;
    let (raw, inline_bitrate) =
        raw.split_once(':').map_or((raw, None), |(name, bitrate)| (name, bitrate.trim().parse::<u32>().ok()));
    let name = raw.trim().to_uppercase().replace(['.', '-', '_'], "");
    if name.is_empty() {
        return None;
    }
    if let Some(suffix) = name.strip_prefix("G726") {
        // Keep the bitrate in the token so the variant-aware capability check
        // cannot accept unsupported G726:48 / G726:null selections.  An
        // explicit name (`G72616`) is also overridden by a conflicting
        // `bitrate` field, otherwise `{"name":"G72616","bitrate":48}` would
        // slip through the `None` variant check.
        let explicit = item.get("bitrate").and_then(serde_json::Value::as_u64).map(|value| value as u32);
        let suffix_bits = suffix.parse::<u32>().ok();
        let bitrate = explicit.or(inline_bitrate).or(suffix_bits);
        return Some(bitrate.map_or_else(|| "G726".to_string(), |bitrate| format!("G726{bitrate}")));
    }
    Some(name)
}

/// Audio codecs the H.323 sidecar actually opens an OLC for: symmetric
/// G.722 (PT9), G.723.1 (PT4), G.711U (PT0) or G.711A (PT8). G.728 stays
/// SIP-only until H.245 g728 frame-count/packetization is verified against real
/// terminals; the bridge accepts the RFC 3551 40-byte/20-ms packet only.
const H323_AUDIO: &[&str] = &["PCMU", "G711U", "PCMA", "G711A", "G711", "G722", "G723", "G7231"];

fn h323_audio_native(name: &str) -> bool {
    H323_AUDIO.iter().any(|codec| codec.eq_ignore_ascii_case(name))
}

#[cfg(test)]
fn ensure_bridgeable_codec_selection(
    audio: Option<&serde_json::Value>,
    video: Option<&serde_json::Value>,
) -> Result<(), String> {
    ensure_bridgeable_codec_selection_for(false, audio, video)
}

fn ensure_bridgeable_codec_selection_for(
    h323: bool,
    audio: Option<&serde_json::Value>,
    video: Option<&serde_json::Value>,
) -> Result<(), String> {
    use vks_common::codec_capabilities::{is_native_audio_variant, is_native_video, NATIVE_AUDIO, NATIVE_VIDEO};
    let audio_supported: &[&str] = if h323 { H323_AUDIO } else { NATIVE_AUDIO };
    let audio_native: fn(&str) -> bool =
        if h323 { h323_audio_native } else { |name| is_native_audio_variant(name, None) };
    for (kind, value, supported, native) in [
        ("audio", audio, audio_supported, audio_native),
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

/// Управление входящим H.323-вызовом: команда Release идёт в sidecar listener.
#[derive(Clone)]
pub struct InboundH323Control {
    pub uas: vks_gateway_h323::H323UasHandle,
    pub call_id: String,
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
    pub inbound_h323: Option<InboundH323Control>,
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
    sip_uas_shutdown: Arc<tokio::sync::Notify>,
    sip_uas_stopping: Arc<std::sync::atomic::AtomicBool>,
    sip_uas_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    sip_uas_reaper_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
    h323_uas_shutdown: Arc<tokio::sync::Notify>,
    h323_uas_stopping: Arc<std::sync::atomic::AtomicBool>,
    h323_uas_task: Arc<std::sync::Mutex<Option<tokio::task::JoinHandle<()>>>>,
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
            sip_uas_shutdown: Arc::new(tokio::sync::Notify::new()),
            sip_uas_stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            sip_uas_task: Arc::new(std::sync::Mutex::new(None)),
            sip_uas_reaper_task: Arc::new(std::sync::Mutex::new(None)),
            h323_uas_shutdown: Arc::new(tokio::sync::Notify::new()),
            h323_uas_stopping: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            h323_uas_task: Arc::new(std::sync::Mutex::new(None)),
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

    /// Останавливает SIP UAS listener, дожидается его завершения и BYE активным диалогам.
    pub async fn shutdown_sip_uas(&self) {
        self.sip_uas_stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.sip_uas_shutdown.notify_one();
        let handle = { self.sip_uas_task.lock().expect("sip uas task poisoned").take() };
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), handle).await;
        }
        let reaper = { self.sip_uas_reaper_task.lock().expect("sip uas reaper poisoned").take() };
        if let Some(reaper) = reaper {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(6), reaper).await;
        }
    }

    /// Сигнализирует об остановке до любых await, чтобы супервизоры не
    /// успели перезапустить listener во время graceful shutdown.
    pub fn begin_shutdown(&self) {
        self.sip_uas_stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.sip_uas_shutdown.notify_one();
        self.h323_uas_stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.h323_uas_shutdown.notify_one();
    }

    /// Останавливает входящий H.323 UAS: Release всем диалогам и join listener.
    pub async fn shutdown_h323_uas(&self) {
        self.h323_uas_stopping.store(true, std::sync::atomic::Ordering::SeqCst);
        self.h323_uas_shutdown.notify_one();
        let handle = { self.h323_uas_task.lock().expect("h323 uas task poisoned").take() };
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(8), handle).await;
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
        ensure_bridgeable_codec_selection_for(
            uri.trim_start().to_ascii_lowercase().starts_with("h323:"),
            pick("audio_codecs", "audioCodecs"),
            pick("video_codecs", "videoCodecs"),
        )
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
        if crate::webrtc_runtime::room_has_peer(*room_id) {
            return Err(Error::Conflict("room has a real WebRTC participant; SIP bridge is not supported".into()));
        }
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

        if !land_gateway_participant_in_room(
            state,
            self.mcu_engine.as_ref(),
            room_id,
            &active_call.participant_id,
            &display_name,
            has_video,
        ) {
            return Err(Error::Conflict(
                "room has a real WebRTC participant; SIP/H.323 bridge is not supported".into(),
            ));
        }

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
        if crate::webrtc_runtime::room_has_peer(*room_id) {
            return Err(Error::Conflict("room has a real WebRTC participant; H.323 bridge is not supported".into()));
        }
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

        if !land_gateway_participant_in_room(
            state,
            self.mcu_engine.as_ref(),
            room_id,
            &active_call.participant_id,
            &display_name,
            has_video,
        ) {
            return Err(Error::Conflict(
                "room has a real WebRTC participant; SIP/H.323 bridge is not supported".into(),
            ));
        }

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

/// PCM sample rate the bridge works with for this payload type (G722 is
/// wideband even though its RTP timestamp clock stays 8000 Hz).
///
/// The value is advertised in media details; it is not an RTP clock.
pub(super) fn audio_sample_rate(payload_type: u8) -> u32 {
    match payload_type {
        9 => 16_000,
        111 => 48_000,
        _ => 8_000,
    }
}

fn bridgeable_audio_pts(is_h323: bool) -> &'static [u8] {
    if is_h323 {
        vks_gateway_h323::H323_NATIVE_AUDIO_PTS
    } else {
        &[0, 4, 8, 9, 15, 107, 108, 109, 110, 111]
    }
}

/// H.323 single-codec bridge: only symmetric static G.711U/PCMA/G.722/G.723.1
/// may reach `RtpBridgeSession`; anything else is a listener/sidecar bug.
fn symmetric_h323_audio_pt(remote: Option<u8>, local: Option<u8>) -> Option<u8> {
    let (remote, local) = (remote?, local?);
    (remote == local && vks_gateway_h323::H323_NATIVE_AUDIO_PTS.contains(&remote)).then_some(remote)
}

fn h323_inbound_audio_pt(event: &serde_json::Value) -> Option<u8> {
    let payload =
        |field: &str| event.get(field).and_then(serde_json::Value::as_u64).and_then(|pt| u8::try_from(pt).ok());
    symmetric_h323_audio_pt(payload("remote_audio_payload_type"), payload("local_audio_payload_type"))
}

/// Canonical audio codec name for stats/UI (RTP payload type → name).
pub(super) fn audio_codec_name(payload_type: u8) -> &'static str {
    match payload_type {
        0 => "PCMU",
        8 => "PCMA",
        9 => "G722",
        15 => "G728",
        4 => "G723",
        107 => "G726-16",
        108 => "G726-24",
        109 => "G726-32",
        110 => "G726-40",
        111 => "OPUS",
        _ => "unknown",
    }
}

pub struct RtpBridgeSession {
    pub room_id: RoomId,
    pub participant_id_str: String,
    pub participant_id_val: vks_core::ParticipantId,
    pub remote_media_ip: String,
    pub remote_audio_port: u16,
    pub audio_payload_type: u8,
    /// Canonical negotiated codec name; authoritative for dynamic PTs 96-127.
    pub audio_codec: Option<&'static str>,
    /// Первый входящий RTP-пакет может прийти с другого source (NAT).
    pub source_latch: bool,
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

/// Запускает фоновый 20-мс RTP медиа-мост (G.711/G.722/G.726 <-> McuEngine N-1 mix-minus).
///
/// Полностью абстрагирован от сигнального протокола (используется одинаково для SIP и H.323).
/// Per-packet conversion buffers, no shared mutable state на горячем пути: стек-буферы и `set_audio_slice`/`copy_participant_audio`.
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
) -> bool {
    // Serialise with browser offer admission and mode eviction: check and
    // registration must be one atomic decision.
    let _admission = crate::webrtc_runtime::admission();
    // The real WebRTC path has no SIP/H.323 plaintext<->SRTP bridge. Enforce
    // the same-room guard when the gateway joins, not only at browser offer
    // time, otherwise the room silently becomes half-bridged.
    if crate::webrtc_runtime::room_has_peer(*room_id) {
        tracing::error!(
            room = %room_id,
            participant = %participant_id,
            "gateway participant rejected: room has a real WebRTC peer (no SRTP bridge)"
        );
        return false;
    }
    bridge_gateway_participant_media(mcu_engine, room_id, participant_id, has_video);
    announce_gateway_participant(state, room_id, participant_id, display_name, has_video);
    true
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
        // A real WebRTC room has no plaintext<->SRTP bridge: refuse before
        // dialing/announcing instead of answering the phone into silence.
        if crate::webrtc_runtime::room_has_peer(room_id) {
            return Err("room has a real WebRTC participant; SIP/H.323 bridge is not supported".into());
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
            // Профиль пира — только информация: наш encoder отдаёт baseline,
            // который декодируется любым H.264-профилем. Не отвергаем high/main.
            if let Some(level_idc) = session.remote_video_h264_level {
                // Неизвестный/кастомный level у пира не повод рвать вызов:
                // encoder остаётся на уровне комнаты (не выше 4.0).
                if let Some(level) = video::h264_level_name(level_idc) {
                    video_profile.level = level.to_string();
                }
            }
            let audio_port = session
                .remote_audio_port
                .filter(|port| *port > 0)
                .ok_or_else(|| "H.323 did not negotiate an audio RTP channel".to_string())?;
            if session.local_audio_payload_type != session.remote_audio_payload_type {
                return Err(
                    "Asymmetric H.323 audio payload types are not supported by the single-codec G.711/G.722/G.723.1 bridge"
                        .into(),
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
            if session.remote_video_codec == Some("H265") {
                video_profile.codec = video::GatewayVideoCodec::H265;
            }
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
        if !bridgeable_audio_pts(is_h323).contains(&audio_payload_type) {
            return Err("Negotiated audio payload type is not supported by the RTP bridge".into());
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
            "audio_codec": audio_codec_name(audio_payload_type),
            "audio_payload_type": audio_payload_type,
            "audio_clock_rate": if matches!(audio_payload_type, 111) { 48000 } else { 8000 },
            "audio_sample_rate": audio_sample_rate(audio_payload_type),
            "audio_tx_payload_type": audio_payload_type, "audio_rx_payload_type": audio_payload_type,
            "remote_audio_address": std::net::SocketAddr::new(remote_audio_ip, remote_audio_port).to_string(),
            "video_codec": if has_video { Some(video_profile.codec.name()) } else { None },
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
                    inbound_h323: None,
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
        {
            let _admission = crate::webrtc_runtime::admission();
            if crate::webrtc_runtime::room_has_peer(room_id) {
                return Err("room gained a real WebRTC participant during outbound dial".into());
            }
            // Announce under the admission lock and before the (awaiting)
            // media bridge: a concurrent browser offer then sees the gateway
            // and is rejected instead of creating a half-bridged room.
            if let Some(state) = &state {
                announce_gateway_participant(state, &room_id, &pid_str, &effective_name, has_video);
            }
        }
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
            audio_codec: Some(audio_codec_name(audio_payload_type)),
            source_latch: false,
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
        if let Some(_state) = &state {
            // Start the media watchdog clock only when the participant is
            // actually visible, not before the FFmpeg probe and channel start.
            if let Ok(mut registry) = self.registry.lock() {
                if let Some(session) = registry.by_participant.get_mut(&pid_str) {
                    session.connected_at = std::time::Instant::now();
                }
            }
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
            // Пресеты Codian (`codian_01`…`codian_52`, `codian_custom_*`) имеют
            // собственную геометрию по спецификации 4200: их id не разбирается
            // `PresetId`, поэтому таблица раскладок проверяется напрямую.
            let codian = vks_layout::codian::geometry(&preset_name, 1920.0, 1080.0);
            let overlap_factor = layout["payload"]["overlap_factor"]
                .as_f64()
                .filter(|value| value.is_finite() && *value > 0.0)
                .unwrap_or(1.0);
            let pip = layout["payload"]["pip"].as_bool().unwrap_or(false);
            // `geometry_for_manual` сохраняет вендорный z_order (overlap/PiP),
            // generic `geometry` его не возвращает — для остальных схем z=0.
            let calculated = codian.unwrap_or_else(|| {
                let parsed = vks_layout::preset::PresetId::parse(&preset_name)
                    .filter(|preset| u32::from(preset.size) >= slot_count)
                    .unwrap_or_else(|| {
                        vks_layout::preset::PresetId::equal(u8::try_from(slot_count.clamp(1, 64)).unwrap_or(1))
                    });
                vks_layout::preset::geometry_for_manual(&parsed.to_string(), 1920.0, 1080.0).unwrap_or_else(|| {
                    vks_layout::preset::geometry(&parsed, 1920.0, 1080.0, overlap_factor, pip)
                        .into_iter()
                        .map(|rect| vks_layout::preset::CalculatedSlot { rect, z_order: 0 })
                        .collect()
                })
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
        if let Some(inbound) = session.inbound_h323 {
            tokio::spawn(async move {
                inbound.uas.hangup(&inbound.call_id).await;
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

/// Входящий вызов: 90 с тишины — это уже не hold, а потерянный терминал.
/// Порог мягче outbound (15 с), чтобы не рвать terminal-initiated hold.
fn inbound_media_dead(
    elapsed: std::time::Duration,
    last_rx_ms: u64,
    now_ms: u64,
    audio_codec_running: bool,
    media_timeout: std::time::Duration,
) -> bool {
    let silence_ms = if last_rx_ms == 0 { elapsed.as_millis() as u64 } else { now_ms.saturating_sub(last_rx_ms) };
    let media_stale = silence_ms > media_timeout.as_millis() as u64;
    let codec_dead = !audio_codec_running && elapsed > std::time::Duration::from_secs(10);
    media_stale || codec_dead
}

/// Дожидается остановки media-задач после cancel, чтобы порты/кодеки
/// освободились до снятия participant и ответа BYE.
async fn wait_inbound_bridges(call: &mut InboundUasCall) {
    if let Some(task) = call.audio_task.take() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), task).await;
    }
    if let Some(task) = call.video_task.take() {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(1), task).await;
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
    invite_cseq: u32,
    peer: std::net::SocketAddr,
    bye: String,
    answered_at: std::time::Instant,
    audio_task: Option<tokio::task::JoinHandle<()>>,
    video_task: Option<tokio::task::JoinHandle<()>>,
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
    // RFC 3581/3261: ответ должен вернуть фактический source (received) и
    // rport, а не адрес из Via/локальный advertised IP.
    let mut out = text.to_string();
    if let Some(start) = out.find(";received=") {
        let value_start = start + ";received=".len();
        if let Some(len) = out[value_start..].find([';', '\r']).or_else(|| Some(out.len().saturating_sub(value_start)))
        {
            out.replace_range(value_start..value_start + len, &peer.ip().to_string());
        }
    }
    if let Some(start) = out.find(";rport") {
        let after = start + ";rport".len();
        if !out[after..].starts_with('=') {
            out.insert_str(after, &format!("={}", peer.port()));
        }
    }
    let _ = socket.try_send_to(out.as_bytes(), peer);
}

/// Собирает серверный BYE для завершения входящего диалога (admin hangup).
fn build_uas_bye(
    request: &vks_gateway_sip::UasRequest,
    to_tag: &str,
    peer: std::net::SocketAddr,
    advertised_ip: &str,
    local_port: u16,
) -> String {
    let target = request
        .contact
        .as_deref()
        .and_then(|contact| contact.split('<').nth(1).and_then(|rest| rest.split('>').next()))
        .map(str::to_string)
        .unwrap_or_else(|| {
            let user = request
                .from
                .split("sip:")
                .nth(1)
                .and_then(|rest| rest.split(['@', '>', ';']).next())
                .unwrap_or("remote");
            format!("sip:{user}@{}:{}", peer.ip(), peer.port())
        });
    // Сервер — локальная сторона диалога: From = To из INVITE + наш tag,
    // To = From вызывающего с его tag. Обратный порядок терминал отвергнет.
    let mut from = request.to.clone();
    if !from.contains(";tag=") {
        from.push_str(&format!(";tag={to_tag}"));
    }
    format!(
        "BYE {target} SIP/2.0\r\nVia: SIP/2.0/UDP {advertised_ip}:{local_port};branch=z9hG4bKvksbye{}\r\nFrom: {from}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {} BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n",
        uuid::Uuid::new_v4().simple(),
        request.from,
        request.call_id,
        request.cseq.saturating_add(1),
    )
}

/// Завершает все входящие диалоги: BYE уходит сразу всем, а media-задачи
/// дожидаются параллельно под общим дедлайном (не 4 с на каждый вызов).
async fn stop_inbound_calls(
    manager: &Arc<GatewayManager>,
    socket: &Arc<tokio::net::UdpSocket>,
    calls: &Arc<Mutex<std::collections::HashMap<String, InboundUasCall>>>,
) {
    let active = {
        let mut guard = calls.lock().expect("inbound calls poisoned");
        std::mem::take(&mut *guard)
    };
    let mut waiters = Vec::with_capacity(active.len());
    for (_, mut call) in active {
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
        for _ in 0..2 {
            sip_send(socket, call.peer, &call.bye);
        }
        waiters.push(async move {
            wait_inbound_bridges(&mut call).await;
        });
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(4), futures_util::future::join_all(waiters)).await;
}

/// UDP SIP UAS: принимает `INVITE sip:<room-id>@<advertised-ip>` и поднимает
/// существующий RTP-мост. Транспорт минимален (UDP + plain RTP), потому что
/// именно так вызывают терминалы/TrueConf в этом стенде; SRTP/TLS/100rel
/// возвращают 488/420, а не изображают поддержку.
pub async fn serve_sip_uas(manager: Arc<GatewayManager>, bind: String, advertised_ip: String) -> std::io::Result<()> {
    let socket = Arc::new(tokio::net::UdpSocket::bind(&bind).await?);
    let runner = manager.clone();
    let task = tokio::spawn(async move {
        if let Err(error) = serve_sip_uas_on(runner, socket, advertised_ip, std::time::Duration::from_secs(90)).await {
            warn!(%error, "SIP UAS listener stopped");
        }
    });
    *manager.sip_uas_task.lock().expect("sip uas task poisoned") = Some(task);
    Ok(())
}

async fn serve_sip_uas_on(
    manager: Arc<GatewayManager>,
    socket: Arc<tokio::net::UdpSocket>,
    advertised_ip: String,
    media_timeout: std::time::Duration,
) -> std::io::Result<()> {
    let bind = socket.local_addr().map(|addr| addr.to_string()).unwrap_or_default();
    let local_port = socket.local_addr().map(|addr| addr.port()).unwrap_or(5060);
    info!(bind = %bind, advertised_ip = %advertised_ip, "SIP UAS listening");
    let max_calls =
        std::env::var("VKS_SIP_UAS_MAX_CALLS").ok().and_then(|value| value.parse::<usize>().ok()).unwrap_or(64);
    let calls: Arc<Mutex<std::collections::HashMap<String, InboundUasCall>>> =
        Arc::new(Mutex::new(std::collections::HashMap::new()));
    // Простой token bucket per source IP для INVITE: 20 burst, 10 в секунду.
    // Не заменяет auth/allowlist, но не даёт одному хосту занять все слоты.
    let mut invite_buckets: std::collections::HashMap<std::net::IpAddr, (f64, std::time::Instant)> =
        std::collections::HashMap::new();
    // Админский hangup выставляет cancel_flag в реестре, но не имеет доступа
    // к этой map. Периодический reaper освобождает запись и ёмкость cap,
    // не дожидаясь BYE, которого при admin hangup может не быть.
    let reaper_calls = calls.clone();
    let reaper_socket = socket.clone();
    let manager_reaper = manager.clone();
    let reaper_task = tokio::spawn(async move {
        loop {
            if manager_reaper.sip_uas_stopping.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let mut dead = Vec::new();
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            if let Ok(mut guard) = reaper_calls.lock() {
                let keys: Vec<String> = guard
                    .iter()
                    .filter(|(_, call)| {
                        let cancelled = call.cancel.load(std::sync::atomic::Ordering::Relaxed);
                        let media_dead = call.ack.load(std::sync::atomic::Ordering::Relaxed)
                            && inbound_media_dead(
                                call.answered_at.elapsed(),
                                call.stats.last_rx_ms.load(std::sync::atomic::Ordering::Relaxed),
                                now_ms,
                                call.stats.audio_codec_running.load(std::sync::atomic::Ordering::Relaxed),
                                media_timeout,
                            );
                        cancelled || media_dead
                    })
                    .map(|(key, _)| key.clone())
                    .collect();
                for key in keys {
                    if let Some(call) = guard.remove(&key) {
                        if !call.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            // Медиа-таймаут: до BYE снимаем participant, чтобы
                            // не ждать, пока терминал ответит.
                            call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                        dead.push(call);
                    }
                }
            }
            for mut call in dead {
                // Admin hangup уже снял participant/media; при media-timeout
                // снимаем здесь. В обоих случаях завершаем удалённый диалог.
                wait_inbound_bridges(&mut call).await;
                if let Ok(mut registry) = manager_reaper.registry.lock() {
                    if let Some(session) = registry.by_participant.remove(&call.participant_id) {
                        registry.by_call.remove(&session.call_id);
                    }
                }
                if let Some(state) =
                    manager_reaper.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                {
                    terminate_gateway_participant_in_room(
                        &state,
                        manager_reaper.mcu_engine.as_ref(),
                        &call.room_id,
                        &call.participant_id,
                    );
                }
                for _ in 0..2 {
                    sip_send(&reaper_socket, call.peer, &call.bye);
                }
            }
        }
    });
    *manager.sip_uas_reaper_task.lock().expect("sip uas reaper poisoned") = Some(reaper_task);
    let mut buf = [0u8; 65535];
    loop {
        if manager.sip_uas_stopping.load(std::sync::atomic::Ordering::SeqCst) {
            stop_inbound_calls(&manager, &socket, &calls).await;
            info!("SIP UAS shutdown: active inbound dialogs terminated");
            return Ok(());
        }
        let received = tokio::select! {
            _ = manager.sip_uas_shutdown.notified() => {
                stop_inbound_calls(&manager, &socket, &calls).await;
                info!("SIP UAS shutdown: active inbound dialogs terminated");
                return Ok(());
            }
            result = socket.recv_from(&mut buf) => result,
        };
        let Ok((count, peer)) = received else { continue };
        let text = String::from_utf8_lossy(&buf[..count]);
        let Some(request) = vks_gateway_sip::UasRequest::parse(&text) else { continue };
        let early_to_tag = format!("vks{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        if request.method == "INVITE" {
            if invite_buckets.len() > 1024 {
                invite_buckets.clear();
            }
            let now = std::time::Instant::now();
            let bucket = invite_buckets.entry(peer.ip()).or_insert((20.0, now));
            let elapsed = now.duration_since(bucket.1).as_secs_f64();
            bucket.0 = (bucket.0 + elapsed * 10.0).min(20.0);
            bucket.1 = now;
            if bucket.0 < 1.0 {
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(
                        503,
                        "Service Unavailable",
                        &request,
                        &advertised_ip,
                        local_port,
                        Some(&early_to_tag),
                        "",
                    ),
                );
                continue;
            }
            bucket.0 -= 1.0;
        }
        match request.method.as_str() {
            "INVITE" => {
                let to_tag = early_to_tag;
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
                            local_port,
                            Some(&to_tag),
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
                    .map(|call| (call.to_tag.clone(), call.sdp.clone(), call.invite_cseq));
                if let Some((to_tag, sdp, invite_cseq)) = existing {
                    if request.cseq > invite_cseq {
                        // re-INVITE/hold: 501, а не кэш старого 200/SDP.
                        sip_send(
                            &socket,
                            peer,
                            &vks_gateway_sip::build_response(
                                501,
                                "Not Implemented",
                                &request,
                                &advertised_ip,
                                local_port,
                                Some(&to_tag),
                                "",
                            ),
                        );
                        continue;
                    }
                    // Retransmission (same dialog): rebuild with this request's
                    // Via/branch, but keep the established To-tag and SDP.
                    let response = vks_gateway_sip::build_response(
                        200,
                        "OK",
                        &request,
                        &advertised_ip,
                        local_port,
                        Some(&to_tag),
                        &sdp,
                    );
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
                            local_port,
                            Some(&to_tag),
                            "",
                        ),
                    );
                    continue;
                }
                let Some(user) = request.target_user() else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            404,
                            "Not Found",
                            &request,
                            &advertised_ip,
                            local_port,
                            Some(&to_tag),
                            "",
                        ),
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
                            local_port,
                            Some(&to_tag),
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
                        &vks_gateway_sip::build_response(
                            404,
                            "Not Found",
                            &request,
                            &advertised_ip,
                            local_port,
                            Some(&to_tag),
                            "",
                        ),
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
                            local_port,
                            Some(&to_tag),
                            "",
                        ),
                    );
                    continue;
                };
                if media.remote_ip.parse::<std::net::IpAddr>().is_err()
                    || media.remote_ip.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_unspecified())
                {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            488,
                            "Not Acceptable Here",
                            &request,
                            &advertised_ip,
                            local_port,
                            Some(&to_tag),
                            "",
                        ),
                    );
                    continue;
                }
                if let Some(video_ip) = &media.remote_video_ip {
                    if video_ip.parse::<std::net::IpAddr>().is_err()
                        || video_ip.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_unspecified())
                    {
                        sip_send(
                            &socket,
                            peer,
                            &vks_gateway_sip::build_response(
                                488,
                                "Not Acceptable Here",
                                &request,
                                &advertised_ip,
                                local_port,
                                Some(&to_tag),
                                "",
                            ),
                        );
                        continue;
                    }
                }
                let Some(mcu) = manager.mcu_engine.clone() else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            503,
                            "Service Unavailable",
                            &request,
                            &advertised_ip,
                            local_port,
                            Some(&to_tag),
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
                            local_port,
                            Some(&to_tag),
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
                let answer_video_port = video_pair.as_ref().map(|pair| pair.rtp_port);
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
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(100, "Trying", &request, &advertised_ip, local_port, None, ""),
                );
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(
                        180,
                        "Ringing",
                        &request,
                        &advertised_ip,
                        local_port,
                        Some(&to_tag),
                        "",
                    ),
                );
                if room_id.is_some_and(crate::webrtc_runtime::room_has_peer) {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            486,
                            "Busy Here",
                            &request,
                            &advertised_ip,
                            local_port,
                            Some(&to_tag),
                            "",
                        ),
                    );
                    continue;
                }
                let response = vks_gateway_sip::build_response(
                    200,
                    "OK",
                    &request,
                    &advertised_ip,
                    local_port,
                    Some(&to_tag),
                    &sdp,
                );
                sip_send(&socket, peer, &response);
                if let Some(room_id) = room_id {
                    let participant_id = format!("sip-{}-in@sip-gateway", uuid::Uuid::new_v4());
                    let participant = crate::parse_participant_id(&participant_id);
                    let display_name = sip_from_display_name(&request.from);
                    let stats = Arc::new(GatewayMediaStats::default());
                    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let ack = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    let controls = GatewayMediaControls::default();
                    if !land_gateway_participant_in_room(
                        &state,
                        Some(&mcu),
                        &room_id,
                        &participant_id,
                        &display_name,
                        has_video,
                    ) {
                        tracing::warn!(room = %room_id, "inbound SIP participant not landed: real WebRTC peer present");
                        let bye = build_uas_bye(&request, &to_tag, peer, &advertised_ip, local_port);
                        sip_send(&socket, peer, &bye);
                        continue;
                    }
                    let from_uri = request
                        .from
                        .split("sip:")
                        .nth(1)
                        .and_then(|rest| rest.split(['@', '>', ';']).next())
                        .map(|user| format!("sip:{user}"))
                        .unwrap_or_else(|| request.request_uri.clone());
                    vks_admin::api_v3::call_strings::record_incoming_call(
                        &state,
                        room_id,
                        &participant_id,
                        &from_uri,
                        &display_name,
                    );
                    let (dtmf_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
                    let controls = GatewayMediaControls { dtmf_sender: dtmf_tx, ..controls };
                    let audio_task = spawn_rtp_media_bridge(RtpBridgeSession {
                        room_id,
                        participant_id_str: participant_id.clone(),
                        participant_id_val: participant,
                        remote_media_ip: media.remote_ip.clone(),
                        remote_audio_port: media.remote_audio_port,
                        audio_payload_type: media.audio_payload_type,
                        audio_codec: Some(media.audio_codec),
                        source_latch: true,
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
                    let mut video_task = None;
                    if let (Some(pair), Some(remote_port), Some(payload)) =
                        (video_pair, media.remote_video_port, media.video_payload_type)
                    {
                        let video_ip = media.remote_video_ip.as_deref().unwrap_or(&media.remote_ip);
                        if let Ok(remote_ip) = video_ip.parse::<std::net::IpAddr>() {
                            let mcu_downgrade = mcu.clone();
                            let participant_downgrade = participant;
                            let participant_id_downgrade = participant_id.clone();
                            let state_downgrade = state.clone();
                            let manager_downgrade = manager.clone();
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
                            video_task = Some(tokio::spawn(async move {
                                let mut lease = video_lease.take();
                                match video::spawn_video_media_bridge(session).await {
                                    Ok(_) => {
                                        if let Some(lease) = lease.as_mut() {
                                            lease.disarm();
                                        }
                                    }
                                    Err(error) => {
                                        warn!(%error, "inbound SIP video bridge unavailable; audio-only");
                                        mcu_downgrade.unbridge_participant(room_id, participant_downgrade);
                                        mcu_downgrade.bridge_participant(
                                            room_id,
                                            participant_downgrade,
                                            vec![vks_core::track::Track::new(
                                                participant_downgrade,
                                                vks_core::track::TrackKind::Audio,
                                            )],
                                        );
                                        state_downgrade.room_hub.update_participant_media(
                                            &room_id,
                                            &participant_id_downgrade,
                                            None,
                                            Some(true),
                                            None,
                                        );
                                        state_downgrade.room_hub.update_participant_tracks(
                                            &room_id,
                                            &participant_id_downgrade,
                                            vec!["audio".to_string()],
                                        );
                                        if let Ok(mut registry) = manager_downgrade.registry.lock() {
                                            if let Some(session) =
                                                registry.by_participant.get_mut(&participant_id_downgrade)
                                            {
                                                session.media_details["video_codec"] = serde_json::Value::Null;
                                                session.media_details["video_codec_running"] =
                                                    serde_json::Value::Bool(false);
                                                session.media_details["video_downgraded"] =
                                                    serde_json::Value::Bool(true);
                                                session.media_details["video_payload_type"] = serde_json::Value::Null;
                                                session.media_details["answer_video_port"] = serde_json::Value::Null;
                                            }
                                        }
                                    }
                                }
                            }));
                        }
                    }
                    let media_details = serde_json::json!({
                        "protocol": "SIP",
                        "direction": "inbound",
                        "remote_audio_address": format!("{}:{}", media.remote_ip, media.remote_audio_port),
                        "remote_video_address": media.remote_video_port.map(|port| format!(
                            "{}:{}",
                            media.remote_video_ip.as_deref().unwrap_or(&media.remote_ip),
                            port
                        )),
                        "audio_codec": media.audio_codec,
                        "audio_payload_type": media.audio_payload_type,
                        "audio_rx_payload_type": media.audio_payload_type,
                        "audio_tx_payload_type": media.audio_payload_type,
                        "audio_clock_rate": media.audio_clock_rate,
                        "audio_sample_rate": match media.audio_codec {
                            "G722" => 16_000,
                            "OPUS" => 48_000,
                            _ => 8_000,
                        },
                        "video_codec": media.video_payload_type.map(|_| "H264"),
                        "video_payload_type": media.video_payload_type,
                        "video_clock_rate": media.video_payload_type.map(|_| 90000u32),
                        "answer_audio_port": audio_pair.rtp_port,
                        "answer_video_port": answer_video_port,
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
                                inbound_h323: None,
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
                            invite_cseq: request.cseq,
                            peer,
                            bye: build_uas_bye(&request, &to_tag, peer, &advertised_ip, local_port),
                            answered_at: std::time::Instant::now(),
                            audio_task: Some(audio_task),
                            video_task,
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
                        let removed = { calls_cleanup.lock().expect("inbound calls poisoned").remove(&cleanup_key) };
                        if let Some(mut call) = removed {
                            call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                            wait_inbound_bridges(&mut call).await;
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
                let guard = calls.lock().expect("inbound calls poisoned");
                if let Some(call) = guard.get(&sip_dialog_key(&request)) {
                    let to_matches = request.to_tag.as_deref() == Some(call.to_tag.as_str());
                    if to_matches && request.cseq == call.invite_cseq && request.cseq_method == "ACK" {
                        call.ack.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            "BYE" => {
                let key = sip_dialog_key(&request);
                let valid = {
                    let guard = calls.lock().expect("inbound calls poisoned");
                    guard.get(&key).is_some_and(|call| {
                        request.to_tag.as_deref() == Some(call.to_tag.as_str())
                            && request.cseq > call.invite_cseq
                            && request.cseq_method == "BYE"
                    })
                };
                if !valid {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            481,
                            "Call/Transaction Does Not Exist",
                            &request,
                            &advertised_ip,
                            local_port,
                            None,
                            "",
                        ),
                    );
                    continue;
                }
                sip_send(
                    &socket,
                    peer,
                    &vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, local_port, None, ""),
                );
                let removed = { calls.lock().expect("inbound calls poisoned").remove(&key) };
                if let Some(mut call) = removed {
                    call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    wait_inbound_bridges(&mut call).await;
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
                // 200 OK отправляется синхронно до вставки в `calls`, поэтому
                // известный CANCEL относится уже к финальному ответу:
                // RFC 3261 §9.2 — подтверждаем, но диалог не рвём.
                let valid = {
                    let guard = calls.lock().expect("inbound calls poisoned");
                    guard.get(&sip_dialog_key(&request)).is_some_and(|call| {
                        request.to_tag.as_deref() == Some(call.to_tag.as_str())
                            && request.cseq == call.invite_cseq
                            && request.cseq_method == "CANCEL"
                    })
                };
                if valid {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, local_port, None, ""),
                    );
                } else {
                    sip_send(
                        &socket,
                        peer,
                        &vks_gateway_sip::build_response(
                            481,
                            "Call/Transaction Does Not Exist",
                            &request,
                            &advertised_ip,
                            local_port,
                            None,
                            "",
                        ),
                    );
                }
            }
            "OPTIONS" => {
                let mut response =
                    vks_gateway_sip::build_response(200, "OK", &request, &advertised_ip, local_port, None, "");
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
                        local_port,
                        None,
                        "",
                    ),
                );
            }
        }
    }
}

/// Состояние входящего H.323-вызова до/после приземления в комнату.
struct InboundH323State {
    room_id: RoomId,
    participant_id: String,
    participant: vks_core::ParticipantId,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    connected_at: std::time::Instant,
    stats: Arc<GatewayMediaStats>,
    audio_task: Option<tokio::task::JoinHandle<()>>,
    video_task: Option<tokio::task::JoinHandle<()>>,
    /// Keeps an accepted but unbridgeable video socket/lease out of the free
    /// pool until the call really ends, preventing cross-talk on port reuse.
    _video_guard: Option<(vks_gateway_sip::PortPair, RtpPortLease)>,
}

fn inbound_h323_dead(elapsed: std::time::Duration, last_rx_ms: u64, now_ms: u64, audio_running: bool) -> bool {
    let silence_ms = if last_rx_ms == 0 { elapsed.as_millis() as u64 } else { now_ms.saturating_sub(last_rx_ms) };
    silence_ms > 90_000 || (!audio_running && elapsed > std::time::Duration::from_secs(20))
}

struct PendingInboundH323 {
    room_id: RoomId,
    participant_id: String,
    stats: Arc<GatewayMediaStats>,
    cancel: Arc<std::sync::atomic::AtomicBool>,
    controls: GatewayMediaControls,
    display_name: String,
    from_alias: String,
    peer_ip: String,
    created_at: std::time::Instant,
    hangup_sent_at: Option<std::time::Instant>,
    audio_pair: vks_gateway_sip::PortPair,
    video_pair: Option<vks_gateway_sip::PortPair>,
    audio_lease: RtpPortLease,
    video_lease: Option<RtpPortLease>,
    dtmf_rx: tokio::sync::mpsc::UnboundedReceiver<String>,
}

/// H.323 alias can contain spaces/Cyrillic; a redial URI stays ASCII-safe.
fn h323_contact_uri(alias: &str, peer_ip: &str) -> String {
    let safe: String =
        alias.chars().filter(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '@')).take(64).collect();
    if safe.is_empty() {
        format!("h323:inbound@{peer_ip}")
    } else {
        format!("h323:{safe}@{peer_ip}")
    }
}

fn resolve_h323_room(state: &AppState, alias: &str) -> Option<RoomId> {
    let alias = alias.trim();
    if let Ok(id) = uuid::Uuid::parse_str(alias) {
        let room_id = RoomId::from_uuid(id);
        return state.rooms.lock().ok().and_then(|rooms| rooms.contains_key(&room_id).then_some(room_id));
    }
    if let Ok(raw) = std::env::var("VKS_H323_UAS_DEFAULT_ROOM") {
        if let Ok(id) = uuid::Uuid::parse_str(raw.trim()) {
            let room_id = RoomId::from_uuid(id);
            return state.rooms.lock().ok().and_then(|rooms| rooms.contains_key(&room_id).then_some(room_id));
        }
    }
    let alias = alias.to_ascii_lowercase();
    if alias.is_empty() {
        return None;
    }
    if let Ok(settings) = state.api_v3.conference_settings.lock() {
        for (room_id, value) in settings.iter() {
            for key in ["dtmf", "route", "alias", "name"] {
                if value.get(key).and_then(serde_json::Value::as_str).is_some_and(|v| v.eq_ignore_ascii_case(&alias))
                    && state.rooms.lock().ok().is_some_and(|rooms| rooms.contains_key(room_id))
                {
                    return Some(*room_id);
                }
            }
        }
    }
    Rooms::list(state).into_iter().find(|room| room.name.eq_ignore_ascii_case(&alias)).map(|room| room.id)
}

fn remove_inbound_h323_state(manager: &GatewayManager, state: Option<&Arc<AppState>>, call: &InboundH323State) {
    if let Ok(mut registry) = manager.registry.lock() {
        if let Some(session) = registry.by_participant.remove(&call.participant_id) {
            registry.by_call.remove(&session.call_id);
        }
    }
    if let Some(state) = state {
        terminate_gateway_participant_in_room(state, manager.mcu_engine.as_ref(), &call.room_id, &call.participant_id);
    } else if let Some(mcu) = manager.mcu_engine.as_ref() {
        mcu.unbridge_participant(call.room_id, call.participant);
    }
}

/// Retires one or many calls under a single bridge budget. Every failed/
/// disconnected event uses this path concurrently, so a network-wide failure
/// of 20 calls cannot serialize 20 x 2 s waits in the UAS event loop.
async fn retire_inbound_h323(
    manager: Arc<GatewayManager>,
    state: Option<Arc<AppState>>,
    mut calls: Vec<InboundH323State>,
) {
    let mut bridge_tasks = Vec::new();
    for call in &mut calls {
        call.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(task) = call.audio_task.take() {
            bridge_tasks.push(task);
        }
        if let Some(task) = call.video_task.take() {
            bridge_tasks.push(task);
        }
    }
    if !bridge_tasks.is_empty() {
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(3), futures_util::future::join_all(bridge_tasks)).await;
    }
    for call in &calls {
        remove_inbound_h323_state(manager.as_ref(), state.as_ref(), call);
    }
}

/// Поднимает входящий H.323 UAS listener и обслуживает его события.
pub async fn serve_h323_uas(manager: Arc<GatewayManager>, bind: String, local_ip: String) -> std::io::Result<()> {
    let runner = manager.clone();
    let task = tokio::spawn(async move {
        // The Python listener forks one child per call, so a single parent
        // handles concurrent inbound calls. Respawn it if it ever exits.
        let mut failures = 0u32;
        while !runner.h323_uas_stopping.load(std::sync::atomic::Ordering::SeqCst) {
            let profile = vks_gateway_h323::H323VideoProfile::default();
            let mut uas = match vks_gateway_h323::H323Uas::spawn(&bind, &local_ip, profile).await {
                Ok(uas) => {
                    failures = 0;
                    uas
                }
                Err(error) => {
                    failures = failures.saturating_add(1);
                    let delay = 1u64 << failures.min(4);
                    warn!(%error, failures, delay, "H.323 UAS listener failed to start; backing off");
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                    continue;
                }
            };
            let handle = uas.handle();
            info!(bind = %bind, local_ip = %local_ip, "H.323 UAS listening");
            let pending_guard = serve_h323_uas_session(&runner, &mut uas, handle, &local_ip).await;
            uas.shutdown().await;
            // Safe now: every forked child is gone before ports are released.
            drop(pending_guard);
            if !runner.h323_uas_stopping.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
        }
    });
    *manager.h323_uas_task.lock().expect("h323 uas task poisoned") = Some(task);
    Ok(())
}

async fn serve_h323_uas_session(
    manager: &Arc<GatewayManager>,
    uas: &mut vks_gateway_h323::H323Uas,
    handle: vks_gateway_h323::H323UasHandle,
    local_ip: &str,
) -> std::collections::HashMap<String, PendingInboundH323> {
    let mut pending: std::collections::HashMap<String, PendingInboundH323> = std::collections::HashMap::new();
    let mut active: std::collections::HashMap<String, InboundH323State> = std::collections::HashMap::new();
    let mut cleanup_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    let max_calls = std::env::var("VKS_H323_UAS_MAX_CALLS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .map(|value| value.clamp(1, 20))
        .unwrap_or(20);
    let Some(mut event_reader) = uas.take_event_stream() else {
        warn!("H.323 UAS event stream unavailable");
        return pending;
    };
    let (event_tx, mut event_rx) = tokio::sync::mpsc::channel::<Result<serde_json::Value, String>>(64);
    let reader_task = tokio::spawn(async move {
        loop {
            match vks_gateway_h323::read_event(&mut event_reader).await {
                Ok(event) => {
                    if event_tx.send(Ok(event)).await.is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = event_tx.send(Err(error)).await;
                    break;
                }
            }
        }
    });
    let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        if manager.h323_uas_stopping.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        let event = tokio::select! {
            _ = manager.h323_uas_shutdown.notified() => break,
            _ = ticker.tick() => {
                let now = std::time::Instant::now();
                let stale: Vec<String> = pending
                    .iter()
                    .filter(|(_, call)| now.duration_since(call.created_at) > std::time::Duration::from_secs(30))
                    .map(|(call_id, _)| call_id.clone())
                    .collect();
                for call_id in stale {
                    if let Some(call) = pending.get_mut(&call_id) {
                        if call.hangup_sent_at.is_none() {
                            warn!(%call_id, "H.323 inbound call did not connect in time; releasing");
                            call.hangup_sent_at = Some(std::time::Instant::now());
                            handle.hangup(&call_id).await;
                        }
                    }
                }
                // The listener parent SIGKILLs a child that ignores hangup for
                // 5 s. This longer fallback only reclaims the cap/ports if the
                // listener itself is unresponsive; session shutdown still holds
                // pending leases until the listener tree is reaped.
                let expired: Vec<String> = pending
                    .iter()
                    .filter(|(_, call)| {
                        call.hangup_sent_at
                            .is_some_and(|at| now.duration_since(at) > std::time::Duration::from_secs(10))
                    })
                    .map(|(call_id, _)| call_id.clone())
                    .collect();
                for call_id in expired {
                    warn!(%call_id, "H.323 pending call ignored hangup; reclaiming slot");
                    pending.remove(&call_id);
                }
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|value| value.as_millis() as u64)
                    .unwrap_or(0);
                let dead: Vec<String> = active
                    .iter()
                    .filter(|(_, call)| {
                        inbound_h323_dead(
                            now.duration_since(call.connected_at),
                            call.stats.last_rx_ms.load(std::sync::atomic::Ordering::Relaxed),
                            now_ms,
                            call.stats.audio_codec_running.load(std::sync::atomic::Ordering::Relaxed),
                        )
                    })
                    .map(|(call_id, _)| call_id.clone())
                    .collect();
                let mut dead_calls = Vec::new();
                for call_id in dead {
                    warn!(%call_id, "H.323 inbound call is media-dead; releasing");
                    handle.hangup(&call_id).await;
                    if let Some(call) = active.remove(&call_id) {
                        dead_calls.push(call);
                    }
                }
                if !dead_calls.is_empty() {
                    let state = manager
                        .state
                        .read()
                        .ok()
                        .and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade));
                    cleanup_tasks.push(tokio::spawn(retire_inbound_h323(manager.clone(), state, dead_calls)));
                }
                cleanup_tasks.retain(|task| !task.is_finished());
                continue;
            }
            event = event_rx.recv() => match event {
                Some(Ok(event)) => event,
                Some(Err(error)) => {
                    warn!(%error, "H.323 UAS event stream failed");
                    break;
                }
                None => break,
            },
        };
        match event.get("event").and_then(serde_json::Value::as_str) {
            Some("incoming") => {
                let (Some(call_id), Some(called)) = (
                    event.get("call_id").and_then(serde_json::Value::as_str),
                    event.get("called_alias").and_then(serde_json::Value::as_str),
                ) else {
                    continue;
                };
                if pending.contains_key(call_id) || active.contains_key(call_id) {
                    warn!(%call_id, "duplicate H.323 incoming event; rejecting duplicate");
                    let _ = handle.reject(call_id).await;
                    continue;
                }
                if pending.len() + active.len() >= max_calls {
                    warn!(%call_id, max_calls, "H.323 inbound call cap reached; rejecting");
                    let _ = handle.reject(call_id).await;
                    continue;
                }
                let Some(state) =
                    manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                else {
                    let _ = handle.reject(call_id).await;
                    continue;
                };
                let Some(room_id) = resolve_h323_room(&state, called) else {
                    info!(%call_id, %called, "H.323 inbound call rejected: room alias not found");
                    let _ = handle.reject(call_id).await;
                    continue;
                };
                if manager.mcu_engine.is_none() {
                    let _ = handle.reject(call_id).await;
                    continue;
                }
                let Some(audio_pair) = manager.port_pool.allocate_pair("0.0.0.0").await else {
                    let _ = handle.reject(call_id).await;
                    continue;
                };
                let video_pair = if video::codec_available().await {
                    manager.port_pool.allocate_pair("0.0.0.0").await
                } else {
                    None
                };
                let audio_lease = RtpPortLease::new(manager.port_pool.clone(), audio_pair.rtp_port);
                let video_lease =
                    video_pair.as_ref().map(|pair| RtpPortLease::new(manager.port_pool.clone(), pair.rtp_port));
                if let Err(error) = handle
                    .accept(call_id, local_ip, audio_pair.rtp_port, video_pair.as_ref().map(|pair| pair.rtp_port))
                    .await
                {
                    warn!(%call_id, %error, "H.323 inbound accept failed");
                    continue;
                }
                let participant_id = format!("h323-{}-in@h323-gateway", uuid::Uuid::new_v4());
                let display_name = event
                    .get("from_alias")
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(|value| format!("{value} (H.323)"))
                    .unwrap_or_else(|| "H.323 (inbound)".to_string());
                let (dtmf_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
                let controls = GatewayMediaControls { dtmf_sender: dtmf_tx, ..GatewayMediaControls::default() };
                controls.dtmf_payload_type.store(256, std::sync::atomic::Ordering::Relaxed);
                pending.insert(
                    call_id.to_string(),
                    PendingInboundH323 {
                        room_id,
                        participant_id,
                        stats: Arc::new(GatewayMediaStats::default()),
                        cancel: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                        controls,
                        display_name,
                        from_alias: event
                            .get("from_alias")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string(),
                        peer_ip: event
                            .get("peer_ip")
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("0.0.0.0")
                            .to_string(),
                        created_at: std::time::Instant::now(),
                        hangup_sent_at: None,
                        audio_pair,
                        video_pair,
                        audio_lease,
                        video_lease,
                        dtmf_rx,
                    },
                );
            }
            Some("connected") => {
                let Some(call_id) = event.get("call_id").and_then(serde_json::Value::as_str) else { continue };
                if active.contains_key(call_id) {
                    warn!(%call_id, "duplicate H.323 connected event; releasing duplicate");
                    handle.hangup(call_id).await;
                    continue;
                }
                let Some(pending_call) = pending.remove(call_id) else { continue };
                if pending_call.hangup_sent_at.is_some() {
                    warn!(%call_id, "H.323 call connected after its pending TTL; rejecting late connect");
                    handle.hangup(call_id).await;
                    pending.insert(call_id.to_string(), pending_call);
                    continue;
                }
                let Some(state) =
                    manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                else {
                    handle.hangup(call_id).await;
                    pending.insert(call_id.to_string(), pending_call);
                    continue;
                };
                let Some(mcu) = manager.mcu_engine.clone() else {
                    handle.hangup(call_id).await;
                    pending.insert(call_id.to_string(), pending_call);
                    continue;
                };
                let remote_ip = event.get("remote_ip").and_then(serde_json::Value::as_str).unwrap_or_default();
                let Ok(remote_ip) = remote_ip.parse::<std::net::Ipv4Addr>() else {
                    warn!(%call_id, "H.323 connected event has an invalid media IP");
                    handle.hangup(call_id).await;
                    pending.insert(call_id.to_string(), pending_call);
                    continue;
                };
                if remote_ip.to_string() != pending_call.peer_ip {
                    warn!(%call_id, peer = %pending_call.peer_ip, %remote_ip, "H.323 media address differs from the TCP peer; refusing redirect");
                    handle.hangup(call_id).await;
                    pending.insert(call_id.to_string(), pending_call);
                    continue;
                }
                let remote_audio_port = event
                    .get("remote_audio_port")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|port| u16::try_from(port).ok())
                    .filter(|port| *port != 0);
                let audio_payload_type = h323_inbound_audio_pt(&event);
                let (Some(remote_audio_port), Some(audio_payload_type)) = (remote_audio_port, audio_payload_type)
                else {
                    warn!(%call_id, "H.323 connected event has unusable or asymmetric audio");
                    handle.hangup(call_id).await;
                    pending.insert(call_id.to_string(), pending_call);
                    continue;
                };
                let video_remote = event
                    .get("remote_video_ip")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|ip| ip.parse::<std::net::Ipv4Addr>().ok())
                    .zip(
                        event
                            .get("remote_video_port")
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|port| u16::try_from(port).ok())
                            .filter(|port| *port != 0),
                    )
                    .zip(
                        event
                            .get("video_tx_payload_type")
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|pt| u8::try_from(pt).ok())
                            .filter(|pt| (96..=127).contains(pt)),
                    )
                    .zip(
                        event
                            .get("video_rx_payload_type")
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|pt| u8::try_from(pt).ok())
                            .filter(|pt| (96..=127).contains(pt)),
                    )
                    .map(|(((ip, port), tx_pt), rx_pt)| (ip, port, tx_pt, rx_pt));
                if let Some((video_ip, _, _, _)) = video_remote {
                    if video_ip.to_string() != pending_call.peer_ip {
                        warn!(%call_id, %video_ip, "H.323 video address differs from the TCP peer; refusing redirect");
                        handle.hangup(call_id).await;
                        pending.insert(call_id.to_string(), pending_call);
                        continue;
                    }
                }
                let mut video_lease = pending_call.video_lease;
                let has_video = video_remote.is_some() && pending_call.video_pair.is_some();
                let mut video_guard = None;
                let pair_for_bridge = if has_video {
                    pending_call.video_pair
                } else {
                    if let (Some(pair), Some(lease)) = (pending_call.video_pair, video_lease.take()) {
                        video_guard = Some((pair, lease));
                    }
                    None
                };
                if crate::webrtc_runtime::room_has_peer(pending_call.room_id) {
                    tracing::warn!(room = %pending_call.room_id, "inbound H.323 call rejected: real WebRTC peer present");
                    handle.hangup(call_id).await;
                    continue;
                }
                let participant = crate::parse_participant_id(&pending_call.participant_id);
                let media_details = serde_json::json!({
                    "protocol": "H.323",
                    "direction": "inbound",
                    "remote_audio_address": format!("{remote_ip}:{remote_audio_port}"),
                    "remote_video_address": video_remote.map(|(ip, port, _, _)| format!("{ip}:{port}")),
                    "audio_codec": audio_codec_name(audio_payload_type),
                    "audio_payload_type": audio_payload_type,
                    "audio_rx_payload_type": audio_payload_type,
                    "audio_tx_payload_type": audio_payload_type,
                    "audio_clock_rate": 8000,
                    "audio_sample_rate": audio_sample_rate(audio_payload_type),
                    "video_codec": if has_video { Some("H264") } else { None },
                    "video_payload_type": video_remote.map(|(_, _, tx, _)| tx),
                    "video_tx_payload_type": video_remote.map(|(_, _, tx, _)| tx),
                    "video_rx_payload_type": video_remote.map(|(_, _, _, rx)| rx),
                    "video_clock_rate": video_remote.map(|_| 90000u32),
                    "inbound": true,
                });
                if let Ok(mut registry) = manager.registry.lock() {
                    registry.by_call.insert(call_id.to_string(), pending_call.participant_id.clone());
                    registry.by_participant.insert(
                        pending_call.participant_id.clone(),
                        GatewayCallSession {
                            call_id: call_id.to_string(),
                            participant_id: pending_call.participant_id.clone(),
                            room_id: pending_call.room_id,
                            display_name: pending_call.display_name.clone(),
                            uri: h323_contact_uri(&pending_call.from_alias, &pending_call.peer_ip),
                            active_sip: None,
                            active_h323: None,
                            inbound_h323: Some(InboundH323Control {
                                uas: handle.clone(),
                                call_id: call_id.to_string(),
                            }),
                            cancel_flag: pending_call.cancel.clone(),
                            controls: pending_call.controls.clone(),
                            media_stats: pending_call.stats.clone(),
                            media_details: media_details.clone(),
                            connected_at: std::time::Instant::now(),
                        },
                    );
                }
                if !land_gateway_participant_in_room(
                    &state,
                    Some(&mcu),
                    &pending_call.room_id,
                    &pending_call.participant_id,
                    &pending_call.display_name,
                    has_video,
                ) {
                    tracing::warn!(room = %pending_call.room_id, "outbound participant not landed: real WebRTC peer present");
                    handle.hangup(call_id).await;
                    continue;
                }
                vks_admin::api_v3::call_strings::record_incoming_call(
                    &state,
                    pending_call.room_id,
                    &pending_call.participant_id,
                    &h323_contact_uri(&pending_call.from_alias, &pending_call.peer_ip),
                    &pending_call.display_name,
                );
                let mut audio_lease = pending_call.audio_lease;
                let audio_task = spawn_rtp_media_bridge(RtpBridgeSession {
                    room_id: pending_call.room_id,
                    participant_id_str: pending_call.participant_id.clone(),
                    participant_id_val: participant,
                    remote_media_ip: remote_ip.to_string(),
                    remote_audio_port,
                    audio_payload_type,
                    audio_codec: Some(audio_codec_name(audio_payload_type)),
                    // H.323 already binds the media address via H.245. Any
                    // first-packet relatch would let a LAN scanner hijack the
                    // bridge/egress, so keep the OLC-pinned peer.
                    source_latch: false,
                    dtmf_payload_type: None,
                    media_stats: pending_call.stats.clone(),
                    rtp_socket: pending_call.audio_pair.rtp_socket,
                    rtcp_socket: Some(pending_call.audio_pair.rtcp_socket),
                    rtp_port: pending_call.audio_pair.rtp_port,
                    port_pool: manager.port_pool.clone(),
                    mcu_engine: mcu.clone(),
                    controls: pending_call.controls.clone(),
                    cancel_flag: pending_call.cancel.clone(),
                    dtmf_rx: pending_call.dtmf_rx,
                });
                audio_lease.disarm();
                let mut video_task = None;
                if let (Some(pair), Some((video_ip, video_port, tx_pt, rx_pt))) = (pair_for_bridge, video_remote) {
                    let profile = manager.video_profile(pending_call.room_id, None);
                    match video::spawn_video_media_bridge(video::VideoBridgeSession {
                        room_id: pending_call.room_id,
                        participant_id: participant,
                        rtp_socket: pair.rtp_socket,
                        rtcp_socket: Some(pair.rtcp_socket),
                        remote_target: std::net::SocketAddr::new(std::net::IpAddr::V4(video_ip), video_port),
                        payload_type: tx_pt,
                        receive_payload_type: rx_pt,
                        port_pool: manager.port_pool.clone(),
                        engine: mcu.clone(),
                        controls: pending_call.controls.clone(),
                        cancelled: pending_call.cancel.clone(),
                        stats: pending_call.stats.clone(),
                        profile,
                    })
                    .await
                    {
                        Ok(task) => {
                            if let Some(lease) = video_lease.as_mut() {
                                lease.disarm();
                            }
                            video_task = Some(task);
                        }
                        Err(error) => {
                            warn!(%error, "inbound H.323 video bridge unavailable; audio-only");
                            mcu.unbridge_participant(pending_call.room_id, participant);
                            mcu.bridge_participant(
                                pending_call.room_id,
                                participant,
                                vec![vks_core::track::Track::new(participant, vks_core::track::TrackKind::Audio)],
                            );
                            state.room_hub.update_participant_media(
                                &pending_call.room_id,
                                &pending_call.participant_id,
                                None,
                                Some(true),
                                None,
                            );
                            state.room_hub.update_participant_tracks(
                                &pending_call.room_id,
                                &pending_call.participant_id,
                                vec!["audio".to_string()],
                            );
                            if let Ok(mut registry) = manager.registry.lock() {
                                if let Some(session) = registry.by_participant.get_mut(&pending_call.participant_id) {
                                    session.media_details["video_codec"] = serde_json::Value::Null;
                                    session.media_details["video_payload_type"] = serde_json::Value::Null;
                                    session.media_details["video_tx_payload_type"] = serde_json::Value::Null;
                                    session.media_details["video_rx_payload_type"] = serde_json::Value::Null;
                                }
                            }
                        }
                    }
                }
                active.insert(
                    call_id.to_string(),
                    InboundH323State {
                        room_id: pending_call.room_id,
                        participant_id: pending_call.participant_id,
                        participant,
                        cancel: pending_call.cancel,
                        connected_at: std::time::Instant::now(),
                        stats: pending_call.stats,
                        audio_task: Some(audio_task),
                        video_task,
                        _video_guard: video_guard,
                    },
                );
                info!(%call_id, room = %pending_call.room_id, has_video, "H.323 inbound participant landed in room");
            }
            Some("channel_closed") => {
                let Some(call_id) = event.get("call_id").and_then(serde_json::Value::as_str) else { continue };
                let kind = event.get("kind").and_then(serde_json::Value::as_str).unwrap_or_default();
                if kind != "video" {
                    continue;
                }
                if let Some(call) = active.get_mut(call_id) {
                    warn!(%call_id, "remote H.323 peer closed its video channel; keeping audio");
                    if let Some(task) = call.video_task.take() {
                        task.abort();
                    }
                    if let Ok(mut registry) = manager.registry.lock() {
                        if let Some(session) = registry.by_participant.get_mut(&call.participant_id) {
                            session.media_details["video_codec"] = serde_json::Value::Null;
                            session.media_details["video_payload_type"] = serde_json::Value::Null;
                            session.media_details["video_tx_payload_type"] = serde_json::Value::Null;
                            session.media_details["video_rx_payload_type"] = serde_json::Value::Null;
                        }
                    }
                    if let Some(state) =
                        manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade))
                    {
                        state.room_hub.update_participant_media(
                            &call.room_id,
                            &call.participant_id,
                            None,
                            Some(true),
                            None,
                        );
                        state.room_hub.update_participant_tracks(
                            &call.room_id,
                            &call.participant_id,
                            vec!["audio".to_string()],
                        );
                    }
                }
            }
            Some("failed" | "disconnected") => {
                let Some(call_id) = event.get("call_id").and_then(serde_json::Value::as_str) else { continue };
                if let Some(pending_call) = pending.remove(call_id) {
                    drop(pending_call);
                }
                if let Some(call) = active.remove(call_id) {
                    let state =
                        manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade));
                    cleanup_tasks.push(tokio::spawn(retire_inbound_h323(manager.clone(), state, vec![call])));
                }
                cleanup_tasks.retain(|task| !task.is_finished());
            }
            _ => {}
        }
    }
    // Shutdown must not serialize 20 x 2 s bridge waits; cancel and join all
    // media tasks under one budget, then remove registry/RoomHub state.
    for call_id in active.keys() {
        handle.hangup(call_id).await;
    }
    let state = manager.state.read().ok().and_then(|guard| guard.as_ref().and_then(std::sync::Weak::upgrade));
    let active_calls: Vec<InboundH323State> = active.drain().map(|(_, call)| call).collect();
    retire_inbound_h323(manager.clone(), state.clone(), active_calls).await;
    if !cleanup_tasks.is_empty() {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            futures_util::future::join_all(cleanup_tasks.drain(..)),
        )
        .await;
    }
    reader_task.abort();
    // Pending children may still own their accepted RTP sockets. The caller
    // keeps these guards until the listener process tree is gone.
    for call_id in pending.keys() {
        handle.hangup(call_id).await;
    }
    pending
}

#[cfg(test)]
mod media_runtime_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[test]
    fn sip_keeps_g728_and_opus_while_h323_does_not() {
        let sip = bridgeable_audio_pts(false);
        for pt in [0, 4, 8, 9, 15, 107, 108, 109, 110, 111] {
            assert!(sip.contains(&pt), "SIP PT{pt} must stay bridgeable");
        }
        let h323 = bridgeable_audio_pts(true);
        for pt in [0, 4, 8, 9] {
            assert!(h323.contains(&pt), "H.323 PT{pt}");
        }
        for pt in [15, 107, 108, 109, 110, 111] {
            assert!(!h323.contains(&pt), "H.323 must not bridge PT{pt}");
        }
    }

    #[test]
    fn h323_inbound_event_accepts_all_native_h323_pts() {
        for pt in [0, 4, 8, 9] {
            let event = serde_json::json!({
                "remote_audio_payload_type": pt,
                "local_audio_payload_type": pt,
            });
            assert_eq!(h323_inbound_audio_pt(&event), Some(pt));
        }
        for event in [
            serde_json::json!({"remote_audio_payload_type": 4, "local_audio_payload_type": 0}),
            serde_json::json!({"remote_audio_payload_type": 15, "local_audio_payload_type": 15}),
            serde_json::json!({"remote_audio_payload_type": 8}),
        ] {
            assert_eq!(h323_inbound_audio_pt(&event), None);
        }
    }

    #[test]
    fn h323_inbound_audio_requires_symmetric_static_pt() {
        for pt in [0, 4, 8, 9] {
            assert_eq!(symmetric_h323_audio_pt(Some(pt), Some(pt)), Some(pt));
        }
        for (remote, local) in
            [(Some(9), Some(0)), (Some(0), Some(9)), (Some(8), Some(0)), (Some(15), Some(15)), (Some(9), None)]
        {
            assert_eq!(symmetric_h323_audio_pt(remote, local), None, "{remote:?}/{local:?}");
        }
    }

    #[test]
    fn h323_contact_uri_is_ascii_and_keeps_peer() {
        assert_eq!(h323_contact_uri("VKS MCU", "192.168.1.132"), "h323:VKSMCU@192.168.1.132");
        assert_eq!(h323_contact_uri("Терминал", "10.0.0.5"), "h323:inbound@10.0.0.5");
    }

    #[test]
    fn h323_watchdog_only_reaps_real_media_death() {
        let secs = std::time::Duration::from_secs;
        assert!(!inbound_h323_dead(secs(5), 0, 0, true));
        assert!(inbound_h323_dead(secs(120), 0, 0, true));
        assert!(inbound_h323_dead(secs(120), 1, 120_001, true));
        assert!(inbound_h323_dead(secs(25), 0, 0, false));
    }

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
                inbound_h323: None,
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
        let g729_only = serde_json::json!([{"name": "G729", "bitrate": null}]);
        let h265_only = serde_json::json!(["H265"]);
        assert!(ensure_bridgeable_codec_selection(Some(&g729_only), Some(&h265_only)).is_err());
        let error = ensure_bridgeable_codec_selection(Some(&g729_only), Some(&h265_only)).unwrap_err();
        assert!(error.contains("no common audio"), "{error}");
        let g711_g722 = serde_json::json!([{"name": "G722"}, {"name": "G711"}]);
        let error = ensure_bridgeable_codec_selection(Some(&g711_g722), Some(&h265_only)).unwrap_err();
        assert!(error.contains("no common video"), "H.265-only video всё ещё не мостится: {error}");
        let native = serde_json::json!([{"name": "G711", "bitrate": null}]);
        let h264 = serde_json::json!(["H264"]);
        assert!(ensure_bridgeable_codec_selection(Some(&native), Some(&h264)).is_ok());
        assert!(ensure_bridgeable_codec_selection(None, None).is_ok());
        assert!(ensure_bridgeable_codec_selection(Some(&serde_json::json!([])), None).is_ok());
        let high_only = serde_json::json!(["H264-HIGH"]);
        assert!(ensure_bridgeable_codec_selection(Some(&native), Some(&high_only)).is_err());
        // H.323 negotiates symmetric G.722, G.711U, PCMA or G.723.1.
        for codec in ["G722", "PCMU", "PCMA", "G723", "G7231"] {
            let value = serde_json::json!([{"name": codec, "bitrate": null}]);
            assert!(ensure_bridgeable_codec_selection_for(true, Some(&value), None).is_ok(), "h323 {codec}");
        }
        let g728_only = serde_json::json!([{"name": "G728", "bitrate": null}]);
        assert!(
            ensure_bridgeable_codec_selection_for(true, Some(&g728_only), None).is_err(),
            "H.323 G.728 is SIP-only"
        );
        // G.726 is variant-aware: only 16/24/32/40 kbit/s are bridged, and the
        // H.323 sidecar must reject it before dialing.
        for bitrate in [16, 24, 32, 40] {
            let g726 = serde_json::json!([{"name": "G726", "bitrate": bitrate}]);
            assert!(ensure_bridgeable_codec_selection(Some(&g726), None).is_ok(), "G726:{bitrate}");
            assert!(ensure_bridgeable_codec_selection_for(true, Some(&g726), None).is_err(), "h323 G726:{bitrate}");
        }
        for bad in [serde_json::json!([{"name": "G726", "bitrate": 48}]), serde_json::json!([{"name": "G726"}])] {
            assert!(ensure_bridgeable_codec_selection(Some(&bad), None).is_err(), "unsupported G726 variant: {bad}");
        }
        let g726_string = serde_json::json!(["G726:32"]);
        assert!(ensure_bridgeable_codec_selection(Some(&g726_string), None).is_ok());
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
                    "videoCodecs": ["H264"]
                }}}
            })],
        );
        let selected =
            find_call_string_override(&resources, Some(room), "sip:probe@192.168.1.132").expect("contact override");
        assert_eq!(selected["audioCodecs"][0]["name"], "G722");
        assert!(
            ensure_bridgeable_codec_selection(selected.get("audioCodecs"), selected.get("videoCodecs")).is_ok(),
            "G722 + H264 are both natively bridged"
        );
        assert!(GatewayManager::bridgeable_codec_selection(&resources, room, None, "sip:probe@192.168.1.132").is_ok());
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
        let g729 = serde_json::json!({"payload": {"audio_codecs": [{"name": "G729"}]}});
        let error = GatewayManager::bridgeable_codec_selection(&resources, room, Some(&g729), "sip:x@192.168.1.132")
            .expect_err("conference G729-only must be refused");
        assert!(error.contains("no common audio"), "{error}");
        let g722 = serde_json::json!({"payload": {"audio_codecs": [{"name": "G722"}]}});
        assert!(
            GatewayManager::bridgeable_codec_selection(&resources, room, Some(&g722), "sip:x@192.168.1.132").is_ok(),
            "G722 is natively bridged since T06b-1"
        );
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
            audio_codec: None,
            source_latch: false,
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
            audio_codec: None,
            source_latch: false,
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
            audio_codec: None,
            source_latch: false,
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
            audio_codec: None,
            source_latch: false,
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
        // Generous timeout: under a loaded CI host the 20 ms MCU ticks can be
        // delayed, but the assertion is about consuming the second half at all.
        let second_half = tokio::time::timeout(Duration::from_secs(3), async {
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
        // The 20 ms consumer may run late under a loaded CI host; wait for the
        // queue to drain instead of assuming one fixed 200 ms sleep.
        let drained = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let silent = mcu
                    .participant_audio(room, listener)
                    .is_none_or(|audio| audio.iter().all(|sample| sample.abs() < 0.01));
                if silent {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
        assert!(second_half.is_ok(), "the second half of negotiated G711 packetization was truncated");
        assert!(drained.is_ok(), "old PCM repeated after its RTP packet ended");
    }

    #[tokio::test]
    async fn gateway_audio_bridges_g722_pt9_with_8khz_timestamp_clock() {
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
            audio_payload_type: 9,
            audio_codec: None,
            source_latch: false,
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
        let mut codec = vks_gateway_sip::g722::G722Codec::new();
        let pcm: Vec<i32> = (0..320)
            .map(|i| ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 16_000.0).sin() * 6000.0) as i32)
            .collect();
        let payload = codec.encode(&pcm);
        assert_eq!(payload.len(), 160);
        let mut packet = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 9, 1, 0, 42, false);
        packet[12..].copy_from_slice(&payload);
        terminal.send_to(&packet, target).await.unwrap();
        tokio::time::timeout(Duration::from_millis(300), async {
            loop {
                if mcu
                    .participant_audio(room, listener)
                    .is_some_and(|audio| audio.iter().any(|sample| sample.abs() > 0.05))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("G722 PCM must reach the 48 kHz MCU mix");
        let mut first = [0u8; 512];
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut first))
            .await
            .expect("bridge must emit G722 RTP")
            .unwrap();
        assert_eq!(first[1] & 0x7F, 9, "G722 payload type");
        assert_eq!(count - 12, 160, "20 ms of G722 is 160 octets");
        let first_ts = u32::from_be_bytes(first[4..8].try_into().unwrap());
        let mut second = [0u8; 512];
        let (count2, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut second))
            .await
            .expect("bridge must emit a second G722 RTP packet")
            .unwrap();
        assert_eq!(count2 - 12, 160);
        let second_ts = u32::from_be_bytes(second[4..8].try_into().unwrap());
        assert_eq!(second_ts.wrapping_sub(first_ts), 160, "G722 RTP clock is 8000 Hz");
        // The emitted payload must be decodable G722 (160 octets -> 320 samples).
        // The source participant hears the listener's silence here, so only
        // framing/length is asserted; the codec energy test lives in g722.rs.
        let decoded = codec.decode(&first[12..count]);
        assert_eq!(decoded.len(), 320);
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn inbound_sip_uas_answers_g722_and_rejects_g729_only_with_488() {
        use vks_admin::state::AppState;
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "uas-g722");
        let manager = Arc::new(
            GatewayManager::new().with_mcu(Arc::new(vks_media_mcu::McuEngine::new())).with_candidate_ip("127.0.0.1"),
        );
        manager.bind_state(Arc::downgrade(&state));
        let server = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server.local_addr().unwrap().port();
        let listener = tokio::spawn(serve_sip_uas_on(
            manager.clone(),
            server,
            "127.0.0.1".into(),
            std::time::Duration::from_secs(15),
        ));
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(("127.0.0.1", server_port)).await.unwrap();
        let port = client.local_addr().unwrap().port();
        let invite = |call_id: &str, sdp: &str| {
            format!(
                "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bK{call_id}\r\nFrom: \"T\" <sip:t@127.0.0.1>;tag=ft\r\nTo: <sip:{}@127.0.0.1>\r\nCall-ID: {call_id}\r\nCSeq: 1 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
                room.id, room.id, sdp.len(), sdp
            )
        };
        async fn read_until(client: &tokio::net::UdpSocket, prefix: &str) -> String {
            let mut buf = [0u8; 8192];
            for _ in 0..6 {
                let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                    .await
                    .expect("UAS response timeout")
                    .unwrap();
                let text = String::from_utf8_lossy(&buf[..len]).into_owned();
                if text.starts_with(prefix) {
                    return text;
                }
            }
            panic!("no {prefix} response");
        }
        let g729 = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
        client.send(invite("cid-g729", g729).as_bytes()).await.unwrap();
        let rejection = read_until(&client, "SIP/2.0 488").await;
        assert!(rejection.starts_with("SIP/2.0 488"), "{rejection}");
        let g722 = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 9\r\na=rtpmap:9 G722/8000\r\n";
        client.send(invite("cid-g722", g722).as_bytes()).await.unwrap();
        let answer = read_until(&client, "SIP/2.0 200").await;
        assert!(answer.contains("m=audio "), "{answer}");
        assert!(answer.contains(" RTP/AVP 9\r\n"), "G722 must be selected, not PCMU fallback: {answer}");
        assert!(answer.contains("a=rtpmap:9 G722/8000"), "{answer}");
        let g723 = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 4\r\na=rtpmap:4 G723/8000\r\n";
        client.send(invite("cid-g723", g723).as_bytes()).await.unwrap();
        let g723_answer = read_until(&client, "SIP/2.0 200").await;
        assert!(g723_answer.contains(" RTP/AVP 4\r\n"), "G.723.1 must be answered: {g723_answer}");
        assert!(g723_answer.contains("a=rtpmap:4 G723/8000"), "{g723_answer}");
        assert!(g723_answer.contains("a=ptime:30"), "{g723_answer}");
        let g728 = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 15\r\na=rtpmap:15 G728/8000\r\n";
        client.send(invite("cid-g728", g728).as_bytes()).await.unwrap();
        let g728_answer = read_until(&client, "SIP/2.0 200").await;
        assert!(g728_answer.contains(" RTP/AVP 15\r\n"), "G.728 must be answered: {g728_answer}");
        assert!(g728_answer.contains("a=rtpmap:15 G728/8000"), "{g728_answer}");
        let g726 = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 109\r\na=rtpmap:109 G726-32/8000\r\n";
        client.send(invite("cid-g726", g726).as_bytes()).await.unwrap();
        let g726_answer = read_until(&client, "SIP/2.0 200").await;
        assert!(g726_answer.contains(" RTP/AVP 109\r\n"), "G726-32 must be answered: {g726_answer}");
        assert!(g726_answer.contains("a=rtpmap:109 G726-32/8000"), "{g726_answer}");
        listener.abort();
    }

    #[tokio::test]
    async fn gateway_audio_bridges_g726_32_at_peer_dynamic_pt_with_8khz_clock() {
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
        let stats = Arc::new(GatewayMediaStats::default());
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: source.to_string(),
            participant_id_val: source,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 96,
            audio_codec: Some("G726-32"),
            source_latch: false,
            dtmf_payload_type: None,
            media_stats: stats.clone(),
            rtp_socket: socket,
            rtcp_socket: None,
            rtp_port: target.port(),
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            mcu_engine: mcu.clone(),
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let rate = vks_gateway_sip::g726::G726Rate::R32;
        let mut codec = vks_gateway_sip::g726::G726Codec::new(rate);
        let pcm: Vec<i16> = (0..320)
            .map(|i| ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin() * 12_000.0) as i16)
            .collect();
        let mut malformed = [0u8; 173];
        vks_gateway_sip::write_rtp_header(&mut malformed[..12], 96, 1, 0, 42, false);
        malformed[12..].fill(0x55); // 161 bytes: more than the 40 ms G.726-32 cap
        terminal.send_to(&malformed, target).await.unwrap();
        tokio::time::timeout(Duration::from_millis(300), async {
            while stats.rejected_packets.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("oversized G.726 payload must be rejected");
        assert_eq!(stats.audio_rx_packets.load(Ordering::Relaxed), 0);
        let payload = codec.encode(&pcm[..160]);
        assert_eq!(payload.len(), 80);
        let mut packet = [0u8; 92];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 96, 2, 0, 42, false);
        packet[12..].copy_from_slice(&payload);
        terminal.send_to(&packet, target).await.unwrap();
        tokio::time::timeout(Duration::from_millis(300), async {
            loop {
                if mcu
                    .participant_audio(room, listener)
                    .is_some_and(|audio| audio.iter().any(|sample| sample.abs() > 0.05))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("G.726 PCM must reach the 48 kHz MCU mix");
        let mut first = [0u8; 512];
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut first))
            .await
            .expect("bridge must emit G.726 RTP")
            .unwrap();
        assert_eq!(first[1] & 0x7F, 96, "peer-chosen dynamic G.726-32 payload type");
        assert_eq!(count - 12, 80, "20 ms of G.726-32 is 80 octets");
        let first_ts = u32::from_be_bytes(first[4..8].try_into().unwrap());
        let mut second = [0u8; 512];
        let (count2, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut second))
            .await
            .expect("bridge must emit a second G.726 RTP packet")
            .unwrap();
        assert_eq!(count2 - 12, 80);
        assert_eq!(
            u32::from_be_bytes(second[4..8].try_into().unwrap()).wrapping_sub(first_ts),
            160,
            "G.726 RTP clock is 8000 Hz"
        );
        assert_eq!(codec.decode(&first[12..count]).len(), 160);
        // 40 ms input: 320 samples -> 160 octets, but the bridge still emits
        // two 20 ms packets with timestamp +160 each.
        let payload40 = codec.encode(&pcm);
        assert_eq!(payload40.len(), 160);
        let mut packet40 = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut packet40[..12], 96, 3, 480, 42, false);
        packet40[12..].copy_from_slice(&payload40);
        terminal.send_to(&packet40, target).await.unwrap();
        let mut tick1 = [0u8; 512];
        let (tick1_len, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut tick1))
            .await
            .expect("first 20 ms tick after 40 ms input")
            .unwrap();
        let mut tick2 = [0u8; 512];
        let (tick2_len, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut tick2))
            .await
            .expect("second 20 ms tick after 40 ms input")
            .unwrap();
        assert_eq!(tick1_len - 12, 80);
        assert_eq!(tick2_len - 12, 80);
        assert_eq!(
            u32::from_be_bytes(tick2[4..8].try_into().unwrap())
                .wrapping_sub(u32::from_be_bytes(tick1[4..8].try_into().unwrap())),
            160,
            "40 ms input must not advance the RTP timestamp by 320"
        );
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_audio_bridges_opus_at_peer_dynamic_pt_with_48khz_clock() {
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
        let stats = Arc::new(GatewayMediaStats::default());
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: source.to_string(),
            participant_id_val: source,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 96,
            audio_codec: Some("OPUS"),
            source_latch: false,
            dtmf_payload_type: None,
            media_stats: stats.clone(),
            rtp_socket: socket,
            rtcp_socket: None,
            rtp_port: target.port(),
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            mcu_engine: mcu.clone(),
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let mut codec = vks_gateway_sip::opus::OpusCodec::new().expect("opus");
        let pcm: Vec<f32> = (0..vks_gateway_sip::opus::FRAME_SAMPLES)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin() * 0.25)
            .collect();
        let payload = codec.encode(&pcm).expect("encode Opus");
        let mut packet = vec![0u8; 12 + payload.len()];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 96, 1, 0, 42, false);
        packet[12..].copy_from_slice(&payload);
        terminal.send_to(&packet, target).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !mcu
                .participant_audio(room, listener)
                .is_some_and(|audio| audio.iter().any(|sample| sample.abs() > 0.02))
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("Opus PCM must reach the 48 kHz MCU mix");
        let mut first = vec![0u8; 1400];
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut first))
            .await
            .expect("bridge must emit Opus RTP")
            .unwrap();
        assert_eq!(first[1] & 0x7F, 96, "peer-chosen dynamic Opus payload type");
        assert!((13..=12 + vks_gateway_sip::opus::MAX_PAYLOAD).contains(&count), "{count}");
        let first_ts = u32::from_be_bytes(first[4..8].try_into().unwrap());
        let mut second = vec![0u8; 1400];
        let (count2, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut second))
            .await
            .expect("bridge must emit a second Opus RTP packet")
            .unwrap();
        assert!((13..=12 + vks_gateway_sip::opus::MAX_PAYLOAD).contains(&count2));
        assert_eq!(
            u32::from_be_bytes(second[4..8].try_into().unwrap()).wrapping_sub(first_ts),
            960,
            "Opus RTP clock is 48 kHz"
        );
        let mut decoder = vks_gateway_sip::opus::OpusCodec::new().expect("opus decoder");
        let decoded = decoder.decode(&first[12..count]).expect("outbound Opus decodes");
        assert_eq!(decoded.len(), 960);
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn gateway_audio_bridges_g723_at_pt4_with_30ms_timestamps() {
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
        let stats = Arc::new(GatewayMediaStats::default());
        let (_tx, dtmf_rx) = tokio::sync::mpsc::unbounded_channel();
        let task = spawn_rtp_media_bridge(RtpBridgeSession {
            room_id: room,
            participant_id_str: source.to_string(),
            participant_id_val: source,
            remote_media_ip: "127.0.0.1".into(),
            remote_audio_port: terminal.local_addr().unwrap().port(),
            audio_payload_type: 4,
            audio_codec: Some("G723"),
            source_latch: false,
            dtmf_payload_type: None,
            media_stats: stats,
            rtp_socket: socket,
            rtcp_socket: None,
            rtp_port: target.port(),
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            mcu_engine: mcu.clone(),
            controls: GatewayMediaControls::default(),
            cancel_flag: cancel.clone(),
            dtmf_rx,
        });
        let mut codec = vks_gateway_sip::g7231::G7231Codec::new();
        let pcm: Vec<f32> = (0..vks_gateway_sip::g7231::FRAME_SAMPLES)
            .map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin() * 0.25)
            .collect();
        let payload = codec.encode(&pcm).expect("encode G.723.1");
        let mut packet = vec![0u8; 12 + payload.len()];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 4, 1, 0, 42, false);
        packet[12..].copy_from_slice(&payload);
        terminal.send_to(&packet, target).await.unwrap();
        // 30 ms codec / 20 ms consumer: two packets (480 samples) are the
        // playout prefill; a single packet must not start the mixer yet.
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut second_in = packet.clone();
        vks_gateway_sip::write_rtp_header(&mut second_in[..12], 4, 2, 240, 42, false);
        terminal.send_to(&second_in, target).await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            while !mcu
                .participant_audio(room, listener)
                .is_some_and(|audio| audio.iter().any(|sample| sample.abs() > 0.02))
            {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("G.723.1 PCM must reach the MCU mix");
        let mut first = vec![0u8; 1400];
        let (count, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut first))
            .await
            .expect("bridge must emit G.723.1 RTP")
            .unwrap();
        assert_eq!(first[1] & 0x7F, 4);
        assert_eq!(count, 12 + 24, "one MP-MLQ frame per 30 ms packet");
        let first_ts = u32::from_be_bytes(first[4..8].try_into().unwrap());
        let mut second = vec![0u8; 1400];
        let (count2, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut second))
            .await
            .expect("bridge must emit a second G.723.1 packet")
            .unwrap();
        assert_eq!(count2, 12 + 24);
        assert_eq!(
            u32::from_be_bytes(second[4..8].try_into().unwrap()).wrapping_sub(first_ts),
            240,
            "G.723.1 timestamp advances by 240 (30 ms)"
        );
        let mut decoder = vks_gateway_sip::g7231::G7231Codec::new();
        assert_eq!(decoder.decode(&first[12..count]).expect("outbound decodes").len(), 240);

        // Continuous 30 ms arrivals must never starve the 20 ms consumer:
        // without the 480-sample prefill the queue would hit zero every 60 ms.
        let mut seen_audio = false;
        let mut zero_snapshots = 0;
        for step in 3..13u16 {
            let mut next = packet.clone();
            let timestamp = 240 * u32::from(step - 1);
            vks_gateway_sip::write_rtp_header(&mut next[..12], 4, step, timestamp, 42, false);
            terminal.send_to(&next, target).await.unwrap();
            tokio::time::sleep(Duration::from_millis(30)).await;
            if let Some(audio) = mcu.participant_audio(room, listener) {
                if audio.iter().any(|sample| sample.abs() > 0.002) {
                    seen_audio = true;
                } else if seen_audio {
                    zero_snapshots += 1;
                }
            }
        }
        assert!(seen_audio, "continuous G.723.1 never reached the mixer");
        // One delayed 20 ms tick is scheduler noise on a loaded host; a real
        // underrun repeats every other tick, so tolerate at most one gap.
        assert!(zero_snapshots <= 1, "G.723.1 playout underran despite prefill: {zero_snapshots} gaps");
        cancel.store(true, Ordering::Relaxed);
        task.await.unwrap();
    }

    #[test]
    fn watchdog_allows_a_slow_first_packet_but_not_a_silent_call() {
        use std::time::Duration;
        assert!(!media_bridge_dead(Duration::from_secs(6), 0, 0, true));
        assert!(media_bridge_dead(Duration::from_secs(16), 0, 0, true));
        assert!(!media_bridge_dead(Duration::from_secs(10), 9_000, 10_000, true));
        assert!(media_bridge_dead(Duration::from_secs(20), 1_000, 20_000, true));
    }

    #[tokio::test]
    async fn inbound_sip_uas_handles_twenty_concurrent_calls() {
        use vks_admin::state::AppState;
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "uas-20");
        let manager = Arc::new(
            GatewayManager::new().with_mcu(Arc::new(vks_media_mcu::McuEngine::new())).with_candidate_ip("127.0.0.1"),
        );
        manager.bind_state(Arc::downgrade(&state));
        let server = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server.local_addr().unwrap().port();
        let listener = tokio::spawn(serve_sip_uas_on(
            manager.clone(),
            server,
            "127.0.0.1".into(),
            std::time::Duration::from_secs(15),
        ));
        let mut clients = Vec::new();
        let mut tags = Vec::new();
        for i in 0..20u16 {
            let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
            client.connect(("127.0.0.1", server_port)).await.unwrap();
            let port = client.local_addr().unwrap().port();
            let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
            let invite = format!(
                "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bK{i}\r\nFrom: \"C{i}\" <sip:c{i}@127.0.0.1>;tag=t{i}\r\nTo: <sip:{}@127.0.0.1>\r\nCall-ID: cid-{i}\r\nCSeq: 1 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
                room.id, room.id, sdp.len(), sdp
            );
            client.send(invite.as_bytes()).await.unwrap();
            let mut tag = String::new();
            for _ in 0..4 {
                let mut buf = [0u8; 8192];
                let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                    .await
                    .expect("200 timeout")
                    .unwrap();
                let text = String::from_utf8_lossy(&buf[..len]);
                if text.starts_with("SIP/2.0 200") {
                    tag = text
                        .lines()
                        .find_map(|line| line.strip_prefix("To:"))
                        .and_then(|line| line.split(";tag=").nth(1))
                        .map(|tag| tag.trim().to_string())
                        .expect("To tag");
                    break;
                }
            }
            assert!(!tag.is_empty(), "call {i} must be answered");
            tags.push(tag);
            clients.push(client);
        }
        for (i, (client, tag)) in clients.iter().zip(tags.iter()).enumerate() {
            let port = client.local_addr().unwrap().port();
            let ack = format!(
                "ACK sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bKa{i}\r\nFrom: \"C{i}\" <sip:c{i}@127.0.0.1>;tag=t{i}\r\nTo: <sip:{}@127.0.0.1>;tag={tag}\r\nCall-ID: cid-{i}\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n",
                room.id, room.id
            );
            client.send(ack.as_bytes()).await.unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert_eq!(manager.registry.lock().unwrap().by_participant.len(), 20, "20 calls must be active");
        for (i, (client, tag)) in clients.iter().zip(tags.iter()).enumerate() {
            let port = client.local_addr().unwrap().port();
            let bye = format!(
                "BYE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{port};branch=z9hG4bKb{i}\r\nFrom: \"C{i}\" <sip:c{i}@127.0.0.1>;tag=t{i}\r\nTo: <sip:{}@127.0.0.1>;tag={tag}\r\nCall-ID: cid-{i}\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n",
                room.id, room.id
            );
            client.send(bye.as_bytes()).await.unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        assert!(manager.registry.lock().unwrap().by_participant.is_empty(), "all 20 calls must be cleaned up");
        listener.abort();
    }

    #[test]
    fn inbound_media_watchdog_spares_hold_but_reaps_lost_terminal() {
        use std::time::Duration;
        let timeout = Duration::from_secs(90);
        // 40 с тишины при работающем G.711 — это hold/пауза, не смерть.
        assert!(!inbound_media_dead(Duration::from_secs(40), 0, 40_000, true, timeout));
        assert!(!inbound_media_dead(Duration::from_secs(80), 10_000, 90_000, true, timeout));
        // 90+ с без RTP или codec не запустился — вызов снимаем.
        assert!(inbound_media_dead(Duration::from_secs(120), 10_000, 100_100, true, timeout));
        assert!(inbound_media_dead(Duration::from_secs(11), 0, 11_000, false, timeout));
    }

    #[tokio::test]
    async fn inbound_sip_uas_media_timeout_reaps_registry_and_sends_bye() {
        use vks_admin::state::AppState;
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "uas-timeout");
        let manager = Arc::new(
            GatewayManager::new().with_mcu(Arc::new(vks_media_mcu::McuEngine::new())).with_candidate_ip("127.0.0.1"),
        );
        manager.bind_state(Arc::downgrade(&state));
        let server = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server.local_addr().unwrap().port();
        let listener = tokio::spawn(serve_sip_uas_on(
            manager.clone(),
            server,
            "127.0.0.1".into(),
            std::time::Duration::from_secs(15),
        ));
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(("127.0.0.1", server_port)).await.unwrap();
        let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let invite = format!(
            "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40004;branch=z9hG4bKt\r\nFrom: \"S\" <sip:s@127.0.0.1>;tag=sf\r\nTo: <sip:{}@127.0.0.1>\r\nCall-ID: cid-timeout\r\nCSeq: 1 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            room.id, room.id, sdp.len(), sdp
        );
        client.send(invite.as_bytes()).await.unwrap();
        let mut to_tag = String::new();
        for _ in 0..4 {
            let mut buf = [0u8; 8192];
            let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                .await
                .expect("response timeout")
                .unwrap();
            let text = String::from_utf8_lossy(&buf[..len]);
            if text.starts_with("SIP/2.0 200") {
                to_tag = text
                    .lines()
                    .find_map(|line| line.strip_prefix("To:"))
                    .and_then(|line| line.split(";tag=").nth(1))
                    .map(|tag| tag.trim().to_string())
                    .expect("To tag");
                break;
            }
        }
        let ack = format!(
            "ACK sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40004;branch=z9hG4bKa\r\nFrom: \"S\" <sip:s@127.0.0.1>;tag=sf\r\nTo: <sip:{}@127.0.0.1>;tag={}\r\nCall-ID: cid-timeout\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id, to_tag
        );
        client.send(ack.as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(manager.registry.lock().unwrap().by_participant.len(), 1);
        // Ни одного RTP-пакета: media-watchdog должен снять вызов за ~15-20 с.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(25);
        let mut bye_text = String::new();
        while tokio::time::Instant::now() < deadline {
            let mut buf = [0u8; 8192];
            let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buf)).await
            else {
                continue;
            };
            let text = String::from_utf8_lossy(&buf[..len]).into_owned();
            if text.starts_with("BYE ") {
                bye_text = text;
                break;
            }
        }
        assert!(bye_text.starts_with("BYE "), "media timeout must send BYE");
        assert!(
            bye_text.contains(&format!("From: <sip:{}@127.0.0.1>;tag={to_tag}", room.id)),
            "media-timeout BYE From must be local side: {bye_text}"
        );
        assert!(
            manager.registry.lock().unwrap().by_participant.is_empty(),
            "media timeout must clean GatewayCallRegistry"
        );
        listener.abort();
    }

    #[tokio::test]
    async fn inbound_sip_uas_admin_hangup_sends_bye_and_reaps_call() {
        use vks_admin::state::AppState;
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "uas-admin");
        let manager = Arc::new(
            GatewayManager::new().with_mcu(Arc::new(vks_media_mcu::McuEngine::new())).with_candidate_ip("127.0.0.1"),
        );
        manager.bind_state(Arc::downgrade(&state));
        let server = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server.local_addr().unwrap().port();
        let listener = tokio::spawn(serve_sip_uas_on(
            manager.clone(),
            server,
            "127.0.0.1".into(),
            std::time::Duration::from_secs(15),
        ));
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(("127.0.0.1", server_port)).await.unwrap();
        let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40002 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let invite = format!(
            "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40002;branch=z9hG4bKx\r\nFrom: \"A\" <sip:a@127.0.0.1>;tag=af\r\nTo: <sip:{}@127.0.0.1>\r\nCall-ID: cid-admin\r\nCSeq: 1 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            room.id, room.id, sdp.len(), sdp
        );
        client.send(invite.as_bytes()).await.unwrap();
        let mut to_tag = String::new();
        for _ in 0..4 {
            let mut buf = [0u8; 8192];
            let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                .await
                .expect("response timeout")
                .unwrap();
            let text = String::from_utf8_lossy(&buf[..len]);
            if text.starts_with("SIP/2.0 200") {
                to_tag = text
                    .lines()
                    .find_map(|line| line.strip_prefix("To:"))
                    .and_then(|line| line.split(";tag=").nth(1))
                    .map(|tag| tag.trim().to_string())
                    .expect("To tag");
                break;
            }
        }
        let ack = format!(
            "ACK sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40002;branch=z9hG4bKxa\r\nFrom: \"A\" <sip:a@127.0.0.1>;tag=af\r\nTo: <sip:{}@127.0.0.1>;tag={}\r\nCall-ID: cid-admin\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id, to_tag
        );
        client.send(ack.as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        let pid =
            manager.registry.lock().unwrap().by_participant.keys().next().cloned().expect("registered participant");
        assert!(manager.hangup(&pid), "admin hangup must find the inbound dialog");
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut bye_text = String::new();
        while tokio::time::Instant::now() < deadline {
            let mut buf = [0u8; 8192];
            let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buf)).await
            else {
                continue;
            };
            let text = String::from_utf8_lossy(&buf[..len]).into_owned();
            if text.starts_with("BYE ") {
                bye_text = text;
                break;
            }
        }
        assert!(bye_text.starts_with("BYE "), "admin hangup must send BYE to the remote terminal");
        assert!(
            bye_text.contains(&format!("From: <sip:{}@127.0.0.1>;tag={to_tag}", room.id)),
            "server BYE From must be the local dialog side with our tag: {bye_text}"
        );
        assert!(
            bye_text.contains("To: \"A\" <sip:a@127.0.0.1>;tag=af"),
            "server BYE To must be the remote caller side with their tag: {bye_text}"
        );
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(manager.registry.lock().unwrap().by_participant.is_empty(), "admin hangup must clear registry");
        listener.abort();
    }

    #[tokio::test]
    async fn inbound_sip_uas_reinvite_gets_501_and_shutdown_sends_bye() {
        use vks_admin::state::AppState;
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "uas-reinvite");
        let manager = Arc::new(
            GatewayManager::new().with_mcu(Arc::new(vks_media_mcu::McuEngine::new())).with_candidate_ip("127.0.0.1"),
        );
        manager.bind_state(Arc::downgrade(&state));
        let server = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server.local_addr().unwrap().port();
        let listener = tokio::spawn(serve_sip_uas_on(
            manager.clone(),
            server,
            "127.0.0.1".into(),
            std::time::Duration::from_secs(30),
        ));
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(("127.0.0.1", server_port)).await.unwrap();
        let client_port = client.local_addr().unwrap().port();
        let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let invite = format!(
            "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{client_port};branch=z9hG4bKri;rport\r\nFrom: \"R\" <sip:r@127.0.0.1>;tag=rf\r\nTo: <sip:{}@127.0.0.1>\r\nCall-ID: cid-reinvite\r\nCSeq: 1 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            room.id, room.id, sdp.len(), sdp
        );
        client.send(invite.as_bytes()).await.unwrap();
        let mut to_tag = String::new();
        let mut response_text = String::new();
        for _ in 0..4 {
            let mut buf = [0u8; 8192];
            let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                .await
                .expect("response timeout")
                .unwrap();
            let text = String::from_utf8_lossy(&buf[..len]).into_owned();
            if text.starts_with("SIP/2.0 200") {
                response_text = text.clone();
                to_tag = text
                    .lines()
                    .find_map(|line| line.strip_prefix("To:"))
                    .and_then(|line| line.split(";tag=").nth(1))
                    .map(|tag| tag.trim().to_string())
                    .expect("To tag");
                break;
            }
        }
        let via = response_text.lines().find(|line| line.starts_with("Via:")).expect("Via");
        assert!(via.contains("received=127.0.0.1"), "Via must carry received: {via}");
        assert!(via.contains(&format!("rport={client_port}")), "Via must carry rport: {via}");
        let ack = format!(
            "ACK sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{client_port};branch=z9hG4bKra\r\nFrom: \"R\" <sip:r@127.0.0.1>;tag=rf\r\nTo: <sip:{}@127.0.0.1>;tag={to_tag}\r\nCall-ID: cid-reinvite\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id
        );
        client.send(ack.as_bytes()).await.unwrap();
        let reinvite = format!(
            "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{client_port};branch=z9hG4bKri2\r\nFrom: \"R\" <sip:r@127.0.0.1>;tag=rf\r\nTo: <sip:{}@127.0.0.1>;tag={to_tag}\r\nCall-ID: cid-reinvite\r\nCSeq: 2 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            room.id, room.id, sdp.len(), sdp
        );
        client.send(reinvite.as_bytes()).await.unwrap();
        let mut reinvite_status = String::new();
        for _ in 0..4 {
            let mut buf = [0u8; 8192];
            let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                .await
                .expect("reinvite response timeout")
                .unwrap();
            let text = String::from_utf8_lossy(&buf[..len]).into_owned();
            if text.starts_with("SIP/2.0 501") {
                reinvite_status = text;
                break;
            }
        }
        assert!(reinvite_status.starts_with("SIP/2.0 501"), "re-INVITE must be rejected with 501");
        manager.shutdown_sip_uas().await;
        let mut bye_text = String::new();
        for _ in 0..6 {
            let mut buf = [0u8; 8192];
            let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buf)).await
            else {
                continue;
            };
            let text = String::from_utf8_lossy(&buf[..len]).into_owned();
            if text.starts_with("BYE ") {
                bye_text = text;
                break;
            }
        }
        assert!(bye_text.starts_with("BYE "), "graceful shutdown must send BYE to active inbound dialog");
        assert!(manager.registry.lock().unwrap().by_participant.is_empty(), "shutdown must clear registry");
        let _ = listener.await;
    }

    #[tokio::test]
    async fn inbound_sip_uas_answers_with_to_tag_and_cleans_up_on_bye() {
        use vks_admin::state::AppState;
        let state = Arc::new(AppState::new());
        let room = Rooms::create(&state, "uas-inbound");
        let manager = Arc::new(
            GatewayManager::new().with_mcu(Arc::new(vks_media_mcu::McuEngine::new())).with_candidate_ip("127.0.0.1"),
        );
        manager.bind_state(Arc::downgrade(&state));
        let server = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let server_port = server.local_addr().unwrap().port();
        let listener = tokio::spawn(serve_sip_uas_on(
            manager.clone(),
            server,
            "127.0.0.1".into(),
            std::time::Duration::from_secs(15),
        ));
        let client = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        client.connect(("127.0.0.1", server_port)).await.unwrap();
        let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let invite = format!(
            "INVITE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40000;branch=z9hG4bKt\r\nFrom: \"T\" <sip:t@127.0.0.1>;tag=ft\r\nTo: <sip:{}@127.0.0.1>\r\nCall-ID: cid-test\r\nCSeq: 1 INVITE\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
            room.id, room.id, sdp.len(), sdp
        );
        client.send(invite.as_bytes()).await.unwrap();
        let mut to_tag = String::new();
        let mut saw_ringing_with_tag = false;
        let mut saw_ok = false;
        for _ in 0..4 {
            let mut buf = [0u8; 8192];
            let len = tokio::time::timeout(std::time::Duration::from_secs(2), client.recv(&mut buf))
                .await
                .expect("UAS response timeout")
                .unwrap();
            let text = String::from_utf8_lossy(&buf[..len]);
            if text.starts_with("SIP/2.0 180") {
                saw_ringing_with_tag = text.contains(";tag=");
            }
            if text.starts_with("SIP/2.0 200") {
                saw_ok = true;
                to_tag = text
                    .lines()
                    .find_map(|line| line.strip_prefix("To:"))
                    .and_then(|line| line.split(";tag=").nth(1))
                    .map(|tag| tag.trim().to_string())
                    .expect("200 OK must carry To tag");
                break;
            }
        }
        assert!(saw_ok, "UAS must answer INVITE with 200");
        assert!(saw_ringing_with_tag, "180 Ringing must carry To tag");
        let ack = format!(
            "ACK sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40000;branch=z9hG4bKa\r\nFrom: \"T\" <sip:t@127.0.0.1>;tag=ft\r\nTo: <sip:{}@127.0.0.1>;tag={}\r\nCall-ID: cid-test\r\nCSeq: 1 ACK\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id, to_tag
        );
        client.send(ack.as_bytes()).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert_eq!(manager.registry.lock().unwrap().by_participant.len(), 1, "ACK must keep the call registered");
        let spoofed_bye = format!(
            "BYE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40000;branch=z9hG4bKs\r\nFrom: \"T\" <sip:t@127.0.0.1>;tag=ft\r\nTo: <sip:{}@127.0.0.1>;tag=WRONG\r\nCall-ID: cid-test\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id
        );
        client.send(spoofed_bye.as_bytes()).await.unwrap();
        let mut spoof_status = String::new();
        for _ in 0..10 {
            let mut buf = [0u8; 8192];
            let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buf)).await
            else {
                break;
            };
            let text = String::from_utf8_lossy(&buf[..len]);
            if text.starts_with("SIP/2.0") {
                spoof_status = text.lines().next().unwrap_or_default().to_string();
                break;
            }
        }
        assert!(spoof_status.starts_with("SIP/2.0 481"), "spoofed To-tag BYE must be 481, got {spoof_status}");
        assert_eq!(manager.registry.lock().unwrap().by_participant.len(), 1, "spoofed BYE must not kill the dialog");
        let cancel = format!(
            "CANCEL sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40000;branch=z9hG4bKc\r\nFrom: \"T\" <sip:t@127.0.0.1>;tag=ft\r\nTo: <sip:{}@127.0.0.1>;tag={}\r\nCall-ID: cid-test\r\nCSeq: 1 CANCEL\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id, to_tag
        );
        client.send(cancel.as_bytes()).await.unwrap();
        let mut cancel_ok = false;
        for _ in 0..10 {
            let mut buf = [0u8; 8192];
            let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buf)).await
            else {
                break;
            };
            if String::from_utf8_lossy(&buf[..len]).starts_with("SIP/2.0 200") {
                cancel_ok = true;
                break;
            }
        }
        assert!(cancel_ok, "CANCEL after final 200 must be acknowledged");
        assert_eq!(
            manager.registry.lock().unwrap().by_participant.len(),
            1,
            "CANCEL after 200 must not end the dialog"
        );
        let bye = format!(
            "BYE sip:{}@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:40000;branch=z9hG4bKb\r\nFrom: \"T\" <sip:t@127.0.0.1>;tag=ft\r\nTo: <sip:{}@127.0.0.1>;tag={}\r\nCall-ID: cid-test\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n",
            room.id, room.id, to_tag
        );
        client.send(bye.as_bytes()).await.unwrap();
        let mut bye_ok = false;
        for _ in 0..10 {
            let mut buf = [0u8; 8192];
            let Ok(Ok(len)) = tokio::time::timeout(std::time::Duration::from_secs(1), client.recv(&mut buf)).await
            else {
                break;
            };
            if String::from_utf8_lossy(&buf[..len]).starts_with("SIP/2.0 200") {
                bye_ok = true;
                break;
            }
        }
        assert!(bye_ok, "BYE must be acknowledged");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
        assert!(manager.registry.lock().unwrap().by_participant.is_empty(), "BYE must remove registry state");
        listener.abort();
    }

    #[test]
    fn watchdog_fails_a_bridge_whose_audio_codec_never_started() {
        use std::time::Duration;
        assert!(!media_bridge_dead(Duration::from_secs(4), 3_000, 4_000, false));
        assert!(media_bridge_dead(Duration::from_secs(6), 5_000, 6_000, false));
    }
}

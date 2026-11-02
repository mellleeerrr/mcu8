//! H.264/H.265 RTP payload processing and isolated FFmpeg codec workers.

use super::audio::RtpPayload;
use super::{GatewayMediaControls, GatewayMediaStats};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use vks_gateway_sip::h265::{nal_type, packetize_h265, strip_start_code, H265Depacketizer};

/// Профиль кодирования шлюзового видео. Заменяет прежние жёсткие
/// `640x360/15fps/256k/baseline/3.0`: оператор задаёт размер/битрейт/fps
/// через env (`VKS_GATEWAY_VIDEO_*`), а `max_frame_rate` из
/// `/api/v3/settings/video` подхватывается в `GatewayManager`.
/// Video codec used by the gateway bridge. H.265 uses the RFC 7798 payloader
/// and libx265; SDP negotiation selects it per call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum GatewayVideoCodec {
    H264,
    H265,
}

impl GatewayVideoCodec {
    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
        }
    }

    const fn demuxer(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
        }
    }

    /// x265 parameter string (H.264 uses x264's own options).
    const fn encoder_params(self) -> &'static str {
        match self {
            Self::H264 => {
                "repeat-headers=1:aud=1:scenecut=0:bframes=0:cabac=0:weightp=0:8x8dct=0:ref=1:rc-lookahead=0:slice-max-size=1180"
            }
            Self::H265 => {
                "log-level=error:bframes=0:rc-lookahead=0:scenecut=0:frame-threads=1:pools=1:aud=1:repeat-headers=1"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VideoProfile {
    pub codec: GatewayVideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub profile: String,
    pub level: String,
}

impl Default for VideoProfile {
    fn default() -> Self {
        Self {
            codec: GatewayVideoCodec::H264,
            width: 1280,
            height: 720,
            fps: 30,
            bitrate_kbps: 2048,
            profile: "baseline".into(),
            level: "3.1".into(),
        }
    }
}

impl VideoProfile {
    /// H.241-совместимый уровень H.264: явный `level` важнее геометрии, иначе
    /// выводим из pixels*fps (1080p30 → 4.0, 720p30 → 3.1, 640x360 → 3.0).
    pub(super) fn h323_level_idc(&self) -> u8 {
        match self.level.trim() {
            "3.0" => 30,
            "3.1" => 31,
            "3.2" => 32,
            "4.0" => 40,
            "4.1" => 41,
            "4.2" => 42,
            "5.0" => 50,
            "5.1" => 51,
            _ => {
                // H.264 Level limits are macroblocks/s, not pixels/s:
                // 3.0 40 500, 3.1 108 000, 4.0 245 760, 4.2 522 240.
                let mb = u64::from(self.width.div_ceil(16)) * u64::from(self.height.div_ceil(16)) * u64::from(self.fps);
                if mb > 522_240 {
                    51
                } else if mb > 245_760 {
                    42
                } else if mb > 108_000 {
                    40
                } else if mb > 40_500 {
                    31
                } else {
                    30
                }
            }
        }
    }

    /// Читает переопределения из env. Невалидные/нулевые значения игнорируются,
    /// чтобы опечатка в unit-файле не отключила видео.
    pub(super) fn from_env() -> Self {
        let mut profile = Self::default();
        // Base profile; per-call negotiation overrides the codec. A bridge must
        // never send H.265 under an H.264 m-line, which is why gateways set this
        // only from the negotiated codec.
        profile.codec = GatewayVideoCodec::H264;
        profile.width = env_u32("VKS_GATEWAY_VIDEO_WIDTH").unwrap_or(profile.width);
        profile.height = env_u32("VKS_GATEWAY_VIDEO_HEIGHT").unwrap_or(profile.height);
        profile.fps = env_u32("VKS_GATEWAY_VIDEO_FPS").unwrap_or(profile.fps);
        profile.bitrate_kbps = env_u32("VKS_GATEWAY_VIDEO_BITRATE_KBPS").unwrap_or(profile.bitrate_kbps);
        if let Ok(value) = std::env::var("VKS_GATEWAY_VIDEO_PROFILE") {
            if !value.trim().is_empty() {
                profile.profile = value.trim().to_string();
            }
        }
        profile.width = (profile.width.clamp(160, 1920) / 2) * 2;
        profile.height = (profile.height.clamp(120, 1080) / 2) * 2;
        profile.fps = profile.fps.clamp(1, 60);
        profile.bitrate_kbps = profile.bitrate_kbps.clamp(64, 8192);
        if profile.profile != "baseline" {
            tracing::warn!(profile = %profile.profile, "gateway video profile is baseline-only until T05b; ignoring env");
            profile.profile = "baseline".into();
        }
        profile.level = std::env::var("VKS_GATEWAY_VIDEO_LEVEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| h264_level(profile.width, profile.height, profile.fps).to_string());
        profile
    }

    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2
    }

    /// Макроблоки 16×16 для H.264 `max-fs` (RFC 6184).
    pub(super) fn max_fs(&self) -> u32 {
        self.width.div_ceil(16).max(1) * self.height.div_ceil(16).max(1)
    }

    pub(super) fn max_mbps(&self) -> u32 {
        self.max_fs().saturating_mul(self.fps.max(1))
    }

    /// `profile-level-id` для Baseline: `42c0` + level_idc. Именно такие
    /// constraint-флаги (`constraint_set0/1`) выдаёт x264 в SPS при
    /// `-profile:v baseline`; `42e0…` был рассинхроном SDP и реального SPS.
    pub(super) fn profile_level_id(&self) -> String {
        let idc = level_idc(&self.level);
        format!("42c0{idc:02x}")
    }

    /// Клампит исходящий профиль по лимитам answer'а терминала. Терминал
    /// может принять видео, но с меньшими `max-fs/max-br/max-mbps`, чем наш
    /// оффер; кодировать выше них нельзя — пакеты будут отброшены.
    pub(super) fn clamp_to_answer(&mut self, max_fs: Option<u32>, max_br: Option<u32>, max_mbps: Option<u32>) {
        if let Some(max_fs) = max_fs.filter(|value| *value > 0) {
            if self.max_fs() > max_fs {
                // Стандартная лестница 16:9: терминалы ожидают один из этих
                // размеров, а не произвольное деление пополам (480×270).
                let (width, height) = SIZE_LADDER
                    .iter()
                    .copied()
                    .find(|(width, height)| macroblocks(*width, *height) <= max_fs)
                    .unwrap_or((320, 180));
                self.width = width;
                self.height = height;
            }
        }
        if let Some(max_br) = max_br.filter(|value| *value > 0) {
            self.bitrate_kbps = self.bitrate_kbps.min(max_br);
        }
        if let Some(max_mbps) = max_mbps.filter(|value| *value > 0) {
            if self.max_fs().saturating_mul(self.fps) > max_mbps {
                // Сначала уменьшаем размер по лестнице, сохраняя разумный fps
                // (>=15): 1080p при max-mbps=49000 → 960×540@24, а не 1080p@6.
                let mut chosen = None;
                let applied_max_fs = self.max_fs();
                for (width, height) in SIZE_LADDER.iter().copied() {
                    let macroblocks = macroblocks(width, height);
                    // Не апскейлим и не выходим за уже применённый max-fs:
                    // иначе терминал отбросит пакеты как превышение H.241.
                    if macroblocks > applied_max_fs {
                        continue;
                    }
                    let candidate_fps = max_mbps / macroblocks.max(1);
                    if candidate_fps >= 15 {
                        chosen = Some((width, height, candidate_fps));
                        break;
                    }
                }
                if let Some((width, height, candidate_fps)) = chosen {
                    self.width = width;
                    self.height = height;
                    self.fps = self.fps.min(candidate_fps).max(1);
                } else {
                    self.width = 320;
                    self.height = 180;
                    self.fps = (max_mbps / macroblocks(320, 180)).max(1);
                }
            }
        }
        self.level = h264_level(self.width, self.height, self.fps).to_string();
    }
}

/// `level_idc` из строки уровня (`3.1` → 31, `4.0` → 40).
fn level_idc(level: &str) -> u8 {
    let mut parts = level.split('.');
    let major = parts.next().and_then(|value| value.parse::<u8>().ok()).unwrap_or(3);
    let minor = parts.next().and_then(|value| value.parse::<u8>().ok()).unwrap_or(1);
    major.saturating_mul(10).saturating_add(minor)
}

fn env_u32(key: &str) -> Option<u32> {
    std::env::var(key).ok()?.trim().parse().ok().filter(|value| *value > 0)
}

/// Минимальный H.264 level, покрывающий `width×height@fps` (уровень нужен
/// peer'у для декодера; занижать его нельзя).
const SIZE_LADDER: &[(u32, u32)] = &[(1920, 1080), (1280, 720), (960, 540), (640, 360), (480, 270), (320, 180)];

fn macroblocks(width: u32, height: u32) -> u32 {
    width.div_ceil(16).max(1) * height.div_ceil(16).max(1)
}

fn h264_level(width: u32, height: u32, fps: u32) -> &'static str {
    let pixels_per_second = u64::from(width) * u64::from(height) * u64::from(fps);
    if width <= 1280 && height <= 720 && pixels_per_second <= 1280 * 720 * 30 {
        "3.1"
    } else if width <= 1920 && height <= 1088 && pixels_per_second <= 1920 * 1088 * 30 {
        "4.0"
    } else {
        "4.2"
    }
}

pub(super) struct VideoBridgeSession {
    pub room_id: vks_core::RoomId,
    pub participant_id: vks_core::ParticipantId,
    pub rtp_socket: tokio::net::UdpSocket,
    pub rtcp_socket: Option<tokio::net::UdpSocket>,
    pub remote_target: std::net::SocketAddr,
    pub payload_type: u8,
    pub receive_payload_type: u8,
    pub port_pool: Arc<vks_gateway_sip::RtpPortPool>,
    pub engine: Arc<vks_media_mcu::McuEngine>,
    pub controls: GatewayMediaControls,
    pub cancelled: Arc<std::sync::atomic::AtomicBool>,
    pub stats: Arc<GatewayMediaStats>,
    pub profile: VideoProfile,
}

static H264_AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();
static H265_AVAILABLE: tokio::sync::OnceCell<bool> = tokio::sync::OnceCell::const_new();

/// Availability of the H.264 bridge (the SDP default when H.265 is absent).
pub(super) async fn codec_available() -> bool {
    codec_available_for(GatewayVideoCodec::H264).await
}

pub(super) async fn codec_available_for(codec: GatewayVideoCodec) -> bool {
    let (cell, encoder, decoder) = match codec {
        GatewayVideoCodec::H264 => (&H264_AVAILABLE, "libx264", "h264"),
        GatewayVideoCodec::H265 => (&H265_AVAILABLE, "libx265", "hevc"),
    };
    *cell
        .get_or_init(|| async {
            let has = |flag: &'static str, needle: &'static str| async move {
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    Command::new("ffmpeg").args(["-hide_banner", flag]).output(),
                )
                .await
                {
                    Ok(Ok(output)) => {
                        if !output.status.success() {
                            return false;
                        }
                        // Match the exact codec token, not a substring:
                        // h264_qsv/vaapi wrappers are not the software decoder
                        // the bridge's `-f h264/hevc` invocation uses.
                        String::from_utf8_lossy(&output.stdout)
                            .lines()
                            .any(|line| line.split_whitespace().nth(1) == Some(needle))
                    }
                    _ => false,
                }
            };
            // The bridge both encodes and decodes, so a half-installed FFmpeg
            // must not be advertised as usable.
            has("-encoders", encoder).await && has("-decoders", decoder).await
        })
        .await
}

fn spawn_codec(encoder: bool, profile: &VideoProfile) -> Result<Child, String> {
    let mut command = Command::new("ffmpeg");
    command.args(["-hide_banner", "-loglevel", "error", "-nostdin", "-threads", "2", "-filter_threads", "2"]);
    if encoder {
        let size = format!("{}x{}", profile.width, profile.height);
        let fps = profile.fps.to_string();
        let bitrate = format!("{}k", profile.bitrate_kbps);
        let buffer = format!("{}k", profile.bitrate_kbps.saturating_mul(2));
        // IDR каждые 500 мс (keyint=15 при 30 fps). Форс-IDR по PLI/FIR невозможен
        // без рестарта child-процесса (у ffmpeg CLI нет runtime-хука), поэтому
        // быстрое восстановление после потери обеспечивается keyint=15.
        let keyint = ((profile.fps / 2).max(1)).to_string();
        let (codec, params_flag) = match profile.codec {
            GatewayVideoCodec::H264 => ("libx264", "-x264-params"),
            GatewayVideoCodec::H265 => ("libx265", "-x265-params"),
        };
        command.args([
            "-f",
            "rawvideo",
            "-pixel_format",
            "yuv420p",
            "-video_size",
            size.as_str(),
            "-framerate",
            fps.as_str(),
            "-i",
            "pipe:0",
            "-an",
            "-c:v",
            codec,
            "-threads",
            "2",
            "-preset",
            "ultrafast",
            "-tune",
            "zerolatency",
            "-pix_fmt",
            "yuv420p",
        ]);
        if profile.codec == GatewayVideoCodec::H264 {
            // x265 has no baseline; keep the H.264 profile/level contract only there.
            command.args(["-profile:v", profile.profile.as_str(), "-level:v", profile.level.as_str()]);
        }
        command.args([
            "-b:v",
            bitrate.as_str(),
            "-maxrate",
            bitrate.as_str(),
            "-bufsize",
            buffer.as_str(),
            "-g",
            keyint.as_str(),
            "-keyint_min",
            keyint.as_str(),
            params_flag,
            profile.codec.encoder_params(),
            "-flush_packets",
            "1",
            "-f",
            profile.codec.demuxer(),
            "pipe:1",
        ]);
    } else {
        let scale = format!("scale={}:{}", profile.width, profile.height);
        // Both decoders are fed live RTP; low_delay avoids buffering a frame
        // inside FFmpeg. H.261/AAC are not routed through this path.
        command.args(["-flags", "low_delay"]);
        command.args([
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-max_pixels",
            "2073600",
            "-f",
            profile.codec.demuxer(),
            "-i",
            "pipe:0",
            "-an",
            "-vf",
            scale.as_str(),
            "-threads",
            "2",
            "-pix_fmt",
            "yuv420p",
            "-fps_mode",
            "passthrough",
            "-flush_packets",
            "1",
            "-f",
            "rawvideo",
            "pipe:1",
        ]);
    }
    command
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    command.spawn().map_err(|error| {
        format!("Cannot start FFmpeg {} {}: {error}", profile.codec.name(), if encoder { "encoder" } else { "decoder" })
    })
}

fn drain_errors(child: &mut Child, kind: &'static str) -> tokio::task::JoinHandle<()> {
    let mut stderr = child.stderr.take().expect("piped codec stderr");
    tokio::spawn(async move {
        let mut buffer = [0u8; 1024];
        let mut reported = 0;
        while let Ok(count) = stderr.read(&mut buffer).await {
            if count == 0 {
                break;
            }
            if reported < 4 {
                tracing::warn!(codec = kind, message = %String::from_utf8_lossy(&buffer[..count]), "FFmpeg codec diagnostic");
                reported += 1;
            }
        }
    })
}

fn is_access_unit_delimiter(codec: GatewayVideoCodec, nal: &[u8]) -> bool {
    let raw = strip_start_code(nal);
    match codec {
        GatewayVideoCodec::H264 => raw.first().is_some_and(|header| header & 31 == 9),
        GatewayVideoCodec::H265 => nal_type(raw) == Some(35),
    }
}

fn injected_aud(codec: GatewayVideoCodec) -> &'static [u8] {
    match codec {
        GatewayVideoCodec::H264 => &[0, 0, 0, 1, 9, 0xf0],
        GatewayVideoCodec::H265 => &[0, 0, 0, 1, 0x46, 0x01, 0x50],
    }
}

fn packetize_access_unit(
    codec: GatewayVideoCodec,
    units: &[Vec<u8>],
    payload_type: u8,
    ssrc: u32,
    sequence: &mut u16,
    timestamp: u32,
) -> Result<Vec<Vec<u8>>, ()> {
    match codec {
        GatewayVideoCodec::H264 => {
            let packets = packetize_h264(units, payload_type, ssrc, sequence, timestamp);
            if packets.is_empty() {
                Err(())
            } else {
                Ok(packets)
            }
        }
        GatewayVideoCodec::H265 => {
            let nals: Vec<&[u8]> = units.iter().map(|unit| strip_start_code(unit)).filter(|u| !u.is_empty()).collect();
            if nals.is_empty() {
                return Err(());
            }
            packetize_h265(&nals, payload_type, ssrc, sequence, timestamp).map_err(|_| ())
        }
    }
}

fn is_idr_nal(codec: GatewayVideoCodec, nal: &[u8]) -> bool {
    let raw = strip_start_code(nal);
    match codec {
        GatewayVideoCodec::H264 => raw.first().is_some_and(|h| h & 0x1f == 5),
        GatewayVideoCodec::H265 => matches!(nal_type(raw), Some(19 | 20)),
    }
}

fn is_sps_nal(codec: GatewayVideoCodec, nal: &[u8]) -> bool {
    let raw = strip_start_code(nal);
    match codec {
        GatewayVideoCodec::H264 => raw.first().is_some_and(|h| h & 0x1f == 7),
        GatewayVideoCodec::H265 => nal_type(raw) == Some(33),
    }
}

/// `O7`: отправляет один access unit зрителю — per-viewer RTP-обвязка
/// (свой SSRC/seq/ts), одинаковый битстрим группы.
#[allow(clippy::too_many_arguments)]
async fn send_access_unit(
    codec: GatewayVideoCodec,
    nals: &[Vec<u8>],
    socket: &tokio::net::UdpSocket,
    target: std::net::SocketAddr,
    payload_type: u8,
    ssrc: u32,
    sequence: &mut u16,
    timestamp: u32,
    stats: &GatewayMediaStats,
    viewer: Option<&vks_media_mcu::GroupViewer>,
    extra_params: &[Arc<[u8]>],
) -> bool {
    let is_idr = nals.iter().any(|nal| is_idr_nal(codec, nal));
    let has_sps = nals.iter().any(|nal| is_sps_nal(codec, nal));
    let prepared_storage: Vec<Vec<u8>>;
    let nals_ref: &[Vec<u8>] = if is_idr && !has_sps {
        let param_source = if !extra_params.is_empty() {
            extra_params
        } else if let Some(viewer) = viewer {
            viewer.parameter_sets.as_slice()
        } else {
            &[]
        };
        if !param_source.is_empty() {
            let mut prep = Vec::with_capacity(nals.len() + param_source.len());
            let mut inserted = false;
            for nal in nals {
                if !inserted && is_idr_nal(codec, nal) {
                    for set in param_source {
                        let clean = strip_start_code(set.as_ref());
                        if !clean.is_empty() {
                            prep.push(clean.to_vec());
                        }
                    }
                    inserted = true;
                }
                prep.push(nal.clone());
            }
            if !inserted {
                for set in param_source {
                    let clean = strip_start_code(set.as_ref());
                    if !clean.is_empty() {
                        prep.push(clean.to_vec());
                    }
                }
            }
            prepared_storage = prep;
            &prepared_storage
        } else {
            nals
        }
    } else {
        nals
    };

    let packets = match packetize_access_unit(codec, nals_ref, payload_type, ssrc, sequence, timestamp) {
        Ok(packets) => packets,
        Err(()) => {
            stats.dropped_video_frames.fetch_add(1, Relaxed);
            tracing::warn!(codec = codec.name(), "video access unit could not be packetized");
            return false;
        }
    };
    let mut successful = false;
    let mut sent_packets: u64 = 0;
    let mut sent_bytes: u64 = 0;
    for (idx, packet) in packets.iter().enumerate() {
        if idx > 0 && idx % 4 == 0 {
            tokio::task::yield_now().await;
        }
        if let Ok(sent) = socket.send_to(packet, target).await {
            stats.video_tx_packets.fetch_add(1, Relaxed);
            stats.video_tx_bytes.fetch_add(sent as u64, Relaxed);
            sent_packets += 1;
            sent_bytes += sent as u64;
            successful = true;
        }
    }
    // `P4a`: агрегат RTP-статистики зрителей группы (метрики `viewer_rtp_*`).
    if let Some(viewer) = viewer {
        viewer.note_rtp(sent_packets, sent_bytes);
    }
    successful
}

/// `O7`: выходной профиль сессии → ключ группы фермы.
///
/// Все поля, от которых зависит битстрим (спека §B.2); caps уже применены
/// (`clamp_to_answer` до этого места), поэтому зрители с разными caps попадут
/// в разные группы — законная деградация, а не ошибка.
/// `VKS_VIDEO_FARM=0` (или `off`) — аварийный выключатель фермы: видео идёт
/// старым per-session путём (как в итерации 2), без пересборки. Нужен как
/// страховка на случай регрессии в проде: включение фермы — `VKS_VIDEO_FARM=1`
/// или переменная не задана.
fn farm_enabled() -> bool {
    std::env::var("VKS_VIDEO_FARM")
        .map_or(true, |value| !value.trim().eq_ignore_ascii_case("0") && !value.trim().eq_ignore_ascii_case("off"))
}

fn farm_spec_for(profile: &VideoProfile, visible_windows: usize) -> vks_media_mcu::farm::EncoderSpec {
    // `P1`: общий энкодер на N окон требует больше полосы (BENCHMARK §10:
    // +8.6 %), но не больше +25 % — кламп внутри `group_bitrate_kbps`.
    let group_bitrate = vks_media_mcu::group_bitrate_kbps(profile.bitrate_kbps, visible_windows);
    let codec = match profile.codec {
        GatewayVideoCodec::H264 => vks_media_mcu::farm::FarmCodec::H264,
        GatewayVideoCodec::H265 => vks_media_mcu::farm::FarmCodec::H265,
    };
    vks_media_mcu::farm::EncoderSpec::canonical(
        codec,
        profile.width,
        profile.height,
        profile.fps,
        group_bitrate,
        &profile.profile,
        &profile.level,
    )
}

/// Dispatches H.264/H.265 RTP payload parsing behind one call site.
enum VideoDepacketizer {
    H264(H264Depacketizer),
    H265(H265Depacketizer),
}

impl VideoDepacketizer {
    fn new(codec: GatewayVideoCodec) -> Self {
        match codec {
            GatewayVideoCodec::H264 => Self::H264(H264Depacketizer::default()),
            GatewayVideoCodec::H265 => Self::H265(H265Depacketizer::default()),
        }
    }

    fn push(&mut self, packet: RtpPayload<'_>) -> Result<Vec<Vec<u8>>, ()> {
        match self {
            Self::H264(depacketizer) => depacketizer.push(packet),
            Self::H265(depacketizer) => {
                depacketizer
                    .push(packet.sequence, packet.timestamp, packet.payload)
                    .map_err(|_| ())
            }
        }
    }
}

pub(super) async fn spawn_video_media_bridge(
    session: VideoBridgeSession,
) -> Result<tokio::task::JoinHandle<()>, String> {
    let local_port = session.rtp_socket.local_addr().map_err(|error| error.to_string())?.port();
    let lease = super::RtpPortLease::new(session.port_pool.clone(), local_port);
    if !codec_available_for(session.profile.codec).await {
        return Err(format!(
            "FFmpeg {} encoder/decoder is unavailable; video was not started",
            session.profile.codec.name()
        ));
    }
    // `O1`: холст комнаты = выходной профиль (первая сессия задаёт; после
    // первого рендера размер не меняется). Обязательно ДО `acquire` фермы:
    // смена холста инвалидирует группы (`farm.invalidate_room`), и группа,
    // взятая на старом размере, осталась бы без кадров.
    session.engine.ensure_composite_size(session.room_id, session.profile.width, session.profile.height);
    // `O7`: ферма энкодеров — один FFmpeg-энкодер на группу зрителей комнаты
    // с одинаковым выходным профилем (спека §B). Условия:
    // - комната без self-view (ADR-0029: при самопросмотре кадры зрителей
    //   различаются, групповой битстрим невозможен — такой зритель идёт
    //   старым per-session путём);
    // - FFmpeg-энкодер доступен (проверено выше);
    // - ферма не переполнена (иначе мягкий fallback на per-session, а не отказ).
    // RTP-обвязка остаётся per-viewer: свой SSRC/seq/ts (F10).
    // `P1`: один авторитетный источник видимых окон на комнату — движок.
    // Сессия только читает канал: 10 render-тасков не перетирают значение
    // друг друга (иначе thrash и десятки групп на 10 ботов).
    let visible_rx = session.engine.visible_windows_receiver(session.room_id);
    let visible_windows = *visible_rx.borrow();
    let farm_spec = farm_spec_for(&session.profile, visible_windows);
    let farm_viewer = if session.engine.self_view(session.room_id) || !farm_enabled() {
        None
    } else {
        match session.engine.acquire_video_viewer(session.room_id, &farm_spec).await {
            Ok(viewer) => {
                tracing::debug!(
                    participant = %session.participant_id,
                    room = %session.room_id,
                    output = %format!("{}x{}", farm_spec.width, farm_spec.height),
                    refcount = viewer.refcount(),
                    "viewer attached to encoder group"
                );
                Some(viewer)
            }
            Err(error) => {
                // Fail-open в старый путь: один лишний энкодер лучше, чем
                // потеря видео у зрителя (лимит групп — это M12, не приговор).
                tracing::warn!(
                    participant = %session.participant_id,
                    room = %session.room_id,
                    %error,
                    "encoder farm unavailable; falling back to a per-session encoder"
                );
                None
            }
        }
    };
    let mut encoder = match farm_viewer {
        Some(_) => None,
        None => Some(spawn_codec(true, &session.profile)?),
    };
    let mut decoder = match spawn_codec(false, &session.profile) {
        Ok(child) => child,
        Err(error) => {
            if let Some(encoder) = encoder.as_mut() {
                let _ = encoder.kill().await;
            }
            return Err(error);
        }
    };
    let encoder_errors = encoder.as_mut().map(|child| drain_errors(child, session.profile.codec.name()));
    let decoder_errors = drain_errors(&mut decoder, session.profile.codec.name());
    let encoder_stdin = encoder.as_mut().and_then(|child| child.stdin.take());
    let encoder_stdout = encoder.as_mut().and_then(|child| child.stdout.take());
    let mut decoder_stdin = decoder.stdin.take().expect("decoder stdin");
    let mut decoder_stdout = decoder.stdout.take().expect("decoder stdout");
    if let Some(ref stdin) = encoder_stdin {
        vks_media_mcu::farm::tune_pipe_capacity(stdin, true);
    }
    if let Some(ref stdout) = encoder_stdout {
        vks_media_mcu::farm::tune_pipe_capacity(stdout, false);
    }
    vks_media_mcu::farm::tune_pipe_capacity(&decoder_stdin, false);
    vks_media_mcu::farm::tune_pipe_capacity(&decoder_stdout, true);
    let (frames_tx, mut frames_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(8);
    let (access_tx, mut access_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(16);
    let rtp_socket = Arc::new(session.rtp_socket);
    let worker_failed = Arc::new(AtomicBool::new(false));
    let ssrc = (uuid::Uuid::new_v4().as_u128() as u32).max(1);
    let last_timestamp = Arc::new(AtomicU32::new(0));

    let encoder_input = encoder_stdin.map(|mut encoder_stdin| {
        let encoder_input_failed = worker_failed.clone();
        tokio::spawn(async move {
            while let Some(frame) = frames_rx.recv().await {
                if !matches!(
                    tokio::time::timeout(Duration::from_secs(1), encoder_stdin.write_all(&frame)).await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            encoder_input_failed.store(true, Relaxed);
        })
    });
    let decoder_input_failed = worker_failed.clone();
    let decoder_input = tokio::spawn(async move {
        while let Some(access_unit) = access_rx.recv().await {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(1), decoder_stdin.write_all(&access_unit)).await,
                Ok(Ok(()))
            ) {
                break;
            }
        }
        decoder_input_failed.store(true, Relaxed);
    });
    let decoded_engine = session.engine.clone();
    let decoded_stats = session.stats.clone();
    let decoded_controls = session.controls.clone();
    let room = session.room_id;
    let participant = session.participant_id;
    let video_width = session.profile.width;
    let video_height = session.profile.height;
    let frame_bytes = session.profile.frame_bytes();
    let decoder_output_failed = worker_failed.clone();
    let decoder_output = tokio::spawn(async move {
        loop {
            let mut bytes = vec![0; frame_bytes];
            if decoder_stdout.read_exact(&mut bytes).await.is_err() {
                break;
            }
            if !decoded_controls.video_tx_muted.load(Relaxed)
                && decoded_engine.set_participant_frame(
                    room,
                    participant,
                    vks_media_mcu::frame::Frame::new(video_width, video_height, bytes),
                )
            {
                decoded_stats.decoded_video_frames.fetch_add(1, Relaxed);
            }
        }
        decoder_output_failed.store(true, Relaxed);
    });
    let outgoing_socket = rtp_socket.clone();
    let outgoing_stats = session.stats.clone();
    let outgoing_controls = session.controls.clone();
    let outgoing_timestamp = last_timestamp.clone();
    let target = session.remote_target;
    let payload_type = session.payload_type;
    let output_codec = session.profile.codec;
    // `O7`: кто отдаёт битстрим — свой child (fallback) или группа фермы.
    let farm_mode = farm_viewer.is_some();
    let farm_engine = session.engine.clone();
    let farm_profile = session.profile.clone();
    let farm_output = farm_viewer.map(|mut viewer| {
        // Клоны для выходного таска: внешняя переменная `visible_rx` живёт до
        // конца функции (страховка от «переиспользования»), таск получает свою.
        let mut visible_rx = visible_rx.clone();
        let farm_engine = farm_engine.clone();
        let farm_profile = farm_profile.clone();
        let socket = outgoing_socket.clone();
        let stats = outgoing_stats.clone();
        let controls = outgoing_controls.clone();
        let timestamp_slot = outgoing_timestamp.clone();
        let owner = session.participant_id;
        tokio::spawn(async move {
            let epoch = std::time::Instant::now();
            let mut sequence = 1u16;
            let mut seen_keyframe = false;
            let mut pending_viewer: Option<vks_media_mcu::GroupViewer> = None;
            let mut pending_delta_count = 0usize;
            // `P1`: смена числа видимых окон — новый ключ группы (битрейт
            // масштабируется коэффициентом), а не пересоздание на каждом кадре.
            enum FarmEvent {
                PendingAu(Result<Arc<vks_media_mcu::AccessUnit>, tokio::sync::broadcast::error::RecvError>),
                CurrentAu(Result<Arc<vks_media_mcu::AccessUnit>, tokio::sync::broadcast::error::RecvError>),
                VisibleWindows,
            }
            loop {
                let event = if let Some(ref mut pv) = pending_viewer {
                    tokio::select! {
                        biased;
                        access_unit = pv.rx.recv() => FarmEvent::PendingAu(access_unit),
                        access_unit = viewer.rx.recv() => FarmEvent::CurrentAu(access_unit),
                        _ = visible_rx.changed() => FarmEvent::VisibleWindows,
                    }
                } else {
                    tokio::select! {
                        biased;
                        access_unit = viewer.rx.recv() => FarmEvent::CurrentAu(access_unit),
                        _ = visible_rx.changed() => FarmEvent::VisibleWindows,
                    }
                };
                let access_unit = match event {
                    FarmEvent::VisibleWindows => {
                        let visible = *visible_rx.borrow();
                        let desired = farm_spec_for(&farm_profile, visible);
                        let current_bitrate = pending_viewer
                            .as_ref()
                            .map(|pv| pv.key.spec.bitrate_kbps)
                            .unwrap_or(viewer.key.spec.bitrate_kbps);
                        if desired.bitrate_kbps != current_bitrate {
                            match farm_engine.acquire_video_viewer(room, &desired).await {
                                Ok(new_viewer) => {
                                    tracing::debug!(
                                        participant = %owner,
                                        visible,
                                        old_bitrate = viewer.key.spec.bitrate_kbps,
                                        new_bitrate = desired.bitrate_kbps,
                                        "encoder group re-keyed on visible windows change (make-before-break)"
                                    );
                                    pending_viewer = Some(new_viewer);
                                    pending_delta_count = 0;
                                }
                                Err(error) => tracing::warn!(
                                    participant = %owner,
                                    %error,
                                    "cannot re-key encoder group; keeping current bitrate"
                                ),
                            }
                        }
                        continue;
                    }
                    FarmEvent::PendingAu(Ok(access_unit)) => {
                        if access_unit.is_idr() || pending_delta_count >= 60 {
                            // Make-before-break cutover: atomically swap viewer on first keyframe or fallback threshold
                            viewer = pending_viewer.take().unwrap();
                            seen_keyframe = access_unit.is_idr();
                            pending_delta_count = 0;
                            access_unit
                        } else {
                            pending_delta_count += 1;
                            if let Some(ref pv) = pending_viewer {
                                pv.note_dropped(1);
                            }
                            continue;
                        }
                    }
                    FarmEvent::PendingAu(Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped))) => {
                        stats.dropped_video_frames.fetch_add(dropped, Relaxed);
                        if let Some(ref pv) = pending_viewer {
                            pv.note_dropped(dropped);
                        }
                        continue;
                    }
                    FarmEvent::PendingAu(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                        pending_viewer = None;
                        pending_delta_count = 0;
                        continue;
                    }
                    FarmEvent::CurrentAu(Ok(access_unit)) => access_unit,
                    FarmEvent::CurrentAu(Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped))) => {
                        stats.dropped_video_frames.fetch_add(dropped, Relaxed);
                        viewer.note_dropped(dropped);
                        continue;
                    }
                    FarmEvent::CurrentAu(Err(tokio::sync::broadcast::error::RecvError::Closed)) => {
                        if let Some(pv) = pending_viewer.take() {
                            tracing::info!(participant = %owner, "current encoder closed during handover; promoting pending encoder");
                            viewer = pv;
                            seen_keyframe = false;
                            pending_delta_count = 0;
                            continue;
                        }
                        tracing::info!(participant = %owner, "encoder group closed; ending video egress");
                        break;
                    }
                };
                // IDR-гейт: дельта-кадры до первого ключевого зрителю бесполезны
                // (чёрный экран); ждём ≤ keyint (1 с) (спека §B.5, §B.6).
                if !seen_keyframe {
                    if !access_unit.is_idr() {
                        // `P4a`: дельта до первого IDR — потеря на стороне зрителя.
                        viewer.note_dropped(1);
                        continue;
                    }
                    seen_keyframe = true;
                }
                if controls.video_rx_muted.load(Relaxed) {
                    continue;
                }
                // AU → NAL без start code (формат packetize_*), последний NAL
                // выталкивается искусственным start code.
                let mut splitter = AnnexB::default();
                let mut nals = splitter.push(access_unit.data());
                nals.extend(splitter.push(&[0, 0, 0, 1][..]));
                if nals.is_empty() {
                    continue;
                }
                let timestamp = (epoch.elapsed().as_micros() * 90 / 1000) as u32;
                if send_access_unit(
                    output_codec,
                    &nals,
                    &socket,
                    target,
                    payload_type,
                    ssrc,
                    &mut sequence,
                    timestamp,
                    &stats,
                    Some(&viewer),
                    access_unit.parameter_sets(),
                )
                .await
                {
                    stats.encoded_video_frames.fetch_add(1, Relaxed);
                    timestamp_slot.store(timestamp, Relaxed);
                }
            }
        })
    });
    let encoder_output_failed = worker_failed.clone();
    let encoder_output = encoder_stdout.map(|mut encoder_stdout| {
        let socket = outgoing_socket.clone();
        let stats = outgoing_stats.clone();
        let controls = outgoing_controls.clone();
        let timestamp_slot = outgoing_timestamp.clone();
        tokio::spawn(async move {
            let epoch = std::time::Instant::now();
            let mut splitter = AnnexB::default();
            let mut pending: Vec<Vec<u8>> = Vec::new();
            let mut sequence = 1u16;
            let mut buffer = [0; 16384];
            while let Ok(count) = encoder_stdout.read(&mut buffer).await {
                if count == 0 {
                    break;
                }
                for nal in splitter.push(&buffer[..count]) {
                    if is_access_unit_delimiter(output_codec, &nal) && !pending.is_empty() {
                        let timestamp = (epoch.elapsed().as_micros() * 90 / 1000) as u32;
                        if !controls.video_rx_muted.load(Relaxed)
                            && send_access_unit(
                                output_codec,
                                &pending,
                                &socket,
                                target,
                                payload_type,
                                ssrc,
                                &mut sequence,
                                timestamp,
                                &stats,
                                None,
                                &[],
                            )
                            .await
                        {
                            stats.encoded_video_frames.fetch_add(1, Relaxed);
                            timestamp_slot.store(timestamp, Relaxed);
                        }
                        pending.clear();
                    }
                    if pending.len() >= 256 || pending.iter().map(Vec::len).sum::<usize>() + nal.len() > MAX_NAL_BYTES {
                        pending.clear();
                        stats.dropped_video_frames.fetch_add(1, Relaxed);
                    }
                    pending.push(nal);
                }
            }
            encoder_output_failed.store(true, Relaxed);
        })
    });
    // `O1`: холст комнаты = выходной профиль первой сессии — тогда
    // egress-`resize_i420` на совпадении становится identity-memcpy
    // (8.0 → 0.12 мс на 720p). Если комната уже рендерила или профиль другой,
    // размер не меняется, и зритель получает обычный resize.
    // `keyframe_requests` инкрементируется на PLI/FIR (RTCP). В режиме фермы
    // запрос уходит группе (счётчик per-EncoderKey, спека §B.6): IDR придёт по
    // keyint ≤1 с, форс-IDR рестартом child'а — P4b (рвёт поток всем зрителям).
    // TODO(keyframe): force IDR by restarting the group encoder (P4b).
    let render_keyframe_requests = session.controls.keyframe_requests.clone();
    let render_engine = session.engine.clone();
    let render_cancelled = session.cancelled.clone();
    let render_stats = session.stats.clone();
    let render_profile = session.profile.clone();
    let render_viewer = session.participant_id;
    let render = if farm_mode {
        // Один композит комнаты публикуется всем группам: N сессий зовут
        // `publish`, но кодируется кадр один раз (дедуп внутри фермы).
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(frame_interval(&render_profile));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !render_cancelled.load(Relaxed) {
                interval.tick().await;
                let engine = render_engine.clone();
                let publish = tokio::task::spawn_blocking(move || {
                    let frame = engine.tick_video_for_viewer(room, super::audio::media_clock_ms(), render_viewer)?;
                    let published = engine.publish_video_frame(room, &frame);
                    // `P1`: тик дебаунса — движок сам публикует смену числа
                    // видимых окон, когда значение стабильно ≥500 мс.
                    engine.refresh_visible_windows(room);
                    Some(published)
                })
                .await;
                let _ = publish;
                let requested = render_keyframe_requests.swap(0, Relaxed);
                if requested > 0 {
                    render_engine.request_video_keyframe(room);
                }
            }
        })
    } else {
        tokio::spawn(async move {
            // fps из профиля, а не жёсткие 15 fps: иначе SPS/поле обещают 30,
            // а в эфир уходит 15 кадров/с.
            let frame_interval = frame_interval(&render_profile);
            let mut interval = tokio::time::interval(frame_interval);
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            while !render_cancelled.load(Relaxed) {
                interval.tick().await;
                let engine = render_engine.clone();
                let render_profile = render_profile.clone();
                let rendered = tokio::task::spawn_blocking(move || {
                    // Кадр запрашивается для конкретного получателя (ADR-0029):
                    // сегодня это общий композит, но путь выбора кадра уже
                    // персональный — персональная дельта добавится без
                    // изменения egress'а.
                    let frame = engine.tick_video_for_viewer(room, super::audio::media_clock_ms(), render_viewer)?;
                    Some(resize_i420(&frame, &render_profile))
                })
                .await;
                if let Ok(Some(bytes)) = rendered {
                    if frames_tx.try_send(bytes).is_err() {
                        render_stats.dropped_video_frames.fetch_add(1, Relaxed);
                    }
                }
                let requested = render_keyframe_requests.swap(0, Relaxed);
                if requested > 0 {
                    tracing::debug!(
                        participant = %render_viewer,
                        requested,
                        "keyframe request consumed; IDR deferred to the group encoder (keyint=15)"
                    );
                }
            }
        })
    };
    session.stats.video_codec_running.store(true, Relaxed);
    // Startup IDR Burst: trigger 3 keyframe bursts at t = 0, 500, 1000 ms to kickstart
    // IDR emission for the new participant on the MCU room side, aborted cleanly on session cancel.
    let burst_cancelled = session.cancelled.clone();
    let burst_engine = session.engine.clone();
    let burst_controls = session.controls.keyframe_requests.clone();
    let burst_room = session.room_id;
    tokio::spawn(async move {
        for delay_ms in [0, 500, 500] {
            if delay_ms > 0 {
                tokio::time::sleep(Duration::from_millis(delay_ms)).await;
            }
            if burst_cancelled.load(Relaxed) {
                break;
            }
            burst_controls.fetch_add(1, Relaxed);
            burst_engine.request_video_keyframe(burst_room);
            tracing::debug!(room = %burst_room, delay_ms, "Startup IDR burst dispatched to MCU room engine");
        }
    });
    Ok(tokio::spawn(async move {
        let _lease = lease;
        let mut source = super::audio::RtpSource::new(session.remote_target, session.receive_payload_type);
        // `P0-ingest`: видео-вход принимает переупорядоченные seq (джиттер
        // bot→MCU); раскладывает их `VideoReorderBuffer` ниже.
        source.accept_reordered();
        source.accept_alternate_payload_type(Some(session.payload_type));
        source.accept_dynamic_payloads();
        let input_codec = session.profile.codec;
        let mut depacketizer = VideoDepacketizer::new(input_codec);
        let mut reorder = VideoReorderBuffer::default();
        let mut pending_access = Vec::new();
        let mut last_access_ts = None;
        let mut receive = [0u8; 65536];
        let mut rtcp_receive = [0u8; 2048];
        let mut interval = tokio::time::interval(Duration::from_millis(20));
        let mut last_rtcp = std::time::Instant::now() - Duration::from_secs(5);
        let mut last_keyframe_request = std::time::Instant::now() - Duration::from_secs(5);
        let mut startup_pli_sent: usize = 0;
        while !session.cancelled.load(Relaxed) && !worker_failed.load(Relaxed) {
            tokio::select! {
                _ = interval.tick() => {
                    if last_rtcp.elapsed() >= Duration::from_secs(1) {
                        if let Some(socket) = &session.rtcp_socket {
                            if let Some(port) = session.remote_target.port().checked_add(1) {
                                let destination = std::net::SocketAddr::new(session.remote_target.ip(), port);
                                let report = sender_report(ssrc, last_timestamp.load(Relaxed), &session.stats);
                                let _ = socket.send_to(&report, destination).await;
                                if startup_pli_sent < 3 {
                                    startup_pli_sent += 1;
                                    let remote_ssrc = source.ssrc().unwrap_or(0);
                                    let mut pli = vec![0x81, 206, 0, 2];
                                    pli.extend_from_slice(&ssrc.to_be_bytes());
                                    pli.extend_from_slice(&remote_ssrc.to_be_bytes());
                                    let _ = socket.send_to(&pli, destination).await;
                                }
                            }
                        }
                        last_rtcp = std::time::Instant::now();
                    }
                    // `P0-ingest`: реальная потеря пакета не должна стопорить
                    // вход — пропускаем дыру, если буфер ждёт дольше 30 мс.
                    for ordered in reorder.flush(std::time::Instant::now()) {
                        ingest_video_packet(
                            ordered,
                            input_codec,
                            &mut depacketizer,
                            &mut pending_access,
                            &mut last_access_ts,
                            &access_tx,
                            &session.stats,
                        );
                    }
                    session.stats.video_rx_lost_packets.store(reorder.gaps + reorder.late, Relaxed);
                    let encoder_dead = encoder
                        .as_mut()
                        .is_some_and(|child| child.try_wait().ok().flatten().is_some());
                    if encoder_dead || decoder.try_wait().ok().flatten().is_some() { break; }
                }
                rtcp_res = async {
                    if let Some(socket) = &session.rtcp_socket {
                        socket.recv_from(&mut rtcp_receive).await.ok()
                    } else {
                        futures_util::future::pending().await
                    }
                } => {
                    if let Some((count, _addr)) = rtcp_res {
                        let feedbacks = vks_gateway_sip::rtcp::parse_feedback(&rtcp_receive[..count]);
                        if !feedbacks.is_empty() {
                            let now = std::time::Instant::now();
                            if now.duration_since(last_keyframe_request) >= Duration::from_secs(1) {
                                last_keyframe_request = now;
                                tracing::debug!(room = %session.room_id, "RTCP PLI/FIR feedback received; requesting keyframe");
                                session.engine.request_video_keyframe(session.room_id);
                                session.controls.keyframe_requests.fetch_add(1, Relaxed);
                            }
                        }
                    }
                }
                received = rtp_socket.recv_from(&mut receive) => {
                    let Ok((count, address)) = received else { break; };
                    if vks_gateway_sip::rtcp::is_rtcp(&receive[..count]) {
                        let feedbacks = vks_gateway_sip::rtcp::parse_feedback(&receive[..count]);
                        if !feedbacks.is_empty() {
                            let now = std::time::Instant::now();
                            if now.duration_since(last_keyframe_request) >= Duration::from_secs(1) {
                                last_keyframe_request = now;
                                tracing::debug!(room = %session.room_id, "rtcp-mux PLI/FIR feedback received; requesting keyframe");
                                session.engine.request_video_keyframe(session.room_id);
                                session.controls.keyframe_requests.fetch_add(1, Relaxed);
                            }
                        }
                        continue;
                    }
                    let Some(packet) = source.accept(address, &receive[..count]) else {
                        session.stats.rejected_packets.fetch_add(1, Relaxed); continue;
                    };
                    let now = std::time::Instant::now();
                    let jitter = session.stats.jitter_calc.update(packet.timestamp, now, 90_000);
                    session.stats.video_rx_jitter_ms.store(jitter, Relaxed);
                    session.stats.video_rx_packets.fetch_add(1, Relaxed);
                    session.stats.video_rx_bytes.fetch_add(count as u64, Relaxed);
                    if session.controls.video_tx_muted.load(Relaxed) { continue; }
                    // `P0-ingest`: сначала в reorder-окно, депакетизатор получает
                    // пакеты строго по seq (иначе FU-A рвался на первом reorder).
                    let ready = reorder.push(BufferedRtp {
                        sequence: packet.sequence,
                        timestamp: packet.timestamp,
                        marker: packet.marker,
                        payload: packet.payload.to_vec(),
                        arrived: now,
                    });
                    session.stats.video_rx_lost_packets.store(reorder.gaps + reorder.late, Relaxed);
                    for ordered in ready {
                        ingest_video_packet(
                            ordered,
                            input_codec,
                            &mut depacketizer,
                            &mut pending_access,
                            &mut last_access_ts,
                            &access_tx,
                            &session.stats,
                        );
                    }
                }
            }
        }
        for task in [decoder_input, decoder_output, render, decoder_errors] {
            task.abort();
            let _ = task.await;
        }
        for task in [encoder_input, encoder_output, farm_output, encoder_errors].into_iter().flatten() {
            task.abort();
            let _ = task.await;
        }
        if let Some(encoder) = encoder.as_mut() {
            let _ = encoder.kill().await;
            let _ = encoder.wait().await;
        }
        let _ = decoder.kill().await;
        let _ = decoder.wait().await;
        session.stats.video_codec_running.store(false, Relaxed);
        tracing::info!(
            participant = %session.participant_id,
            codec = session.profile.codec.name(),
            "video media bridge and codec workers stopped"
        );
    }))
}

/// Масштабирует кадр холста в I420 выходного профиля.
///
/// `O3`: строчные срезы + инкрементальный сэмплер Брезенхема вместо
/// `x * source_stride / plane_width` на каждый пиксель. Микробенч показал, что
/// деление давало лишь ≤8 % времени: ~60 % стоили проверки делителя (паника на
/// нуле) и bounds-checked `get().unwrap_or(16)` на каждый сэмпл. Измерено на
/// E5-2699 v3: 8.02–8.58 мс/кадр (1920×1080→720p) до правки и 2.02–2.78 мс
/// после; identity — `copy_from_slice` 0.26 мс на 1080p.
///
/// Карта сэмплинга — та же точная floor-формула `floor(x*stride/width)`, поэтому
/// выход побайтово совпадает со старой реализацией (пинует тест
/// `resize_matches_reference_sampler`).
fn resize_i420(frame: &vks_media_mcu::frame::Frame, profile: &VideoProfile) -> Vec<u8> {
    let width = profile.width as usize;
    let height = profile.height as usize;
    let frame_bytes = profile.frame_bytes();
    let mut output = vec![128; frame_bytes];
    let luma_pixels = width * height;
    output[..luma_pixels].fill(16);
    if frame.width == 0 || frame.height == 0 || width == 0 || height == 0 {
        return output;
    }
    let source_width = frame.width as usize;
    let source_height = frame.height as usize;
    // `O1`: холст комнаты уже равен профилю — это чистый memcpy.
    if frame.is_yuv420p() && source_width == width && source_height == height && frame.data.len() >= frame_bytes {
        output.copy_from_slice(&frame.data[..frame_bytes]);
        return output;
    }
    for plane in 0..if frame.is_yuv420p() { 3 } else { 1 } {
        let divisor = if plane == 0 { 1 } else { 2 };
        let plane_width = width / divisor;
        let plane_height = height / divisor;
        let source_stride = source_width / divisor;
        let source_rows = source_height / divisor;
        if plane_width == 0 || plane_height == 0 || source_stride == 0 || source_rows == 0 {
            continue;
        }
        let source_offset = if plane == 0 {
            0
        } else {
            source_width * source_height + (plane - 1) * (source_width * source_height / 4)
        };
        let output_offset = if plane == 0 { 0 } else { luma_pixels + (plane - 1) * (luma_pixels / 4) };
        let (step_x, rem_x) = (source_stride / plane_width, source_stride % plane_width);
        let (step_y, rem_y) = (source_rows / plane_height, source_rows % plane_height);
        let mut source_y = 0usize;
        let mut y_acc = 0usize;
        for y in 0..plane_height {
            let row_start = output_offset + y * plane_width;
            let Some(output_row) = output.get_mut(row_start..row_start + plane_width) else {
                source_y += step_y;
                y_acc += rem_y;
                if y_acc >= plane_height {
                    y_acc -= plane_height;
                    source_y += 1;
                }
                continue;
            };
            let source_start = source_offset + source_y * source_stride;
            match frame.data.get(source_start..source_start + source_stride) {
                Some(source_row) => sample_row_nearest(output_row, source_row, step_x, rem_x),
                // Как и раньше: отсутствующие сэмплы — ограниченный чёрный 16.
                None => output_row.fill(16),
            }
            source_y += step_y;
            y_acc += rem_y;
            if y_acc >= plane_height {
                y_acc -= plane_height;
                source_y += 1;
            }
        }
    }
    output
}

/// Nearest-neighbour выборка строки: `destination[x] = source[floor(x*src_len/dst_len)]`
/// при `src_len = step*dst_len + rem`.
///
/// Exact-integer downscale (`rem == 0`) берёт каждый `step`-й сэмпл, identity
/// (`step == 1`) — чистый `copy_from_slice`; общий случай — инкрементальный
/// Bresenham без деления в теле цикла.
fn sample_row_nearest(destination: &mut [u8], source: &[u8], step: usize, rem: usize) {
    let width = destination.len();
    if width == 0 {
        return;
    }
    if step == 1 && rem == 0 {
        if let Some(head) = source.get(..width) {
            destination.copy_from_slice(head);
        }
        return;
    }
    if rem == 0 && step > 1 {
        // `step_by`, а не `chunks_exact`: (width-1)*step+1 может быть нечётным,
        // и `chunks_exact` потерял бы последний сэмпл (это ловит тест на 2:1).
        for (pixel, sample) in destination.iter_mut().zip(source.iter().step_by(step)) {
            *pixel = *sample;
        }
        return;
    }
    let mut index = 0usize;
    let mut acc = 0usize;
    for pixel in destination.iter_mut() {
        *pixel = source.get(index).copied().unwrap_or(16);
        index += step;
        acc += rem;
        if acc >= width {
            acc -= width;
            index += 1;
        }
    }
}

#[derive(Default)]
struct AnnexB {
    buffer: Vec<u8>,
}

impl AnnexB {
    fn push(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        self.buffer.extend_from_slice(bytes);
        if self.buffer.len() > MAX_NAL_BYTES {
            self.buffer.clear();
            return Vec::new();
        }
        let mut units = Vec::new();
        loop {
            let Some((first, prefix)) = find_start_code(&self.buffer, 0) else {
                break;
            };
            let Some((next, _)) = find_start_code(&self.buffer, first + prefix) else {
                if first > 0 {
                    self.buffer.drain(..first);
                }
                break;
            };
            if next > first + prefix {
                units.push(self.buffer[first + prefix..next].to_vec());
            }
            self.buffer.drain(..next);
        }
        units
    }
}

fn find_start_code(bytes: &[u8], start: usize) -> Option<(usize, usize)> {
    (start..bytes.len().saturating_sub(2)).find_map(|position| {
        if bytes.get(position..position + 4) == Some(&[0, 0, 0, 1][..]) {
            Some((position, 4))
        } else if bytes.get(position..position + 3) == Some(&[0, 0, 1][..]) {
            Some((position, 3))
        } else {
            None
        }
    })
}

fn packetize_h264<T: AsRef<[u8]>>(
    units: &[T],
    payload_type: u8,
    ssrc: u32,
    sequence: &mut u16,
    timestamp: u32,
) -> Vec<Vec<u8>> {
    let clean_nals: Vec<&[u8]> = units
        .iter()
        .map(|u| strip_start_code(u.as_ref()))
        .filter(|u| !u.is_empty() && (u[0] & 0x1f) != 0 && (u[0] & 0x80) == 0)
        .collect();
    let mut packets = Vec::new();
    let nals_count = clean_nals.len();
    for (index, nal) in clean_nals.iter().enumerate() {
        let is_last_nal = index + 1 == nals_count;
        if nal.len() <= 1188 {
            let mut packet = vec![0; 12];
            vks_gateway_sip::write_rtp_header(
                &mut packet,
                payload_type,
                *sequence,
                timestamp,
                ssrc,
                is_last_nal,
            );
            *sequence = sequence.wrapping_add(1);
            packet.extend_from_slice(nal);
            packets.push(packet);
        } else {
            let chunks: Vec<&[u8]> = nal[1..].chunks(1186).collect();
            let chunks_count = chunks.len();
            for (part, chunk) in chunks.iter().enumerate() {
                let is_last_chunk = part + 1 == chunks_count;
                let mut packet = vec![0; 12];
                vks_gateway_sip::write_rtp_header(
                    &mut packet,
                    payload_type,
                    *sequence,
                    timestamp,
                    ssrc,
                    is_last_nal && is_last_chunk,
                );
                *sequence = sequence.wrapping_add(1);
                packet.push((nal[0] & 0xe0) | 28);
                packet.push(
                    (nal[0] & 31)
                        | if part == 0 {
                            0x80
                        } else if is_last_chunk {
                            0x40
                        } else {
                            0
                        },
                );
                packet.extend_from_slice(chunk);
                packets.push(packet);
            }
        }
    }
    // RFC 6184 §5.1: post-condition guarantee — exactly the last emitted packet of AU has M=1
    if let Some(last) = packets.last_mut() {
        last[1] |= 0x80;
    }
    packets
}

fn sender_report(ssrc: u32, timestamp: u32, stats: &GatewayMediaStats) -> Vec<u8> {
    super::audio::rtcp_sender_report(
        ssrc,
        timestamp,
        stats.video_tx_packets.load(Relaxed),
        stats.video_tx_bytes.load(Relaxed),
    )
}

const MAX_NAL_BYTES: usize = 2 * 1024 * 1024;

/// `P0-ingest`: окно реордера RTP на входе видео (пакеты).
///
/// Джиттер bot→MCU переставляет пакеты. До фикса: `RtpSource` отбрасывал
/// seq «старше последнего», а `H264Depacketizer` требовал `seq == last+1` для
/// FU-A — один переставленный пакет убивал кадр, декодер получал рвань, и
/// видео падало в ~100 раз при нулевых потерях. Теперь пакеты копятся в окне
/// и выдаются строго по порядку; реальная дыра (потеря) пропускается после
/// [`REORDER_MAX_DELAY`] или переполнения окна.
const REORDER_WINDOW_PACKETS: usize = 32;
const REORDER_MAX_DELAY: Duration = Duration::from_millis(30);
/// Сколько подряд «опоздавших» пакетов терпим, прежде чем считать, что
/// отправитель перезапустил нумерацию (seq сбросился): тогда окно
/// пере-синхронизируется на новый seq, иначе поток навсегда остался бы «late».
const REORDER_RESYNC_AFTER: u64 = 64;

struct BufferedRtp {
    sequence: u16,
    timestamp: u32,
    marker: bool,
    payload: Vec<u8>,
    arrived: std::time::Instant,
}

#[derive(Default)]
struct VideoReorderBuffer {
    next: Option<u16>,
    buffered: Vec<BufferedRtp>,
    late: u64,
    gaps: u64,
    late_streak: u64,
}

impl VideoReorderBuffer {
    fn push(&mut self, packet: BufferedRtp) -> Vec<BufferedRtp> {
        let next = *self.next.get_or_insert(packet.sequence);
        // Расстояние вперёд по модулю 2^16: <0 — опоздавший/уже отданный.
        if (packet.sequence.wrapping_sub(next) as i16) < 0 {
            self.late += 1;
            self.late_streak += 1;
            // Отправитель, вероятно, перезапустил нумерацию: без пере-синхры
            // все его пакеты считались бы опоздавшими до конца вызова.
            if self.late_streak >= REORDER_RESYNC_AFTER {
                self.next = Some(packet.sequence);
                self.late_streak = 0;
                let mut ready = Vec::new();
                self.drain(&mut ready);
                return ready;
            }
            return Vec::new();
        }
        self.late_streak = 0;
        if self.buffered.iter().any(|known| known.sequence == packet.sequence) {
            return Vec::new();
        }
        self.buffered.push(packet);
        let mut ready = Vec::new();
        self.drain(&mut ready);
        while self.buffered.len() > REORDER_WINDOW_PACKETS {
            self.skip_gap();
            self.drain(&mut ready);
        }
        ready
    }

    /// Тик (20 мс): если самый старый буферизованный пакет ждёт дольше
    /// [`REORDER_MAX_DELAY`], значит пакет `next` реально потерян — пропускаем
    /// дыру, иначе один потерянный пакет застопорил бы весь вход.
    fn flush(&mut self, now: std::time::Instant) -> Vec<BufferedRtp> {
        let mut ready = Vec::new();
        let stale =
            self.buffered.iter().any(|packet| now.saturating_duration_since(packet.arrived) >= REORDER_MAX_DELAY);
        if stale {
            self.skip_gap();
            self.drain(&mut ready);
        }
        ready
    }

    fn drain(&mut self, ready: &mut Vec<BufferedRtp>) {
        loop {
            let Some(next) = self.next else { return };
            let Some(index) = self.buffered.iter().position(|packet| packet.sequence == next) else {
                return;
            };
            ready.push(self.buffered.remove(index));
            self.next = Some(next.wrapping_add(1));
        }
    }

    fn skip_gap(&mut self) {
        let Some(next) = self.next else { return };
        let Some((index, _)) =
            self.buffered.iter().enumerate().min_by_key(|(_, packet)| packet.sequence.wrapping_sub(next) as i16)
        else {
            return;
        };
        self.next = Some(self.buffered[index].sequence);
        self.gaps += 1;
    }
}

/// Отдаёт один (уже упорядоченный) пакет депакетизатору и, по маркеру кадра,
/// готовый Annex-B access unit в канал декодера.
fn ingest_video_packet(
    packet: BufferedRtp,
    codec: GatewayVideoCodec,
    depacketizer: &mut VideoDepacketizer,
    pending_access: &mut Vec<u8>,
    last_access_ts: &mut Option<u32>,
    access_tx: &tokio::sync::mpsc::Sender<Vec<u8>>,
    stats: &GatewayMediaStats,
) {
    if last_access_ts.is_some_and(|timestamp| timestamp != packet.timestamp) {
        pending_access.clear();
    }
    *last_access_ts = Some(packet.timestamp);
    let marker = packet.marker;
    let payload = RtpPayload {
        payload: &packet.payload,
        payload_type: 0,
        sequence: packet.sequence,
        timestamp: packet.timestamp,
        marker,
    };
    match depacketizer.push(payload) {
        Ok(units) => {
            for nal in units {
                if pending_access.len() + nal.len() + 4 > MAX_NAL_BYTES {
                    pending_access.clear();
                    break;
                }
                pending_access.extend_from_slice(&[0, 0, 0, 1]);
                pending_access.extend(nal);
            }
            if marker && !pending_access.is_empty() {
                pending_access.extend_from_slice(injected_aud(codec));
                if access_tx.try_send(std::mem::take(pending_access)).is_err() {
                    stats.dropped_video_frames.fetch_add(1, Relaxed);
                }
            }
        }
        Err(()) => {
            pending_access.clear();
            stats.rejected_packets.fetch_add(1, Relaxed);
        }
    }
}

#[derive(Default)]
struct H264Depacketizer {
    fragment: Vec<u8>,
    last_sequence: u16,
    timestamp: u32,
}

impl H264Depacketizer {
    fn push(&mut self, packet: RtpPayload<'_>) -> Result<Vec<Vec<u8>>, ()> {
        let data = packet.payload;
        let Some(&header) = data.first() else {
            return Err(());
        };
        if header & 0x80 != 0 || data.len() > MAX_NAL_BYTES {
            self.fragment.clear();
            return Err(());
        }
        match header & 31 {
            1..=23 => {
                self.fragment.clear();
                Ok(vec![data.to_vec()])
            }
            24 => {
                self.fragment.clear();
                let mut position = 1;
                let mut units = Vec::new();
                while position < data.len() {
                    if position + 2 > data.len() {
                        return Err(());
                    }
                    let count = usize::from(u16::from_be_bytes([data[position], data[position + 1]]));
                    position += 2;
                    if count == 0 || position + count > data.len() {
                        return Err(());
                    }
                    let unit = &data[position..position + count];
                    if unit[0] & 0x80 != 0 || !(1..=23).contains(&(unit[0] & 31)) {
                        return Err(());
                    }
                    units.push(unit.to_vec());
                    position += count;
                }
                if units.is_empty() {
                    Err(())
                } else {
                    Ok(units)
                }
            }
            28 => {
                if data.len() < 3 {
                    self.fragment.clear();
                    return Err(());
                }
                let fu = data[1];
                let start = fu & 0x80 != 0;
                let end = fu & 0x40 != 0;
                let reconstructed = (header & 0xe0) | (fu & 31);
                if fu & 0x20 != 0 || !(1..=23).contains(&(fu & 31)) || start && end {
                    self.fragment.clear();
                    return Err(());
                }
                if start {
                    self.fragment.clear();
                    self.fragment.push(reconstructed);
                    self.timestamp = packet.timestamp;
                } else if self.fragment.is_empty()
                    || packet.sequence != self.last_sequence.wrapping_add(1)
                    || packet.timestamp != self.timestamp
                    || self.fragment[0] != reconstructed
                {
                    self.fragment.clear();
                    return Err(());
                }
                self.last_sequence = packet.sequence;
                if self.fragment.len() + data.len() - 2 > MAX_NAL_BYTES {
                    self.fragment.clear();
                    return Err(());
                }
                self.fragment.extend_from_slice(&data[2..]);
                if end {
                    Ok(vec![std::mem::take(&mut self.fragment)])
                } else {
                    Ok(Vec::new())
                }
            }
            _ => {
                self.fragment.clear();
                Err(())
            }
        }
    }
}

/// Применяет настройки к профилю: global `max_frame_rate`, затем payload
/// комнаты (`bitrate`, `resolution.width/height/fps_limit`) — комната
/// приоритетнее. Вынесено из `GatewayManager`, чтобы проверять без AppState.
pub(super) fn apply_video_settings(
    profile: &mut VideoProfile,
    global: &serde_json::Value,
    conference: Option<&serde_json::Value>,
) {
    if let Some(fps) = global.get("max_frame_rate").and_then(serde_json::Value::as_u64) {
        profile.fps = u32::try_from(fps.clamp(1, 60)).unwrap_or(profile.fps);
    }
    if let Some(value) = conference {
        let payload = value.get("payload").unwrap_or(value);
        if let Some(bitrate) = payload.get("bitrate").and_then(serde_json::Value::as_u64) {
            profile.bitrate_kbps = u32::try_from(bitrate.clamp(64, 8192)).unwrap_or(profile.bitrate_kbps);
        }
        if let Some(resolution) = payload.get("resolution") {
            if let Some(width) = resolution.get("width").and_then(serde_json::Value::as_u64) {
                profile.width = u32::try_from(width.clamp(16, 1920)).unwrap_or(profile.width);
            }
            if let Some(height) = resolution.get("height").and_then(serde_json::Value::as_u64) {
                profile.height = u32::try_from(height.clamp(16, 1080)).unwrap_or(profile.height);
            }
            if let Some(fps) = resolution.get("fps_limit").and_then(serde_json::Value::as_u64) {
                profile.fps = u32::try_from(fps.clamp(1, 60)).unwrap_or(profile.fps);
            }
        }
    }
    normalize_profile(profile);
}

/// Профиль конкретного адреса (`call_string.sip/h323`) — высший приоритет:
/// оператор задал его для этого терминала, а не для всей комнаты.
pub(super) fn apply_call_string_settings(profile: &mut VideoProfile, value: &serde_json::Value) {
    let payload = value.get("payload").unwrap_or(value);
    if let Some(bitrate) = payload.get("bitrate").and_then(serde_json::Value::as_u64) {
        profile.bitrate_kbps = u32::try_from(bitrate.clamp(64, 8192)).unwrap_or(profile.bitrate_kbps);
    }
    if let Some(resolution) = payload.get("resolution") {
        if let Some(width) = resolution.get("width").and_then(serde_json::Value::as_u64) {
            profile.width = u32::try_from(width.clamp(16, 1920)).unwrap_or(profile.width);
        }
        if let Some(height) = resolution.get("height").and_then(serde_json::Value::as_u64) {
            profile.height = u32::try_from(height.clamp(16, 1080)).unwrap_or(profile.height);
        }
        if let Some(fps) = resolution.get("fps_limit").and_then(serde_json::Value::as_u64) {
            profile.fps = u32::try_from(fps.clamp(1, 60)).unwrap_or(profile.fps);
        }
    }
    normalize_profile(profile);
}

/// H.264 level_idc → имя для FFmpeg (`-level`), только валидные уровни.
pub(super) fn h264_level_name(level_idc: u8) -> Option<&'static str> {
    Some(match level_idc {
        30 => "3.0",
        31 => "3.1",
        32 => "3.2",
        40 => "4.0",
        41 => "4.1",
        42 => "4.2",
        50 => "5.0",
        51 => "5.1",
        _ => return None,
    })
}

fn normalize_profile(profile: &mut VideoProfile) {
    profile.width = (profile.width.clamp(160, 1920) / 2) * 2;
    profile.height = (profile.height.clamp(120, 1080) / 2) * 2;
    profile.level = h264_level(profile.width, profile.height, profile.fps).to_string();
}

/// Каденция рендера из профиля: 30 fps → 33 333 мкс, 15 fps → 66 666 мкс.
fn frame_interval(profile: &VideoProfile) -> Duration {
    Duration::from_micros(1_000_000 / u64::from(profile.fps.max(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Старая реализация `resize_i420` — эталон карты сэмплинга (floor-формула
    /// на каждый пиксель). Новая обязана совпадать с ней побайтово.
    fn reference_resize(frame: &vks_media_mcu::frame::Frame, profile: &VideoProfile) -> Vec<u8> {
        let width = profile.width as usize;
        let height = profile.height as usize;
        let mut output = vec![128; profile.frame_bytes()];
        output[..width * height].fill(16);
        if frame.width == 0 || frame.height == 0 {
            return output;
        }
        let source_width = frame.width as usize;
        let source_height = frame.height as usize;
        for plane in 0..if frame.is_yuv420p() { 3 } else { 1 } {
            let divisor = if plane == 0 { 1 } else { 2 };
            let plane_width = width / divisor;
            let plane_height = height / divisor;
            let source_stride = source_width / divisor;
            let source_rows = source_height / divisor;
            let source_offset = if plane == 0 {
                0
            } else {
                source_width * source_height + (plane - 1) * source_width * source_height / 4
            };
            let output_offset = if plane == 0 { 0 } else { width * height + (plane - 1) * width * height / 4 };
            for y in 0..plane_height {
                let source_y = y * source_rows / plane_height;
                for x in 0..plane_width {
                    let source_x = x * source_stride / plane_width;
                    output[output_offset + y * plane_width + x] =
                        frame.data.get(source_offset + source_y * source_stride + source_x).copied().unwrap_or(16);
                }
            }
        }
        output
    }

    fn pattern_i420(width: u32, height: u32) -> vks_media_mcu::frame::Frame {
        let pixels = (width * height) as usize;
        let mut data = vec![0u8; pixels * 3 / 2];
        for y in 0..height as usize {
            for x in 0..width as usize {
                data[y * width as usize + x] = ((x * 7 + y * 13) % 251) as u8;
            }
        }
        for (i, sample) in data[pixels..].iter_mut().enumerate() {
            *sample = ((i * 29) % 256) as u8;
        }
        vks_media_mcu::frame::Frame::new(width, height, data)
    }

    fn profile_of(width: u32, height: u32) -> VideoProfile {
        VideoProfile { width, height, ..VideoProfile::default() }
    }

    /// `P1`: битрейт группы масштабируется по видимым окнам и клампится;
    /// смена числа окон даёт **новый ключ группы** (новый энкодер).
    #[test]
    fn farm_spec_scales_bitrate_by_visible_windows_and_rekeys_group() {
        let profile = VideoProfile { bitrate_kbps: 2048, ..profile_of(1280, 720) };
        let single = farm_spec_for(&profile, 1);
        assert_eq!(single.bitrate_kbps, 2048, "N=1 → base");
        assert_eq!(single.maxrate_kbps, 2048, "maxrate сохраняется");
        assert_eq!(single.bufsize_kbps, 4096, "bufsize = 2 × bitrate");
        let empty = farm_spec_for(&profile, 0);
        assert_eq!(empty.bitrate_kbps, 2048, "visible=0 → base");
        let crowded = farm_spec_for(&profile, 20);
        assert!(crowded.bitrate_kbps >= 2048, "не ниже base");
        assert!(crowded.bitrate_kbps <= 2560, "не выше 1.25 × base");
        assert_eq!(crowded.bitrate_kbps, 2560, "20 окон → кламп 1.25 × base");
        assert_eq!(crowded.maxrate_kbps, crowded.bitrate_kbps, "maxrate масштабируется тем же коэффициентом");
        assert_eq!(crowded.bufsize_kbps, crowded.bitrate_kbps * 2, "bufsize масштабируется");
        let room = vks_core::RoomId::new();
        assert_ne!(
            farm_spec_for(&profile, 1).key(room, 0),
            farm_spec_for(&profile, 20).key(room, 0),
            "смена числа окон → новый ключ группы (не пересоздание на каждом кадре)"
        );
        assert_eq!(
            farm_spec_for(&profile, 20).key(room, 0),
            farm_spec_for(&profile, 30).key(room, 0),
            "оба значения в клампе дают один ключ — лишних пере-ключёвок нет"
        );
    }

    #[test]
    fn resize_matches_reference_sampler() {
        // (источник, профиль): downscale 16:9, 4:3-источник в 16:9, upscale,
        // нечётные размеры источника/профиля, точные целые соотношения.
        let cases = [
            ((1920u32, 1080u32), (1280u32, 720u32)),
            ((1280, 720), (640, 360)),
            ((1280, 720), (1280, 720)),
            ((720, 576), (1280, 720)),
            ((1920, 1080), (1920, 1080)),
            ((640, 360), (1920, 1080)),
            ((333, 187), (640, 360)),
            ((1000, 1000), (1280, 720)),
            ((1280, 720), (426, 240)),
            ((960, 540), (1280, 720)),
        ];
        for (source, target) in cases {
            let frame = pattern_i420(source.0, source.1);
            let profile = profile_of(target.0, target.1);
            let got = resize_i420(&frame, &profile);
            let want = reference_resize(&frame, &profile);
            assert_eq!(got.len(), profile.frame_bytes(), "длина выхода {source:?} → {target:?}");
            assert_eq!(got, want, "побайтовое расхождение {source:?} → {target:?}");
        }
    }

    #[test]
    fn resize_identity_is_memcpy_and_keeps_chroma() {
        let profile = profile_of(1280, 720);
        let frame = pattern_i420(1280, 720);
        let out = resize_i420(&frame, &profile);
        assert_eq!(out, frame.data, "identity-путь не должен ничего менять");
    }

    #[test]
    fn resize_odd_dimensions_and_edges_do_not_panic() {
        // Нечётный профиль: chroma-плоскости усекаются (w/2, h/2).
        let frame = pattern_i420(63, 47);
        let out = resize_i420(&frame, &profile_of(32, 18));
        assert_eq!(out.len(), 32 * 18 * 3 / 2);
        // Пустой источник → чёрный кадр нужного размера.
        let empty = vks_media_mcu::frame::Frame::new(0, 0, Vec::new());
        let out = resize_i420(&empty, &profile_of(64, 48));
        assert_eq!(out.len(), 64 * 48 * 3 / 2);
        assert!(out[..64 * 48].iter().all(|&s| s == 16));
        assert!(out[64 * 48..].iter().all(|&s| s == 128));
        // Серый (не-YUV) кадр: пишется только luma, chroma остаётся 128.
        let gray = vks_media_mcu::frame::Frame::new(100, 99, vec![42u8; 100 * 99]);
        let out = resize_i420(&gray, &profile_of(50, 50));
        assert_eq!(out.len(), 50 * 50 * 3 / 2);
        assert!(out[..50 * 50].iter().all(|&s| s == 42));
        assert!(out[50 * 50..].iter().all(|&s| s == 128));
    }

    #[test]
    fn h241_level_names_are_whitelisted() {
        for (idc, name) in
            [(30, "3.0"), (31, "3.1"), (32, "3.2"), (40, "4.0"), (41, "4.1"), (42, "4.2"), (50, "5.0"), (51, "5.1")]
        {
            assert_eq!(h264_level_name(idc), Some(name));
        }
        for invalid in [0, 35, 64, 75, 99] {
            assert_eq!(h264_level_name(invalid), None);
        }
    }

    #[test]
    fn h323_level_follows_explicit_level_then_geometry() {
        let explicit = VideoProfile { level: "4.0".into(), ..VideoProfile::default() };
        assert_eq!(explicit.h323_level_idc(), 40);
        let hd = VideoProfile { width: 1920, height: 1080, fps: 30, level: String::new(), ..VideoProfile::default() };
        assert_eq!(hd.h323_level_idc(), 40);
        let hd60 = VideoProfile { width: 1920, height: 1080, fps: 60, level: String::new(), ..VideoProfile::default() };
        assert_eq!(hd60.h323_level_idc(), 42);
        let sd = VideoProfile { width: 640, height: 360, fps: 30, level: String::new(), ..VideoProfile::default() };
        assert_eq!(sd.h323_level_idc(), 30);
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg with libx264"]
    async fn ffmpeg_encoder_sps_respects_negotiated_level_30() {
        let profile = VideoProfile { level: "3.0".into(), ..VideoProfile::default() };
        let mut encoder = spawn_codec(true, &profile).unwrap();
        let mut stdin = encoder.stdin.take().unwrap();
        let mut stdout = encoder.stdout.take().unwrap();
        let mut frame = vec![128; profile.frame_bytes()];
        frame[..(profile.width * profile.height) as usize].fill(76);
        stdin.write_all(&frame).await.unwrap();
        stdin.write_all(&frame).await.unwrap();
        drop(stdin);
        let mut encoded = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), stdout.read_to_end(&mut encoded)).await.unwrap().unwrap();
        assert!(encoder.wait().await.unwrap().success());
        let mut splitter = AnnexB::default();
        let units = splitter.push(&encoded);
        let sps = units.iter().find(|unit| unit[0] & 31 == 7).expect("real encoder SPS");
        assert_eq!(sps[1], 66, "encoder must use Baseline");
        assert_eq!(sps[3], 30, "SPS must not advertise3.1 when peer accepted3.0");
    }

    #[test]
    fn profile_derives_h264_limits_and_level() {
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate_kbps: 4096,
            profile: "baseline".into(),
            level: "4.0".into(),
        };
        assert_eq!(profile.max_fs(), 8160, "1080p = 68×120 macroblocks");
        assert_eq!(profile.profile_level_id(), "42c028");
        assert_eq!(frame_interval(&profile), Duration::from_micros(33_333));
        let low = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 512,
            profile: "baseline".into(),
            level: "3.0".into(),
        };
        assert_eq!(low.max_fs(), 920);
        assert_eq!(low.profile_level_id(), "42c01e");
        assert_eq!(frame_interval(&low), Duration::from_micros(66_666));
    }

    #[test]
    fn conference_settings_override_global_and_env() {
        let mut profile = VideoProfile::default();
        apply_video_settings(
            &mut profile,
            &serde_json::json!({"max_frame_rate": 25}),
            Some(&serde_json::json!({
                "payload": {
                    "bitrate": 4096,
                    "resolution": {"width": 1920, "height": 1080, "fps_limit": 30}
                }
            })),
        );
        assert_eq!((profile.width, profile.height), (1920, 1080));
        assert_eq!(profile.bitrate_kbps, 4096);
        assert_eq!(profile.fps, 30, "resolution.fps_limit приоритетнее global max_frame_rate");
        assert_eq!(profile.level, "4.0");
        apply_video_settings(&mut profile, &serde_json::json!({"max_frame_rate": 10}), None);
        assert_eq!(profile.fps, 10, "без комнаты остаётся global fps");
    }

    #[test]
    fn call_string_settings_override_conference() {
        let mut profile = VideoProfile::default();
        apply_video_settings(
            &mut profile,
            &serde_json::json!({"max_frame_rate": 25}),
            Some(&serde_json::json!({"bitrate": 4096, "resolution": {"width": 1920, "height": 1080, "fps_limit": 30}})),
        );
        apply_call_string_settings(
            &mut profile,
            &serde_json::json!({"bitrate": 256, "resolution": {"width": 320, "height": 180, "fps_limit": 10}}),
        );
        assert_eq!((profile.width, profile.height), (320, 180));
        assert_eq!(profile.bitrate_kbps, 256);
        assert_eq!(profile.fps, 10);
        assert_eq!(profile.level, "3.1", "ниже 3.1 профиль не опускаем: baseline 3.1 — минимальный рабочий");
    }

    #[test]
    fn answer_limits_handle_each_limit_independently() {
        let base = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate_kbps: 4096,
            profile: "baseline".into(),
            level: "4.0".into(),
        };
        let mut only_fs = base.clone();
        only_fs.clamp_to_answer(Some(920), None, None);
        assert_eq!((only_fs.width, only_fs.height), (640, 360));
        assert_eq!(only_fs.fps, 30);
        assert_eq!(only_fs.bitrate_kbps, 4096);
        let mut only_br = base.clone();
        only_br.clamp_to_answer(None, Some(512), None);
        assert_eq!((only_br.width, only_br.height), (1920, 1080));
        assert_eq!(only_br.bitrate_kbps, 512);
        let mut only_mbps = base.clone();
        only_mbps.clamp_to_answer(None, None, Some(244_800));
        assert_eq!((only_mbps.width, only_mbps.height), (1920, 1080));
        assert_eq!(only_mbps.fps, 30);
        let mut tight_mbps = base.clone();
        tight_mbps.clamp_to_answer(None, None, Some(49_000));
        assert_eq!((tight_mbps.width, tight_mbps.height), (960, 540), "сначала размер, затем fps");
        assert_eq!(tight_mbps.fps, 24);
        let mut tiny_mbps = base.clone();
        tiny_mbps.clamp_to_answer(None, None, Some(1_000));
        assert_eq!((tiny_mbps.width, tiny_mbps.height), (320, 180));
        assert_eq!(tiny_mbps.fps, 4);
        let mut max_fs_and_mbps = base.clone();
        max_fs_and_mbps.fps = 60;
        max_fs_and_mbps.clamp_to_answer(Some(240), None, Some(8_000));
        assert_eq!((max_fs_and_mbps.width, max_fs_and_mbps.height), (320, 180), "не выходить за max-fs");
        assert_eq!(max_fs_and_mbps.fps, 33);
        let mut tiny = base;
        tiny.clamp_to_answer(Some(1), None, None);
        assert_eq!((tiny.width, tiny.height), (320, 180));
        assert!(tiny.max_fs() > 1, "при невозможном лимите остаётся минимальный размер");
    }

    #[test]
    fn answer_limits_clamp_bitrate_size_and_fps() {
        let mut profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate_kbps: 4096,
            profile: "baseline".into(),
            level: "4.0".into(),
        };
        profile.clamp_to_answer(Some(920), Some(512), Some(13_800));
        assert_eq!((profile.width, profile.height), (640, 360));
        assert_eq!(profile.bitrate_kbps, 512);
        assert_eq!(profile.fps, 15);
        assert_eq!(profile.level, "3.1");
    }

    fn payload(data: &[u8], sequence: u16, timestamp: u32) -> RtpPayload<'_> {
        RtpPayload { payload: data, payload_type: 96, sequence, timestamp, marker: false }
    }

    fn buffered_rtp(sequence: u16, timestamp: u32) -> BufferedRtp {
        BufferedRtp {
            sequence,
            timestamp,
            marker: false,
            payload: vec![sequence as u8],
            arrived: std::time::Instant::now(),
        }
    }

    #[test]
    fn video_reorder_buffer_restores_sequence_and_drops_duplicates() {
        let mut buffer = VideoReorderBuffer::default();
        assert_eq!(buffer.push(buffered_rtp(10, 0)).len(), 1, "первый пакет отдаётся сразу");
        assert!(buffer.push(buffered_rtp(12, 0)).is_empty(), "12 ждёт 11");
        assert!(buffer.push(buffered_rtp(12, 0)).is_empty(), "дубль 12 не копится");
        let ready = buffer.push(buffered_rtp(11, 0));
        assert_eq!(
            ready.iter().map(|packet| packet.sequence).collect::<Vec<_>>(),
            vec![11, 12],
            "reorder 12→11 разложен по порядку"
        );
        assert_eq!(buffer.late, 0);
        assert_eq!(buffer.push(buffered_rtp(9, 0)).len(), 0, "опоздавший 9 отброшен");
        assert_eq!(buffer.late, 1);
    }

    #[test]
    fn video_reorder_buffer_skips_lost_packet_after_window_and_timeout() {
        let mut buffer = VideoReorderBuffer::default();
        assert_eq!(buffer.push(buffered_rtp(0, 0)).len(), 1);
        // Пакет 1 потерян: 2..=40 копятся, окно переполняется → дыра пропускается.
        let mut ready = Vec::new();
        for sequence in 2..=40u16 {
            ready.extend(buffer.push(buffered_rtp(sequence, 0)));
        }
        assert!(buffer.gaps >= 1, "переполнение окна должно пропустить дыру");
        assert!(ready.len() > 30, "после пропуска дыры очередь отдаётся: got {}", ready.len());

        // Таймаут: ждём 1, пришёл 2 — через 30 мс дыра пропускается.
        let mut buffer = VideoReorderBuffer::default();
        let _ = buffer.push(buffered_rtp(100, 0));
        assert!(buffer.push(buffered_rtp(102, 0)).is_empty());
        let ready = buffer.flush(std::time::Instant::now() + REORDER_MAX_DELAY);
        assert_eq!(ready.iter().map(|packet| packet.sequence).collect::<Vec<_>>(), vec![102]);
        assert_eq!(buffer.gaps, 1);
    }

    #[test]
    fn h264_fu_a_reassembles_only_consecutive_fragments_of_the_same_frame() {
        let mut decoder = H264Depacketizer::default();
        assert!(decoder.push(payload(&[0x7c, 0x85, 1, 2], 10, 9000)).unwrap().is_empty());
        assert_eq!(decoder.push(payload(&[0x7c, 0x45, 3, 4], 11, 9000)).unwrap(), vec![vec![0x65, 1, 2, 3, 4]]);
        assert!(decoder.push(payload(&[0x7c, 0x85, 1], 12, 18000)).unwrap().is_empty());
        assert!(
            decoder.push(payload(&[0x7c, 0x45, 2], 14, 18000)).is_err(),
            "lost fragments must never feed a corrupt NAL to decoder"
        );
    }

    #[test]
    fn h264_stap_a_extracts_sps_and_pps_with_strict_lengths() {
        let mut decoder = H264Depacketizer::default();
        assert_eq!(
            decoder.push(payload(&[0x78, 0, 2, 0x67, 1, 0, 2, 0x68, 2], 1, 0)).unwrap(),
            vec![vec![0x67, 1], vec![0x68, 2]]
        );
        assert!(decoder.push(payload(&[0x78, 0, 10, 0x67], 2, 0)).is_err());
    }

    #[tokio::test]
    #[ignore = "requires installed ffmpeg with libx264; sends only loopback RTP"]
    async fn ffmpeg_h264_rtp_roundtrip_preserves_color_and_stops_workers() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;
        let sender_engine = Arc::new(vks_media_mcu::McuEngine::new());
        let receiver_engine = Arc::new(vks_media_mcu::McuEngine::new());
        let room = vks_core::RoomId::new();
        let sender = vks_core::ParticipantId::new();
        let receiver = vks_core::ParticipantId::new();
        for (engine, pid) in [(&sender_engine, sender), (&receiver_engine, receiver)] {
            engine.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)],
            );
        }
        let pixels = (VideoProfile::default().width * VideoProfile::default().height) as usize;
        let mut red = vec![76; pixels];
        red.extend(vec![85; pixels / 4]);
        red.extend(vec![255; pixels / 4]);
        assert!(sender_engine.set_participant_frame(
            room,
            sender,
            vks_media_mcu::frame::Frame::new(VideoProfile::default().width, VideoProfile::default().height, red)
        ));
        let sender_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_address = sender_socket.local_addr().unwrap();
        let receiver_address = receiver_socket.local_addr().unwrap();
        let sender_stats = Arc::new(GatewayMediaStats::default());
        let receiver_stats = Arc::new(GatewayMediaStats::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let sender_controls = GatewayMediaControls::default();
        sender_controls.video_tx_muted.store(true, Ordering::Relaxed);
        let receiver_controls = GatewayMediaControls::default();
        receiver_controls.video_rx_muted.store(true, Ordering::Relaxed);
        let sender_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: sender,
            rtp_socket: sender_socket,
            rtcp_socket: None,
            remote_target: receiver_address,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: sender_engine,
            controls: sender_controls,
            cancelled: cancelled.clone(),
            stats: sender_stats.clone(),
            profile: VideoProfile::default(),
        })
        .await
        .expect("real H264 encoder/decoder must start");
        let receiver_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: receiver,
            rtp_socket: receiver_socket,
            rtcp_socket: None,
            remote_target: sender_address,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: receiver_engine.clone(),
            controls: receiver_controls,
            cancelled: cancelled.clone(),
            stats: receiver_stats.clone(),
            profile: VideoProfile::default(),
        })
        .await
        .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(7), async {
            while receiver_stats.decoded_video_frames.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        cancelled.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(3), sender_task).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), receiver_task).await.unwrap().unwrap();
        assert!(received.is_ok(), "no actual H264 video was decoded from loopback RTP");
        assert!(sender_stats.encoded_video_frames.load(Ordering::Relaxed) > 0);
        let frame = receiver_engine.tick_video_at(room, u64::MAX / 2).unwrap();
        assert!(frame.is_yuv420p());
        let pixels = frame.width as usize * frame.height as usize;
        assert!((frame.data[pixels + pixels / 8] as i16 - 85).abs() < 8, "U color plane lost");
        assert!(frame.data[pixels * 5 / 4 + pixels / 8] > 230, "V color plane lost");
    }

    /// `P1`: production-путь — декодер моста реально пишет кадры через
    /// `set_participant_frame` (FFmpeg-поток по RTP между двумя мостами), а
    /// появление второго видео-участника пере-ключёвывает группу: base →
    /// `base × 1.02`. Тест не использует мок `set_frame`.
    #[tokio::test]
    async fn production_decoder_path_fills_visible_windows_and_rekeys() {
        use std::sync::atomic::Ordering;
        if !codec_available_for(GatewayVideoCodec::H264).await {
            eprintln!("skipping: libx264 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        let engine = Arc::new(vks_media_mcu::McuEngine::new());
        let viewer = vks_core::ParticipantId::new();
        let other = vks_core::ParticipantId::new();
        engine.bridge_participant(
            room,
            viewer,
            vec![vks_core::track::Track::new(viewer, vks_core::track::TrackKind::Video)],
        );
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 2048,
            ..VideoProfile::default()
        };
        let viewer_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let other_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let viewer_address = viewer_socket.local_addr().unwrap();
        let other_address = other_socket.local_addr().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let viewer_stats = Arc::new(GatewayMediaStats::default());
        let mut tasks = Vec::new();
        tasks.push(
            spawn_video_media_bridge(VideoBridgeSession {
                room_id: room,
                participant_id: viewer,
                rtp_socket: viewer_socket,
                rtcp_socket: None,
                remote_target: other_address,
                payload_type: 96,
                receive_payload_type: 96,
                port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
                engine: engine.clone(),
                controls: GatewayMediaControls::default(),
                cancelled: cancelled.clone(),
                stats: viewer_stats.clone(),
                profile: profile.clone(),
            })
            .await
            .expect("H264 encoder/decoder must start"),
        );
        let base = farm_spec_for(&profile, 1).bitrate_kbps;
        let scaled = farm_spec_for(&profile, 2).bitrate_kbps;
        assert!(scaled > base);

        // Первый зритель: окно одно → группа с базовым битрейтом.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !engine.video_farm_stats().iter().any(|group| group.key.spec.bitrate_kbps == base) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "группа base не появилась: {:?}",
                engine.video_farm_stats().iter().map(|g| g.key.spec.bitrate_kbps).collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Второй мост: реальный H.264-поток по RTP в обе стороны — декодер
        // первого моста пишет кадры через set_participant_frame (production).
        engine.bridge_participant(
            room,
            other,
            vec![vks_core::track::Track::new(other, vks_core::track::TrackKind::Video)],
        );
        tasks.push(
            spawn_video_media_bridge(VideoBridgeSession {
                room_id: room,
                participant_id: other,
                rtp_socket: other_socket,
                rtcp_socket: None,
                remote_target: viewer_address,
                payload_type: 96,
                receive_payload_type: 96,
                port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
                engine: engine.clone(),
                controls: GatewayMediaControls::default(),
                cancelled: cancelled.clone(),
                stats: Arc::new(GatewayMediaStats::default()),
                profile: profile.clone(),
            })
            .await
            .expect("H264 encoder/decoder must start"),
        );
        assert_eq!(engine.visible_windows(room), 2, "второй видео-участник занял второе окно");
        // Две независимые проверки: (1) production-декодер моста реально
        // записал кадры из RTP-потока, (2) группа пере-ключёвана на base×1.02.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        let mut decoded = viewer_stats.decoded_video_frames.load(Ordering::Relaxed);
        let mut specs: Vec<u32> = engine.video_farm_stats().iter().map(|group| group.key.spec.bitrate_kbps).collect();
        while (decoded == 0 || !specs.contains(&scaled)) && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(50)).await;
            decoded = viewer_stats.decoded_video_frames.load(Ordering::Relaxed);
            specs = engine.video_farm_stats().iter().map(|group| group.key.spec.bitrate_kbps).collect();
        }
        cancelled.store(true, Ordering::Relaxed);
        for task in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        }
        assert!(decoded > 0, "production-путь декодера не записал кадры (decoded={decoded})");
        assert!(
            specs.contains(&scaled),
            "после появления второго окна группа не пере-ключёвалась: specs={specs:?}, expected {scaled}"
        );
    }

    /// `P1`: смена числа видимых окон пере-ключёвывает группу на новый
    /// битрейт (новый `EncoderKey`) — по событию, а не на каждом кадре.
    #[tokio::test]
    async fn dynamic_visible_windows_rekey_the_encoder_group() {
        use std::sync::atomic::Ordering;
        if !codec_available_for(GatewayVideoCodec::H264).await {
            eprintln!("skipping: libx264 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        let engine = Arc::new(vks_media_mcu::McuEngine::new());
        let viewer = vks_core::ParticipantId::new();
        let other = vks_core::ParticipantId::new();
        engine.bridge_participant(
            room,
            viewer,
            vec![vks_core::track::Track::new(viewer, vks_core::track::TrackKind::Video)],
        );
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 2048,
            ..VideoProfile::default()
        };
        assert_eq!(engine.visible_windows(room), 1, "один видео-участник = одно окно");
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = socket.local_addr().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let stats = Arc::new(GatewayMediaStats::default());
        let task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: viewer,
            rtp_socket: socket,
            rtcp_socket: None,
            remote_target: target,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: engine.clone(),
            controls: GatewayMediaControls::default(),
            cancelled: cancelled.clone(),
            stats,
            profile: profile.clone(),
        })
        .await
        .expect("H264 encoder/decoder must start");

        let base = farm_spec_for(&profile, 1).bitrate_kbps;
        let scaled = farm_spec_for(&profile, 2).bitrate_kbps;
        assert!(scaled > base, "2 окна должны поднять битрейт: {base} → {scaled}");
        let wait_spec = |want: u32| {
            let engine = engine.clone();
            async move {
                let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
                loop {
                    let specs: Vec<u32> =
                        engine.video_farm_stats().iter().map(|group| group.key.spec.bitrate_kbps).collect();
                    if specs.contains(&want) {
                        return specs;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        panic!("группа с битрейтом {want} не появилась; есть {specs:?}");
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            }
        };
        wait_spec(base).await;
        // Появился второй видео-участник → занято второе окно → рендер сообщает
        // об изменении, output-таск берёт новый ключ (битрейт base × 1.02).
        engine.bridge_participant(
            room,
            other,
            vec![vks_core::track::Track::new(other, vks_core::track::TrackKind::Video)],
        );
        assert_eq!(engine.visible_windows(room), 2);
        wait_spec(scaled).await;
        cancelled.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    }

    /// `P1`+`P0`: три зрителя, добавленных по одному, держат **ровно одну**
    /// активную группу на комнату (старая группа умирает вместе с процессом),
    /// после ухода всех — групп нет. Раньше per-session render-таски перетирали
    /// `watch` и плодили группы (на 10 ботах — десятки).
    #[tokio::test]
    async fn three_sequential_viewers_keep_one_encoder_group() {
        use std::sync::atomic::Ordering;
        use vks_media_mcu::group_bitrate_kbps;
        if !codec_available_for(GatewayVideoCodec::H264).await {
            eprintln!("skipping: libx264 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        let engine = Arc::new(vks_media_mcu::McuEngine::new());
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 2048,
            ..VideoProfile::default()
        };
        // Три цикла «3 зрителя по одному»: после каждого цикла группы не
        // должно остаться вовсе (ретир убил энкодеры и сделал reap).
        for cycle in 0..3 {
            let cancelled = Arc::new(AtomicBool::new(false));
            let mut tasks = Vec::new();
            let mut participants = Vec::new();
            let mut addresses: Vec<std::net::SocketAddr> = Vec::new();
            for index in 0..3usize {
                let participant = vks_core::ParticipantId::new();
                engine.bridge_participant(
                    room,
                    participant,
                    vec![vks_core::track::Track::new(participant, vks_core::track::TrackKind::Video)],
                );
                participants.push(participant);
                let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
                let local = socket.local_addr().unwrap();
                let target = if index == 0 { local } else { addresses[index - 1] };
                addresses.push(local);
                tasks.push(
                    spawn_video_media_bridge(VideoBridgeSession {
                        room_id: room,
                        participant_id: participant,
                        rtp_socket: socket,
                        rtcp_socket: None,
                        remote_target: target,
                        payload_type: 96,
                        receive_payload_type: 96,
                        port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
                        engine: engine.clone(),
                        controls: GatewayMediaControls::default(),
                        cancelled: cancelled.clone(),
                        stats: Arc::new(GatewayMediaStats::default()),
                        profile: profile.clone(),
                    })
                    .await
                    .expect("H264 encoder/decoder must start"),
                );
                // Зритель добавлен по одному: ждём, что на комнате осталась
                // ровно одна группа с битрейтом base × 1.02^(N−1).
                let expected = group_bitrate_kbps(profile.bitrate_kbps, index + 1);
                let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
                loop {
                    let stats: Vec<vks_media_mcu::GroupStat> =
                        engine.video_farm_stats().into_iter().filter(|stat| stat.key.room == room).collect();
                    if stats.len() == 1 && stats[0].key.spec.bitrate_kbps == expected && stats[0].restarts == 0 {
                        break;
                    }
                    assert!(
                        tokio::time::Instant::now() < deadline,
                        "цикл {cycle}, после {n} зрителей не ровно одна группа: {shown:?}",
                        n = index + 1,
                        shown =
                            stats.iter().map(|stat| (stat.key.spec.bitrate_kbps, stat.restarts)).collect::<Vec<_>>()
                    );
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
            cancelled.store(true, Ordering::Relaxed);
            for task in tasks {
                let _ = tokio::time::timeout(Duration::from_secs(10), task).await;
            }
            // Убираем участников из комнаты: иначе следующий цикл стартует с
            // видимыми окнами прошлых зрителей и битрейт считается не от того N.
            for participant in participants {
                engine.unbridge_participant(room, participant);
            }
            // Все зрители ушли: группы ретирены (kill+reap проверены
            // farm-тестами, включая зомби), в статистике комнаты пусто.
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while engine.video_farm_stats().iter().any(|stat| stat.key.room == room) {
                assert!(
                    tokio::time::Instant::now() < deadline,
                    "цикл {cycle}: группы не ретирены после ухода всех зрителей"
                );
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }

    /// `P0-ingest`: переупорядоченный вход (джиттер bot→MCU) не должен ломать
    /// видео. Мост A кодирует композит и шлёт RTP прокси, прокси переставляет
    /// пакеты (1,0,3,2), мост B декодирует: с reorder-буфером кадры доходят,
    /// без него (до фикса) декодер не получает ни одного AU.
    #[tokio::test]
    async fn reordered_ingress_is_reassembled_before_decoder() {
        use std::sync::atomic::Ordering;
        if !codec_available_for(GatewayVideoCodec::H264).await {
            eprintln!("skipping: libx264 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        let engine = Arc::new(vks_media_mcu::McuEngine::new());
        let sender = vks_core::ParticipantId::new();
        let receiver = vks_core::ParticipantId::new();
        for participant in [sender, receiver] {
            engine.bridge_participant(
                room,
                participant,
                vec![vks_core::track::Track::new(participant, vks_core::track::TrackKind::Video)],
            );
        }
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 300,
            ..VideoProfile::default()
        };
        let cancelled = Arc::new(AtomicBool::new(false));
        let proxy_cancelled = cancelled.clone();
        let sender_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        // Прокси из двух сокетов: ingress принимает поток A, egress отдаёт
        // переставленный поток B (он же сток для egress самого B).
        let proxy_ingress = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let proxy_egress = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let proxy_address = proxy_ingress.local_addr().unwrap();
        let proxy_egress_address = proxy_egress.local_addr().unwrap();
        let receiver_address = receiver_socket.local_addr().unwrap();
        let sender_stats = Arc::new(GatewayMediaStats::default());
        let receiver_stats = Arc::new(GatewayMediaStats::default());
        // Мост-отправитель: egress уходит в прокси (его decoder не нужен).
        let sender_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: sender,
            rtp_socket: sender_socket,
            rtcp_socket: None,
            remote_target: proxy_address,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: engine.clone(),
            controls: GatewayMediaControls::default(),
            cancelled: cancelled.clone(),
            stats: sender_stats,
            profile: profile.clone(),
        })
        .await
        .expect("sender bridge must start");
        // Мост-получатель: ingress получает переставленный поток от прокси.
        let receiver_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: receiver,
            rtp_socket: receiver_socket,
            rtcp_socket: None,
            remote_target: proxy_egress_address,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: engine.clone(),
            controls: GatewayMediaControls::default(),
            cancelled: cancelled.clone(),
            stats: receiver_stats.clone(),
            profile,
        })
        .await
        .expect("receiver bridge must start");
        // Прокси: детерминированный reorder — каждая четвёрка уходит 1,0,3,2.
        let proxy = tokio::spawn(async move {
            let mut buffer = [0u8; 2048];
            let mut drain = [0u8; 2048];
            let mut queue: Vec<Vec<u8>> = Vec::with_capacity(4);
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(5)) => {
                        if proxy_cancelled.load(Ordering::Relaxed) { break; }
                    }
                    // egress самого B — просто сток.
                    drained = proxy_egress.recv_from(&mut drain) => {
                        if drained.is_err() { break; }
                    }
                    received = proxy_ingress.recv_from(&mut buffer) => {
                        let Ok((count, _)) = received else { break };
                        queue.push(buffer[..count].to_vec());
                        if queue.len() == 4 {
                            for index in [1usize, 0, 3, 2] {
                                let _ = proxy_egress.send_to(&queue[index], receiver_address).await;
                            }
                            queue.clear();
                        }
                    }
                }
            }
            for packet in queue {
                let _ = proxy_egress.send_to(&packet, receiver_address).await;
            }
        });
        let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
        while receiver_stats.decoded_video_frames.load(Ordering::Relaxed) == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "reorder сломал вход: декодер не получил ни одного кадра (rx_packets={}, rejected={})",
                receiver_stats.video_rx_packets.load(Ordering::Relaxed),
                receiver_stats.rejected_packets.load(Ordering::Relaxed)
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(
            receiver_stats.video_rx_packets.load(Ordering::Relaxed) > 0,
            "пакеты должны приниматься (не отбрасываться RtpSource)"
        );
        cancelled.store(true, Ordering::Relaxed);
        drop(proxy);
        let _ = tokio::time::timeout(Duration::from_secs(5), sender_task).await;
        let _ = tokio::time::timeout(Duration::from_secs(5), receiver_task).await;
        let _ = receiver_stats.decoded_video_frames.load(Ordering::Relaxed);
    }

    /// `O7`: self-view несовместим с фермой (кадры зрителей различаются) —
    /// такой зритель идёт per-session путём.
    #[tokio::test]
    async fn self_view_room_falls_back_to_per_session_encoder() {
        use std::sync::atomic::Ordering;
        if !codec_available_for(GatewayVideoCodec::H264).await {
            eprintln!("skipping: libx264 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        let engine = Arc::new(vks_media_mcu::McuEngine::new());
        let viewer = vks_core::ParticipantId::new();
        engine.bridge_participant(
            room,
            viewer,
            vec![vks_core::track::Track::new(viewer, vks_core::track::TrackKind::Video)],
        );
        engine.set_self_view(room, true);
        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let target = socket.local_addr().unwrap();
        let cancelled = Arc::new(AtomicBool::new(false));
        let task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: viewer,
            rtp_socket: socket,
            rtcp_socket: None,
            remote_target: target,
            payload_type: 96,
            receive_payload_type: 96,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: engine.clone(),
            controls: GatewayMediaControls::default(),
            cancelled: cancelled.clone(),
            stats: Arc::new(GatewayMediaStats::default()),
            profile: VideoProfile {
                codec: GatewayVideoCodec::H264,
                width: 320,
                height: 180,
                fps: 15,
                bitrate_kbps: 300,
                ..VideoProfile::default()
            },
        })
        .await
        .expect("per-session encoder must start");
        assert!(engine.video_farm_stats().is_empty(), "self-view комната не должна попадать в ферму");
        cancelled.store(true, Ordering::Relaxed);
        let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
    }

    /// `O7`: два зрителя с одинаковым профилем делят один энкодер комнаты;
    /// per-viewer остаётся только RTP-обвязка (свои SSRC/seq/ts).
    #[tokio::test]
    async fn two_subscribers_share_one_encoder_group() {
        use std::sync::atomic::Ordering;
        if !codec_available_for(GatewayVideoCodec::H264).await {
            eprintln!("skipping: libx264 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        // Один серверный движок на комнату — как в проде: ферма общая.
        let engine = Arc::new(vks_media_mcu::McuEngine::new());
        let sender = vks_core::ParticipantId::new();
        let first = vks_core::ParticipantId::new();
        let second = vks_core::ParticipantId::new();
        for pid in [sender, first, second] {
            engine.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)],
            );
        }
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H264,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 600,
            ..VideoProfile::default()
        };
        let (width, height) = (profile.width, profile.height);
        let pixels = (width * height) as usize;
        let mut frame = vec![120; pixels];
        frame.extend(vec![70; pixels / 4]);
        frame.extend(vec![50; pixels / 4]);
        assert!(engine.set_participant_frame(room, sender, vks_media_mcu::frame::Frame::new(width, height, frame)));
        let sender_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let first_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let second_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let (sender_address, first_address) = (sender_socket.local_addr().unwrap(), first_socket.local_addr().unwrap());
        let cancelled = Arc::new(AtomicBool::new(false));
        let stats: Vec<_> = (0..3).map(|_| Arc::new(GatewayMediaStats::default())).collect();
        let mut tasks = Vec::new();
        for (index, (pid, socket, target, participant_stats)) in [
            (sender, sender_socket, first_address, stats[0].clone()),
            (first, first_socket, sender_address, stats[1].clone()),
            (second, second_socket, sender_address, stats[2].clone()),
        ]
        .into_iter()
        .enumerate()
        {
            let controls = GatewayMediaControls::default();
            // Отправитель не публикует своё видео из входящего RTP (кадр задан
            // напрямую), зрители отдают битстрим группы в свои RTP-потоки.
            if index == 0 {
                controls.video_tx_muted.store(true, Ordering::Relaxed);
            }
            let task = spawn_video_media_bridge(VideoBridgeSession {
                room_id: room,
                participant_id: pid,
                rtp_socket: socket,
                rtcp_socket: None,
                remote_target: target,
                payload_type: 96,
                receive_payload_type: 96,
                port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
                engine: engine.clone(),
                controls,
                cancelled: cancelled.clone(),
                stats: participant_stats,
                profile: profile.clone(),
            })
            .await
            .expect("H264 encoder/decoder must start");
            tasks.push(task);
        }
        // Один энкодер на комнату + все три сессии в одной группе.
        let stats_snapshot = engine.video_farm_stats();
        assert_eq!(stats_snapshot.len(), 1, "один энкодер на комнату, а не на зрителя");
        assert_eq!(stats_snapshot[0].refcount, 4, "3 сессии + сам энкодер");
        let received = tokio::time::timeout(Duration::from_secs(10), async {
            while stats[1].encoded_video_frames.load(Ordering::Relaxed) == 0
                || stats[2].encoded_video_frames.load(Ordering::Relaxed) == 0
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(received.is_ok(), "оба зрителя получают битстрим одной группы");
        // Живой прогон ловил регрессию: актор с write+read в одном цикле отдавал
        // 1 AU (IDR) и замолкал. Требуем устойчивый поток, а не «первый кадр».
        let before = stats[1].encoded_video_frames.load(Ordering::Relaxed);
        tokio::time::sleep(Duration::from_millis(1200)).await;
        let steady = stats[1].encoded_video_frames.load(Ordering::Relaxed);
        assert!(
            steady >= before + 10,
            "поток AU не устойчив: {before} → {steady} за 1.2 с (ожидалось ≥10, ферма обязана давать полный fps)"
        );
        let group = engine.video_farm_stats();
        assert_eq!(group.len(), 1);
        assert!(group[0].frames >= 10, "группа закодировала {} кадров", group[0].frames);
        assert!(
            stats[1].video_tx_packets.load(Ordering::Relaxed) > 0
                && stats[2].video_tx_packets.load(Ordering::Relaxed) > 0,
            "per-viewer RTP у каждого зрителя"
        );
        assert!(
            stats[0].encoded_video_frames.load(Ordering::Relaxed)
                <= 2 * stats[1].encoded_video_frames.load(Ordering::Relaxed).max(1),
            "группа кодирует не больше кадров, чем получает один зритель"
        );
        cancelled.store(true, Ordering::Relaxed);
        for task in tasks {
            let _ = tokio::time::timeout(Duration::from_secs(3), task).await;
        }
        // Все зрители ушли — группа и её FFmpeg остановлены.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while engine.video_farm_stats().iter().any(|group| group.refcount > 1) && tokio::time::Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(engine.video_farm_stats().is_empty(), "refcount 0 → группа убита");
    }

    #[tokio::test]
    async fn h265_loopback_video_bridge_encodes_and_decodes() {
        use std::sync::atomic::Ordering;
        if !codec_available_for(GatewayVideoCodec::H265).await {
            eprintln!("skipping: libx265 not available");
            return;
        }
        let room = vks_core::RoomId::new();
        let sender_engine = Arc::new(vks_media_mcu::McuEngine::new());
        let receiver_engine = Arc::new(vks_media_mcu::McuEngine::new());
        let sender = vks_core::ParticipantId::new();
        let receiver = vks_core::ParticipantId::new();
        for (engine, pid) in [(&sender_engine, sender), (&receiver_engine, receiver)] {
            engine.bridge_participant(
                room,
                pid,
                vec![vks_core::track::Track::new(pid, vks_core::track::TrackKind::Video)],
            );
        }
        let profile = VideoProfile {
            codec: GatewayVideoCodec::H265,
            width: 640,
            height: 360,
            fps: 15,
            bitrate_kbps: 600,
            ..VideoProfile::default()
        };
        let (pixels_w, pixels_h) = (profile.width, profile.height);
        let pixels = (pixels_w * pixels_h) as usize;
        let mut green = vec![140; pixels];
        green.extend(vec![85; pixels / 4]);
        green.extend(vec![55; pixels / 4]);
        assert!(sender_engine.set_participant_frame(
            room,
            sender,
            vks_media_mcu::frame::Frame::new(pixels_w, pixels_h, green)
        ));
        let sender_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let receiver_socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let sender_address = sender_socket.local_addr().unwrap();
        let receiver_address = receiver_socket.local_addr().unwrap();
        let sender_stats = Arc::new(GatewayMediaStats::default());
        let receiver_stats = Arc::new(GatewayMediaStats::default());
        let cancelled = Arc::new(AtomicBool::new(false));
        let sender_controls = GatewayMediaControls::default();
        sender_controls.video_tx_muted.store(true, Ordering::Relaxed);
        let receiver_controls = GatewayMediaControls::default();
        receiver_controls.video_rx_muted.store(true, Ordering::Relaxed);
        let sender_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: sender,
            rtp_socket: sender_socket,
            rtcp_socket: None,
            remote_target: receiver_address,
            payload_type: 99,
            receive_payload_type: 99,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: sender_engine,
            controls: sender_controls,
            cancelled: cancelled.clone(),
            stats: sender_stats.clone(),
            profile: profile.clone(),
        })
        .await
        .expect("real H265 encoder/decoder must start");
        let receiver_task = spawn_video_media_bridge(VideoBridgeSession {
            room_id: room,
            participant_id: receiver,
            rtp_socket: receiver_socket,
            rtcp_socket: None,
            remote_target: sender_address,
            payload_type: 99,
            receive_payload_type: 99,
            port_pool: Arc::new(vks_gateway_sip::RtpPortPool::new(49000, 49000)),
            engine: receiver_engine.clone(),
            controls: receiver_controls,
            cancelled: cancelled.clone(),
            stats: receiver_stats.clone(),
            profile,
        })
        .await
        .unwrap();
        let received = tokio::time::timeout(Duration::from_secs(10), async {
            while receiver_stats.decoded_video_frames.load(Ordering::Relaxed) == 0 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        cancelled.store(true, Ordering::Relaxed);
        tokio::time::timeout(Duration::from_secs(3), sender_task).await.unwrap().unwrap();
        tokio::time::timeout(Duration::from_secs(3), receiver_task).await.unwrap().unwrap();
        assert!(received.is_ok(), "no actual H265 video was decoded from loopback RTP");
        assert!(sender_stats.encoded_video_frames.load(Ordering::Relaxed) > 0);
        assert!(sender_stats.video_tx_packets.load(Ordering::Relaxed) > 0);
        let frame = receiver_engine.tick_video_at(room, u64::MAX / 2).expect("decoded H265 frame");
        assert!(frame.is_yuv420p());
    }

    #[test]
    fn packetize_h264_strips_start_codes_and_sets_marker_bit_correctly() {
        let mut seq = 100u16;
        let ts = 90000u32;
        let ssrc = 0x12345678;
        let pt = 96u8;

        // 1. Single NAL with 4-byte start code + 3-byte start code + empty tail NAL
        let nal1 = vec![0, 0, 0, 1, 0x67, 0x42, 0xe0, 0x1f]; // SPS
        let nal2 = vec![0, 0, 1, 0x68, 0xce, 0x3c, 0x80]; // PPS
        let nal3_empty = vec![];
        let nals = vec![nal1, nal2, nal3_empty];

        let packets = packetize_h264(&nals, pt, ssrc, &mut seq, ts);
        assert_eq!(packets.len(), 2);

        // First packet: SPS without start code, Marker = 0
        assert_eq!(&packets[0][12..], &[0x67, 0x42, 0xe0, 0x1f]);
        assert_eq!(packets[0][1] & 0x80, 0, "first NAL must not have marker bit");

        // Second packet: PPS without start code, Marker = 1 (even though 3rd NAL was empty)
        assert_eq!(&packets[1][12..], &[0x68, 0xce, 0x3c, 0x80]);
        assert_eq!(packets[1][1] & 0x80, 0x80, "last actual NAL must have marker bit M=1");
    }

    #[test]
    fn packetize_h264_fu_a_fragmentation_and_marker() {
        let mut seq = 1u16;
        let ts = 1000u32;
        let ssrc = 0xabcdef;
        let pt = 96u8;

        // Large IDR frame (2500 bytes > 1188)
        let mut large_nal = vec![0, 0, 0, 1];
        large_nal.push(0x65); // NAL type 5 (IDR), NRI = 3 (0x60)
        large_nal.resize(2504, 0xaa);

        let packets = packetize_h264(&[large_nal], pt, ssrc, &mut seq, ts);
        assert!(packets.len() >= 2);

        // First FU-A chunk: NRI=3, FU type=28, Start bit=1, End bit=0, Marker=0
        assert_eq!(packets[0][12], 0x60 | 28);
        assert_eq!(packets[0][13], 0x80 | 5);
        assert_eq!(packets[0][1] & 0x80, 0);

        // Last FU-A chunk: NRI=3, FU type=28, Start bit=0, End bit=1, Marker=1
        let last = packets.last().unwrap();
        assert_eq!(last[12], 0x60 | 28);
        assert_eq!(last[13], 0x40 | 5);
        assert_eq!(last[1] & 0x80, 0x80, "final FU-A chunk must have marker bit M=1");
    }

    #[test]
    fn packetize_h264_filters_bare_start_codes_and_invalid_nal_headers() {
        let mut seq = 1u16;
        let ts = 1000u32;
        let ssrc = 0xabcdef;
        let pt = 96u8;

        // Bare 3-byte and 4-byte start codes, zero padding, and invalid forbidden_zero_bit
        let bare4 = vec![0, 0, 0, 1];
        let bare3 = vec![0, 0, 1];
        let zeros = vec![0, 0, 0, 0];
        let nal_type0 = vec![0, 0, 0, 1, 0x00]; // type 0 invalid
        let invalid_fbit = vec![0, 0, 0, 1, 0x85, 0x12]; // forbidden_zero_bit=1
        let valid_slice = vec![0, 0, 0, 1, 0x25, 0x88]; // type 5, NRI=1, valid

        let packets = packetize_h264(
            &[bare4, bare3, zeros, nal_type0, invalid_fbit, valid_slice],
            pt,
            ssrc,
            &mut seq,
            ts,
        );

        assert_eq!(packets.len(), 1, "only valid slice should produce an RTP packet");
        assert_eq!(&packets[0][12..], &[0x25, 0x88]);
        assert_eq!(packets[0][1] & 0x80, 0x80, "single valid packet must have Marker bit M=1");
    }
}

//! Plain RTP bridge, port allocation, G.711 codec and SDP parsing for hardware terminals (RFC 3550 / RFC 4566).

use std::collections::BTreeSet;
use std::sync::Mutex;
use tokio::net::UdpSocket;

/// Выделенная пара UDP-сокетов (RTP четный, RTCP нечетный).
pub struct PortPair {
    pub rtp_port: u16,
    pub rtcp_port: u16,
    pub rtp_socket: UdpSocket,
    pub rtcp_socket: UdpSocket,
}

/// Пул портов RTP/RTCP (RFC 3550 §11: четные порты для RTP).
pub struct RtpPortPool {
    free_ports: Mutex<BTreeSet<u16>>,
}

impl Default for RtpPortPool {
    fn default() -> Self {
        Self::new(41000, 41200)
    }
}

impl RtpPortPool {
    pub fn new(start: u16, end: u16) -> Self {
        let mut set = BTreeSet::new();
        let mut p = if start.is_multiple_of(2) { start } else { start + 1 };
        while p < end {
            set.insert(p);
            p += 2;
        }
        Self { free_ports: Mutex::new(set) }
    }

    /// Выделяет пару свободных сокетов (RTP + RTCP) с привязкой к адресу.
    pub async fn allocate_pair(&self, bind_ip: &str) -> Option<PortPair> {
        let ports: Vec<u16> = {
            let lock = self.free_ports.lock().ok()?;
            lock.iter().copied().collect()
        };

        for port in ports {
            let rtp_addr = format!("{bind_ip}:{port}");
            let rtcp_addr = format!("{bind_ip}:{}", port + 1);
            if let Ok(rtp) = UdpSocket::bind(&rtp_addr).await {
                if let Ok(rtcp) = UdpSocket::bind(&rtcp_addr).await {
                    if let Ok(mut lock) = self.free_ports.lock() {
                        lock.remove(&port);
                    }
                    return Some(PortPair { rtp_port: port, rtcp_port: port + 1, rtp_socket: rtp, rtcp_socket: rtcp });
                }
            }
        }
        None
    }

    /// Возвращает порт в пул.
    pub fn release_port(&self, port: u16) {
        if let Ok(mut lock) = self.free_ports.lock() {
            let even = if port.is_multiple_of(2) { port } else { port - 1 };
            lock.insert(even);
        }
    }
}

// ---------------------------------------------------------------------------
// G.711 µ-law & A-law Codecs + Resampling
// ---------------------------------------------------------------------------

/// Кодирует 16-битный PCM сэмпл в G.711 µ-law байт.
pub fn linear_to_ulaw(sample: i16) -> u8 {
    let sign = (sample >> 8) & 0x80;
    let mut abs = if sample < 0 { -sample } else { sample };
    if abs > 32635 {
        abs = 32635;
    }
    abs += 0x84;

    let mut exponent = 7;
    let mut mask = 0x4000;
    while (abs & mask) == 0 && exponent > 0 {
        exponent -= 1;
        mask >>= 1;
    }
    let mantissa = (abs >> (exponent + 3)) & 0x0F;
    let byte = (sign | (exponent << 4) | mantissa) as u8;
    !byte
}

/// Декодирует G.711 µ-law байт в linear PCM сэмпл (f32 в диапазоне [-1.0, 1.0]).
pub fn ulaw_to_f32(byte: u8) -> f32 {
    let byte = !byte;
    let sign = (byte & 0x80) != 0;
    let exponent = ((byte >> 4) & 0x07) as i32;
    let mantissa = (byte & 0x0F) as i32;

    let mut sample = ((mantissa << 3) + 0x84) << exponent;
    sample -= 0x84;
    let i_sample = if sign { -sample } else { sample };
    (i_sample as f32) / 32768.0
}

/// Декодирует массив G.711 µ-law байтов в сэмплы f32.
pub fn decode_ulaw(src: &[u8], dst: &mut [f32]) {
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        *d = ulaw_to_f32(*s);
    }
}

/// Кодирует массив f32 [-1.0, 1.0] в G.711 µ-law байты.
pub fn encode_ulaw(src: &[f32], dst: &mut [u8]) {
    for (s, d) in src.iter().zip(dst.iter_mut()) {
        let clamped = s.clamp(-1.0, 1.0);
        let sample = (clamped * 32767.0) as i16;
        *d = linear_to_ulaw(sample);
    }
}

/// Понижает частоту дискретизации с 48 кГц до 8 кГц (6:1 усреднение).
pub fn downsample_48k_to_8k(src: &[f32], dst: &mut [f32; 160]) {
    dst.fill(0.0);
    let chunks = (src.len() / 6).min(160);
    for i in 0..chunks {
        let chunk = &src[i * 6..(i + 1) * 6];
        let sum: f32 = chunk.iter().sum();
        dst[i] = sum / 6.0;
    }
}

/// Повышает частоту дискретизации с 8 кГц до 48 кГц (линейная интерполяция 1:6).
pub fn upsample_8k_to_48k(src: &[f32; 160], dst: &mut [f32; 960]) {
    for i in 0..159 {
        let s0 = src[i];
        let s1 = src[i + 1];
        let step = (s1 - s0) / 6.0;
        for k in 0..6 {
            dst[i * 6 + k] = s0 + step * (k as f32);
        }
    }
    let last = src[159];
    for k in 0..6 {
        dst[954 + k] = last;
    }
}

/// Повышает 16 кГц → 48 кГц (линейная интерполяция 1:3, 20 мс).
pub fn upsample_16k_to_48k(src: &[f32; 320], dst: &mut [f32; 960]) {
    for i in 0..319 {
        let s0 = src[i];
        let s1 = src[i + 1];
        let step = (s1 - s0) / 3.0;
        for k in 0..3 {
            dst[i * 3 + k] = s0 + step * (k as f32);
        }
    }
    let last = src[319];
    for k in 0..3 {
        dst[957 + k] = last;
    }
}

/// Понижает 48 кГц → 16 кГц (усреднение 3 отсчётов, 20 мс).
pub fn downsample_48k_to_16k(src: &[f32], dst: &mut [f32; 320]) {
    dst.fill(0.0);
    let chunks = (src.len() / 3).min(320);
    for i in 0..chunks {
        let chunk = &src[i * 3..(i + 1) * 3];
        dst[i] = chunk.iter().sum::<f32>() / 3.0;
    }
}

// ---------------------------------------------------------------------------
// RFC 3550 RTP Header Serializer / Deserializer
// ---------------------------------------------------------------------------

/// Записывает 12-байтовый RTP заголовок (RFC 3550).
pub fn write_rtp_header(buf: &mut [u8], pt: u8, seq: u16, ts: u32, ssrc: u32, marker: bool) {
    assert!(buf.len() >= 12, "buffer too small for RTP header");
    buf[0] = 0x80; // V=2, P=0, X=0, CC=0
    buf[1] = (if marker { 0x80 } else { 0x00 }) | (pt & 0x7F);
    buf[2..4].copy_from_slice(&seq.to_be_bytes());
    buf[4..8].copy_from_slice(&ts.to_be_bytes());
    buf[8..12].copy_from_slice(&ssrc.to_be_bytes());
}

/// Разбирает базовый RTP заголовок. Возвращает (payload_type, seq, ts, ssrc, payload_offset).
pub fn parse_rtp_header(packet: &[u8]) -> Option<(u8, u16, u32, u32, usize)> {
    if packet.len() < 12 {
        return None;
    }
    let v = (packet[0] >> 6) & 0x03;
    if v != 2 {
        return None;
    }
    let cc = (packet[0] & 0x0F) as usize;
    let pt = packet[1] & 0x7F;
    let seq = u16::from_be_bytes([packet[2], packet[3]]);
    let ts = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    let ssrc = u32::from_be_bytes([packet[8], packet[9], packet[10], packet[11]]);

    let mut header_len = 12 + cc * 4;
    if packet.len() < header_len {
        return None;
    }
    if packet[0] & 0x10 != 0 {
        if packet.len() < header_len + 4 {
            return None;
        }
        let words = u16::from_be_bytes([packet[header_len + 2], packet[header_len + 3]]) as usize;
        header_len = header_len.checked_add(4 + words * 4)?;
        if packet.len() < header_len {
            return None;
        }
    }
    if packet[0] & 0x20 != 0 {
        let padding = *packet.last()? as usize;
        if padding == 0 || padding > packet.len() - header_len {
            return None;
        }
    }
    Some((pt, seq, ts, ssrc, header_len))
}

#[cfg(test)]
mod header_regressions {
    use super::*;
    #[test]
    fn rtp_extension_is_not_audio_payload() {
        let mut packet = vec![0x90, 0, 0, 1, 0, 0, 0, 160, 0, 0, 0, 1, 0xbe, 0xde, 0, 1, 0x10, 0x7f, 0, 0, 0xff];
        assert_eq!(parse_rtp_header(&packet).unwrap().4, 20);
        packet.truncate(19);
        assert!(parse_rtp_header(&packet).is_none());
    }
}

// ---------------------------------------------------------------------------
// Outbound codec matrix (SDP offer) and the gateway's real capabilities
// ---------------------------------------------------------------------------
//
// The offer advertises the full terminal compatibility matrix so any hardware
// terminal completes offer/answer in one shot. Advertising a codec in SDP is
// **not** a claim that the media bridge can process it:
//
// * Native today (really decoded and re-encoded by this gateway):
//   PCMU, PCMA (G.711), G.722 (PT9, Mode1 64k), G.726-16/24/32/40
//   (PT107-110, RFC 3551 LSB-first), G.728 (PT15), Opus (PT111, 48 kHz)
//   and H.264 Constrained Baseline, packetization-mode=1.
// * Everything else is offered for interoperability and **refused on answer**:
//   [`negotiate_media_detailed`] returns [`NegotiationFailure::UnsupportedAudio`]
//   instead of handing a codec we cannot bridge to the RTP bridge (that would
//   produce a connected call with dead audio). The outgoing caller answers such
//   an SDP with ACK+BYE; the inbound SIP UAS (T07c) refuses before 200 with a
//   wire-level 488 Not Acceptable Here.
//
// Payload numbering follows RFC 3551 for static assignments (0/8/9/15/4/18/31/34)
// and dynamic 96..127 otherwise. G.722.1 is `G7221/16000` (24/32 kbit/s) and
// G.722.1C is `G7221/32000` (24/32/48 kbit/s) — the same encoding names a real
// Polycom RPD 3.5 uses (`tests/fixtures/polycom_rpd_3_5_answer.sdp`).

/// One codec of the outbound offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OfferCodec {
    /// RTP payload type used in the offer.
    pub payload: u8,
    /// `a=rtpmap` encoding name (case-insensitive on the wire).
    pub name: &'static str,
    /// `a=rtpmap` clock rate.
    pub clock: u32,
    /// `a=rtpmap` channel count (1 = omitted in SDP).
    pub channels: u8,
    /// Optional `a=fmtp` parameters.
    pub fmtp: Option<&'static str>,
    /// `true` only when this gateway really decodes/encodes the codec.
    pub native: bool,
    /// RFC 4733 `telephone-event` pseudo-codec (never selected as media).
    pub dtmf: bool,
}

/// Audio codecs advertised in the outbound offer, in `m=audio` order.
///
/// G.729/G.722.1(C)/G.719/MP4A-LATM are listed only in [`CodecProfile::Full`]
/// (lab/transcoder matrix) and are answered with an explicit 488 until their
/// worker bridge lands — never silently downgraded. The native profile omits
/// every `native: false` entry.
pub const AUDIO_OFFER: &[OfferCodec] = &[
    OfferCodec { payload: 0, name: "PCMU", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec { payload: 8, name: "PCMA", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec { payload: 9, name: "G722", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec { payload: 15, name: "G728", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec {
        payload: 4,
        name: "G723",
        clock: 8000,
        channels: 1,
        fmtp: Some("annexa=no"),
        native: true,
        dtmf: false,
    },
    OfferCodec {
        payload: 18,
        name: "G729",
        clock: 8000,
        channels: 1,
        fmtp: Some("annexb=no"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 102,
        name: "G7221",
        clock: 16000,
        channels: 1,
        fmtp: Some("bitrate=24000"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 103,
        name: "G7221",
        clock: 16000,
        channels: 1,
        fmtp: Some("bitrate=32000"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 104,
        name: "G7221",
        clock: 32000,
        channels: 1,
        fmtp: Some("bitrate=24000"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 105,
        name: "G7221",
        clock: 32000,
        channels: 1,
        fmtp: Some("bitrate=32000"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 106,
        name: "G7221",
        clock: 32000,
        channels: 1,
        fmtp: Some("bitrate=48000"),
        native: false,
        dtmf: false,
    },
    OfferCodec { payload: 107, name: "G726-16", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec { payload: 108, name: "G726-24", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec { payload: 109, name: "G726-32", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec { payload: 110, name: "G726-40", clock: 8000, channels: 1, fmtp: None, native: true, dtmf: false },
    OfferCodec {
        payload: 111,
        name: "opus",
        clock: 48000,
        channels: 2,
        // `stereo=0`/`sprop-stereo=0`: the bridge mixes mono even though the
        // rtpmap channel count is the codec's maximum (RFC 7587 §7.1).
        fmtp: Some("useinbandfec=1;stereo=0;sprop-stereo=0"),
        native: true,
        dtmf: false,
    },
    OfferCodec { payload: 112, name: "G719", clock: 48000, channels: 1, fmtp: None, native: false, dtmf: false },
    OfferCodec {
        payload: 113,
        name: "MP4A-LATM",
        clock: 90000,
        // RFC 3016: MPEG-4 LATM uses a fixed 90 kHz RTP clock and no
        // `channels` parameter in rtpmap.
        channels: 1,
        // FFmpeg's latm muxer emits the StreamMuxConfig in-band on the first
        // audioMuxElement, so cpresent=1 (and no config=) is the honest fmtp.
        fmtp: Some("profile-level-id=1;object=2;cpresent=1"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 101,
        name: "telephone-event",
        clock: 8000,
        channels: 1,
        fmtp: Some("0-16"),
        native: false,
        dtmf: true,
    },
];

/// Параметры видео-секции оффера из единого профиля шлюза.
///
/// `max-fs` — макроблоки 16×16, `max-mbps` — макроблоки/с, `max-br` — кбит/с.
/// Это не косметика: терминал верит fmtp и режет поток выше лимитов.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSdpProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub profile_level_id: String,
}

impl Default for VideoSdpProfile {
    fn default() -> Self {
        Self { width: 1280, height: 720, fps: 30, bitrate_kbps: 2048, profile_level_id: "42c01f".into() }
    }
}

impl VideoSdpProfile {
    pub fn max_fs(&self) -> u32 {
        self.width.div_ceil(16).max(1) * self.height.div_ceil(16).max(1)
    }
    pub fn max_mbps(&self) -> u32 {
        self.max_fs().saturating_mul(self.fps.max(1))
    }
    fn high_profile_level_id(&self) -> String {
        let suffix = self.profile_level_id.get(4..).unwrap_or("1f");
        format!("6400{suffix}")
    }
    fn fmtp(&self, high: bool) -> String {
        let id = if high { self.high_profile_level_id() } else { self.profile_level_id.clone() };
        format!(
            "packetization-mode=1;profile-level-id={id};max-mbps={};max-fs={};max-br={}",
            self.max_mbps(),
            self.max_fs(),
            self.bitrate_kbps
        )
    }
}

/// Video codecs advertised in the outbound offer, in `m=video` order.
///
/// Only bridgeable video is advertised: H.264 baseline/high and H.265.
/// H.261/H.263 entries return once their RFC 4587/4629 payloaders are wired.
pub const VIDEO_OFFER: &[OfferCodec] = &[
    OfferCodec {
        payload: 97,
        name: "H264",
        clock: 90000,
        channels: 0,
        fmtp: Some("packetization-mode=1;profile-level-id=42801e;max-mbps=13800;max-fs=920;max-br=256"),
        native: true,
        dtmf: false,
    },
    OfferCodec {
        payload: 98,
        name: "H264",
        clock: 90000,
        channels: 0,
        fmtp: Some("packetization-mode=1;profile-level-id=64001f;max-mbps=13800;max-fs=920;max-br=256"),
        native: false,
        dtmf: false,
    },
    OfferCodec {
        payload: 99,
        name: "H265",
        clock: 90000,
        channels: 0,
        // No packetization-mode for RFC 7798; profile-id 1 (Main) is what
        // libx265 emits at the gateway's slot profiles. sprop-* are optional
        // and would be stale after a resolution change, so they are omitted.
        fmtp: Some("profile-id=1"),
        native: true,
        dtmf: false,
    },
];

fn push_codec_attrs(sdp: &mut String, codecs: &[&OfferCodec]) {
    for codec in codecs.iter().copied() {
        if codec.channels > 1 {
            sdp.push_str(&format!("a=rtpmap:{} {}/{}/{}\r\n", codec.payload, codec.name, codec.clock, codec.channels));
        } else {
            sdp.push_str(&format!("a=rtpmap:{} {}/{}\r\n", codec.payload, codec.name, codec.clock));
        }
        if let Some(fmtp) = codec.fmtp {
            sdp.push_str(&format!("a=fmtp:{} {fmtp}\r\n", codec.payload));
        }
    }
}

fn push_video_attrs(sdp: &mut String, codecs: &[&OfferCodec], video: &VideoSdpProfile) {
    for codec in codecs.iter().copied() {
        sdp.push_str(&format!("a=rtpmap:{} {}/{}\r\n", codec.payload, codec.name, codec.clock));
        if codec.name == "H264" {
            let high = is_h264_high(codec);
            sdp.push_str(&format!("a=fmtp:{} {}\r\n", codec.payload, video.fmtp(high)));
        } else if let Some(fmtp) = codec.fmtp {
            sdp.push_str(&format!("a=fmtp:{} {fmtp}\r\n", codec.payload));
        }
    }
}

/// H.264 High определяется по fmtp (`profile-level-id=6400…`), а не по
/// магическому PT 98: нумерация payload type может измениться.
/// Availability gate for offered video entries. H.264/H.265 need a working
/// FFmpeg bridge; H.261/H.263 stay lab-only until their payloaders exist.
fn offer_video_enabled(name: &str, h264: bool, h265: bool) -> bool {
    match name.to_ascii_uppercase().as_str() {
        "H264" => h264,
        "H265" | "HEVC" => h265,
        _ => true,
    }
}

fn is_h264_high(codec: &OfferCodec) -> bool {
    codec.fmtp.is_some_and(|fmtp| fmtp.contains("profile-level-id=6400"))
}

fn payload_list(codecs: &[&OfferCodec]) -> String {
    codecs.iter().map(|codec| codec.payload.to_string()).collect::<Vec<_>>().join(" ")
}

/// Which codecs an outbound offer advertises.
///
/// * [`CodecProfile::Native`] — only codecs the media bridge really
///   decodes/encodes (G.711, G.722, G.723.1, G.726, G.728, Opus, H.264,
///   H.265 when the host probe found libx265, plus `telephone-event`).
///   This is the production default: a terminal answers with a codec we can actually
///   bridge, and the RTP bridge's single-payload-type pinning stays valid.
/// * [`CodecProfile::Full`] — the whole vendor matrix. Required for lab
///   interoperability checks, but it changes what a terminal chooses to send:
///   a Polycom RPD 3.5 offered the full matrix transmits **G.722.1C (PT 106)**
///   and **H.264 High (PT 98)** instead of PCMU/H.264 Baseline, and the current
///   bridge (which pins one payload type per direction) would drop that RTP.
///   Use `VKS_SDP_CODEC_MATRIX=full` only together with transcoding workers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodecProfile {
    /// Offer only codecs this gateway can bridge today (default).
    Native,
    /// Offer the complete compatibility matrix.
    Full,
}

impl CodecProfile {
    /// Reads `VKS_SDP_CODEC_MATRIX` (`full` enables the complete matrix).
    ///
    /// Полная матрица объявляет кодеки, которые без T06b-транскодера не
    /// мостятся; поэтому она разрешена только при `VKS_TRANSCODER_ENABLED=1`
    /// или явном lab-флаге `VKS_SDP_CODEC_MATRIX_LAB=1`. Иначе — Native.
    #[must_use]
    pub fn from_env() -> Self {
        let matrix = std::env::var("VKS_SDP_CODEC_MATRIX").ok();
        let transcoder = std::env::var("VKS_TRANSCODER_ENABLED").ok();
        let lab = std::env::var("VKS_SDP_CODEC_MATRIX_LAB").ok();
        Self::from_config(matrix.as_deref(), transcoder.as_deref(), lab.as_deref())
    }

    /// Тестируемая версия [`Self::from_env`] без чтения процесса.
    #[must_use]
    pub fn from_config(matrix: Option<&str>, transcoder: Option<&str>, lab: Option<&str>) -> Self {
        let requested_full = matrix.is_some_and(|value| value.trim().eq_ignore_ascii_case("full"));
        let mode = vks_common::codec_capabilities::matrix_mode(matrix, transcoder, lab);
        if requested_full && mode != "full" {
            tracing::warn!("VKS_SDP_CODEC_MATRIX=full ignored: transcoder is unavailable (T06b); using Native");
        }
        if mode == "full" {
            Self::Full
        } else {
            Self::Native
        }
    }

    fn audio(self) -> Vec<&'static OfferCodec> {
        // AAC-LATM is advertised only when the host probed both FFmpeg
        // directions; otherwise it would be "offer then 488 after dial".
        self.audio_with(vks_common::codec_capabilities::audio_codec_available("MP4A-LATM"))
    }

    fn audio_with(self, aac: bool) -> Vec<&'static OfferCodec> {
        AUDIO_OFFER
            .iter()
            .filter(|codec| if codec.payload == 113 { aac } else { self == Self::Full || codec.native || codec.dtmf })
            .collect()
    }

    fn video(self) -> Vec<&'static OfferCodec> {
        self.video_with(
            vks_common::codec_capabilities::video_codec_available("H264"),
            vks_common::codec_capabilities::video_codec_available("H265"),
        )
    }

    fn video_with(self, h264: bool, h265: bool) -> Vec<&'static OfferCodec> {
        VIDEO_OFFER
            .iter()
            .filter(|codec| self == Self::Full || codec.native)
            .filter(|codec| offer_video_enabled(codec.name, h264, h265))
            .collect()
    }
}

/// Builds the offer for an explicit profile (tests and callers that must not
/// depend on the environment).
pub fn build_offer(profile: CodecProfile, local_ip: &str, audio_port: u16, video_port: Option<u16>) -> String {
    build_offer_with_video(profile, &VideoSdpProfile::default(), local_ip, audio_port, video_port)
}

/// Строит оффер с видео-лимитами из единого профиля шлюза: `b=AS`,
/// `max-fs/max-mbps/max-br`, `profile-level-id` соответствуют реальному
/// энкодеру, а не зашитым 640×360.
pub fn build_offer_with_video(
    profile: CodecProfile,
    video_profile: &VideoSdpProfile,
    local_ip: &str,
    audio_port: u16,
    video_port: Option<u16>,
) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let audio = profile.audio();
    let video = profile.video();

    let mut sdp = format!(
        "v=0\r\n\
o=VKS-MCU {now} 1 IN IP4 {local_ip}\r\n\
s=VKS Conference\r\n\
c=IN IP4 {local_ip}\r\n\
t=0 0\r\n\
m=audio {audio_port} RTP/AVP {}\r\n",
        payload_list(&audio)
    );
    push_codec_attrs(&mut sdp, &audio);
    // G.723.1 uses 30 ms frames; a media-level 20 ms ptime would contradict
    // it.  Both the full/lab and native profiles now contain G723, so no
    // single ptime is emitted; 20 ms codecs use their default (RFC 3551).
    if audio.iter().any(|codec| codec.name == "G723") {
        // One 30 ms G.723.1 frame per packet; no media-level ptime preference.
        sdp.push_str("a=maxptime:30\r\n");
    } else {
        sdp.push_str("a=ptime:20\r\na=maxptime:20\r\n");
    }
    sdp.push_str(&format!("a=sendrecv\r\na=rtcp:{}\r\n", audio_port + 1));

    if let Some(vport) = video_port {
        if !video.is_empty() {
            sdp.push_str(&format!("m=video {vport} RTP/AVP {}\r\n", payload_list(&video)));
            sdp.push_str(&format!("b=AS:{}\r\n", video_profile.bitrate_kbps));
            push_video_attrs(&mut sdp, &video, video_profile);
            sdp.push_str(&format!("a=sendrecv\r\na=rtcp:{}\r\n", vport + 1));
        }
    }

    sdp
}

/// Формирует plain RTP SDP offer для аппаратного терминала с профилем из
/// окружения (`VKS_SDP_CODEC_MATRIX`, по умолчанию `native`).
pub fn build_plain_sdp(local_ip: &str, audio_port: u16, video_port: Option<u16>) -> String {
    build_offer(CodecProfile::from_env(), local_ip, audio_port, video_port)
}

/// То же, но с профилем видео из настроек комнаты/гейтвея.
pub fn build_plain_sdp_with_video(
    video_profile: &VideoSdpProfile,
    local_ip: &str,
    audio_port: u16,
    video_port: Option<u16>,
) -> String {
    build_offer_with_video(CodecProfile::from_env(), video_profile, local_ip, audio_port, video_port)
}

/// Лимиты видео, которые терминал объявил в answer. Если поле отсутствует,
/// используется наш профиль; если присутствует — исходящий поток обязан быть
/// не выше него.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VideoAnswerLimits {
    pub max_fs: Option<u32>,
    pub max_br: Option<u32>,
    pub max_mbps: Option<u32>,
    /// `b=AS` видео-секции answer (кбит/с), если терминал его прислал.
    pub b_as: Option<u32>,
}

/// Читает `max-fs/max-br/max-mbps` из видео-секции answer. `payload_type`
/// выбирает конкретный fmtp (терминал может прислать несколько H.264).
#[must_use]
pub fn video_answer_limits(answer: &str, payload_type: Option<u8>) -> VideoAnswerLimits {
    let Some(parsed) = parse_sdp(answer) else { return VideoAnswerLimits::default() };
    let Some(video) = parsed.media.iter().find(|media| media.kind == "video") else {
        return VideoAnswerLimits::default();
    };
    let payload = payload_type
        .filter(|payload| video.maps.contains_key(payload))
        .or_else(|| {
            // HashMap не даёт порядка: без явного PT берём H.264 с
            // минимальным payload type, иначе CI-флейк между 97/98.
            video
                .maps
                .iter()
                .filter(|(_, entry)| entry.name.eq_ignore_ascii_case("H264"))
                .map(|(payload, _)| *payload)
                .min()
        })
        .or_else(|| video.maps.keys().next().copied());
    let b_as = video.b_as;
    let Some(payload) = payload else { return VideoAnswerLimits { b_as, ..VideoAnswerLimits::default() } };
    let Some(fmtp) = video.fmtp.get(&payload) else {
        return VideoAnswerLimits { b_as, ..VideoAnswerLimits::default() };
    };
    let number =
        |key: &str| fmtp.get(key).and_then(|value| value.trim().parse::<u32>().ok()).filter(|value| *value > 0);
    VideoAnswerLimits { max_fs: number("max-fs"), max_br: number("max-br"), max_mbps: number("max-mbps"), b_as }
}

/// Извлекает IP и медиа-порты (audio, video) из SDP answer удаленного терминала.
pub fn parse_remote_media_ports(sdp: &str) -> (Option<String>, Option<u16>, Option<u16>) {
    let mut remote_ip = None;
    let mut audio_port = None;
    let mut video_port = None;

    for line in sdp.lines() {
        let trimmed = line.trim();
        if let Some(c_line) = trimmed.strip_prefix("c=IN IP4 ") {
            if remote_ip.is_none() {
                remote_ip = Some(c_line.trim().to_string());
            }
        } else if let Some(m_line) = trimmed.strip_prefix("m=") {
            let mut parts = m_line.split_whitespace();
            if let (Some(kind), Some(port_str)) = (parts.next(), parts.next()) {
                if let Ok(port) = port_str.parse::<u16>() {
                    if kind == "audio" {
                        audio_port = Some(port);
                    } else if kind == "video" {
                        video_port = Some(port);
                    }
                }
            }
        }
    }

    (remote_ip, audio_port, video_port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g711_ulaw_roundtrip() {
        let samples = [0.0f32, 0.5, -0.5, 0.99, -0.99];
        let mut encoded = [0u8; 5];
        let mut decoded = [0.0f32; 5];
        encode_ulaw(&samples, &mut encoded);
        decode_ulaw(&encoded, &mut decoded);

        for (orig, dec) in samples.iter().zip(decoded.iter()) {
            assert!((orig - dec).abs() < 0.05, "diff too large: {orig} vs {dec}");
        }
    }

    #[test]
    fn resampling_cadence() {
        let mut src_8k = [0.0f32; 160];
        // 200 Hz fundamental tone at 8 kHz
        for (i, sample) in src_8k.iter_mut().enumerate() {
            *sample = ((i as f32) * 2.0 * std::f32::consts::PI * 200.0 / 8000.0).sin();
        }
        let mut dst_48k = [0.0f32; 960];
        upsample_8k_to_48k(&src_8k, &mut dst_48k);
        assert_eq!(dst_48k.len(), 960);

        let mut roundtrip_8k = [0.0f32; 160];
        downsample_48k_to_8k(&dst_48k, &mut roundtrip_8k);
        for i in 1..159 {
            assert!((src_8k[i] - roundtrip_8k[i]).abs() < 0.2);
        }
    }

    #[test]
    fn wideband_resampling_cadence() {
        let mut src_16k = [0.0f32; 320];
        for (i, sample) in src_16k.iter_mut().enumerate() {
            *sample = ((i as f32) * 2.0 * std::f32::consts::PI * 440.0 / 16_000.0).sin();
        }
        let mut dst_48k = [0.0f32; 960];
        upsample_16k_to_48k(&src_16k, &mut dst_48k);
        let mut roundtrip = [0.0f32; 320];
        downsample_48k_to_16k(&dst_48k, &mut roundtrip);
        for i in 1..319 {
            assert!((src_16k[i] - roundtrip[i]).abs() < 0.1, "sample {i}");
        }
    }

    #[test]
    fn rtp_header_roundtrip() {
        let mut buf = [0u8; 12];
        write_rtp_header(&mut buf, 0, 100, 16000, 0x12345678, true);
        let (pt, seq, ts, ssrc, offset) = parse_rtp_header(&buf).expect("parse rtp");
        assert_eq!(pt, 0);
        assert_eq!(seq, 100);
        assert_eq!(ts, 16000);
        assert_eq!(ssrc, 0x12345678);
        assert_eq!(offset, 12);
    }

    #[test]
    fn sdp_parse_answer() {
        let sdp = "v=0\r\n\
o=term 1 1 IN IP4 192.168.1.132\r\n\
c=IN IP4 192.168.1.132\r\n\
m=audio 5004 RTP/AVP 0\r\n\
m=video 5006 RTP/AVP 96\r\n";
        let (ip, a_port, v_port) = parse_remote_media_ports(sdp);
        assert_eq!(ip, Some("192.168.1.132".to_string()));
        assert_eq!(a_port, Some(5004));
        assert_eq!(v_port, Some(5006));
    }

    #[test]
    fn offer_carries_full_audio_matrix_with_correct_clock_rates_and_fmtp() {
        let sdp = build_offer(CodecProfile::Full, "192.168.1.93", 41000, Some(41002));
        for line in [
            "m=audio 41000 RTP/AVP 0 8 9 15 4 18 102 103 104 105 106 107 108 109 110 111 112 101",
            "a=rtpmap:0 PCMU/8000",
            "a=rtpmap:8 PCMA/8000",
            "a=rtpmap:9 G722/8000",
            "a=rtpmap:15 G728/8000",
            "a=rtpmap:4 G723/8000",
            "a=rtpmap:18 G729/8000",
            "a=fmtp:18 annexb=no",
            "a=rtpmap:102 G7221/16000",
            "a=fmtp:102 bitrate=24000",
            "a=rtpmap:103 G7221/16000",
            "a=fmtp:103 bitrate=32000",
            "a=rtpmap:104 G7221/32000",
            "a=rtpmap:105 G7221/32000",
            "a=rtpmap:106 G7221/32000",
            "a=fmtp:106 bitrate=48000",
            "a=rtpmap:107 G726-16/8000",
            "a=rtpmap:108 G726-24/8000",
            "a=rtpmap:109 G726-32/8000",
            "a=rtpmap:110 G726-40/8000",
            "a=rtpmap:111 opus/48000/2",
            "a=rtpmap:112 G719/48000",
            "a=rtpmap:101 telephone-event/8000",
            "a=fmtp:101 0-16",
            "a=rtcp:41001",
        ] {
            assert!(sdp.contains(line), "offer misses `{line}`:\n{sdp}");
        }
        assert!(!sdp.contains("a=rtpmap:113 "), "AAC needs the probe even in Full:\n{sdp}");
    }

    #[test]
    fn full_matrix_is_gated_by_transcoder_or_lab_flag() {
        assert_eq!(CodecProfile::from_config(Some("full"), None, None), CodecProfile::Native);
        assert_eq!(CodecProfile::from_config(Some("full"), Some("0"), None), CodecProfile::Native);
        assert_eq!(CodecProfile::from_config(Some("full"), Some("1"), None), CodecProfile::Full);
        assert_eq!(CodecProfile::from_config(Some("FULL"), None, Some("1")), CodecProfile::Full);
        assert_eq!(CodecProfile::from_config(Some("native"), Some("1"), Some("1")), CodecProfile::Native);
    }

    #[test]
    fn offer_native_flags_match_shared_capabilities() {
        use vks_common::codec_capabilities::{is_native_audio, is_native_video};
        for codec in AUDIO_OFFER {
            if codec.dtmf {
                continue;
            }
            assert_eq!(codec.native, is_native_audio(codec.name), "audio native mismatch: {codec:?}");
        }
        for codec in VIDEO_OFFER {
            let expected = is_native_video(codec.name) && !is_h264_high(codec);
            assert_eq!(codec.native, expected, "video native mismatch: {codec:?}");
        }
    }

    #[test]
    fn offer_carries_bridgeable_video_matrix() {
        let sdp = build_offer(CodecProfile::Full, "192.168.1.93", 41000, Some(41002));
        for line in [
            "m=video 41002 RTP/AVP 97 98 99",
            "a=rtpmap:97 H264/90000",
            "b=AS:2048",
            "a=fmtp:97 packetization-mode=1;profile-level-id=42c01f;max-mbps=108000;max-fs=3600;max-br=2048",
            "a=rtpmap:98 H264/90000",
            "a=fmtp:98 packetization-mode=1;profile-level-id=64001f;max-mbps=108000;max-fs=3600;max-br=2048",
            "a=rtpmap:99 H265/90000",
            "a=fmtp:99 profile-id=1",
            "a=rtcp:41003",
        ] {
            assert!(sdp.contains(line), "offer misses `{line}`:\n{sdp}");
        }
    }

    #[test]
    fn offer_video_limits_follow_effective_profile() {
        let low =
            VideoSdpProfile { width: 640, height: 360, fps: 15, bitrate_kbps: 512, profile_level_id: "42c01e".into() };
        let high = VideoSdpProfile {
            width: 1920,
            height: 1080,
            fps: 30,
            bitrate_kbps: 4096,
            profile_level_id: "42c028".into(),
        };
        let low_sdp = build_offer_with_video(CodecProfile::Native, &low, "192.168.1.93", 41000, Some(41002));
        assert!(low_sdp.contains("b=AS:512"), "{low_sdp}");
        assert!(low_sdp.contains("max-fs=920;max-br=512"), "{low_sdp}");
        assert!(low_sdp.contains("max-mbps=13800"), "{low_sdp}");
        assert!(low_sdp.contains("profile-level-id=42c01e"), "{low_sdp}");
        let high_sdp = build_offer_with_video(CodecProfile::Native, &high, "192.168.1.93", 41000, Some(41002));
        assert!(high_sdp.contains("b=AS:4096"), "{high_sdp}");
        assert!(high_sdp.contains("max-fs=8160;max-br=4096"), "{high_sdp}");
        assert!(high_sdp.contains("max-mbps=244800"), "{high_sdp}");
        assert!(high_sdp.contains("profile-level-id=42c028"), "{high_sdp}");
        let parsed = parse_sdp(&high_sdp).expect("dynamic offer must parse");
        let video = parsed.media.iter().find(|media| media.kind == "video").expect("video section");
        assert_eq!(video.b_as, Some(4096));
    }

    #[test]
    fn answer_limits_read_h264_fmtp_and_b_as_deterministically() {
        let answer = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\n\
c=IN IP4 192.168.1.132\r\nt=0 0\r\n\
m=audio 5000 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
m=video 5002 RTP/AVP 98 97 99\r\nb=AS:3000\r\n\
a=rtpmap:98 H264/90000\r\na=fmtp:98 profile-level-id=64001f;max-fs=8160;max-br=4096\r\n\
a=rtpmap:97 H264/90000\r\na=fmtp:97 profile-level-id=42c01f;max-fs=920;max-br=1024;max-mbps=13800\r\n\
a=rtpmap:99 H265/90000\r\na=fmtp:99 profile-id=1\r\n";
        let limits = video_answer_limits(answer, Some(97));
        assert_eq!(limits.max_fs, Some(920));
        assert_eq!(limits.max_br, Some(1024));
        assert_eq!(limits.max_mbps, Some(13800));
        assert_eq!(limits.b_as, Some(3000));
        // Без явного PT выбираем H.264, а не H.265: порядок maps не задаёт протокол.
        let fallback = video_answer_limits(answer, None);
        assert_eq!(fallback.max_fs, Some(920), "без PT выбираем H.264 с минимальным payload type (97)");
    }

    #[test]
    fn native_profile_offers_only_bridgeable_codecs() {
        let sdp = build_offer(CodecProfile::Native, "192.168.1.93", 41000, Some(41002));
        for line in [
            "m=audio 41000 RTP/AVP 0 8 9 15 4 107 108 109 110 111 101",
            "a=rtpmap:111 opus/48000/2",
            "a=fmtp:111 useinbandfec=1;stereo=0;sprop-stereo=0",
            "a=rtpmap:4 G723/8000",
            "a=fmtp:4 annexa=no",
            "a=rtpmap:0 PCMU/8000",
            "a=rtpmap:8 PCMA/8000",
            "a=rtpmap:101 telephone-event/8000",
            "m=video 41002 RTP/AVP 97 99",
            "a=rtpmap:97 H264/90000",
            "a=rtpmap:99 H265/90000",
            "a=fmtp:99 profile-id=1",
        ] {
            assert!(sdp.contains(line), "native offer misses `{line}`:\n{sdp}");
        }
        // G.723.1 is 30 ms; a single 20 ms ptime hint would contradict it.
        assert!(!sdp.contains("a=ptime:"), "native G723 offer must not claim 20 ms ptime:\n{sdp}");
        assert!(sdp.contains("a=maxptime:30"), "native G723 offer caps packets at 30 ms:\n{sdp}");
        for absent in ["G7221", "G729", "G719", "H263", "MP4A-LATM"] {
            assert!(!sdp.contains(absent), "native offer must not advertise unbridgeable `{absent}`:\n{sdp}");
        }
    }

    #[test]
    fn native_offer_negotiates_polycom_style_answer_without_pt_drift() {
        // With the native profile a terminal has no unbridgeable choice, so the
        // payload types it answers with are exactly the ones the bridge pins.
        let offer = build_offer(CodecProfile::Native, "192.168.1.93", 41000, Some(41002));
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\n\
m=audio 3230 RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n\
m=video 3232 RTP/AVP 97\r\na=rtpmap:97 H264/90000\r\n\
a=fmtp:97 packetization-mode=1;profile-level-id=42801e;max-mbps=49000;max-br=20010\r\n";
        let media = negotiate_media(&offer, answer).expect("PCMU + H264 baseline");
        assert_eq!(media.audio_payload_type, 0);
        assert_eq!(media.audio_codec, "PCMU");
        assert_eq!(media.video_payload_type, Some(97));
        assert_eq!(media.local_video_payload_type, Some(97));
    }

    #[test]
    fn audio_only_offer_has_no_video_section() {
        let sdp = build_offer(CodecProfile::Full, "192.168.1.93", 41000, None);
        assert!(!sdp.contains("m=video"));
        assert!(sdp.contains("m=audio 41000"));
    }

    #[test]
    fn own_offer_round_trips_and_telephone_event_is_never_selected_as_audio() {
        let offer = build_offer(CodecProfile::Full, "10.0.0.1", 42000, Some(42002));
        let parsed = parse_sdp(&offer).expect("own offer must parse");
        assert_eq!(parsed.media.len(), 2);
        assert_eq!(parsed.media[0].kind, "audio");
        assert_eq!(parsed.media[0].port, 42000);
        assert_eq!(parsed.media[1].kind, "video");
        assert_eq!(parsed.media[1].port, 42002);
        assert!(parsed.media[0].maps.get(&101).is_some_and(|map| map.name.eq_ignore_ascii_case("telephone-event")));
        let answer =
            "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 101\r\na=rtpmap:101 telephone-event/8000\r\n";
        assert_eq!(negotiate_media_detailed(&offer, answer), Err(NegotiationFailure::NoCommonAudio));
    }
}

/// Accepted plain-RTP answer, restricted to codecs this bridge actually bridges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NegotiatedMedia {
    pub ip: String,
    pub audio_port: u16,
    pub audio_payload_type: u8,
    /// Canonical name of the negotiated audio codec (always natively bridged).
    pub audio_codec: &'static str,
    pub video_port: Option<u16>,
    pub video_payload_type: Option<u8>,
    pub dtmf_payload_type: Option<u8>,
    pub local_video_payload_type: Option<u8>,
    pub video_ip: Option<String>,
    /// Canonical name of the negotiated video codec ("H264" or "H265").
    pub video_codec: Option<&'static str>,
}

/// Why a remote answer cannot be bridged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NegotiationFailure {
    /// The SDP has no usable (inactive/zero-port/parse) audio section.
    UnusableSdp,
    /// No payload type is common to both sides.
    NoCommonAudio,
    /// A common codec was selected by the peer, but this gateway cannot
    /// decode/encode it today (needs a transcoder worker, not a silent bridge).
    UnsupportedAudio { codec: String, payload: u8 },
}

/// One `a=rtpmap` declaration of a parsed media section.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RtpMapEntry {
    name: String,
    clock: u32,
    channels: u8,
}

#[derive(Default)]
struct SdpMedia {
    kind: String,
    transport: String,
    port: u16,
    ip: Option<String>,
    duplex: bool,
    payloads: Vec<u8>,
    maps: std::collections::HashMap<u8, RtpMapEntry>,
    fmtp: std::collections::HashMap<u8, std::collections::HashMap<String, String>>,
    b_as: Option<u32>,
}

struct SdpDescription {
    ip: Option<String>,
    media: Vec<SdpMedia>,
}

/// Tolerant SDP parser. Malformed or vendor-specific attribute lines are
/// skipped instead of aborting the whole description: a terminal that adds one
/// unknown `a=rtpmap`/`a=fmtp` must still be able to complete a call.
fn parse_sdp(sdp: &str) -> Option<SdpDescription> {
    let mut description = SdpDescription { ip: None, media: Vec::new() };
    let mut session_duplex = true;
    for line in sdp.lines().map(str::trim) {
        if let Some(value) = line.strip_prefix("m=") {
            let mut parts = value.split_whitespace();
            let kind = parts.next()?.to_string();
            let port = parts.next()?.split('/').next()?.parse::<u16>().ok()?;
            let transport = parts.next()?.to_string();
            let payloads: Vec<u8> = parts.filter_map(|value| value.parse::<u8>().ok()).collect();
            description.media.push(SdpMedia {
                kind,
                transport,
                port,
                duplex: session_duplex,
                payloads,
                ..SdpMedia::default()
            });
        } else if let Some(value) = line.strip_prefix("b=AS:") {
            if let Some(media) = description.media.last_mut() {
                media.b_as = value.trim().parse::<u32>().ok();
            }
        } else if let Some(value) = line.strip_prefix("c=IN IP4 ") {
            if let Some(ip) = value.split('/').next().map(str::trim) {
                // 0.0.0.0 (hold) and multicast addresses are not media endpoints;
                // ignore them instead of rejecting the whole answer.
                if ip.parse::<std::net::Ipv4Addr>().is_ok_and(|address| {
                    !address.is_unspecified() && !address.is_multicast() && !address.is_broadcast()
                }) {
                    if let Some(media) = description.media.last_mut() {
                        media.ip = Some(ip.to_string());
                    } else {
                        description.ip = Some(ip.to_string());
                    }
                }
            }
        } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
            if let Some((payload, rest)) = value.split_once(char::is_whitespace) {
                let mut fields = rest.trim().split('/');
                let name = fields.next().unwrap_or_default().trim().to_string();
                let clock = fields.next().and_then(|clock| clock.trim().parse::<u32>().ok());
                if let (Ok(payload), Some(clock), false) = (payload.trim().parse::<u8>(), clock, name.is_empty()) {
                    let channels = fields.next().and_then(|channels| channels.trim().parse::<u8>().ok()).unwrap_or(1);
                    if let Some(media) = description.media.last_mut() {
                        media.maps.insert(payload, RtpMapEntry { name, clock, channels });
                    }
                }
            }
        } else if let Some(value) = line.strip_prefix("a=fmtp:") {
            if let Some((payload, parameters)) = value.split_once(char::is_whitespace) {
                if let Some(media) = description.media.last_mut() {
                    if let Ok(payload) = payload.trim().parse::<u8>() {
                        let entry = media.fmtp.entry(payload).or_default();
                        for parameter in parameters.split(';').map(str::trim) {
                            if let Some((key, value)) = parameter.split_once('=') {
                                let key = key.trim().to_ascii_lowercase();
                                let value = value.trim();
                                // Conflicting duplicates must fail closed; a
                                // "last key wins" map would hide object=8.
                                if entry.get(&key).is_some_and(|previous| previous != value) {
                                    entry.insert(key, "!conflict!".to_string());
                                } else {
                                    entry.insert(key, value.to_string());
                                }
                            }
                        }
                    }
                }
            }
        } else if ["a=sendrecv", "a=sendonly", "a=recvonly", "a=inactive"].contains(&line) {
            let duplex = line == "a=sendrecv";
            if let Some(media) = description.media.last_mut() {
                media.duplex = duplex;
            } else {
                session_duplex = duplex;
            }
        }
    }
    Some(description)
}

fn usable_media<'a>(description: &'a SdpDescription, kind: &str) -> Option<&'a SdpMedia> {
    description
        .media
        .iter()
        .find(|media| media.kind == kind && media.port > 0 && media.transport == "RTP/AVP" && media.duplex)
}

fn audio_offer_entry(payload: u8) -> Option<&'static OfferCodec> {
    AUDIO_OFFER.iter().find(|codec| codec.payload == payload)
}

/// Resolve a payload type of a parsed media section against [`AUDIO_OFFER`].
///
/// The declared `a=rtpmap` must agree with the table: a static payload type
/// redefined as another codec makes that payload unusable. RFC 3551 allows
/// static payload types to omit `a=rtpmap`.
fn audio_identity(media: &SdpMedia, payload: u8) -> Option<&'static OfferCodec> {
    // A static payload type is still only present if it is listed in `m=`.
    // Without this check PT9 G722 was treated as offered merely because the
    // static table knows it, and a PCMU-only offer negotiated G722.
    if !media.payloads.contains(&payload) {
        return None;
    }
    let local = audio_offer_entry(payload)?;
    match media.maps.get(&payload) {
        Some(map) => {
            let matches = map.name.eq_ignore_ascii_case(local.name)
                && map.clock == local.clock
                && (local.channels <= 1 || map.channels == local.channels);
            matches.then_some(local)
        }
        None => (payload < 96).then_some(local),
    }
}

fn telephone_event(media: &SdpMedia) -> Option<u8> {
    media.payloads.iter().copied().find(|payload| {
        (96..=127).contains(payload)
            && media
                .maps
                .get(payload)
                .is_some_and(|map| map.name.eq_ignore_ascii_case("telephone-event") && map.clock == 8000)
    })
}

/// Local video codec selected for one payload. H.264 high/mode0 are distinct
/// configurations and are not selected; H.265 is RFC 7798 without DONL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NegotiatedVideoCodec {
    H264,
    H265,
}

impl NegotiatedVideoCodec {
    const fn name(self) -> &'static str {
        match self {
            Self::H264 => "H264",
            Self::H265 => "H265",
        }
    }
}

/// RFC 7798 `sprop-max-don-diff=0` means "no DONL" and is compatible with the
/// DON-less payloader/depacketizer. Any positive or unparsable value is not.
fn h265_don_compatible(value: Option<&str>) -> bool {
    match value {
        None => true,
        Some(value) => value.trim().parse::<u32>().is_ok_and(|difference| difference == 0),
    }
}

/// Recognises an H.265 payload in SDP (dynamic PT, H265/HEVC name, 90 kHz).
/// RFC 7798 has no packetization-mode; `profile-id`/`tier-flag`/`level-id` are
/// advisory here because libx265 chooses them from the slot.
fn h265_video(media: &SdpMedia, payload: u8) -> bool {
    let Some(map) = media.maps.get(&payload) else {
        return false;
    };
    if !((96..=127).contains(&payload)
        && (map.name.eq_ignore_ascii_case("H265") || map.name.eq_ignore_ascii_case("HEVC"))
        && map.clock == 90000)
    {
        return false;
    }
    let fmtp = media.fmtp.get(&payload);
    // Our H.265 sender is Main profile/Main tier; an answer that pins another
    // configuration under the same PT cannot be mirrored.
    if fmtp.and_then(|parameters| parameters.get("profile-id")).is_some_and(|value| value != "1") {
        return false;
    }
    if fmtp.and_then(|parameters| parameters.get("tier-flag")).is_some_and(|value| value != "0") {
        return false;
    }
    h265_don_compatible(fmtp.and_then(|parameters| parameters.get("sprop-max-don-diff")).map(String::as_str))
}

/// Resolves one remote video payload to a codec this bridge can carry.
fn video_identity(media: &SdpMedia, payload: u8) -> Option<NegotiatedVideoCodec> {
    if h264_baseline_level(media, payload).is_some() {
        return Some(NegotiatedVideoCodec::H264);
    }
    if h265_video(media, payload) {
        return Some(NegotiatedVideoCodec::H265);
    }
    None
}

/// Finds a local payload for the same codec (H.264 prefers compatible levels).
fn local_video_payload(media: &SdpMedia, codec: NegotiatedVideoCodec, remote_level: Option<u8>) -> Option<u8> {
    media.payloads.iter().copied().find(|payload| match codec {
        NegotiatedVideoCodec::H264 => h264_baseline_level(media, *payload)
            .is_some_and(|local_level| local_level.min(remote_level.unwrap_or(local_level)) >= 30),
        NegotiatedVideoCodec::H265 => h265_video(media, *payload),
    })
}

fn h264_baseline_level(media: &SdpMedia, payload: u8) -> Option<u8> {
    let map = media.maps.get(&payload)?;
    if !(96..=127).contains(&payload) || !map.name.eq_ignore_ascii_case("H264") || map.clock != 90000 {
        return None;
    }
    let parameters = media.fmtp.get(&payload)?;
    if parameters.get("packetization-mode").and_then(|value| value.parse::<u8>().ok()).unwrap_or(0) != 1 {
        return None;
    }
    let profile = parameters.get("profile-level-id")?;
    if profile.len() != 6 || !profile.is_ascii() {
        return None;
    }
    let bits = u32::from_str_radix(profile, 16).ok()?;
    let profile_idc = ((bits >> 16) & 255) as u8;
    let constraints = ((bits >> 8) & 255) as u8;
    let level = (bits & 255) as u8;
    if profile_idc != 66 || constraints & 3 != 0 || ![30, 31, 32, 40, 41, 42, 50, 51, 52].contains(&level) {
        return None;
    }
    for (key, needed) in [("max-fs", 920u32), ("max-mbps", 13800), ("max-br", 256)] {
        if let Some(value) = parameters.get(key) {
            if value.parse::<u32>().ok()? < needed {
                return None;
            }
        }
    }
    Some(level)
}

/// Intersect local offer with the peer's answer and report *why* it failed.
///
/// Selection rule: scan the answer's payloads in order and take the **first**
/// one that is present in our offer with the same payload number and the same
/// `rtpmap` identity (name + clock + channels). Payload numbers must be
/// symmetric because the RTP bridge uses one payload type for both directions.
///
/// Strict-first is deliberate: the peer expects to send its first choice. A
/// later native codec must not mask that choice (Polycom lists G722.1C before
/// PCMU), otherwise the gateway reports a connected call while the peer's RTP
/// is dropped by the source validator. If the first common codec is not
/// natively bridged, the call is refused with
/// [`NegotiationFailure::UnsupportedAudio`] instead of "connected with dead
/// audio"; vendor/unknown codecs never cause a parse error. Unsupported video
/// leaves audio usable.
pub fn negotiate_media_detailed(offer: &str, answer: &str) -> Result<NegotiatedMedia, NegotiationFailure> {
    negotiate_media_detailed_with(offer, answer, vks_common::codec_capabilities::audio_codec_available("MP4A-LATM"))
}

fn negotiate_media_detailed_with(
    offer: &str,
    answer: &str,
    aac_available: bool,
) -> Result<NegotiatedMedia, NegotiationFailure> {
    let local = parse_sdp(offer).ok_or(NegotiationFailure::UnusableSdp)?;
    let remote = parse_sdp(answer).ok_or(NegotiationFailure::UnusableSdp)?;
    let local_audio = usable_media(&local, "audio").ok_or(NegotiationFailure::NoCommonAudio)?;
    let remote_audio = usable_media(&remote, "audio").ok_or(NegotiationFailure::NoCommonAudio)?;

    // Strict-first: the first common codec is what the peer expects to send.
    // Falling through to a later native codec while the peer's first choice is
    // unbridged produces "connected" with dead audio (Polycom lists G722.1C
    // before PCMU). Either bridge the first one or refuse before 200 OK.
    let mut first_common = None;
    for payload in &remote_audio.payloads {
        let Some(remote_codec) = audio_identity(remote_audio, *payload) else {
            continue;
        };
        if remote_codec.dtmf {
            continue;
        }
        let Some(local_codec) = audio_identity(local_audio, *payload) else {
            continue;
        };
        if !local_codec.name.eq_ignore_ascii_case(remote_codec.name) || local_codec.clock != remote_codec.clock {
            continue;
        }
        first_common = Some(local_codec);
        break;
    }
    let codec = first_common.ok_or(NegotiationFailure::NoCommonAudio)?;
    let aac_latm = codec.payload == 113 && aac_available;
    if !codec.native && !aac_latm {
        return Err(NegotiationFailure::UnsupportedAudio { codec: codec.name.to_string(), payload: codec.payload });
    }
    if aac_latm {
        // Symmetric with the UAS: the answer must describe AAC-LC with in-band
        // SMC. bitrate/config hints are legal; cpresent=0, SBR and other object
        // types mean this bridge would answer but never decode audio.
        let fmtp = remote_audio.fmtp.get(&codec.payload);
        let rendered =
            fmtp.map(|params| params.iter().map(|(key, value)| format!("{key}={value}")).collect::<Vec<_>>().join(";"));
        if rendered.as_deref().is_some_and(|value| !crate::uas::aac_fmtp_is_bridgeable(value)) {
            return Err(NegotiationFailure::UnsupportedAudio { codec: codec.name.to_string(), payload: codec.payload });
        }
    }

    let ip = remote_audio.ip.clone().or(remote.ip.clone()).ok_or(NegotiationFailure::NoCommonAudio)?;
    let dtmf_payload_type = telephone_event(local_audio).and_then(|_| telephone_event(remote_audio));
    let mut video = None;
    if let (Some(local_video), Some(remote_video)) = (usable_media(&local, "video"), usable_media(&remote, "video")) {
        for transmit_payload in &remote_video.payloads {
            let Some(codec) = video_identity(remote_video, *transmit_payload) else {
                continue;
            };
            let remote_level = h264_baseline_level(remote_video, *transmit_payload);
            let Some(receive_payload) = local_video_payload(local_video, codec, remote_level) else {
                continue;
            };
            if let Some(video_ip) = remote_video.ip.clone().or(remote.ip.clone()) {
                video = Some((video_ip, remote_video.port, *transmit_payload, receive_payload, codec));
                break;
            }
        }
    }
    Ok(NegotiatedMedia {
        ip,
        audio_port: remote_audio.port,
        audio_payload_type: codec.payload,
        audio_codec: codec.name,
        dtmf_payload_type,
        video_ip: video.as_ref().map(|value| value.0.clone()),
        video_port: video.as_ref().map(|value| value.1),
        video_payload_type: video.as_ref().map(|value| value.2),
        local_video_payload_type: video.as_ref().map(|value| value.3),
        video_codec: video.as_ref().map(|value| value.4.name()),
    })
}

/// [`negotiate_media_detailed`] for callers that only need success/failure.
pub fn negotiate_media(offer: &str, answer: &str) -> Option<NegotiatedMedia> {
    negotiate_media_detailed(offer, answer).ok()
}

/// Answer inspection using this bridge's default capabilities. Production UAC
/// uses `negotiate_media` with the exact local offer instead of this convenience.
pub fn parse_negotiated_media(answer: &str) -> Option<NegotiatedMedia> {
    negotiate_media(&build_plain_sdp("127.0.0.1", 2, Some(4)), answer)
}

#[cfg(test)]
mod h265_offer_tests {
    use super::*;

    #[test]
    fn unavailable_h265_is_not_offered() {
        assert!(!offer_video_enabled("H265", true, false));
        assert!(!offer_video_enabled("HEVC", true, false));
        assert!(offer_video_enabled("H264", true, false));
        assert!(offer_video_enabled("H265", true, true));
        assert!(offer_video_enabled("H261", false, false));
    }

    #[test]
    fn native_offer_omits_unavailable_video_codecs() {
        let native = CodecProfile::Native.video_with(true, false);
        assert!(native.iter().any(|codec| codec.name == "H264"));
        assert!(!native.iter().any(|codec| codec.name == "H265"));
        let h265_only = CodecProfile::Native.video_with(false, true);
        assert!(!h265_only.iter().any(|codec| codec.name == "H264"));
        assert!(h265_only.iter().any(|codec| codec.name == "H265"));
        assert!(CodecProfile::Native.video_with(false, false).is_empty());
    }

    #[test]
    fn hevc_alias_is_accepted_in_answers() {
        let offer = build_offer(CodecProfile::Native, "127.0.0.1", 41000, Some(41002));
        let answer = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\n\
m=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
m=video 5006 RTP/AVP 125\r\na=rtpmap:125 HEVC/90000\r\na=fmtp:125 profile-id=1\r\n";
        let media = negotiate_media(&offer, answer).expect("audio + HEVC");
        assert_eq!(media.video_codec, Some("H265"));
    }

    #[test]
    fn don_diff_zero_is_compatible_but_positive_is_not() {
        assert!(h265_don_compatible(None));
        assert!(h265_don_compatible(Some("0")));
        assert!(!h265_don_compatible(Some("1")));
        assert!(!h265_don_compatible(Some("garbage")));
    }

    #[test]
    fn answer_with_other_h265_profile_or_tier_is_not_selected() {
        let offer = build_offer(CodecProfile::Native, "127.0.0.1", 41000, Some(41002));
        for fmtp in ["profile-id=2", "profile-id=1;tier-flag=1"] {
            let answer = format!(
                "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\n\
m=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
m=video 5006 RTP/AVP 99\r\na=rtpmap:99 H265/90000\r\na=fmtp:99 {fmtp}\r\n"
            );
            let media = negotiate_media(&offer, &answer).expect("audio still negotiates");
            assert_eq!(media.video_codec, None, "{fmtp} must not be selected");
        }
    }

    #[test]
    fn answer_with_h265_don_diff_one_is_not_selected() {
        let offer = build_offer(CodecProfile::Native, "127.0.0.1", 41000, Some(41002));
        let answer = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\n\
m=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
m=video 5006 RTP/AVP 99\r\na=rtpmap:99 H265/90000\r\na=fmtp:99 profile-id=1;sprop-max-don-diff=1\r\n";
        let media = negotiate_media(&offer, answer).expect("audio still negotiates");
        assert_eq!(media.video_codec, None);
        assert_eq!(media.video_port, None);
    }

    #[test]
    fn answer_with_h265_don_diff_zero_is_selected() {
        let offer = build_offer(CodecProfile::Native, "127.0.0.1", 41000, Some(41002));
        let answer = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\n\
m=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n\
m=video 5006 RTP/AVP 99\r\na=rtpmap:99 H265/90000\r\na=fmtp:99 profile-id=1;sprop-max-don-diff=0\r\n";
        let media = negotiate_media(&offer, answer).expect("audio + H265");
        assert_eq!(media.video_codec, Some("H265"));
        assert_eq!(media.video_port, Some(5006));
    }
}

#[cfg(test)]
mod g722_and_strict_first_regressions {
    use super::*;

    #[test]
    fn native_offer_selects_each_g726_rate_when_peer_lists_it_first() {
        let offer = build_offer(CodecProfile::Native, "192.168.1.93", 41000, None);
        for (payload, name) in [(107, "G726-16"), (108, "G726-24"), (109, "G726-32"), (110, "G726-40")] {
            let answer = format!(
                "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP {payload} 9 0\r\na=rtpmap:{payload} {name}/8000\r\na=rtpmap:9 G722/8000\r\na=rtpmap:0 PCMU/8000\r\n"
            );
            let media = negotiate_media(&offer, &answer).expect("G.726 is native");
            assert_eq!(media.audio_payload_type, payload, "{name}");
            assert_eq!(media.audio_codec, name);
        }
    }

    #[test]
    fn native_offer_selects_g722_when_peer_lists_it_before_pcmu() {
        let offer = build_offer(CodecProfile::Native, "192.168.1.93", 41000, None);
        assert!(offer.contains("m=audio 41000 RTP/AVP 0 8 9 15 4 107 108 109 110 111 101"), "{offer}");
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\n\
m=audio 3230 RTP/AVP 9 0 8 101\r\na=rtpmap:9 G722/8000\r\na=rtpmap:0 PCMU/8000\r\n\
a=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\n";
        let media = negotiate_media(&offer, answer).expect("G722 is now native");
        assert_eq!(media.audio_payload_type, 9);
        assert_eq!(media.audio_codec, "G722");
        assert_eq!(media.dtmf_payload_type, Some(101));
    }

    #[test]
    fn native_offer_selects_g728_and_opus_when_peer_answers_with_them() {
        let offer = build_offer(CodecProfile::Native, "192.168.1.93", 41000, None);
        for (payload, name, clock, channels) in [(15, "G728", 8000, 1), (111, "opus", 48000, 2)] {
            // RFC 7587 requires two channels in the Opus rtpmap even for mono
            // payloads; mono is signalled by `stereo=0`.
            let answer = format!(
                "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP {payload}\r\na=rtpmap:{payload} {name}/{clock}/{channels}\r\n"
            );
            let media = negotiate_media(&offer, &answer).expect("native codec must be accepted");
            assert_eq!(media.audio_payload_type, payload);
            assert_eq!(media.audio_codec.to_ascii_uppercase(), name.to_ascii_uppercase());
        }
    }

    #[test]
    fn strict_first_rejects_unbridged_codec_before_a_later_pcmu() {
        let offer = build_offer(CodecProfile::Full, "192.168.1.93", 41000, None);
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\n\
m=audio 3230 RTP/AVP 18 0\r\na=rtpmap:18 G729/8000\r\na=rtpmap:0 PCMU/8000\r\n";
        assert_eq!(
            negotiate_media_detailed(&offer, answer),
            Err(NegotiationFailure::UnsupportedAudio { codec: "G729".into(), payload: 18 }),
            "late PCMU must not mask a peer-first G729 choice"
        );
    }
}

#[cfg(test)]
mod negotiation_regressions {
    use super::*;
    const POLYCOM_OFFER: &str = include_str!("../tests/fixtures/polycom_rpd_3_5_offer.sdp");
    const POLYCOM_ANSWER: &str = include_str!("../tests/fixtures/polycom_rpd_3_5_answer.sdp");

    #[test]
    fn captured_polycom_answer_selects_baseline_mode1_tx110_and_offered_rx96() {
        let media = negotiate_media(POLYCOM_OFFER, POLYCOM_ANSWER).unwrap();
        assert_eq!(media.audio_payload_type, 0);
        assert_eq!(media.video_port, Some(3232), "compatible H264 was silently discarded");
        assert_eq!(media.video_payload_type, Some(110), "High111 and mode0 PT109 must not be selected");
        assert_eq!(media.local_video_payload_type, Some(96));
        assert_eq!(media.video_ip.as_deref(), Some("192.168.1.132"));
        assert_eq!(media.dtmf_payload_type, Some(119), "answer PT101 is G7221, not telephone-event");
    }

    #[test]
    fn h264_high_interleaved_and_default_mode0_are_not_offered_as_usable_video() {
        for parameters in [
            "profile-level-id=64001e;packetization-mode=1",
            "profile-level-id=42801e;packetization-mode=0",
            "profile-level-id=42801e",
            "profile-level-id=42801e;packetization-mode=2",
            "profile-level-id=428015;packetization-mode=1",
        ] {
            let answer=format!("v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 0\r\nm=video 3232 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 {parameters}\r\n");
            assert!(
                negotiate_media(POLYCOM_OFFER, &answer).unwrap().video_port.is_none(),
                "unsupported H264 configuration accepted: {parameters}"
            );
        }
    }

    #[test]
    fn unsolicited_video_and_invalid_audio_payload_maps_are_rejected() {
        let audio_only = POLYCOM_OFFER.split("m=video").next().unwrap();
        assert!(negotiate_media(audio_only, POLYCOM_ANSWER).unwrap().video_port.is_none());
        let wrong_audio = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 0\r\na=rtpmap:0 G729/8000\r\n";
        assert!(negotiate_media(POLYCOM_OFFER, wrong_audio).is_none());
        let incompatible_offer = POLYCOM_OFFER.replace("profile-level-id=42e01f", "profile-level-id=64001f");
        assert!(negotiate_media(&incompatible_offer, POLYCOM_ANSWER).unwrap().video_port.is_none());
        let mode0_offer = POLYCOM_OFFER.replace("packetization-mode=1", "packetization-mode=0");
        assert!(negotiate_media(&mode0_offer, POLYCOM_ANSWER).unwrap().video_port.is_none());
    }

    #[test]
    fn offered_h264_rx_payload_is_independent_from_answer_tx_payload() {
        let offer = POLYCOM_OFFER.replace("96", "112");
        let media = negotiate_media(&offer, POLYCOM_ANSWER).unwrap();
        assert_eq!(media.video_payload_type, Some(110));
        assert_eq!(media.local_video_payload_type, Some(112));
    }
    #[test]
    fn pcma_and_h264_are_negotiated_without_inventing_dtmf() {
        let m=parse_negotiated_media("v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 8\r\nm=video 3232 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 profile-level-id=42801e;packetization-mode=1\r\n").unwrap();
        assert_eq!(m.audio_payload_type, 8);
        assert_eq!(m.video_payload_type, Some(96));
        assert_eq!(m.dtmf_payload_type, None);
    }
    #[test]
    fn disabled_or_unsupported_audio_is_rejected() {
        for section in ["m=audio 0 RTP/AVP 0", "m=audio 3230 RTP/AVP 18"] {
            assert!(parse_negotiated_media(&format!("v=0\r\nc=IN IP4 192.168.1.132\r\n{section}\r\n")).is_none());
        }
    }

    #[test]
    fn answer_selects_first_common_native_codec_in_answer_order() {
        let offer = build_plain_sdp("127.0.0.1", 41000, None);
        let pcma_first = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 8 0\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:0 PCMU/8000\r\n";
        let media = negotiate_media(&offer, pcma_first).expect("PCMA is common and native");
        assert_eq!(media.audio_payload_type, 8);
        assert_eq!(media.audio_codec, "PCMA");
        let pcmu_first = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 0 8\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n";
        assert_eq!(negotiate_media(&offer, pcmu_first).unwrap().audio_codec, "PCMU");
    }

    #[test]
    fn unknown_vendor_codecs_and_malformed_lines_do_not_abort_the_answer() {
        let offer = build_plain_sdp("127.0.0.1", 41000, None);
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\n\
m=audio 3230 RTP/AVP 118 115 111 8\r\n\
a=rtpmap:118 SIRENLPR/48000/1\r\n\
a=rtpmap:115 G7221/32000\r\n\
a=fmtp:115 bitrate=48000\r\n\
a=rtpmap:bogus ???\r\n\
a=rtpmap:111 SIREN14/16000\r\n\
a=rtpmap:8 PCMA/8000\r\n\
a=fmtp:8 bitrate=64000\r\n\
a=sendrecv\r\n";
        let media = negotiate_media(&offer, answer).expect("vendor codecs must be skipped, not fatal");
        assert_eq!(media.audio_payload_type, 8);
        assert_eq!(media.audio_codec, "PCMA");
    }

    #[test]
    fn common_but_unbridged_audio_is_a_typed_rejection() {
        // Only the full matrix offers G.722; the native profile would report
        // NoCommonAudio instead, which is a different (also correct) verdict.
        let offer = build_offer(CodecProfile::Full, "127.0.0.1", 41000, None);
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
        assert_eq!(
            negotiate_media_detailed(&offer, answer),
            Err(NegotiationFailure::UnsupportedAudio { codec: "G729".into(), payload: 18 })
        );
        assert!(negotiate_media(&offer, answer).is_none(), "G.729 must never be reported as connected media");
    }

    #[test]
    fn answer_without_any_common_codec_is_refused() {
        let offer = build_plain_sdp("127.0.0.1", 41000, None);
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 3\r\na=rtpmap:3 GSM/8000\r\n";
        assert_eq!(negotiate_media_detailed(&offer, answer), Err(NegotiationFailure::NoCommonAudio));
        assert_eq!(
            negotiate_media_detailed(&offer, "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 0 RTP/AVP 0\r\n"),
            Err(NegotiationFailure::NoCommonAudio),
            "a hold (port 0) answer carries no media endpoint"
        );
    }

    #[test]
    fn h265_answer_negotiates_h265_video_and_keeps_native_audio() {
        let offer = build_plain_sdp("127.0.0.1", 41000, Some(41002));
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 0\r\n\
m=video 3232 RTP/AVP 99\r\na=rtpmap:99 H265/90000\r\na=fmtp:99 profile-id=1\r\n";
        let media = negotiate_media(&offer, answer).expect("PCMU + H265");
        assert_eq!(media.audio_payload_type, 0);
        assert_eq!(media.video_port, Some(3232));
        assert_eq!(media.video_codec, Some("H265"));
        assert_eq!(media.local_video_payload_type, Some(99));
    }

    #[test]
    fn h264_baseline_in_answer_is_bridged_even_when_high_profile_is_listed_first() {
        // Real Polycom answers list High (64001e) before Baseline (42801e).
        let offer = build_plain_sdp("127.0.0.1", 41000, Some(41002));
        let answer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 3230 RTP/AVP 0\r\n\
m=video 3232 RTP/AVP 98 97\r\n\
a=rtpmap:98 H264/90000\r\na=fmtp:98 packetization-mode=1;profile-level-id=64001f\r\n\
a=rtpmap:97 H264/90000\r\na=fmtp:97 packetization-mode=1;profile-level-id=42801e\r\n";
        let media = negotiate_media(&offer, answer).expect("PCMU + baseline H264");
        assert_eq!(media.video_payload_type, Some(97));
        assert_eq!(media.local_video_payload_type, Some(97));
        assert_eq!(media.video_codec, Some("H264"));
    }

    #[test]
    fn aac_latm_is_offered_last_and_only_with_probe_availability() {
        let without = CodecProfile::Native.audio_with(false);
        assert!(without.iter().all(|codec| codec.payload != 113));
        let with = CodecProfile::Native.audio_with(true);
        let aac = with.iter().find(|codec| codec.payload == 113).expect("PT113 advertised");
        assert_eq!(aac.name, "MP4A-LATM");
        assert_eq!(aac.clock, 90_000);
        let last_audio = with.iter().rev().find(|codec| !codec.dtmf).expect("audio codec");
        assert_eq!(last_audio.payload, 113, "AAC must stay the last audio codec before DTMF");
        let full_without_probe = build_offer(CodecProfile::Full, "127.0.0.1", 41000, None);
        assert!(!full_without_probe.contains("a=rtpmap:113 "), "Full must not bypass the probe: {full_without_probe}");
        let full_with_probe = CodecProfile::Full.audio_with(true);
        assert!(full_with_probe.iter().any(|codec| codec.payload == 113));
    }

    #[test]
    fn aac_answer_needs_the_probe_and_has_no_ptime20() {
        let offer = "v=0\r\nc=IN IP4 192.0.2.1\r\nm=audio 41000 RTP/AVP 0 113\r\n\
a=rtpmap:0 PCMU/8000\r\na=rtpmap:113 MP4A-LATM/90000\r\n\
a=fmtp:113 profile-level-id=1;object=2;cpresent=1\r\n";
        let answer = "v=0\r\nc=IN IP4 192.0.2.10\r\nm=audio 32000 RTP/AVP 113\r\n\
a=rtpmap:113 MP4A-LATM/90000\r\na=fmtp:113 profile-level-id=1;object=2;cpresent=1\r\n";
        let media = negotiate_media_detailed_with(offer, answer, true).expect("AAC with a probe");
        assert_eq!(media.audio_payload_type, 113);
        assert_eq!(media.audio_codec, "MP4A-LATM");
        assert!(matches!(
            negotiate_media_detailed_with(offer, answer, false),
            Err(NegotiationFailure::UnsupportedAudio { payload: 113, .. })
        ));
        let inbound = crate::uas::InboundMedia {
            audio_payload_type: 113,
            audio_codec: "MP4A-LATM",
            audio_clock_rate: 90_000,
            audio_channels: 1,
            audio_fmtp: Some("profile-level-id=1;object=2;cpresent=1"),
            audio_ptime_ms: 0,
            video_payload_type: None,
            video_codec: None,
            video_fmtp: None,
            remote_ip: "192.0.2.10".into(),
            remote_audio_port: 32000,
            remote_video_port: None,
            remote_video_ip: None,
            video_offered: false,
        };
        let answer_sdp = crate::uas::build_answer("127.0.0.1", 1, 41000, None, &inbound);
        assert!(answer_sdp.contains("a=rtpmap:113 MP4A-LATM/90000"), "{answer_sdp}");
        assert!(!answer_sdp.contains("a=ptime"), "AAC frame is 21.33 ms: {answer_sdp}");

        for bad in [
            "profile-level-id=1;object=2;cpresent=0;config=40002420",
            "profile-level-id=1;object=8;cpresent=1",
            "profile-level-id=1;object=2;SBR=1;cpresent=1",
            "profile-level-id=1;object=2;SBR=true;cpresent=1",
            "profile-level-id=1;object=8;object=2;cpresent=1",
            "profile-level-id=1;object=2;cpresent=0;cpresent=1",
        ] {
            let bad_answer = format!(
                "v=0\r\nc=IN IP4 192.0.2.10\r\nm=audio 32000 RTP/AVP 113\r\n\
a=rtpmap:113 MP4A-LATM/90000\r\na=fmtp:113 {bad}\r\n"
            );
            assert!(
                matches!(
                    negotiate_media_detailed_with(offer, &bad_answer, true),
                    Err(NegotiationFailure::UnsupportedAudio { payload: 113, .. })
                ),
                "answer must be refused: {bad}"
            );
        }
        let config_hint = answer.replace("cpresent=1", "cpresent=1;config=40002420;bitrate=64000");
        assert!(
            negotiate_media_detailed_with(offer, &config_hint, true).is_ok(),
            "config hint with in-band SMC is legal"
        );
    }
}

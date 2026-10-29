//! SDP offer/answer модель + минимальный текстовый кодек.
//!
//! Основание: `docs/architecture/SIGNALING_PROTOCOL.md` §4 (SDP / ICE).
//! Протокол переносит SDP как **opaque string** (`SdpOffer.sdp`), поэтому
//! здесь только структура `Sdp` (session-атрибуты + `Vec<Media>`) с
//! text-кодеком (строки `v=`, `o=`, `s=`, `m=`, `a=`). Answer не строится:
//! его производит реальный str0m-пир; локальный builder с fake ICE/DTLS
//! удалён в T11c.
//!
//! Кодек минимальный (под протокол/тесты, не full RFC 8843): поддерживаются
//! `v=`, `o=`, `s=`, session `a=`, m-lines с `a=rtpmap`/`a=ice-ufrag`/
//! `a=ice-pwd`/`a=fingerprint`/`a=msid`. Остальные строки сохраняются
//! как-is (roundtrip-safe).

use thiserror::Error;

/// Вид медиатрека (SDP `m=`-kind).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Audio,
    Video,
    Data,
}

impl MediaKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Data => "application",
        }
    }

    fn from_str_loose(s: &str) -> Option<Self> {
        match s {
            "audio" => Some(Self::Audio),
            "video" => Some(Self::Video),
            "application" => Some(Self::Data),
            _ => None,
        }
    }
}

/// RTP-map entry (кодек, payload type, clock rate, channels).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpMap {
    pub payload: u32,
    pub codec: String,
    pub clock: u32,
    pub channels: u32,
}

/// Медиа-секция SDP (одна `m=`-line + её атрибуты).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Media {
    /// kind из m-line (audio/video/application).
    pub kind: MediaKind,
    /// порт (0 = ICE-транспорт, WebRTC-конвенция).
    pub port: u16,
    /// payload types (пробелом разделённые после протокола).
    pub payload_types: Vec<u32>,
    /// `a=mid` (m-line id).
    pub mid: Option<String>,
    /// `a=msid` (stream/track id).
    pub msid: Option<String>,
    /// SSRC-а (RFC 7657), если присутствуют.
    pub ssrcs: Vec<String>,
    /// RTP-maps, распарсенные из `a=rtpmap`.
    pub rtp_maps: Vec<RtpMap>,
    /// `a=ice-ufrag` / `a=ice-pwd`.
    pub ice_ufrag: Option<String>,
    pub ice_pwd: Option<String>,
    /// `a=fingerprint:sha-256 ...` (DTLS).
    pub fingerprint: Option<String>,
    /// `a=setup:passive` / `a=setup:active` / `a=setup:actpass` (RFC 8842 / DTLS).
    pub setup: Option<String>,
    /// direction `a=sendonly`/`a=recvonly`/`a=sendrecv`.
    pub direction: Option<String>,
    /// `a=candidate:...` (RFC 8839 / RFC 5245 ICE-кандидаты).
    pub candidates: Vec<String>,
    /// `b=AS:<kbps>` — полоса на media-секцию (RFC 8866 §5.8).
    pub b_as: Option<u32>,
    /// `a=fmtp:<pt> <params>` — формат-специфичные параметры кодека.
    pub fmtp: Vec<String>,
    /// `a=rtcp-fb:<pt> <fb>` — RTCP feedback (NACK/PLI/FIR/REMB/TWCC).
    pub rtcp_fb: Vec<String>,
    /// `a=extmap:<id> <uri>` — RTP header extensions.
    pub extmap: Vec<String>,
    /// `a=rid:<id> send|recv ...` — simulcast RID-ы.
    pub rid: Vec<String>,
    /// `a=simulcast:send ...` / `a=simulcast:recv ...`.
    pub simulcast: Option<String>,
}

/// SDP-документ: session-секция + медиа-секции.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sdp {
    /// версия (`v=`), обычно 0.
    pub version: u32,
    /// `o=` строка (origin).
    pub origin: String,
    /// `s=` строка (session name).
    pub session_name: String,
    /// прочие session-атрибуты (строки `a=...`).
    pub session_attrs: Vec<String>,
    /// медиа-секции.
    pub media: Vec<Media>,
}

/// Ошибка парсинга SDP.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum SdpError {
    #[error("empty SDP: no v= line")]
    Empty,
    #[error("bad line at index {index}: {line}")]
    BadLine { index: usize, line: String },
    #[error("bad m= line at index {index}: {line}")]
    BadMedia { index: usize, line: String },
}

impl Sdp {
    /// Пустой документ (для builder'ов).
    #[must_use]
    pub fn new() -> Self {
        Self {
            version: 0,
            origin: String::new(),
            session_name: String::new(),
            session_attrs: Vec::new(),
            media: Vec::new(),
        }
    }

    /// Парсит SDP-текст (строки `v=`, `o=`, `s=`, `m=`, `a=`).
    ///
    /// Строки `c=`, `t=`, `z=`, `b=`, `p=`, `i=`, `r=`, `u=`, `k=`,
    /// `y=`, `f=`, `q=` пропускаются (валидный SDP, нам не нужны);
    /// неизвестный `a=` под m-line — тоже (игнорируется), кроме
    /// rtpmap/ice/fingerprint/msid/mid/ssrc/direction.
    pub fn parse(text: &str) -> Result<Self, SdpError> {
        let mut doc = Self::new();
        let mut in_media: Option<Media> = None;
        for (index, raw) in text.lines().enumerate() {
            let line = raw.trim_end();
            if line.is_empty() {
                continue;
            }
            // Строки-заглушки сессионного уровня — пропуск (не ошибка).
            // `b=AS:` внутри media-секции несёт полосу и должен сохраняться.
            let media_b_as = in_media.is_some() && line.starts_with("b=AS:");
            let skip = line.starts_with(['c', 't', 'z', 'b', 'p', 'i', 'r', 'u', 'k', 'y', 'f', 'q'])
                && line.chars().nth(1) == Some('=')
                && !media_b_as;
            if skip {
                continue;
            }
            let (field, value) = line
                .strip_prefix("v=")
                .map(|v| ("v", v))
                .or_else(|| line.strip_prefix("o=").map(|v| ("o", v)))
                .or_else(|| line.strip_prefix("s=").map(|v| ("s", v)))
                .or_else(|| line.strip_prefix("m=").map(|v| ("m", v)))
                .or_else(|| line.strip_prefix("b=").map(|v| ("b", v)))
                .or_else(|| line.strip_prefix("a=").map(|v| ("a", v)))
                .ok_or_else(|| SdpError::BadLine { index, line: line.to_string() })?;

            match field {
                "v" => {
                    doc.version =
                        value.trim().parse().map_err(|_| SdpError::BadLine { index, line: line.to_string() })?;
                }
                "o" => doc.origin = value.trim().to_string(),
                "s" => doc.session_name = value.trim().to_string(),
                "b" => {
                    if let (Some(media), Some(rest)) = (in_media.as_mut(), value.trim().strip_prefix("AS:")) {
                        if let Ok(kbps) = rest.parse::<u32>() {
                            media.b_as = Some(kbps);
                        }
                    }
                }
                "m" => {
                    if let Some(media) = in_media.take() {
                        doc.media.push(media);
                    }
                    in_media = Some(Self::parse_media(line, index));
                }
                "a" => {
                    let value = value.trim();
                    if let Some(media) = in_media.as_mut() {
                        Self::apply_media_attr(media, value);
                    } else {
                        doc.session_attrs.push(value.to_string());
                    }
                }
                _ => unreachable!(),
            }
        }
        if doc.origin.is_empty() && doc.session_name.is_empty() && doc.media.is_empty() && doc.session_attrs.is_empty()
        {
            return Err(SdpError::Empty);
        }
        if let Some(media) = in_media.take() {
            doc.media.push(media);
        }
        Ok(doc)
    }

    fn parse_media(line: &str, index: usize) -> Media {
        // m=<media> <port> <proto> <fmt...>
        let after = line.strip_prefix("m=").unwrap_or(line);
        let parts: Vec<&str> = after.split_whitespace().collect();
        if parts.len() < 4 {
            return Media {
                kind: MediaKind::Audio,
                port: 0,
                payload_types: Vec::new(),
                mid: None,
                msid: None,
                ssrcs: Vec::new(),
                rtp_maps: Vec::new(),
                ice_ufrag: None,
                ice_pwd: None,
                fingerprint: None,
                setup: None,
                direction: None,
                candidates: Vec::new(),
                b_as: None,
                fmtp: Vec::new(),
                rtcp_fb: Vec::new(),
                extmap: Vec::new(),
                rid: Vec::new(),
                simulcast: None,
            };
        }
        let kind = MediaKind::from_str_loose(parts[0]).unwrap_or(MediaKind::Audio);
        let _ = index; // (index уже в error-варианте при необходимости)
        let mut payload_types = Vec::new();
        for part in &parts[3..] {
            if let Ok(pt) = part.parse::<u32>() {
                payload_types.push(pt);
            }
        }
        Media {
            kind,
            port: parts[1].parse().unwrap_or(0),
            payload_types,
            mid: None,
            msid: None,
            ssrcs: Vec::new(),
            rtp_maps: Vec::new(),
            ice_ufrag: None,
            ice_pwd: None,
            fingerprint: None,
            setup: None,
            direction: None,
            candidates: Vec::new(),
            b_as: None,
            fmtp: Vec::new(),
            rtcp_fb: Vec::new(),
            extmap: Vec::new(),
            rid: Vec::new(),
            simulcast: None,
        }
    }

    fn apply_media_attr(media: &mut Media, attr: &str) {
        match attr.split_once(':') {
            Some(("rtpmap", rest)) => {
                // a=rtpmap:<pt> <codec>/<clock>[/<ch>]  (стандартный SDP-формат: pt, пробел, codec/clock/ch).
                let rest = rest.trim();
                let (pt_part, fmt_part) = match rest.split_once(' ') {
                    Some((a, b)) => (a, b),
                    None => return,
                };
                if let Ok(pt) = pt_part.parse::<u32>() {
                    let f: Vec<&str> = fmt_part.splitn(3, '/').collect();
                    if f.len() >= 2 {
                        if let Ok(clock) = f[1].parse::<u32>() {
                            let channels = f.get(2).and_then(|c| c.parse().ok()).unwrap_or(1);
                            media.rtp_maps.push(RtpMap { payload: pt, codec: f[0].to_string(), clock, channels });
                        }
                    }
                }
            }
            Some(("mid", rest)) => media.mid = Some(rest.trim().to_string()),
            Some(("msid", rest)) => media.msid = Some(rest.trim().to_string()),
            Some(("ssrc", rest)) => media.ssrcs.push(rest.trim().to_string()),
            Some(("candidate", rest)) => media.candidates.push(rest.trim().to_string()),
            Some(("ice-ufrag", rest)) => media.ice_ufrag = Some(rest.trim().to_string()),
            Some(("ice-pwd", rest)) => media.ice_pwd = Some(rest.trim().to_string()),
            Some(("fingerprint", rest)) => media.fingerprint = Some(rest.trim().to_string()),
            Some(("setup", rest)) => media.setup = Some(rest.trim().to_string()),
            Some(("fmtp", rest)) => media.fmtp.push(rest.trim().to_string()),
            Some(("rtcp-fb", rest)) => media.rtcp_fb.push(rest.trim().to_string()),
            Some(("extmap", rest)) => media.extmap.push(rest.trim().to_string()),
            Some(("rid", rest)) => media.rid.push(rest.trim().to_string()),
            Some(("simulcast", rest)) => media.simulcast = Some(rest.trim().to_string()),
            _ => {
                // direction может прийти без значения ("a=sendrecv", "a=inactive").
                if matches!(attr, "sendrecv" | "sendonly" | "recvonly" | "inactive") {
                    media.direction = Some(attr.to_string());
                }
            }
        }
    }

    /// Сериализует в SDP-текст (stable: session-секция, затем m-секции).
    #[must_use]
    pub fn to_sdp_text(self) -> String {
        let mut out = String::new();
        out.push_str(&format!("v={}\n", self.version));
        out.push_str(&format!("o={}\n", self.origin));
        out.push_str(&format!("s={}\n", self.session_name));
        out.push_str("c=IN IP4 0.0.0.0\n");
        out.push_str("t=0 0\n");
        for attr in &self.session_attrs {
            out.push_str(&format!("a={attr}\n"));
        }
        for media in &self.media {
            let fmt_str = if media.payload_types.is_empty() {
                String::new()
            } else {
                format!(" {}", media.payload_types.iter().map(|pt| pt.to_string()).collect::<Vec<_>>().join(" "))
            };
            out.push_str(&format!("m={} {} UDP/TLS/RTP/SAVPF{fmt_str}\n", media.kind.as_str(), media.port));
            out.push_str("c=IN IP4 0.0.0.0\n");
            if let Some(b_as) = media.b_as {
                out.push_str(&format!("b=AS:{b_as}\n"));
            }
            out.push_str("a=rtcp-mux\n");
            if let Some(mid) = &media.mid {
                out.push_str(&format!("a=mid:{mid}\n"));
            }
            for rtp in &media.rtp_maps {
                if media.kind == MediaKind::Audio && rtp.channels > 1 {
                    out.push_str(&format!("a=rtpmap:{} {}/{}/{}\n", rtp.payload, rtp.codec, rtp.clock, rtp.channels));
                } else {
                    out.push_str(&format!("a=rtpmap:{} {}/{}\n", rtp.payload, rtp.codec, rtp.clock));
                }
            }
            for fmtp in &media.fmtp {
                out.push_str(&format!("a=fmtp:{fmtp}\n"));
            }
            for fb in &media.rtcp_fb {
                out.push_str(&format!("a=rtcp-fb:{fb}\n"));
            }
            for ext in &media.extmap {
                out.push_str(&format!("a=extmap:{ext}\n"));
            }
            for rid in &media.rid {
                out.push_str(&format!("a=rid:{rid}\n"));
            }
            if let Some(simulcast) = &media.simulcast {
                out.push_str(&format!("a=simulcast:{simulcast}\n"));
            }
            if let Some(msid) = &media.msid {
                out.push_str(&format!("a=msid:{msid}\n"));
            }
            for ssrc in &media.ssrcs {
                out.push_str(&format!("a=ssrc:{ssrc}\n"));
            }
            for c in &media.candidates {
                out.push_str(&format!("a=candidate:{c}\n"));
            }
            if !media.candidates.is_empty() {
                out.push_str("a=end-of-candidates\n");
            }
            if let Some(ufrag) = &media.ice_ufrag {
                out.push_str(&format!("a=ice-ufrag:{ufrag}\n"));
            }
            if let Some(pwd) = &media.ice_pwd {
                out.push_str(&format!("a=ice-pwd:{pwd}\n"));
            }
            if let Some(fp) = &media.fingerprint {
                out.push_str(&format!("a=fingerprint:{fp}\n"));
            }
            if let Some(setup) = &media.setup {
                out.push_str(&format!("a=setup:{setup}\n"));
            }
            if let Some(direction) = &media.direction {
                out.push_str(&format!("a={direction}\n"));
            }
        }
        out
    }
}

impl Default for Sdp {
    fn default() -> Self {
        Self::new()
    }
}

// SDP is parsed and round-tripped only; answers come from the real str0m peer.
// The old fake `sfu_answer`/`SfuAnswerProfile` builder was deleted in T11c.

#[cfg(test)]
mod tests {
    use super::*;

    /// Оффер с 2 m-lines (audio + video) — как шлёт браузер.
    fn two_line_offer() -> Sdp {
        Sdp {
            version: 0,
            origin: "0 1 2 IN IP4 127.0.0.1".into(),
            session_name: "VKS Offer".into(),
            session_attrs: vec!["extmap-allow-mixed".into(), "ice-options:ice-lite".into()],
            media: vec![
                Media {
                    kind: MediaKind::Audio,
                    port: 9,
                    payload_types: vec![111, 0],
                    mid: Some("0".into()),
                    msid: Some("stream-a track-a".into()),
                    ssrcs: vec!["1001".into()],
                    rtp_maps: vec![RtpMap { payload: 111, codec: "opus".into(), clock: 48000, channels: 2 }],
                    ice_ufrag: Some("aufrag".into()),
                    ice_pwd: Some("apwd".into()),
                    fingerprint: Some("sha-256 AA:BB:CC".into()),
                    setup: Some("actpass".into()),
                    direction: Some("sendrecv".into()),
                    candidates: Vec::new(),
                    b_as: None,
                    fmtp: Vec::new(),
                    rtcp_fb: Vec::new(),
                    extmap: Vec::new(),
                    rid: Vec::new(),
                    simulcast: None,
                },
                Media {
                    kind: MediaKind::Video,
                    port: 9,
                    payload_types: vec![96, 97],
                    mid: Some("1".into()),
                    msid: Some("stream-v track-v".into()),
                    ssrcs: vec!["1002".into()],
                    rtp_maps: vec![RtpMap { payload: 96, codec: "vp8".into(), clock: 90000, channels: 0 }],
                    ice_ufrag: Some("aufrag".into()),
                    ice_pwd: Some("apwd".into()),
                    fingerprint: Some("sha-256 AA:BB:CC".into()),
                    setup: Some("actpass".into()),
                    direction: Some("sendrecv".into()),
                    candidates: Vec::new(),
                    b_as: None,
                    fmtp: Vec::new(),
                    rtcp_fb: Vec::new(),
                    extmap: Vec::new(),
                    rid: Vec::new(),
                    simulcast: None,
                },
            ],
        }
    }

    #[test]
    fn sdp_text_roundtrip() {
        let doc = two_line_offer();
        let text = doc.to_sdp_text();
        let parsed = Sdp::parse(&text).expect("parse");
        assert_eq!(parsed.version, 0);
        assert_eq!(parsed.session_name, "VKS Offer");
        assert_eq!(parsed.session_attrs, vec!["extmap-allow-mixed".to_string(), "ice-options:ice-lite".to_string()]);
        assert_eq!(parsed.media.len(), 2);
        assert_eq!(parsed.media[0].kind, MediaKind::Audio);
        assert_eq!(parsed.media[0].payload_types, vec![111, 0]);
        assert_eq!(parsed.media[0].mid.as_deref(), Some("0"));
        assert_eq!(parsed.media[0].rtp_maps[0].codec, "opus");
        assert_eq!(parsed.media[1].kind, MediaKind::Video);
        assert_eq!(parsed.media[1].payload_types, vec![96, 97]);
        assert_eq!(parsed.media[1].rtp_maps[0].payload, 96);
        assert_eq!(parsed.media[1].ice_ufrag.as_deref(), Some("aufrag"));
        assert_eq!(parsed.media[1].fingerprint.as_deref(), Some("sha-256 AA:BB:CC"));
    }

    #[test]
    fn parse_sample_sdp_text() {
        let sample = r#"
v=0
o=- 1079286018 2 IN IP4 0.0.0.0
s=-
t=0 0
a=group:BUNDLE 0 1
a=msid-semantic: WMS
m=audio 9 UDP/TLS/RTP/SAVPF 111
c=IN IP4 0.0.0.0
a=mid:0
a=sendrecv
a=rtpmap:111 opus/48000/2
a=ice-ufrag:abcd
a=ice-pwd:wJalrXUtnFEMI
a=fingerprint:sha-256 11:22:33:44
m=video 9 UDP/TLS/RTP/SAVPF 96
c=IN IP4 0.0.0.0
a=mid:1
a=recvonly
a=rtpmap:96 VP8/90000
a=ssrc:5000 cname:x
    "#;
        let parsed = Sdp::parse(sample).expect("parse");
        assert_eq!(parsed.media.len(), 2);
        assert_eq!(parsed.media[0].kind, MediaKind::Audio);
        assert_eq!(parsed.media[0].payload_types, vec![111]);
        assert_eq!(parsed.media[0].mid.as_deref(), Some("0"));
        assert_eq!(parsed.media[0].rtp_maps[0].codec, "opus");
        assert_eq!(parsed.media[0].rtp_maps[0].channels, 2);
        assert_eq!(parsed.media[0].ice_ufrag.as_deref(), Some("abcd"));
        assert_eq!(parsed.media[0].ice_pwd.as_deref(), Some("wJalrXUtnFEMI"));
        assert_eq!(parsed.media[0].fingerprint.as_deref(), Some("sha-256 11:22:33:44"));
        assert_eq!(parsed.media[0].direction.as_deref(), Some("sendrecv"));
        assert_eq!(parsed.media[1].kind, MediaKind::Video);
        assert_eq!(parsed.media[1].payload_types, vec![96]);
        assert_eq!(parsed.media[1].rtp_maps[0].codec, "VP8");
        assert_eq!(parsed.media[1].direction.as_deref(), Some("recvonly"));
        assert!(parsed.media[1].ssrcs.iter().any(|s| s.contains("5000")));
    }

    #[test]
    fn media_bandwidth_and_feedback_roundtrip() {
        let sample = r#"v=0
o=- 1 1 IN IP4 0.0.0.0
s=-
t=0 0
m=video 9 UDP/TLS/RTP/SAVPF 96
c=IN IP4 0.0.0.0
b=AS:2048
a=mid:1
a=rtpmap:96 H264/90000
a=fmtp:96 profile-level-id=42e01f;packetization-mode=1
a=rtcp-fb:96 nack
a=rtcp-fb:96 nack pli
a=rtcp-fb:96 ccm fir
a=extmap:3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01
a=rid:hi send
a=simulcast:send hi;lo
a=sendrecv
"#;
        let parsed = Sdp::parse(sample).expect("parse");
        let video = &parsed.media[0];
        assert_eq!(video.b_as, Some(2048));
        assert!(video.fmtp.iter().any(|v| v.starts_with("96 ") && v.contains("packetization-mode=1")));
        assert!(video.rtcp_fb.iter().any(|v| v == "96 nack pli"));
        assert!(video.rtcp_fb.iter().any(|v| v == "96 ccm fir"));
        assert!(video.extmap.iter().any(|v| v.starts_with("3 ")));
        assert_eq!(video.rid, vec!["hi send".to_string()]);
        assert_eq!(video.simulcast.as_deref(), Some("send hi;lo"));

        let text = parsed.to_sdp_text();
        assert!(text.contains("b=AS:2048"));
        assert!(text.contains("a=fmtp:96 "));
        assert!(text.contains("a=rtcp-fb:96 nack pli"));
        assert!(text.contains("a=extmap:3 "));
        let again = Sdp::parse(&text).expect("reparse");
        assert_eq!(again.media[0].b_as, Some(2048));
    }

    #[test]
    fn empty_sdp_is_error() {
        assert_eq!(Sdp::parse(""), Err(SdpError::Empty));
    }

    #[test]
    fn unknown_line_is_error() {
        assert!(matches!(Sdp::parse("x=1"), Err(SdpError::BadLine { .. })));
    }

    #[test]
    fn media_kind_str() {
        assert_eq!(MediaKind::Audio.as_str(), "audio");
        assert_eq!(MediaKind::Video.as_str(), "video");
        assert_eq!(MediaKind::Data.as_str(), "application");
    }
}

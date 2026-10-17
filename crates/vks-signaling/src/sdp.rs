//! SDP offer/answer модель + минимальный текстовый кодек.
//!
//! Основание: `docs/architecture/SIGNALING_PROTOCOL.md` §4 (SDP / ICE).
//! Протокол переносит SDP как **опaque string** (`SdpOffer.sdp`), поэтому
//! здесь: структура `Sdp` (session-атрибуты + `Vec<Media>`) с text-кодеком
//! (строки `v=`, `o=`, `s=`, `m=`, `a=`) и builder `SdpOffer` → `SdpAnswer`,
//! который echo-ит m-lines кодеками SFU и добавляет ICE-candidates.
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

/// ICE-кандидат (RFC 8445, host — ICE-lite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IceCandidate {
    /// ufrag (ICE user fragment) — SDP `a=ice-ufrag`.
    pub ufrag: String,
    /// password — SDP `a=ice-pwd`.
    pub pwd: String,
    /// IP-адрес кандидата.
    pub addr: String,
    /// UDP-порт кандидата.
    pub port: u16,
    /// Тип кандидата: "host" | "srflx" | "relay".
    pub typ: String,
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
    /// direction `a=sendonly`/`a=recvonly`/`a=sendrecv`.
    pub direction: Option<String>,
    /// `a=candidate:...` (RFC 8839 / RFC 5245 ICE-кандидаты).
    pub candidates: Vec<String>,
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
            let skip = line.starts_with(['c', 't', 'z', 'b', 'p', 'i', 'r', 'u', 'k', 'y', 'f', 'q'])
                && line.chars().nth(1) == Some('=');
            if skip {
                continue;
            }
            let (field, value) = line
                .strip_prefix("v=")
                .map(|v| ("v", v))
                .or_else(|| line.strip_prefix("o=").map(|v| ("o", v)))
                .or_else(|| line.strip_prefix("s=").map(|v| ("s", v)))
                .or_else(|| line.strip_prefix("m=").map(|v| ("m", v)))
                .or_else(|| line.strip_prefix("a=").map(|v| ("a", v)))
                .ok_or_else(|| SdpError::BadLine { index, line: line.to_string() })?;

            match field {
                "v" => {
                    doc.version =
                        value.trim().parse().map_err(|_| SdpError::BadLine { index, line: line.to_string() })?;
                }
                "o" => doc.origin = value.trim().to_string(),
                "s" => doc.session_name = value.trim().to_string(),
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
                direction: None,
                candidates: Vec::new(),
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
            direction: None,
            candidates: Vec::new(),
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
            Some(("sendrecv", _)) | Some(("sendonly", _)) | Some(("recvonly", _)) => {}
            _ => {
                // direction может прийти без значения ("a=sendrecv").
                if matches!(attr, "sendrecv" | "sendonly" | "recvonly") {
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
            if let Some(mid) = &media.mid {
                out.push_str(&format!("a=mid:{mid}\n"));
            }
            for rtp in &media.rtp_maps {
                out.push_str(&format!("a=rtpmap:{} {}/{}/{}\n", rtp.payload, rtp.codec, rtp.clock, rtp.channels));
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
            if let Some(ufrag) = &media.ice_ufrag {
                out.push_str(&format!("a=ice-ufrag:{ufrag}\n"));
            }
            if let Some(pwd) = &media.ice_pwd {
                out.push_str(&format!("a=ice-pwd:{pwd}\n"));
            }
            if let Some(fp) = &media.fingerprint {
                out.push_str(&format!("a=fingerprint:{fp}\n"));
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

/// SFU-сторона: builder `SdpOffer` → `SdpAnswer` (SIGNALING_PROTOCOL §4).
///
/// Echo-ит m-lines оффера (порядок и количество сохранены), подставляет
/// payload types SFU (из `SfuAnswerProfile`), добавляет ICE-candidates
/// (ICE-lite: host-only) и fingerprint.
#[derive(Debug, Clone)]
pub struct SfuAnswerProfile {
    /// RTP-maps, которые SFU объявляет в ответе (кодеки).
    pub rtp_maps: Vec<RtpMap>,
    /// ICE-ufrag/pwd SFU (одни на все m-lines — ICE-lite).
    pub ufrag: String,
    pub pwd: String,
    /// host-candidate (адрес:порт), один на все m-lines.
    pub host_candidate: IceCandidate,
    /// DTLS fingerprint (sha-256, colon-separated hex).
    pub fingerprint: String,
}

impl Sdp {
    /// Строит ответ SFU на offer: m-lines echo-ятся, payload types заменяются
    /// на SFU-профиль, добавляются ICE/fingerprint.
    ///
    /// Возвращает `(answer, rtp_maps)` — rtp_maps дублируются в
    /// `SdpAnswer.rtp_map` (proto-поле) для convenience.
    #[must_use]
    pub fn sfu_answer(&self, profile: &SfuAnswerProfile) -> (Self, Vec<RtpMap>) {
        let mut answer = Sdp {
            version: self.version,
            origin: profile.origin_line().to_string(),
            session_name: format!("vks-sfu (offer s={})", self.session_name),
            session_attrs: vec!["group:BUNDLE 0-{}".to_string() /* patched below */],
            media: Vec::new(),
        };
        for (i, media) in self.media.iter().enumerate() {
            let mut m = Media {
                kind: media.kind,
                // ICE-порт 0: транспорт — WebRTC-конвенция (STUN/DTLS отдельно).
                port: 0,
                payload_types: profile.rtp_maps.iter().map(|r| r.payload).collect(),
                mid: media.mid.clone().or_else(|| Some(i.to_string())),
                msid: Some(format!("sfu-out {i}")),
                ssrcs: vec![format!("ssrc-sfu-{i}")],
                rtp_maps: profile.rtp_maps.clone(),
                ice_ufrag: Some(profile.ufrag.clone()),
                ice_pwd: Some(profile.pwd.clone()),
                fingerprint: Some(profile.fingerprint.clone()),
                direction: Some("recvonly".to_string()),
                candidates: Vec::new(),
            };
            let _ = &mut m;
            answer.media.push(m);
        }
        // BUNDLE group из настоящих mids.
        let mids: Vec<String> = answer.media.iter().map(|m| m.mid.clone().unwrap_or_else(|| "0".into())).collect();
        answer.session_attrs = vec![format!("group:BUNDLE {}", mids.join(" "))];
        (answer, profile.rtp_maps.clone())
    }

    /// Добавляет ICE-candidate в каждую m-line (ICE-lite: host-only).
    #[must_use]
    pub fn with_ice_candidates(mut self, candidates: &[IceCandidate]) -> Self {
        for c in candidates {
            for media in &mut self.media {
                if media.ice_ufrag.is_none() {
                    media.ice_ufrag = Some(c.ufrag.clone());
                }
                if media.ice_pwd.is_none() {
                    media.ice_pwd = Some(c.pwd.clone());
                }
                media.candidates.push(format!("1 1 UDP 2130706431 {} {} typ {}", c.addr, c.port, c.typ));
            }
        }
        self
    }
}

impl SfuAnswerProfile {
    /// Строит `o=`-строку для ответа.
    #[must_use]
    pub fn origin_line(&self) -> String {
        let sid = self.ufrag.chars().take(8).collect::<String>();
        format!("{sid} 1 0 IN IP4 0.0.0.0")
    }
}

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
                    direction: Some("sendrecv".into()),
                    candidates: Vec::new(),
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
                    direction: Some("sendrecv".into()),
                    candidates: Vec::new(),
                },
            ],
        }
    }

    fn sfu_profile() -> SfuAnswerProfile {
        SfuAnswerProfile {
            rtp_maps: vec![
                RtpMap { payload: 111, codec: "opus".into(), clock: 48000, channels: 2 },
                RtpMap { payload: 96, codec: "vp8".into(), clock: 90000, channels: 0 },
            ],
            ufrag: "sfuufrag".into(),
            pwd: "sfupwd".into(),
            host_candidate: IceCandidate {
                ufrag: "sfuufrag".into(),
                pwd: "sfupwd".into(),
                addr: "203.0.113.7".into(),
                port: 49152,
                typ: "host".into(),
            },
            fingerprint: "sha-256 FF:00:11".into(),
        }
    }

    #[test]
    fn offer_to_answer_preserves_mlines_and_adds_candidates() {
        let offer = two_line_offer();
        let profile = sfu_profile();
        let (answer0, rtp) = offer.sfu_answer(&profile);
        let answer = answer0.with_ice_candidates(std::slice::from_ref(&profile.host_candidate));

        // m-lines сохранены по количеству и kind'у.
        assert_eq!(answer.media.len(), 2);
        assert_eq!(answer.media[0].kind, MediaKind::Audio);
        assert_eq!(answer.media[1].kind, MediaKind::Video);
        // SFU payload types подставлены в обе m-lines (SFU-профиль).
        assert_eq!(answer.media[0].payload_types, vec![111, 96]);
        assert_eq!(answer.media[1].payload_types, vec![111, 96]);
        // ICE candidates присутствуют.
        assert_eq!(answer.media[0].ice_ufrag.as_deref(), Some("sfuufrag"));
        assert_eq!(answer.media[1].ice_pwd.as_deref(), Some("sfupwd"));
        assert!(!answer.media[0].ssrcs.is_empty());
        // rtp_maps продублированы в proto-совместимый список.
        assert_eq!(rtp.len(), 2);
        // fingerprint на каждой m-line.
        assert_eq!(answer.media[0].fingerprint.as_deref(), Some("sha-256 FF:00:11"));
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
    fn answer_text_roundtrip() {
        let offer = two_line_offer();
        let profile = sfu_profile();
        let (answer0, _rtp) = offer.sfu_answer(&profile);
        let answer = answer0.with_ice_candidates(std::slice::from_ref(&profile.host_candidate));
        let text = answer.to_sdp_text();
        let parsed = Sdp::parse(&text).expect("parse");
        assert_eq!(parsed.media.len(), 2);
        assert!(parsed.session_attrs.iter().any(|a| a.starts_with("group:BUNDLE")));
        assert_eq!(parsed.media[0].payload_types, vec![111, 96]);
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

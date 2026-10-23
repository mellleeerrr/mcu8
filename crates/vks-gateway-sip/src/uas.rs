//! Минимальный SIP UAS для прямого вызова MCU по `sip:<room>@<ip>`.
//!
//! Транспорт (UDP-сокет) и жизненный цикл живут в `vks-server`; здесь только
//! разбор запроса, выбор кодека и сериализация ответов, чтобы это тестировалось
//! без сети. Поддерживаются INVITE/ACK/BYE/CANCEL/OPTIONS и plain RTP/AVP
//! (PCMU/PCMA/G722/G726/G728/Opus + H.264/H.265, если мост доступен).
//! SRTP/TLS/100rel/digest осознанно отклоняются, а не имитируются.

/// Разобранный входящий SIP-запрос.
#[derive(Debug, Clone)]
pub struct UasRequest {
    pub method: String,
    pub request_uri: String,
    pub call_id: String,
    pub cseq: u32,
    pub cseq_method: String,
    pub from: String,
    pub from_tag: Option<String>,
    pub to: String,
    pub to_tag: Option<String>,
    pub via: String,
    pub contact: Option<String>,
    pub require: Option<String>,
    pub body: String,
}

/// Выбранные параметры входящего вызова.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundMedia {
    pub audio_payload_type: u8,
    /// Canonical codec name (`PCMU`, `PCMA`, `G722`, `G726-16`…), independent
    /// of the peer-chosen payload number for dynamic PTs 96-127.
    pub audio_codec: &'static str,
    /// RTP timestamp clock (8000 for G.711/G.722/G.726, 48000 for Opus).
    pub audio_clock_rate: u32,
    /// Channel count in `a=rtpmap` (2 for Opus, 1 otherwise).
    pub audio_channels: u8,
    /// Opus `a=fmtp` (FEC/stereo policy); `None` for static codecs.
    pub audio_fmtp: Option<&'static str>,
    /// Packetization time advertised in the answer.
    pub audio_ptime_ms: u16,
    pub video_payload_type: Option<u8>,
    /// Negotiated video codec name (`H264`/`H265`), mirroring
    /// [`InboundMedia::video_payload_type`].
    pub video_codec: Option<&'static str>,
    pub remote_ip: String,
    pub remote_audio_port: u16,
    pub remote_video_port: Option<u16>,
    /// Per-media `c=` video-секции (если отличается от audio/session).
    pub remote_video_ip: Option<String>,
    /// В оффере была секция video (даже с портом 0): answer обязан
    /// сохранить число m-line.
    pub video_offered: bool,
}

impl UasRequest {
    /// Парсит запрос; возвращает `None` для не-SIP/слишком больших сообщений.
    pub fn parse(buf: &str) -> Option<Self> {
        if buf.len() > 64 * 1024 {
            return None;
        }
        let (head, body) = split_head_body(buf);
        let mut lines = head.lines();
        let request_line = lines.next()?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next()?.to_ascii_uppercase();
        let request_uri = parts.next()?.to_string();
        if !parts.next()?.starts_with("SIP/2.0") {
            return None;
        }
        let mut request = Self {
            method,
            request_uri,
            call_id: String::new(),
            cseq: 0,
            cseq_method: String::new(),
            from: String::new(),
            from_tag: None,
            to: String::new(),
            to_tag: None,
            via: String::new(),
            contact: None,
            require: None,
            body: body.to_string(),
        };
        let mut content_length = None;
        for line in lines {
            let Some((name, value)) = line.split_once(':') else { continue };
            let name = name.trim().to_ascii_lowercase();
            let value = value.trim();
            match name.as_str() {
                "call-id" | "i" => request.call_id = value.to_string(),
                "cseq" => {
                    let mut cseq = value.split_whitespace();
                    request.cseq = cseq.next()?.parse().ok()?;
                    request.cseq_method = cseq.next()?.to_ascii_uppercase();
                }
                "from" | "f" => request.from = value.to_string(),
                "to" | "t" => request.to = value.to_string(),
                "via" | "v" if request.via.is_empty() => request.via = value.to_string(),
                "contact" | "m" => request.contact = Some(value.to_string()),
                "require" => request.require = Some(value.to_string()),
                "content-length" | "l" => content_length = value.parse::<usize>().ok(),
                _ => {}
            }
        }
        request.from_tag = header_tag(&request.from);
        request.to_tag = header_tag(&request.to);
        if request.call_id.is_empty() || request.cseq == 0 {
            return None;
        }
        if let Some(length) = content_length {
            if length != request.body.len() {
                return None;
            }
        }
        Some(request)
    }

    /// Локальная часть request-URI (`sip:<room>@host`) с percent-decode.
    pub fn target_user(&self) -> Option<String> {
        let uri = self.request_uri.strip_prefix("sip:").or_else(|| self.request_uri.strip_prefix("sips:"))?;
        let user = uri.split('@').next()?.split(';').next()?.trim();
        if user.is_empty() {
            return None;
        }
        Some(percent_decode(user))
    }

    pub fn is_retransmission_key(&self, other: &Self) -> bool {
        self.call_id == other.call_id && self.cseq == other.cseq && self.method == other.method
    }
}

fn split_head_body(buf: &str) -> (&str, &str) {
    if let Some(pos) = buf.find("\r\n\r\n") {
        (&buf[..pos], &buf[pos + 4..])
    } else if let Some(pos) = buf.find("\n\n") {
        (&buf[..pos], &buf[pos + 2..])
    } else {
        (buf, "")
    }
}

fn header_tag(header: &str) -> Option<String> {
    header
        .split(';')
        .find_map(|part| part.trim().strip_prefix("tag=").map(|tag| tag.trim_matches('"').to_string()))
        .filter(|tag| !tag.is_empty())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(high), Some(low)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(high * 16 + low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Выбирает первый поддерживаемый аудиокодек в порядке оффера
/// (PCMU/PCMA/G722/G726); динамический PT сохраняется, кодек едет отдельно.
/// Видео — H.264, иначе H.265, если его мост доступен на хосте.
pub fn negotiate(offer: &str) -> Option<InboundMedia> {
    let session_ip = media_ip(offer).unwrap_or_default();
    let audio = media_section(offer, "audio")?;
    if media_proto(audio) != Some("RTP/AVP") {
        return None;
    }
    let audio_port = media_port(audio)?;
    if audio_port == 0 {
        return None;
    }
    // Peer's m-line order is its preference order; pick the first codec we
    // actually bridge. G722 (PT9) keeps inbound calls wideband instead of
    // silently downgrading to PCMU when both are offered.  Dynamic PTs keep
    // the peer's number but carry the codec identity separately.
    let (audio_payload_type, bridge_audio) = audio_payload_order(audio)
        .into_iter()
        .find_map(|payload| bridgeable_audio(audio, payload).map(|codec| (payload, codec)))?;
    let video_section = media_section(offer, "video");
    let video_offered = video_section.is_some();
    let video = video_section
        .filter(|section| media_proto(section) == Some("RTP/AVP"))
        .filter(|section| media_port(section).is_some_and(|port| port > 0));
    let video_codec = video.and_then(|section| {
        if let Some(payload) = h264_payload(section) {
            Some((payload, "H264"))
        } else if vks_common::codec_capabilities::video_codec_available("H265") {
            h265_payload(section).map(|payload| (payload, "H265"))
        } else {
            None
        }
    });
    let video_payload_type = video_codec.map(|(payload, _)| payload);
    let remote_video_port = video.and_then(media_port);
    let audio_ip = section_ip(audio, &session_ip);
    if audio_ip.is_empty() {
        return None;
    }
    let video_ip = video.map(|section| section_ip(section, &audio_ip));
    Some(InboundMedia {
        audio_payload_type,
        audio_codec: bridge_audio.codec,
        audio_clock_rate: bridge_audio.clock,
        audio_channels: bridge_audio.channels,
        audio_fmtp: bridge_audio.fmtp,
        audio_ptime_ms: bridge_audio.ptime_ms,
        video_payload_type,
        video_codec: video_codec.map(|(_, codec)| codec),
        remote_ip: audio_ip,
        remote_audio_port: audio_port,
        remote_video_port,
        remote_video_ip: video_ip,
        video_offered,
    })
}

/// SDP-ответ с локальными RTP/RTCP-портами и выбранными кодеками.
pub fn build_answer(
    local_ip: &str,
    session_id: u64,
    audio_port: u16,
    video_port: Option<u16>,
    media: &InboundMedia,
) -> String {
    let mut sdp = format!(
        "v=0\r\no=- {session_id} {session_id} IN IP4 {local_ip}\r\ns=VKS MCU\r\nc=IN IP4 {local_ip}\r\nt=0 0\r\nm=audio {audio_port} RTP/AVP {}\r\n",
        media.audio_payload_type
    );
    let wire_name = if media.audio_codec == "OPUS" { "opus" } else { media.audio_codec };
    let channels = if media.audio_channels > 1 { format!("/{}", media.audio_channels) } else { String::new() };
    sdp.push_str(&format!(
        "a=rtpmap:{} {}/{}{}\r\n",
        media.audio_payload_type, wire_name, media.audio_clock_rate, channels
    ));
    if let Some(fmtp) = media.audio_fmtp {
        sdp.push_str(&format!("a=fmtp:{} {fmtp}\r\n", media.audio_payload_type));
    }
    sdp.push_str(&format!("a=ptime:{}\r\n", media.audio_ptime_ms));
    if media.audio_codec == "G723" {
        // G.723.1 can pack multiple 30 ms frames; the bridge decodes one.
        sdp.push_str("a=maxptime:30\r\n");
    }
    if media.video_offered {
        if let (Some(port), Some(payload)) = (video_port, media.video_payload_type) {
            let (name, fmtp) = match media.video_codec {
                Some("H265") => ("H265", "profile-id=1"),
                _ => ("H264", "packetization-mode=1;profile-level-id=42e01f"),
            };
            sdp.push_str(&format!(
                "m=video {port} RTP/AVP {payload}\r\na=rtpmap:{payload} {name}/90000\r\na=fmtp:{payload} {fmtp}\r\n"
            ));
        } else {
            // Оффер содержал video, но совместимого H.264/H.265 нет: сохраняем
            // число m-line и явно отключаем поток (RFC 3264 §6).
            sdp.push_str("m=video 0 RTP/AVP 0\r\n");
        }
    }
    sdp
}

/// Собирает SIP-ответ. `to_tag` обязателен для 200 OK (диалог).
pub fn build_response(
    status: u16,
    reason: &str,
    request: &UasRequest,
    local_ip: &str,
    local_port: u16,
    to_tag: Option<&str>,
    body: &str,
) -> String {
    let mut to = if request.to.is_empty() {
        format!("<sip:{}@{}>", request.target_user().unwrap_or_default(), local_ip)
    } else {
        request.to.clone()
    };
    if let Some(tag) = to_tag {
        if !to.contains(";tag=") {
            to.push_str(&format!(";tag={tag}"));
        }
    }
    let mut via = request.via.clone();
    if !via.to_ascii_lowercase().contains("received=") {
        via.push_str(&format!(";received={local_ip}"));
    }
    let mut response = format!(
        "SIP/2.0 {status} {reason}\r\nVia: {via}\r\nFrom: {}\r\nTo: {to}\r\nCall-ID: {}\r\nCSeq: {} {}\r\n",
        request.from, request.call_id, request.cseq, request.method
    );
    if status == 200 && request.method == "INVITE" {
        response.push_str(&format!(
            "Contact: <sip:{}@{}:{}>\r\n",
            request.target_user().unwrap_or_default(),
            local_ip,
            local_port
        ));
    }
    if !body.is_empty() {
        response.push_str("Content-Type: application/sdp\r\n");
    }
    response.push_str(&format!("Content-Length: {}\r\n\r\n{body}", body.len()));
    response
}

fn media_section<'a>(sdp: &'a str, kind: &str) -> Option<&'a str> {
    let marker = format!("m={kind} ");
    let start = sdp.find(&marker)?;
    let end =
        sdp[start + marker.len()..].find("\nm=").map(|offset| start + marker.len() + offset + 1).unwrap_or(sdp.len());
    Some(&sdp[start..end])
}

fn media_proto(section: &str) -> Option<&str> {
    section.lines().next()?.split_whitespace().nth(2)
}

fn media_port(section: &str) -> Option<u16> {
    let line = section.lines().next()?;
    line.split_whitespace().nth(1)?.parse().ok()
}

fn audio_payload_order(section: &str) -> Vec<u8> {
    let Some(line) = section.lines().next() else { return Vec::new() };
    line.split_whitespace().skip(3).filter_map(|item| item.parse().ok()).collect()
}

/// A static payload type may omit `a=rtpmap`; if present it must agree.
fn payload_is(section: &str, payload: u8, name: &str, clock: u32) -> bool {
    let declared = section.lines().filter_map(|line| {
        let line = line.trim().strip_prefix("a=rtpmap:")?;
        let (pt, rest) = line.split_once(' ')?;
        (pt.parse::<u8>().ok()? == payload).then_some(rest)
    });
    let mut declared = declared.peekable();
    if declared.peek().is_none() {
        return true;
    }
    declared.any(|rest| {
        let mut parts = rest.split('/');
        parts.next().is_some_and(|declared_name| declared_name.eq_ignore_ascii_case(name))
            && parts.next().and_then(|value| value.parse::<u32>().ok()) == Some(clock)
    })
}

/// Declared `a=rtpmap` for a payload, if any: (name, clock, channels).
fn declared_codec(section: &str, payload: u8) -> Option<(&str, u32, Option<u8>)> {
    section
        .lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("a=rtpmap:")?;
            let (pt, rest) = line.split_once(' ')?;
            if pt.parse::<u8>().ok()? != payload {
                return None;
            }
            let mut parts = rest.split('/');
            let name = parts.next()?;
            let clock = parts.next()?.parse::<u32>().ok()?;
            let channels = parts.next().and_then(|value| value.parse::<u8>().ok());
            Some((name, clock, channels))
        })
        .next()
}

/// A codec the bridge can open for this offer, with the metadata the answer
/// needs.  Keeping clock/channels/fmtp here avoids repeating PT tables in the
/// SIP and media layers.
#[derive(Debug, Clone, Copy)]
struct BridgeableAudio {
    codec: &'static str,
    clock: u32,
    channels: u8,
    fmtp: Option<&'static str>,
    ptime_ms: u16,
}

impl BridgeableAudio {
    const fn narrow(codec: &'static str) -> Self {
        Self { codec, clock: 8000, channels: 1, fmtp: None, ptime_ms: 20 }
    }
}

/// Map one offered payload to a codec the bridge can handle without a
/// transcoder.  Static PTs 0/8/9 may omit rtpmap; dynamic PTs (including our
/// 107-110) must declare a matching name+clock, otherwise a hostile offer
/// could claim PCMU while sending G.726.
fn bridgeable_audio(section: &str, payload: u8) -> Option<BridgeableAudio> {
    let static_codec = match payload {
        0 => Some("PCMU"),
        8 => Some("PCMA"),
        9 => Some("G722"),
        15 => Some("G728"),
        4 => Some("G723"),
        107 => Some("G726-16"),
        108 => Some("G726-24"),
        109 => Some("G726-32"),
        110 => Some("G726-40"),
        _ => None,
    };
    if let Some(codec) = static_codec {
        // Dynamic G.726 must be explicitly mapped; static G.711/G.722 may rely
        // on the RFC 3551 default map, but a present rtpmap must still agree.
        if payload >= 96 {
            declared_codec(section, payload)
                .is_some_and(|(name, clock, _)| name.eq_ignore_ascii_case(codec) && clock == 8000)
                .then_some(BridgeableAudio::narrow(codec))
        } else if codec == "G723" {
            payload_is(section, payload, codec, 8000).then_some(BridgeableAudio {
                codec: "G723",
                clock: 8000,
                channels: 1,
                fmtp: Some("annexa=no"),
                ptime_ms: 30,
            })
        } else {
            payload_is(section, payload, codec, 8000).then_some(BridgeableAudio::narrow(codec))
        }
    } else if (96..=127).contains(&payload) {
        let (name, clock, channels) = declared_codec(section, payload)?;
        let name = name.trim().to_ascii_uppercase().replace(['.', '-', '_'], "");
        if name == "OPUS" {
            // RFC 7587: 48 kHz clock, rtpmap channel count 2 (codec maximum);
            // the bridge itself is mono, advertised via stereo=0.
            if clock != 48_000 || channels.unwrap_or(2) != 2 {
                return None;
            }
            return Some(BridgeableAudio {
                codec: "OPUS",
                clock: 48_000,
                channels: 2,
                fmtp: Some("useinbandfec=1;stereo=0;sprop-stereo=0"),
                ptime_ms: 20,
            });
        }
        if clock != 8000 {
            return None;
        }
        let codec = match name.as_str() {
            "PCMU" => "PCMU",
            "PCMA" => "PCMA",
            "G722" => "G722",
            "G723" | "G7231" => "G723",
            "G72616" => "G726-16",
            "G72624" => "G726-24",
            "G72632" => "G726-32",
            "G72640" => "G726-40",
            _ => return None,
        };
        if codec == "G723" {
            return Some(BridgeableAudio {
                codec: "G723",
                clock: 8000,
                channels: 1,
                fmtp: Some("annexa=no"),
                ptime_ms: 30,
            });
        }
        Some(BridgeableAudio::narrow(codec))
    } else {
        None
    }
}

/// Payload types listed in the section's `m=` line. A stray `a=rtpmap:` for a
/// PT that is not negotiated must never be answered.
fn declared_payloads(section: &str) -> Vec<u8> {
    section
        .lines()
        .next()
        .map(|line| line.split_whitespace().skip(3).filter_map(|payload| payload.parse().ok()).collect())
        .unwrap_or_default()
}

fn rtpmap_h265(section: &str) -> Option<u8> {
    let declared = declared_payloads(section);
    section.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("a=rtpmap:")?;
        let (payload, encoding) = rest.split_once(' ')?;
        let payload: u8 = payload.parse().ok()?;
        let (name, clock) = encoding.split_once('/')?;
        ((96..=127).contains(&payload)
            && declared.contains(&payload)
            && (name.eq_ignore_ascii_case("H265") || name.eq_ignore_ascii_case("HEVC"))
            && clock == "90000")
            .then_some(payload)
    })
}

/// `sprop-max-don-diff=0` means "no DONL" and works with the DON-less
/// depacketizer; positive or unparsable values must not be answered.
fn fmtp_allows_h265(section: &str, payload: u8) -> bool {
    let prefix = format!("a=fmtp:{payload} ");
    for line in section.lines().map(str::trim) {
        let Some(parameters) = line.strip_prefix(&prefix) else { continue };
        for parameter in parameters.split(';') {
            let Some((key, value)) = parameter.trim().split_once('=') else { continue };
            if key.trim().eq_ignore_ascii_case("sprop-max-don-diff") {
                return value.trim().parse::<u32>().is_ok_and(|difference| difference == 0);
            }
        }
    }
    true
}

fn h265_payload(section: &str) -> Option<u8> {
    rtpmap_h265(section).filter(|payload| fmtp_allows_h265(section, *payload))
}

fn h264_payload(section: &str) -> Option<u8> {
    let declared = declared_payloads(section);
    section.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("a=rtpmap:")?;
        let (payload, encoding) = rest.split_once(' ')?;
        let payload: u8 = payload.parse().ok()?;
        let (name, clock) = encoding.split_once('/')?;
        (declared.contains(&payload)
            && (96..=127).contains(&payload)
            && name.eq_ignore_ascii_case("H264")
            && clock == "90000")
            .then_some(payload)
    })
}

fn media_ip(sdp: &str) -> Option<String> {
    let line = sdp.lines().find(|line| line.starts_with("c=IN IP4 "))?;
    Some(line.trim_start_matches("c=IN IP4 ").trim().to_string())
}

fn section_ip(section: &str, fallback: &str) -> String {
    section
        .lines()
        .find_map(|line| line.strip_prefix("c=IN IP4 "))
        .map(|value| value.trim().to_string())
        .unwrap_or_else(|| fallback.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const INVITE: &str = "INVITE sip:4d313d01-8300-450c-ab08-28c8a00f9f25@192.168.1.93 SIP/2.0\r\n\
Via: SIP/2.0/UDP 192.168.1.132:5060;branch=z9hG4bK1\r\n\
From: <sip:user0041@192.168.1.132>;tag=ftag\r\n\
To: <sip:4d313d01-8300-450c-ab08-28c8a00f9f25@192.168.1.93>\r\n\
Call-ID: cid-1\r\nCSeq: 1 INVITE\r\nContact: <sip:user0041@192.168.1.132:5060>\r\n\
Content-Type: application/sdp\r\nContent-Length: %LEN%\r\n\r\n%SDP%";

    fn invite(offer: &str) -> String {
        let template = INVITE.replace("%LEN%", "0").replace("%SDP%", offer);
        let head = template.split("\r\n\r\n").next().unwrap();
        let len = offer.len();
        format!("{}\r\n\r\n{}", head.replace("Content-Length: 0", &format!("Content-Length: {len}")), offer)
    }

    const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\nm=video 5006 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n";

    #[test]
    fn h265_don_diff_and_stray_rtpmap_are_rejected() {
        let base = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let don_one = format!("{base}m=video 5006 RTP/AVP 125\r\na=rtpmap:125 H265/90000\r\na=fmtp:125 profile-id=1;sprop-max-don-diff=1\r\n");
        let media = negotiate(&don_one).unwrap();
        assert_eq!(media.video_codec, None, "DONL is not implemented");
        assert_eq!(media.video_payload_type, None);
        let don_zero = format!("{base}m=video 5006 RTP/AVP 125\r\na=rtpmap:125 H265/90000\r\na=fmtp:125 profile-id=1;sprop-max-don-diff=0\r\n");
        let media = negotiate(&don_zero).unwrap();
        assert_eq!(media.video_codec, Some("H265"));
        let stray = format!("{base}m=video 5006 RTP/AVP 0\r\na=rtpmap:125 H265/90000\r\n");
        let media = negotiate(&stray).unwrap();
        assert_eq!(media.video_codec, None, "PT125 is not in m=video");
    }

    #[test]
    fn h265_only_offer_is_answered_with_h265() {
        let offer = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\nm=video 5006 RTP/AVP 125\r\na=rtpmap:125 H265/90000\r\na=fmtp:125 profile-id=1\r\n";
        assert!(UasRequest::parse(&invite(offer)).is_some());
        let media = negotiate(offer).expect("PCMU + H265 offer");
        assert_eq!(media.video_codec, Some("H265"));
        assert_eq!(media.video_payload_type, Some(125));
        let answer = build_answer("192.168.1.93", 7, 41000, Some(41002), &media);
        assert!(answer.contains("m=video 41002 RTP/AVP 125"), "{answer}");
        assert!(answer.contains("a=rtpmap:125 H265/90000"), "{answer}");
        assert!(answer.contains("a=fmtp:125 profile-id=1"), "{answer}");
        assert!(!answer.contains("H264"), "{answer}");
    }

    #[test]
    fn parses_invite_uri_and_negotiates_media() {
        let request = UasRequest::parse(&invite(OFFER)).unwrap();
        assert_eq!(request.method, "INVITE");
        assert_eq!(request.target_user().as_deref(), Some("4d313d01-8300-450c-ab08-28c8a00f9f25"));
        let media = negotiate(&request.body).unwrap();
        assert_eq!(media.audio_payload_type, 0);
        assert_eq!(media.remote_audio_port, 5004);
        assert_eq!(media.remote_video_port, Some(5006));
        assert_eq!(media.video_payload_type, Some(96));
    }

    #[test]
    fn inbound_offer_selects_g722_when_it_is_first_bridgeable() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 9 0 8 101\r\n\
a=rtpmap:9 G722/8000\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n\
a=rtpmap:101 telephone-event/8000\r\n";
        let media = negotiate(offer).expect("G722 inbound offer must be accepted");
        assert_eq!(media.audio_payload_type, 9);
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("m=audio 41000 RTP/AVP 9"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:9 G722/8000"), "{sdp}");
        // Static payload types redefined as another codec must be refused.
        for (payload, spoof) in [(9, "Opus/48000/2"), (0, "G722/8000"), (8, "PCMU/8000")] {
            let spoofed = format!(
                "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP {payload}\r\na=rtpmap:{payload} {spoof}\r\n"
            );
            assert!(negotiate(&spoofed).is_none(), "PT{payload} spoofed as {spoof} must be rejected");
        }
    }

    #[test]
    fn dynamic_pt_g726_keeps_peer_payload_number_and_codec_name() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 96 0\r\n\
a=rtpmap:96 G726-32/8000\r\na=rtpmap:0 PCMU/8000\r\n";
        let media = negotiate(offer).expect("dynamic G.726-32 must be accepted");
        assert_eq!(media.audio_payload_type, 96);
        assert_eq!(media.audio_codec, "G726-32");
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("m=audio 41000 RTP/AVP 96"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:96 G726-32/8000"), "{sdp}");
        // A dynamic PT without rtpmap is not a codec declaration.
        let undeclared = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 96\r\n";
        assert!(negotiate(undeclared).is_none());
    }

    #[test]
    fn licensed_or_unimplemented_audio_is_rejected_without_fallback() {
        for (payload, name, clock) in [
            (18, "G729", 8000),
            (102, "G7221", 16000),
            (104, "G7221", 32000),
            (112, "G719", 48000),
            (113, "MP4A-LATM", 90000),
        ] {
            let offer = format!(
                "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP {payload}\r\na=rtpmap:{payload} {name}/{clock}\r\n"
            );
            assert!(negotiate(&offer).is_none(), "PT{payload} {name}/{clock} must be 488, not silent fallback");
        }
        // A rejected codec next to a supported one still selects the supported one.
        let mixed = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 18 0\r\n\
a=rtpmap:18 G729/8000\r\na=rtpmap:0 PCMU/8000\r\n";
        let media = negotiate(mixed).expect("PCMU is still selectable");
        assert_eq!(media.audio_payload_type, 0);
    }

    #[test]
    fn g723_static_pt_is_accepted_with_30ms_ptime() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 4 0\r\n\
a=rtpmap:4 G723/8000\r\na=rtpmap:0 PCMU/8000\r\n";
        let media = negotiate(offer).expect("G.723.1 must be accepted");
        assert_eq!(media.audio_payload_type, 4);
        assert_eq!(media.audio_codec, "G723");
        assert_eq!(media.audio_ptime_ms, 30);
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("m=audio 41000 RTP/AVP 4"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:4 G723/8000"), "{sdp}");
        assert!(sdp.contains("a=fmtp:4 annexa=no"), "{sdp}");
        assert!(sdp.contains("a=ptime:30"), "{sdp}");
        // A wrong clock must not be accepted as G723.
        let wrong_clock = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 4\r\n\
a=rtpmap:4 G723/16000\r\n";
        assert!(negotiate(wrong_clock).is_none());
    }

    #[test]
    fn dynamic_g723_keeps_30ms_ptime_and_annexa_no() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 G723/8000\r\n";
        let media = negotiate(offer).expect("dynamic G723 must be accepted");
        assert_eq!(media.audio_payload_type, 96);
        assert_eq!(media.audio_codec, "G723");
        assert_eq!(media.audio_ptime_ms, 30);
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("a=fmtp:96 annexa=no"), "{sdp}");
        assert!(sdp.contains("a=ptime:30"), "{sdp}");
    }

    #[test]
    fn g728_static_pt_is_answered_with_its_own_rtpmap() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 15 0\r\n\
a=rtpmap:15 G728/8000\r\na=rtpmap:0 PCMU/8000\r\n";
        let media = negotiate(offer).expect("G.728 must be accepted");
        assert_eq!(media.audio_payload_type, 15);
        assert_eq!(media.audio_codec, "G728");
        assert_eq!(media.audio_clock_rate, 8000);
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("m=audio 41000 RTP/AVP 15"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:15 G728/8000"), "{sdp}");
        assert!(sdp.contains("a=ptime:20"), "{sdp}");
    }

    #[test]
    fn opus_offer_keeps_peer_pt_and_advertises_48k_mono() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 96 0\r\n\
a=rtpmap:96 opus/48000/2\r\na=fmtp:96 minptime=10;useinbandfec=1\r\na=rtpmap:0 PCMU/8000\r\n";
        let media = negotiate(offer).expect("Opus offer must be accepted");
        assert_eq!(media.audio_payload_type, 96);
        assert_eq!(media.audio_codec, "OPUS");
        assert_eq!(media.audio_clock_rate, 48_000);
        assert_eq!(media.audio_channels, 2);
        assert!(media.audio_fmtp.is_some_and(|fmtp| fmtp.contains("useinbandfec=1")));
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("m=audio 41000 RTP/AVP 96"), "{sdp}");
        assert!(sdp.contains("a=rtpmap:96 opus/48000/2"), "{sdp}");
        assert!(sdp.contains("a=fmtp:96 "), "{sdp}");
        assert!(sdp.contains("a=ptime:20"), "{sdp}");
        // Opus is 48 kHz; an 8 kHz claim for the same name must not pass.
        let wrong_clock = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 96\r\n\
a=rtpmap:96 opus/8000/2\r\n";
        assert!(negotiate(wrong_clock).is_none());
    }

    #[test]
    fn rejects_sdp_without_g711_or_with_invalid_length() {
        let mut request = invite("v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 111\r\n");
        assert!(UasRequest::parse(&request).is_some());
        assert!(negotiate("v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 111\r\n").is_none());
        request = request.replace("Content-Length: ", "Content-Length: 9");
        assert!(UasRequest::parse(&request).is_none());
    }

    #[test]
    fn builds_200_with_sdp_and_contact() {
        let request = UasRequest::parse(&invite(OFFER)).unwrap();
        let media = negotiate(&request.body).unwrap();
        let sdp = build_answer("192.168.1.93", 7, 41000, Some(41002), &media);
        let response = build_response(200, "OK", &request, "192.168.1.93", 5060, Some("stag"), &sdp);
        assert!(response.starts_with("SIP/2.0 200 OK\r\n"));
        assert!(response.contains("To: <sip:4d313d01-8300-450c-ab08-28c8a00f9f25@192.168.1.93>;tag=stag"));
        assert!(response.contains("Contact: <sip:4d313d01-8300-450c-ab08-28c8a00f9f25@192.168.1.93:5060>"));
        assert!(response.contains("m=audio 41000 RTP/AVP 0"));
        assert!(response.contains("a=ptime:20"));
        assert!(response.contains("m=video 41002 RTP/AVP 96"));
        assert!(response.contains(&format!("Content-Length: {}", sdp.len())));
    }

    #[test]
    fn rejects_srtp_proto_and_disables_unoffered_video() {
        let savp = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/SAVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        assert!(negotiate(savp).is_none(), "SRTP offer must be 488, not downgraded");
        let audio_only = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 8\r\na=rtpmap:8 PCMA/8000\r\nm=video 5006 RTP/AVP 96\r\na=rtpmap:96 VP8/90000\r\n";
        let media = negotiate(audio_only).unwrap();
        assert_eq!(media.audio_payload_type, 8);
        assert_eq!(media.video_payload_type, None);
        let sdp = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(sdp.contains("m=audio 41000 RTP/AVP 8"));
        assert!(sdp.contains("m=video 0 RTP/AVP 0"));
    }
    #[test]
    fn percent_decodes_room_id_and_ignores_params() {
        let raw = invite(OFFER).replace("4d313d01-8300-450c-ab08-28c8a00f9f25", "room%2D1;transport=udp");
        let request = UasRequest::parse(&raw).unwrap();
        assert_eq!(request.target_user().as_deref(), Some("room-1"));
    }
}

//! Минимальный SIP UAS для прямого вызова MCU по `sip:<room>@<ip>`.
//!
//! Транспорт (UDP-сокет) и жизненный цикл живут в `vks-server`; здесь только
//! разбор запроса, выбор кодека и сериализация ответов, чтобы это тестировалось
//! без сети. Поддерживаются INVITE/ACK/BYE/CANCEL/OPTIONS и plain RTP/AVP
//! (PCMU/PCMA + H.264). SRTP/TLS/100rel/digest осознанно отклоняются, а не
//! имитируются.

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
    pub video_payload_type: Option<u8>,
    pub remote_ip: String,
    pub remote_audio_port: u16,
    pub remote_video_port: Option<u16>,
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

/// Выбирает PCMU/PCMA и, если есть, H.264 из SDP-оффера.
pub fn negotiate(offer: &str) -> Option<InboundMedia> {
    let session_ip = media_ip(offer)?;
    let audio = media_section(offer, "audio")?;
    if media_proto(audio) != Some("RTP/AVP") {
        return None;
    }
    let audio_port = media_port(audio)?;
    if audio_port == 0 {
        return None;
    }
    let audio_payload_type = if has_payload(audio, 0) {
        0
    } else if has_payload(audio, 8) {
        8
    } else {
        return None;
    };
    let video_section = media_section(offer, "video");
    let video_offered = video_section.is_some();
    let video = video_section
        .filter(|section| media_proto(section) == Some("RTP/AVP"))
        .filter(|section| media_port(section).is_some_and(|port| port > 0));
    let video_payload_type =
        video.filter(|section| has_h264(section)).and_then(|section| dynamic_payload(section, "H264"));
    let remote_video_port = video.and_then(media_port);
    Some(InboundMedia {
        audio_payload_type,
        video_payload_type,
        remote_ip: section_ip(audio, &session_ip),
        remote_audio_port: audio_port,
        remote_video_port,
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
    if media.audio_payload_type == 0 {
        sdp.push_str("a=rtpmap:0 PCMU/8000\r\n");
    } else {
        sdp.push_str("a=rtpmap:8 PCMA/8000\r\n");
    }
    sdp.push_str("a=ptime:20\r\n");
    if media.video_offered {
        if let (Some(port), Some(payload)) = (video_port, media.video_payload_type) {
            sdp.push_str(&format!(
                "m=video {port} RTP/AVP {payload}\r\na=rtpmap:{payload} H264/90000\r\na=fmtp:{payload} packetization-mode=1;profile-level-id=42e01f\r\n"
            ));
        } else {
            // Оффер содержал video, но совместимого H.264 нет: сохраняем
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

fn has_payload(section: &str, payload: u8) -> bool {
    let Some(line) = section.lines().next() else { return false };
    line.split_whitespace().skip(3).any(|item| item == payload.to_string())
}

fn has_h264(section: &str) -> bool {
    section.lines().any(|line| {
        let line = line.trim();
        line.starts_with("a=rtpmap:") && line.to_ascii_uppercase().contains("H264/90000")
    })
}

fn dynamic_payload(section: &str, codec: &str) -> Option<u8> {
    section.lines().find_map(|line| {
        let line = line.trim().strip_prefix("a=rtpmap:")?;
        let (payload, rest) = line.split_once(' ')?;
        if rest.to_ascii_uppercase().starts_with(codec) {
            payload.parse().ok()
        } else {
            None
        }
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

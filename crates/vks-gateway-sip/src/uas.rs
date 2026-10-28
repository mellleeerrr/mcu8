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
    /// Exact `a=fmtp` our sender announces in the answer for the selected
    /// video payload. RFC 6184 §8.2.2: this describes the *answerer's* stream,
    /// so it is our Baseline TX profile, not a copy of the offer's.
    pub video_fmtp: Option<String>,
    /// Decimal H.264 level our encoder must run at (31 = level 3.1); the
    /// answer's `profile-level-id` and `-level:v` have to agree.
    pub video_tx_level: Option<u8>,
    /// Negotiated RFC 4733 `telephone-event` payload type (only when offered).
    pub dtmf_payload_type: Option<u8>,
    /// The video section was offered as `RTP/AVPF` (RFC 4585 feedback usable).
    pub video_avpf: bool,
    /// The offer listed `a=rtcp-fb:<pt> nack pli` / `ccm fir` for our payload.
    pub video_fb_pli: bool,
    pub video_fb_fir: bool,
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
    negotiate_with(offer, vks_common::codec_capabilities::audio_codec_available("MP4A-LATM"))
}

fn negotiate_with(offer: &str, aac_available: bool) -> Option<InboundMedia> {
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
        .find_map(|payload| bridgeable_audio(audio, payload, aac_available).map(|codec| (payload, codec)))?;
    let video_section = media_section(offer, "video");
    let video_offered = video_section.is_some();
    // RFC 4585 §3: AVPF feedback is only meaningful when the m-line itself was
    // offered as RTP/AVPF; a plain RTP/AVP section keeps working unchanged.
    let video = video_section
        .filter(|section| matches!(media_proto(section), Some("RTP/AVP") | Some("RTP/AVPF")))
        .filter(|section| media_port(section).is_some_and(|port| port > 0));
    let video_avpf = video.is_some_and(|section| media_proto(section) == Some("RTP/AVPF"));
    let video_codec = video.and_then(|section| {
        select_video(
            section,
            vks_common::codec_capabilities::video_codec_available("H264"),
            vks_common::codec_capabilities::video_codec_available("H265"),
        )
    });
    let video_payload_type = video_codec.as_ref().map(|selected| selected.payload);
    let video_tx_level = video_codec.as_ref().and_then(|selected| selected.tx_level);
    let (video_fb_pli, video_fb_fir) = match (video, video_payload_type) {
        (Some(section), Some(payload)) if video_avpf => offered_feedback(section, payload),
        _ => (false, false),
    };
    let dtmf_payload_type = telephone_event_payload(audio, bridge_audio.clock);
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
        video_codec: video_codec.as_ref().map(|selected| selected.codec),
        video_fmtp: video_codec.map(|selected| selected.fmtp),
        video_tx_level,
        dtmf_payload_type,
        video_avpf,
        video_fb_pli,
        video_fb_fir,
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
    // Answer formats must be a subset of the offer (RFC 3264 §6.1): the
    // telephone-event PT is appended only when the peer offered it.
    let mut audio_formats = media.audio_payload_type.to_string();
    if let Some(dtmf) = media.dtmf_payload_type {
        audio_formats.push(' ');
        audio_formats.push_str(&dtmf.to_string());
    }
    let mut sdp = format!(
        "v=0\r\no=- {session_id} {session_id} IN IP4 {local_ip}\r\ns=VKS MCU\r\nc=IN IP4 {local_ip}\r\nt=0 0\r\nm=audio {audio_port} RTP/AVP {audio_formats}\r\n"
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
    // MP4A-LATM is 1024 samples (21.33 ms) per frame; a ptime of 20 would
    // promise packets the encoder cannot produce. 0 means "omit".
    if media.audio_ptime_ms > 0 {
        sdp.push_str(&format!("a=ptime:{}\r\n", media.audio_ptime_ms));
    }
    if media.audio_codec == "G723" {
        // G.723.1 can pack multiple 30 ms frames; the bridge decodes one.
        sdp.push_str("a=maxptime:30\r\n");
    }
    // RFC 3605: name the RTCP port explicitly instead of relying on the
    // implicit "+1" convention; the gateway binds the odd port of the pair.
    sdp.push_str(&format!("a=rtcp:{}\r\n", audio_port.saturating_add(1)));
    if let Some(dtmf) = media.dtmf_payload_type {
        // RFC 4733 §2.5: events 0-15 are the DTMF digits; the payload carries
        // one event at a time, so no redundancy parameters are advertised.
        sdp.push_str(&format!("a=rtpmap:{dtmf} telephone-event/{}\r\na=fmtp:{dtmf} 0-15\r\n", media.audio_clock_rate));
    }
    if media.video_offered {
        if let (Some(port), Some(payload)) = (video_port, media.video_payload_type) {
            let (name, fmtp) = match (media.video_codec, media.video_fmtp.as_deref()) {
                (Some("H265"), Some(fmtp)) => ("H265", fmtp.to_string()),
                (Some("H265"), None) => ("H265", "profile-id=1".to_string()),
                (_, Some(fmtp)) => ("H264", fmtp.to_string()),
                _ => ("H264", "packetization-mode=1;profile-level-id=42c01f".to_string()),
            };
            // RFC 4585 §3: AVPF is only used when the offer itself was AVPF;
            // the feedback types echoed back are limited to the offered ones
            // (§4.2: an answer must not enable unsupported feedback).
            let proto = if media.video_avpf { "RTP/AVPF" } else { "RTP/AVP" };
            sdp.push_str(&format!(
                "m=video {port} {proto} {payload}\r\na=rtpmap:{payload} {name}/90000\r\na=fmtp:{payload} {fmtp}\r\na=rtcp:{}\r\n",
                port.saturating_add(1)
            ));
            if media.video_avpf && media.video_fb_pli {
                sdp.push_str(&format!("a=rtcp-fb:{payload} nack pli\r\n"));
            }
            if media.video_avpf && media.video_fb_fir {
                sdp.push_str(&format!("a=rtcp-fb:{payload} ccm fir\r\n"));
            }
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

/// AAC-LATM fmtp gate for the in-band-SMC bridge: `bitrate`, `profile-level-id`
/// and unknown parameters are ignored (RFC 3016 allows them); the codec itself
/// must be AAC-LC (object type 2) and SMC must travel in-band (`cpresent=1`).
pub(crate) fn aac_fmtp_is_bridgeable(fmtp: &str) -> bool {
    let parameters: Vec<(String, String)> = fmtp
        .split(';')
        .filter_map(|parameter| {
            let (key, value) = parameter.trim().split_once('=')?;
            Some((key.trim().to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    aac_fmtp_parameters_bridgeable(&parameters)
}

/// Fail-closed across every occurrence: `object=8;object=2` or two
/// `a=fmtp` lines must not pass on "last key wins". SBR is rejected unless
/// it is explicitly disabled.
fn aac_fmtp_parameters_bridgeable(parameters: &[(String, String)]) -> bool {
    let mut object_ok = true;
    let mut cpresent_ok = true;
    let mut sbr_ok = true;
    for (key, value) in parameters {
        let value = value.trim();
        if value == "!conflict!" {
            return false;
        }
        match key.as_str() {
            "object" => object_ok &= value == "2",
            // RFC 3016 defines cpresent as 0 or 1; only 1 is bridgeable
            // in-band, and anything non-canonical must fail closed.
            "cpresent" => cpresent_ok &= value == "1",
            "sbr" => {
                let disabled = value.eq_ignore_ascii_case("0") || value.eq_ignore_ascii_case("false");
                sbr_ok &= disabled;
            }
            _ => {}
        }
    }
    object_ok && cpresent_ok && sbr_ok
}

/// Map one offered payload to a codec the bridge can handle without a
/// transcoder. Static PTs 0/8/9 may omit rtpmap; dynamic PTs (including our
/// 107-110) must declare a matching name+clock, otherwise a hostile offer
/// could claim PCMU while sending G.726.
fn bridgeable_audio(section: &str, payload: u8, aac_available: bool) -> Option<BridgeableAudio> {
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
        if name == "MP4ALATM" {
            if !aac_available {
                return None;
            }
            // Slice 3 supports in-band SMC (cpresent=1) only: a config-only
            // stream needs a synthetic SMC built from fmtp config=, which is
            // not implemented yet. Refusing it here yields 488 instead of
            // "200 OK and silence".
            if clock != 90_000 || channels.unwrap_or(1) != 1 {
                return None;
            }
            if !aac_fmtp_parameters_bridgeable(&aac_fmtp_pairs(section, payload)) {
                return None;
            }
            return Some(BridgeableAudio {
                codec: "MP4A-LATM",
                clock: 90_000,
                channels: 1,
                fmtp: Some("profile-level-id=1;object=2;cpresent=1"),
                ptime_ms: 0,
            });
        }
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

/// Video payload selected for the answer, including the fmtp that must be
/// mirrored to keep profile/packetization symmetric with the offer.
#[derive(Debug)]
struct VideoSelection {
    payload: u8,
    codec: &'static str,
    fmtp: String,
    /// H.264 transmit level (decimal) when the codec is H.264.
    tx_level: Option<u8>,
}

/// Picks the first video codec the host can actually bridge *and* whose SDP
/// configuration our sender implements, honouring the peer's payload order.
fn select_video(section: &str, h264: bool, h265: bool) -> Option<VideoSelection> {
    if h264 {
        if let Some(selected) = h264_payload(section) {
            return Some(selected);
        }
    }
    if h265 {
        if let Some(selected) = h265_payload(section) {
            return Some(selected);
        }
    }
    None
}

fn video_payloads(section: &str, codec_name: &str) -> Vec<u8> {
    let declared = declared_payloads(section);
    section
        .lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("a=rtpmap:")?;
            let (payload, encoding) = rest.split_once(' ')?;
            let payload: u8 = payload.parse().ok()?;
            let (name, clock) = encoding.split_once('/')?;
            let matches_codec = if codec_name.eq_ignore_ascii_case("H265") {
                // HEVC is the pre-RFC 7798 alias still emitted by some peers.
                name.eq_ignore_ascii_case("H265") || name.eq_ignore_ascii_case("HEVC")
            } else {
                name.eq_ignore_ascii_case(codec_name)
            };
            (declared.contains(&payload) && (96..=127).contains(&payload) && matches_codec && clock == "90000")
                .then_some(payload)
        })
        .collect()
}

/// Every key/value pair from every `a=fmtp:<payload>` line, duplicates kept:
/// the AAC gate must fail closed across all occurrences.
fn aac_fmtp_pairs(section: &str, payload: u8) -> Vec<(String, String)> {
    let mut parameters = Vec::new();
    for line in section.lines() {
        let lower = line.trim().to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix("a=fmtp:") else { continue };
        // RFC 8866 allows any whitespace after the integer payload type, and
        // SDP integers may carry leading zeroes; a literal-space prefix would
        // silently skip the whole gate on `a=fmtp:113\t...`.
        // Also split on `;` so the invalid `a=fmtp:113;object=8` can never be
        // skipped and default the gate to accept.
        let Some((declared, rest)) = rest.split_once(|c: char| c.is_whitespace() || c == ';') else {
            continue;
        };
        if declared.trim().parse::<u8>().ok() != Some(payload) {
            continue;
        }
        for parameter in rest.split(';') {
            let Some((key, value)) = parameter.trim().split_once('=') else { continue };
            parameters.push((key.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    parameters
}

/// Parameter map from every `a=fmtp:<payload>` line (last value wins).
fn fmtp_parameters(section: &str, payload: u8) -> Vec<(String, String)> {
    let prefix = format!("a=fmtp:{payload} ");
    let mut parameters: Vec<(String, String)> = Vec::new();
    for line in section.lines().map(str::trim) {
        let Some(rest) = line.strip_prefix(&prefix) else { continue };
        for parameter in rest.split(';') {
            let Some((key, value)) = parameter.trim().split_once('=') else { continue };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim().to_string();
            if let Some(existing) = parameters.iter_mut().find(|(name, _)| *name == key) {
                existing.1 = value;
            } else {
                parameters.push((key, value));
            }
        }
    }
    parameters
}

fn fmtp_get<'a>(parameters: &'a [(String, String)], name: &str) -> Option<&'a str> {
    parameters.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.as_str())
}

/// H.264 profiles whose 8-bit 4:2:0 stream survives the gateway pipeline and
/// the FFmpeg decoder behind it: Baseline (66), Main (77), High (100).
/// High 10 / 4:2:2 / 4:4:4 (110/122/244) are rejected: the MCU canvas is yuv420p.
fn h264_bridgeable_profile(profile_idc: u8) -> bool {
    matches!(profile_idc, 66 | 77 | 100)
}

/// Our H.264 sender: x264 Baseline emits constraint_set0/1, i.e. `42c0XX`
/// (matches `VideoProfile::profile_level_id` in `gateway_video`).
/// Level 3.1 is the highest the bridge encoder is provisioned for.
const H264_TX_LEVEL_MAX: u8 = 31;

fn h264_payload(section: &str) -> Option<VideoSelection> {
    // Our sender emits FU-A (packetization-mode=1). An offer that only signals
    // mode 0 (the RFC 6184 default when the fmtp is absent) cannot receive
    // FU-A, and the answer may not claim a mode the offer did not allow
    // (RFC 6184 §8.2.2) — such an offer deliberately stays video-less.
    video_payloads(section, "H264").into_iter().find_map(|payload| {
        let parameters = fmtp_parameters(section, payload);
        if fmtp_get(&parameters, "packetization-mode") != Some("1") {
            return None;
        }
        let profile_level_id = fmtp_get(&parameters, "profile-level-id")?;
        let bytes = parse_profile_level_id(profile_level_id)?;
        if !h264_bridgeable_profile(bytes[0]) || bytes[2] < 10 {
            return None;
        }
        // The answer's profile-level-id describes our TX stream (RFC 6184
        // §8.2.2). Without `level-asymmetry-allowed=1` it must not exceed the
        // offered level, so the encoder is pinned to min(offered, 3.1).
        let asymmetry = fmtp_get(&parameters, "level-asymmetry-allowed") == Some("1");
        let tx_level = if asymmetry { H264_TX_LEVEL_MAX } else { bytes[2].min(H264_TX_LEVEL_MAX) };
        let mut fmtp = format!("packetization-mode=1;profile-level-id=42c0{tx_level:02x}");
        if asymmetry {
            fmtp.push_str(";level-asymmetry-allowed=1");
        }
        Some(VideoSelection { payload, codec: "H264", fmtp, tx_level: Some(tx_level) })
    })
}

/// RFC 4733 `telephone-event` payload type from the offer, if any. Only the
/// clock rate matching the negotiated audio codec is usable, and the
/// conventional 101 wins over other dynamic numbers.
fn telephone_event_payload(section: &str, clock_rate: u32) -> Option<u8> {
    let mut best: Option<(u8, u8)> = None;
    for line in section.lines() {
        let Some(rest) = line.trim().strip_prefix("a=rtpmap:") else { continue };
        let Some((payload, encoding)) = rest.split_once(' ') else { continue };
        let Ok(payload) = payload.parse::<u8>() else { continue };
        let mut parts = encoding.split('/');
        let Some(name) = parts.next() else { continue };
        let Some(clock) = parts.next().and_then(|value| value.parse::<u32>().ok()) else { continue };
        if !name.eq_ignore_ascii_case("telephone-event") || clock != clock_rate {
            continue;
        }
        let preference = if payload == 101 {
            0
        } else if payload >= 96 {
            1
        } else {
            2
        };
        match best {
            None => best = Some((preference, payload)),
            Some((existing, _)) if preference < existing => best = Some((preference, payload)),
            _ => {}
        }
    }
    best.map(|(_, payload)| payload)
}

/// Which RTCP feedback types the offer allows for the selected payload
/// (RFC 4585 §4; `*` covers every payload).
fn offered_feedback(section: &str, payload: u8) -> (bool, bool) {
    let mut pli = false;
    let mut fir = false;
    for line in section.lines() {
        let Some(rest) = line.trim().strip_prefix("a=rtcp-fb:") else { continue };
        let Some((pt, feedback)) = rest.split_once(' ') else { continue };
        if pt != "*" && pt.parse::<u8>().ok() != Some(payload) {
            continue;
        }
        match feedback.trim() {
            "nack pli" => pli = true,
            "ccm fir" => fir = true,
            _ => {}
        }
    }
    (pli, fir)
}

fn parse_profile_level_id(value: &str) -> Option<[u8; 3]> {
    let hex = value.trim();
    if hex.len() != 6 {
        return None;
    }
    let mut bytes = [0u8; 3];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

fn h265_payload(section: &str) -> Option<VideoSelection> {
    video_payloads(section, "H265").into_iter().find_map(|payload| {
        let parameters = fmtp_parameters(section, payload);
        // Main profile, main tier only; profile-id defaults to 1 per RFC 7798.
        if fmtp_get(&parameters, "profile-id").is_some_and(|value| value != "1") {
            return None;
        }
        if fmtp_get(&parameters, "tier-flag").is_some_and(|value| value != "0") {
            return None;
        }
        if fmtp_get(&parameters, "sprop-max-don-diff").is_some_and(|value| value.parse::<u32>().ok() != Some(0)) {
            return None;
        }
        Some(VideoSelection { payload, codec: "H265", fmtp: "profile-id=1".to_string(), tx_level: None })
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

    const OFFER: &str = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\nm=video 5006 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\n";

    #[test]
    fn hevc_alias_is_selected_for_h265() {
        let base = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let offer = format!("{base}m=video 5006 RTP/AVP 125\r\na=rtpmap:125 HEVC/90000\r\na=fmtp:125 profile-id=1\r\n");
        let media = negotiate(&offer).unwrap();
        assert_eq!(media.video_codec, Some("H265"));
        let answer = build_answer("192.168.1.93", 7, 41000, Some(41002), &media);
        assert!(answer.contains("m=video 41002 RTP/AVP 125"), "{answer}");
    }

    #[test]
    fn video_fmtp_gate_accepts_baseline_main_high() {
        let base = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let video = |fmtp: &str| format!("{base}m=video 5006 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n{fmtp}");
        // Default packetization-mode is 0; our sender emits FU-A (mode 1).
        assert_eq!(negotiate(&video("")).unwrap().video_codec, None);
        assert_eq!(
            negotiate(&video("a=fmtp:96 packetization-mode=0;profile-level-id=42e01f\r\n")).unwrap().video_codec,
            None
        );
        assert_eq!(
            negotiate(&video("a=fmtp:96 packetization-mode=2;profile-level-id=42e01f\r\n")).unwrap().video_codec,
            None
        );
        // Baseline/Main/High all decode into the yuv420 MCU pipeline. The answer
        // declares *our* Baseline TX profile (RFC 6184 §8.2.2), and the encoder
        // level follows the offered level unless level-asymmetry is allowed.
        for (offered, expected, level) in [
            ("42e01f", "packetization-mode=1;profile-level-id=42c01f", 31u8),
            ("42e01e", "packetization-mode=1;profile-level-id=42c01e", 30),
            ("4d001f", "packetization-mode=1;profile-level-id=42c01f", 31),
            ("64001f", "packetization-mode=1;profile-level-id=42c01f", 31),
        ] {
            let offer = video(&format!("a=fmtp:96 packetization-mode=1;profile-level-id={offered}\r\n"));
            let media = negotiate(&offer).unwrap();
            assert_eq!(media.video_codec, Some("H264"), "{offered}");
            assert_eq!(media.video_fmtp.as_deref(), Some(expected), "{offered}");
            assert_eq!(media.video_tx_level, Some(level), "{offered}");
        }
        // High 10 (idc 110) needs a 10-bit decoder outside the yuv420 pipeline.
        assert_eq!(
            negotiate(&video("a=fmtp:96 packetization-mode=1;profile-level-id=6e001f\r\n")).unwrap().video_codec,
            None
        );
        let h265 = |fmtp: &str| format!("{base}m=video 5006 RTP/AVP 125\r\na=rtpmap:125 H265/90000\r\n{fmtp}");
        assert_eq!(negotiate(&h265("a=fmtp:125 profile-id=2\r\n")).unwrap().video_codec, None);
        assert_eq!(negotiate(&h265("a=fmtp:125 profile-id=1;tier-flag=1\r\n")).unwrap().video_codec, None);
        assert_eq!(negotiate(&h265("")).unwrap().video_codec, Some("H265"));
    }

    #[test]
    fn level_asymmetry_lets_us_announce_31_for_low_level_offer() {
        let base = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let offer = format!(
            "{base}m=video 5006 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01e;level-asymmetry-allowed=1\r\n"
        );
        let media = negotiate(&offer).unwrap();
        assert_eq!(media.video_tx_level, Some(31));
        assert_eq!(
            media.video_fmtp.as_deref(),
            Some("packetization-mode=1;profile-level-id=42c01f;level-asymmetry-allowed=1")
        );
        let answer = build_answer("192.168.1.93", 7, 41000, Some(41002), &media);
        assert!(
            answer.contains("a=fmtp:96 packetization-mode=1;profile-level-id=42c01f;level-asymmetry-allowed=1"),
            "{answer}"
        );
    }

    #[test]
    fn avpf_answer_echoes_only_offered_feedback() {
        let base = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\nm=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
        let offer = format!(
            "{base}m=video 5006 RTP/AVPF 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\na=rtcp-fb:96 nack pli\r\na=rtcp-fb:96 ccm fir\r\na=rtcp-fb:96 nack\r\n"
        );
        let media = negotiate(&offer).unwrap();
        assert!(media.video_avpf && media.video_fb_pli && media.video_fb_fir);
        let answer = build_answer("192.168.1.93", 7, 41000, Some(41002), &media);
        assert!(answer.contains("m=video 41002 RTP/AVPF 96"), "{answer}");
        assert!(answer.contains("a=rtcp-fb:96 nack pli"), "{answer}");
        assert!(answer.contains("a=rtcp-fb:96 ccm fir"), "{answer}");
        assert!(answer.contains("a=rtcp:41003"), "{answer}");
        // A plain RTP/AVP offer cannot use AVPF feedback even if it lists rtcp-fb.
        let avp = offer.replace("RTP/AVPF", "RTP/AVP");
        let media = negotiate(&avp).unwrap();
        assert!(!media.video_avpf && !media.video_fb_pli && !media.video_fb_fir);
        let answer = build_answer("192.168.1.93", 7, 41000, Some(41002), &media);
        assert!(answer.contains("m=video 41002 RTP/AVP 96"), "{answer}");
        assert!(!answer.contains("rtcp-fb"), "{answer}");
    }

    #[test]
    fn telephone_event_is_answered_only_when_offered() {
        let base = "v=0\r\no=- 1 1 IN IP4 192.168.1.132\r\ns=-\r\nc=IN IP4 192.168.1.132\r\nt=0 0\r\n";
        let with_events = format!(
            "{base}m=audio 5004 RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-16\r\n"
        );
        let media = negotiate(&with_events).unwrap();
        assert_eq!(media.dtmf_payload_type, Some(101));
        let answer = build_answer("192.168.1.93", 7, 41000, None, &media);
        assert!(answer.contains("m=audio 41000 RTP/AVP 0 101"), "{answer}");
        assert!(answer.contains("a=rtpmap:101 telephone-event/8000"), "{answer}");
        assert!(answer.contains("a=fmtp:101 0-15"), "{answer}");
        // No telephone-event in the offer => the answer must not introduce it
        // (RFC 3264 §6.1: answer formats are a subset of the offer).
        let plain = format!("{base}m=audio 5004 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n");
        let media = negotiate(&plain).unwrap();
        assert_eq!(media.dtmf_payload_type, None);
        let answer = build_answer("192.168.1.93", 7, 41000, None, &media);
        assert!(!answer.contains("telephone-event"), "{answer}");
        // Opus runs on a 48 kHz clock, so only telephone-event/48000 matches.
        let opus = format!(
            "{base}m=audio 5004 RTP/AVP 96 101\r\na=rtpmap:96 opus/48000/2\r\na=fmtp:96 minptime=10;useinbandfec=1\r\na=rtpmap:101 telephone-event/48000\r\n"
        );
        let media = negotiate(&opus).unwrap();
        assert_eq!(media.audio_codec, "OPUS");
        assert_eq!(media.dtmf_payload_type, Some(101));
    }

    #[test]
    fn video_selection_follows_host_availability() {
        let both = "m=video 5006 RTP/AVP 96 125\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\na=rtpmap:125 H265/90000\r\na=fmtp:125 profile-id=1\r\n";
        let h264_only = "m=video 5006 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=42e01f\r\n";
        let h265_only = "m=video 5006 RTP/AVP 125\r\na=rtpmap:125 H265/90000\r\na=fmtp:125 profile-id=1\r\n";
        let pick = |section, h264, h265| select_video(section, h264, h265).map(|s| (s.payload, s.codec));
        assert_eq!(pick(both, true, true), Some((96, "H264")));
        assert_eq!(pick(both, false, true), Some((125, "H265")));
        assert_eq!(pick(both, true, false), Some((96, "H264")));
        assert_eq!(pick(both, false, false), None);
        assert_eq!(pick(h264_only, false, true), None);
        assert_eq!(pick(h265_only, true, true), Some((125, "H265")));
    }

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
        let don_garbage = format!(
            "{base}m=video 5006 RTP/AVP 125\r\na=rtpmap:125 H265/90000\r\na=fmtp:125 sprop-max-don-diff=abc\r\n"
        );
        assert_eq!(negotiate(&don_garbage).unwrap().video_codec, None);
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

    #[test]
    fn inbound_aac_latm_cpresent1_is_answered_without_ptime() {
        let offer = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 113 101\r\n\
a=rtpmap:113 MP4A-LATM/90000\r\na=fmtp:113 profile-level-id=1;object=2;cpresent=1\r\n\
a=rtpmap:101 telephone-event/8000\r\n";
        assert!(negotiate(offer).is_none(), "probe off must refuse AAC");
        let media = negotiate_with(offer, true).expect("cpresent=1 accepted");
        assert_eq!(media.audio_codec, "MP4A-LATM");
        assert_eq!(media.audio_clock_rate, 90_000);
        let answer = build_answer("192.168.1.93", 1, 41000, None, &media);
        assert!(answer.contains("a=rtpmap:113 MP4A-LATM/90000"), "{answer}");
        assert!(answer.contains("cpresent=1"), "{answer}");
        assert!(!answer.contains("a=ptime"), "21.33 ms frames cannot promise ptime:20: {answer}");

        let config_only = offer.replace("cpresent=1", "cpresent=0").replace(
            "profile-level-id=1;object=2;cpresent=0",
            "profile-level-id=1;object=2;cpresent=0;config=40002420",
        );
        assert!(negotiate_with(&config_only, true).is_none(), "cpresent=0 needs config support");
        let bad_clock = offer.replace("MP4A-LATM/90000", "MP4A-LATM/48000");
        assert!(negotiate_with(&bad_clock, true).is_none(), "48 kHz clock is not bridged yet");
        let he_aac = offer.replace("object=2", "object=8");
        assert!(negotiate_with(&he_aac, true).is_none(), "HE-AAC is not bridged");
        for object in ["22", "23", "29"] {
            let variant = offer.replace("object=2", &format!("object={object}"));
            assert!(negotiate_with(&variant, true).is_none(), "object={object} is not AAC-LC");
        }
        for sbr in ["SBR=1", "SBR=true", "SBR=TRUE"] {
            let variant = offer.replace("object=2", &format!("object=2;{sbr}"));
            assert!(negotiate_with(&variant, true).is_none(), "SBR implies HE-AAC: {sbr}");
        }
        assert!(
            negotiate_with(&offer.replace("object=2", "object=2;SBR=0"), true).is_some(),
            "explicitly disabled SBR is fine"
        );
        for (bad, label) in [
            ("object=8;object=2", "conflicting object duplicates"),
            ("cpresent=0;cpresent=1", "conflicting cpresent duplicates"),
        ] {
            let variant = offer.replace("cpresent=1", &format!("cpresent=1;{bad}"));
            assert!(negotiate_with(&variant, true).is_none(), "{label} must be refused");
        }
        // RFC 3016 legal parameters must not be mistaken for a rate limit.
        let bitrate = offer.replace("object=2", "object=2;bitrate=64000");
        assert!(negotiate_with(&bitrate, true).is_some(), "bitrate is informational");
        let config_hint = offer.replace("cpresent=1", "cpresent=1;config=40002420");
        assert!(negotiate_with(&config_hint, true).is_some(), "config hint with in-band SMC is legal");
    }

    #[test]
    fn inbound_aac_fmtp_first_parameter_and_multiple_lines_fail_closed() {
        let base = "v=0\r\nc=IN IP4 192.168.1.132\r\nm=audio 5004 RTP/AVP 113\r\n\
a=rtpmap:113 MP4A-LATM/90000\r\n";
        let cases = [
            "a=fmtp:113 object=8;cpresent=1\r\n",
            "a=fmtp:113 cpresent=0;object=2\r\n",
            "a=fmtp:113 SBR=true;object=2;cpresent=1\r\n",
            "a=fmtp:113 profile-level-id=1;object=2;cpresent=1\r\na=fmtp:113 object=8\r\n",
            "a=fmtp:113 profile-level-id=1;object=2;cpresent=1\r\na=fmtp:113 cpresent=0\r\n",
        ];
        for fmtp in cases {
            let offer = format!("{base}{fmtp}");
            assert!(negotiate_with(&offer, true).is_none(), "must refuse:\n{fmtp}");
        }
        let legal = format!("{base}a=fmtp:113 object=2;cpresent=1;bitrate=64000;config=40002420\r\n");
        assert!(negotiate_with(&legal, true).is_some(), "legal in-band offer must pass");
        for bad_prefix in
            ["a=fmtp:113\tobject=8;cpresent=1\r\n", "a=fmtp:00113 object=8;cpresent=1\r\n", "a=fmtp:113;object=8\r\n"]
        {
            let offer = format!("{base}{bad_prefix}");
            assert!(negotiate_with(&offer, true).is_none(), "must refuse prefix: {bad_prefix:?}");
        }
        let other_pt = format!("{base}a=fmtp:1130 object=8;cpresent=1\r\n");
        assert!(negotiate_with(&other_pt, true).is_some(), "PT1130 is a different payload, fmtp must not gate 113");
        for weird in ["00", "false", "01", "true"] {
            let fmtp = format!("{base}a=fmtp:113 object=2;cpresent={weird}\r\n");
            assert!(negotiate_with(&fmtp, true).is_none(), "non-canonical cpresent={weird} must be refused");
        }
    }
}

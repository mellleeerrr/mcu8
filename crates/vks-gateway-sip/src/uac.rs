//! Real outbound SIP dialogs with negotiated RTP codecs and remote teardown.
use crate::rtp_bridge::negotiate_media_detailed;
use crate::sip_message::{build_ack, build_bye, build_cancel, build_invite, SipResponse};
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SipDialError {
    #[error("Invalid or unsupported target URI: {0}")]
    InvalidUri(String),
    #[error("Socket IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Call rejected by remote terminal: {0} {1}")]
    Rejected(u16, String),
    #[error("Dial timeout (no answer from {0} within {1:?})")]
    Timeout(String, Duration),
    #[error("Invalid or unsupported SDP answer from remote terminal")]
    InvalidSdpAnswer,
    /// Терминал выбрал кодек, который шлюз не мостит (нужен T06b-транскодер),
    /// либо ответил 488/606 на кодек-негоциацию. Явная структура вместо
    /// generic «invalid SDP» / молчаливого «connected без медиа».
    #[error("No mutually supported codec: codec={codec:?} payload={payload:?} peer_status={peer_status:?}")]
    NoCommonCodec { codec: Option<String>, payload: Option<u8>, peer_status: Option<u16> },
}

#[derive(Debug, Clone)]
pub struct ActiveSipSession {
    pub call_id: String,
    pub local_ip: String,
    pub local_sip_port: u16,
    pub target_addr: SocketAddr,
    pub contact_uri: String,
    pub from_tag: String,
    pub to_tag: String,
    pub remote_media_ip: String,
    pub remote_audio_port: Option<u16>,
    pub remote_video_port: Option<u16>,
    pub remote_audio_payload_type: u8,
    /// Negotiated audio codec name, independent of the wire payload number.
    pub remote_audio_codec: Option<&'static str>,
    pub remote_video_payload_type: Option<u8>,
    pub local_video_payload_type: Option<u8>,
    pub remote_video_ip: Option<String>,
    /// Negotiated video codec name ("H264"/"H265"), when video is bridged.
    pub remote_video_codec: Option<&'static str>,
    pub remote_dtmf_payload_type: Option<u8>,
    /// Local offer SDP sent in the initial INVITE.
    pub local_offer_sdp: String,
    /// Raw `200 OK` SDP body, kept verbatim for diagnostics and live-call evidence.
    pub answer_sdp: String,
    pub socket: Arc<UdpSocket>,
    pub keyframe_requests: Arc<std::sync::atomic::AtomicU64>,
    ended: Arc<AtomicBool>,
}
impl ActiveSipSession {
    pub fn is_ended(&self) -> bool {
        self.ended.load(Ordering::Acquire)
    }
    pub async fn hangup(&self) -> Result<(), std::io::Error> {
        if self.ended.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let bye = build_bye(
            &self.contact_uri,
            &self.local_ip,
            self.local_sip_port,
            &self.call_id,
            &self.from_tag,
            &self.to_tag,
            2,
        );
        self.socket.send_to(bye.as_bytes(), self.target_addr).await?;
        info!(call_id=%self.call_id,"SIP BYE sent");
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct SipUacClient {
    local_candidate_ip: String,
}
impl Default for SipUacClient {
    fn default() -> Self {
        Self::new("127.0.0.1")
    }
}

#[derive(Clone)]
struct InviteContext {
    socket: Arc<UdpSocket>,
    target: SocketAddr,
    uri: String,
    ip: String,
    port: u16,
    call_id: String,
    from_tag: String,
    branch: String,
}
/// Dropping a pending dial (including a stopped session) cancels the remote ring.
struct PendingInvite {
    context: InviteContext,
    armed: bool,
}
impl Drop for PendingInvite {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let context = self.context.clone();
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let cancel = build_cancel(
                    &context.uri,
                    &context.ip,
                    context.port,
                    &context.call_id,
                    &context.from_tag,
                    &context.branch,
                );
                let _ = context.socket.send_to(cancel.as_bytes(), context.target).await;
                let mut buf = [0; 16384];
                let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                while let Ok(Ok((len, source))) =
                    tokio::time::timeout_at(deadline, context.socket.recv_from(&mut buf)).await
                {
                    if source != context.target {
                        continue;
                    }
                    let text = String::from_utf8_lossy(&buf[..len]);
                    let Some(response) = SipResponse::parse(&text) else {
                        continue;
                    };
                    if response.call_id != Some(&context.call_id) || response.cseq_method != Some("INVITE") {
                        continue;
                    }
                    if response.status_code >= 200 {
                        let ack = build_ack(
                            &context.uri,
                            &context.ip,
                            context.port,
                            &context.call_id,
                            &context.from_tag,
                            response.to_tag,
                            &context.branch,
                            response.status_code < 300,
                        );
                        let _ = context.socket.send_to(ack.as_bytes(), context.target).await;
                        if response.status_code < 300 {
                            let bye = build_bye(
                                response.contact.unwrap_or(&context.uri),
                                &context.ip,
                                context.port,
                                &context.call_id,
                                &context.from_tag,
                                response.to_tag.unwrap_or(""),
                                2,
                            );
                            let _ = context.socket.send_to(bye.as_bytes(), context.target).await;
                        }
                        break;
                    }
                }
            });
        }
    }
}

impl SipUacClient {
    pub fn new(local_candidate_ip: impl Into<String>) -> Self {
        Self { local_candidate_ip: local_candidate_ip.into() }
    }
    fn parse_target_addr(raw: &str) -> Result<(String, SocketAddr), SipDialError> {
        let raw = raw.trim();
        if raw.starts_with("sips:")
            || raw.chars().any(|c| c == '\r' || c == '\n')
            || raw.contains("transport=tcp")
            || raw.contains("transport=tls")
        {
            return Err(SipDialError::InvalidUri(raw.into()));
        }
        let clean = raw.strip_prefix("sip:").unwrap_or(raw).split(';').next().unwrap_or(raw);
        let host = clean.split(';').next().unwrap_or(clean).rsplit('@').next().unwrap_or(clean);
        let endpoint = if host.contains(':') { host.to_string() } else { format!("{host}:5060") };
        Ok((clean.to_string(), endpoint.parse().map_err(|_| SipDialError::InvalidUri(raw.into()))?))
    }
    pub async fn dial(
        &self,
        target_uri: &str,
        sdp_offer: &str,
        timeout_dur: Duration,
    ) -> Result<ActiveSipSession, SipDialError> {
        let (uri, target) = Self::parse_target_addr(target_uri)?;
        let socket = Arc::new(UdpSocket::bind("0.0.0.0:0").await?);
        let context = InviteContext {
            port: socket.local_addr()?.port(),
            socket,
            target,
            uri,
            ip: self.local_candidate_ip.clone(),
            call_id: format!("call-out-{}", Uuid::new_v4()),
            from_tag: format!("tag-{}", Uuid::new_v4().simple()),
            branch: format!("br-{}", Uuid::new_v4().simple()),
        };
        let invite = build_invite(
            &context.uri,
            &context.ip,
            context.port,
            &context.call_id,
            &context.from_tag,
            &context.branch,
            sdp_offer,
        );
        context.socket.send_to(invite.as_bytes(), context.target).await?;
        let mut pending = PendingInvite { context: context.clone(), armed: true };
        let result = tokio::time::timeout(timeout_dur, Self::wait_for_answer(&context, &invite, sdp_offer)).await;
        match result {
            Ok(result) => {
                pending.armed = false;
                result
            }
            Err(_) => {
                warn!(target=%target,"SIP answer timeout; cancelling invite");
                Err(SipDialError::Timeout(target_uri.into(), timeout_dur))
            }
        }
    }
    async fn wait_for_answer(
        c: &InviteContext,
        invite: &str,
        sdp_offer: &str,
    ) -> Result<ActiveSipSession, SipDialError> {
        let mut buf = [0; 16384];
        let mut provisional = false;
        let mut retry = tokio::time::interval(Duration::from_millis(500));
        retry.tick().await;
        loop {
            let (len, source) = tokio::select! {
                _=retry.tick(),if !provisional=>{c.socket.send_to(invite.as_bytes(),c.target).await?;continue;},
                received=c.socket.recv_from(&mut buf)=>received?,
            };
            if source != c.target {
                continue;
            }
            let text = String::from_utf8_lossy(&buf[..len]);
            let Some(response) = SipResponse::parse(&text) else {
                continue;
            };
            if response.call_id != Some(&c.call_id) || response.cseq_num != 1 || response.cseq_method != Some("INVITE")
            {
                continue;
            }
            debug!(status=response.status_code,target=%source,"SIP INVITE response");
            match response.status_code {
                100..=199 => {
                    provisional = true;
                }
                200..=299 => {
                    let to_tag = response.to_tag.ok_or(SipDialError::InvalidSdpAnswer)?;
                    let contact = response.contact.unwrap_or(&c.uri);
                    // 2xx ACK targets Contact. Preserve original To identity in the dialog.
                    let contact_clean = contact.strip_prefix("sip:").unwrap_or(contact);
                    let mut ack =
                        build_ack(contact_clean, &c.ip, c.port, &c.call_id, &c.from_tag, Some(to_tag), &c.branch, true);
                    ack = ack.replace(&format!("To: <sip:{contact_clean}>"), &format!("To: <sip:{}>", c.uri));
                    c.socket.send_to(ack.as_bytes(), c.target).await?;
                    let media = match negotiate_media_detailed(sdp_offer, response.body) {
                        Ok(media) => media,
                        Err(failure) => {
                            // The peer selected a codec this gateway cannot bridge (or nothing
                            // common is left): refuse the call instead of reporting a connected
                            // session with dead media. As the offerer we can only ACK+BYE here
                            // (488 is a UAS-before-200 response, used by the inbound SIP UAS).
                            warn!(call_id=%c.call_id, ?failure, "SIP answer refused: no mutually bridged codec");
                            let bye = build_bye(contact, &c.ip, c.port, &c.call_id, &c.from_tag, to_tag, 2);
                            c.socket.send_to(bye.as_bytes(), c.target).await?;
                            let (codec, payload) = match failure {
                                crate::rtp_bridge::NegotiationFailure::UnsupportedAudio { codec, payload } => {
                                    (Some(codec), Some(payload))
                                }
                                _ => (None, None),
                            };
                            return Err(SipDialError::NoCommonCodec { codec, payload, peer_status: None });
                        }
                    };
                    let session = ActiveSipSession {
                        call_id: c.call_id.clone(),
                        local_ip: c.ip.clone(),
                        local_sip_port: c.port,
                        target_addr: c.target,
                        contact_uri: contact.into(),
                        from_tag: c.from_tag.clone(),
                        to_tag: to_tag.into(),
                        remote_media_ip: media.ip,
                        remote_audio_port: Some(media.audio_port),
                        remote_video_port: media.video_port,
                        remote_audio_payload_type: media.audio_payload_type,
                        remote_audio_codec: Some(media.audio_codec),
                        remote_video_payload_type: media.video_payload_type,
                        local_video_payload_type: media.local_video_payload_type,
                        remote_video_ip: media.video_ip,
                        remote_video_codec: media.video_codec,
                        remote_dtmf_payload_type: media.dtmf_payload_type,
                        local_offer_sdp: sdp_offer.to_string(),
                        answer_sdp: response.body.to_string(),
                        socket: c.socket.clone(),
                        keyframe_requests: Arc::new(std::sync::atomic::AtomicU64::new(0)),
                        ended: Arc::new(AtomicBool::new(false)),
                    };
                    Self::monitor_dialog(session.clone(), ack);
                    info!(call_id=%session.call_id,audio_port=?session.remote_audio_port,video_port=?session.remote_video_port,audio_codec=media.audio_codec,video_codec=?media.video_codec,"SIP dialog confirmed with usable media");
                    return Ok(session);
                }
                300..=699 => {
                    let ack =
                        build_ack(&c.uri, &c.ip, c.port, &c.call_id, &c.from_tag, response.to_tag, &c.branch, false);
                    c.socket.send_to(ack.as_bytes(), c.target).await?;
                    if matches!(response.status_code, 488 | 606) {
                        return Err(SipDialError::NoCommonCodec {
                            codec: None,
                            payload: None,
                            peer_status: Some(response.status_code),
                        });
                    }
                    return Err(SipDialError::Rejected(response.status_code, response.reason.into()));
                }
                _ => {}
            }
        }
    }
    fn monitor_dialog(session: ActiveSipSession, ack: String) {
        tokio::spawn(async move {
            let mut buf = [0; 16384];
            while !session.is_ended() {
                let received =
                    tokio::time::timeout(Duration::from_millis(250), session.socket.recv_from(&mut buf)).await;
                let Ok(Ok((len, source))) = received else {
                    continue;
                };
                if source != session.target_addr {
                    continue;
                }
                let text = String::from_utf8_lossy(&buf[..len]);
                if let Some(response) = SipResponse::parse(&text) {
                    if response.call_id == Some(&session.call_id)
                        && response.status_code < 300
                        && response.cseq_method == Some("INVITE")
                    {
                        let _ = session.socket.send_to(ack.as_bytes(), source).await;
                    }
                    continue;
                }
                let header = |name: &str| {
                    text.lines()
                        .skip(1)
                        .filter_map(|line| line.split_once(':'))
                        .find(|(n, _)| n.trim().eq_ignore_ascii_case(name))
                        .map(|(_, v)| v.trim())
                };
                if header("Call-ID") != Some(&session.call_id) {
                    continue;
                }
                if !header("From").is_some_and(|v| v.contains(&format!("tag={}", session.to_tag)))
                    || !header("To").is_some_and(|v| v.contains(&format!("tag={}", session.from_tag)))
                {
                    continue;
                }
                let method = text.split_whitespace().next().unwrap_or("");
                let (code, reason) = match method {
                    "BYE" | "OPTIONS" | "INVITE" | "INFO" => (200, "OK"),
                    _ => (501, "Not Implemented"),
                };
                let mut reply = format!("SIP/2.0 {code} {reason}\r\n");
                for name in ["Via", "From", "To", "Call-ID", "CSeq"] {
                    for (_, value) in text
                        .lines()
                        .skip(1)
                        .filter_map(|l| l.split_once(':'))
                        .filter(|(n, _)| n.trim().eq_ignore_ascii_case(name))
                    {
                        reply.push_str(&format!("{name}: {}\r\n", value.trim()));
                    }
                }
                if method == "OPTIONS" || method == "INFO" {
                    reply.push_str("Allow: INVITE, ACK, BYE, CANCEL, OPTIONS, INFO\r\n");
                }
                if method == "INVITE" && !session.local_offer_sdp.is_empty() {
                    reply.push_str(&format!(
                        "Contact: <sip:mcu@{}:{}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{}",
                        session.local_ip,
                        session.local_sip_port,
                        session.local_offer_sdp.len(),
                        session.local_offer_sdp
                    ));
                } else {
                    reply.push_str("Content-Length: 0\r\n\r\n");
                }
                let _ = session.socket.send_to(reply.as_bytes(), source).await;
                if method == "INFO" {
                    let is_keyframe = text.to_ascii_lowercase().contains("picture_fast_update")
                        || text.to_ascii_lowercase().contains("media_control");
                    if is_keyframe {
                        session.keyframe_requests.fetch_add(1, Ordering::Relaxed);
                        info!(call_id=%session.call_id, "SIP INFO picture_fast_update received in UAC dialog");
                    }
                }
                if method == "BYE" {
                    session.ended.store(true, Ordering::Release);
                    info!(call_id=%session.call_id,"Remote SIP BYE received and acknowledged");
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn captured_polycom_sdp_reaches_directional_active_session_fields() {
        let terminal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let address = terminal.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buffer = [0; 16384];
            let (count, source) = terminal.recv_from(&mut buffer).await.unwrap();
            let request = String::from_utf8_lossy(&buffer[..count]);
            let id = request.lines().find_map(|line| line.strip_prefix("Call-ID: ")).unwrap();
            let sdp = include_str!("../tests/fixtures/polycom_rpd_3_5_answer.sdp");
            let response=format!("SIP/2.0 200 OK\r\nTo: <sip:terminal>;tag=polycom-fixture\r\nCall-ID: {id}\r\nCSeq: 1 INVITE\r\nContact: <sip:terminal@{address}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",sdp.len());
            terminal.send_to(response.as_bytes(), source).await.unwrap();
            let (count, _) = terminal.recv_from(&mut buffer).await.unwrap();
            assert!(buffer[..count].starts_with(b"ACK "));
            let (count, _) =
                tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut buffer)).await.unwrap().unwrap();
            assert!(buffer[..count].starts_with(b"BYE "));
        });
        let offer = include_str!("../tests/fixtures/polycom_rpd_3_5_offer.sdp");
        let session = SipUacClient::new("127.0.0.1")
            .dial(&format!("sip:{address}"), offer, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(session.remote_audio_payload_type, 0);
        assert_eq!(session.remote_video_payload_type, Some(111), "High RX accepted, baseline TX kept");
        assert_eq!(session.local_video_payload_type, Some(96));
        assert_eq!(session.remote_video_ip.as_deref(), Some("192.168.1.132"));
        assert_eq!(session.remote_dtmf_payload_type, Some(119));
        session.hangup().await.unwrap();
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn remote_info_picture_fast_update_increments_keyframe_requests() {
        let terminal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = terminal.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buf = [0; 4096];
            let (size, source) = terminal.recv_from(&mut buf).await.unwrap();
            let invite = String::from_utf8_lossy(&buf[..size]);
            let id = invite.lines().find_map(|l| l.strip_prefix("Call-ID: ")).unwrap().to_string();
            let local_from = invite.lines().find_map(|l| l.strip_prefix("From: ")).unwrap().to_string();
            let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 45000 RTP/AVP 0\r\n";
            let answer = format!("SIP/2.0 200 OK\r\nFrom: {local_from}\r\nTo: <sip:terminal>;tag=remote-info\r\nCall-ID: {id}\r\nCSeq: 1 INVITE\r\nContact: <sip:terminal@{addr}>\r\nContent-Length: {}\r\n\r\n{sdp}", sdp.len());
            terminal.send_to(answer.as_bytes(), source).await.unwrap();
            let (size, _) = terminal.recv_from(&mut buf).await.unwrap();
            assert!(buf[..size].starts_with(b"ACK "));

            // Send SIP INFO with picture_fast_update
            let body = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\r\n<media_control>\r\n  <vc_primitive>\r\n    <to_encoder>\r\n      <picture_fast_update/>\r\n    </to_encoder>\r\n  </vc_primitive>\r\n</media_control>";
            let info = format!("INFO sip:vks@{source} SIP/2.0\r\nVia: SIP/2.0/UDP {addr};branch=z9hG4bKinfo\r\nFrom: <sip:terminal>;tag=remote-info\r\nTo: {local_from}\r\nCall-ID: {id}\r\nCSeq: 2 INFO\r\nContent-Type: application/media_control+xml\r\nContent-Length: {}\r\n\r\n{body}", body.len());
            terminal.send_to(info.as_bytes(), source).await.unwrap();

            let (size, _) =
                tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut buf)).await.unwrap().unwrap();
            let reply = String::from_utf8_lossy(&buf[..size]);
            assert!(reply.starts_with("SIP/2.0 200 OK"));
            assert!(reply.contains("CSeq: 2 INFO"));
        });
        let session = SipUacClient::new("127.0.0.1")
            .dial(
                &format!("sip:{addr}"),
                "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 40000 RTP/AVP 0\r\n",
                Duration::from_secs(2),
            )
            .await
            .unwrap();

        for _ in 0..20 {
            if session.keyframe_requests.load(Ordering::Relaxed) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(session.keyframe_requests.load(Ordering::Relaxed), 1);
        session.hangup().await.unwrap();
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn remote_bye_is_acknowledged_and_marks_dialog_ended() {
        let terminal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = terminal.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buf = [0; 4096];
            let (size, source) = terminal.recv_from(&mut buf).await.unwrap();
            let invite = String::from_utf8_lossy(&buf[..size]);
            let id = invite.lines().find_map(|l| l.strip_prefix("Call-ID: ")).unwrap().to_string();
            let local_from = invite.lines().find_map(|l| l.strip_prefix("From: ")).unwrap().to_string();
            let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 45000 RTP/AVP 0\r\n";
            let answer=format!("SIP/2.0 200 OK\r\nFrom: {local_from}\r\nTo: <sip:terminal>;tag=remote-bye\r\nCall-ID: {id}\r\nCSeq: 1 INVITE\r\nContact: <sip:terminal@{addr}>\r\nContent-Length: {}\r\n\r\n{sdp}",sdp.len());
            terminal.send_to(answer.as_bytes(), source).await.unwrap();
            let (size, _) = terminal.recv_from(&mut buf).await.unwrap();
            assert!(buf[..size].starts_with(b"ACK "));
            let bye=format!("BYE sip:vks@{source} SIP/2.0\r\nVia: SIP/2.0/UDP {addr};branch=z9hG4bKbye\r\nFrom: <sip:terminal>;tag=remote-bye\r\nTo: {local_from}\r\nCall-ID: {id}\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n");
            terminal.send_to(bye.as_bytes(), source).await.unwrap();
            let (size, _) =
                tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut buf)).await.unwrap().unwrap();
            assert!(buf[..size].starts_with(b"SIP/2.0 200 OK"));
        });
        let session = SipUacClient::new("127.0.0.1")
            .dial(
                &format!("sip:{addr}"),
                "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 41000 RTP/AVP 0\r\n",
                Duration::from_secs(2),
            )
            .await
            .unwrap();
        peer.await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            while !session.is_ended() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        session.hangup().await.unwrap();
    }

    #[tokio::test]
    async fn aborting_pending_dial_sends_cancel_instead_of_leaving_terminal_ringing() {
        let terminal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = terminal.local_addr().unwrap();
        let job = tokio::spawn(async move {
            SipUacClient::new("127.0.0.1")
                .dial(
                    &format!("sip:{addr}"),
                    "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 41000 RTP/AVP 0\r\n",
                    Duration::from_secs(60),
                )
                .await
        });
        let mut buf = [0; 4096];
        let (size, source) = terminal.recv_from(&mut buf).await.unwrap();
        assert!(buf[..size].starts_with(b"INVITE "));
        job.abort();
        let _ = job.await;
        let (size, _) =
            tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut buf)).await.unwrap().unwrap();
        assert!(buf[..size].starts_with(b"CANCEL "));
        assert_eq!(source.ip(), "127.0.0.1".parse::<std::net::IpAddr>().unwrap());
    }

    #[tokio::test]
    async fn unusable_sdp_must_not_create_connected_call() {
        let terminal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = terminal.local_addr().unwrap();
        let peer = tokio::spawn(async move {
            let mut buf = [0; 4096];
            let (size, source) = terminal.recv_from(&mut buf).await.unwrap();
            let invite = String::from_utf8_lossy(&buf[..size]);
            let id = invite.lines().find_map(|l| l.strip_prefix("Call-ID: ")).unwrap();
            let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 45000 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
            let response=format!("SIP/2.0 200 OK\r\nTo: <sip:terminal>;tag=remote\r\nCall-ID: {id}\r\nCSeq: 1 INVITE\r\nContact: <sip:terminal@{addr}>\r\nContent-Length: {}\r\n\r\n{sdp}",sdp.len());
            terminal.send_to(response.as_bytes(), source).await.unwrap();
            let (size, _) = terminal.recv_from(&mut buf).await.unwrap();
            assert!(buf[..size].starts_with(b"ACK "));
            // Unsupported answer must be torn down, not just ACKed.
            let (size, _) = tokio::time::timeout(Duration::from_secs(2), terminal.recv_from(&mut buf))
                .await
                .expect("BYE after ACK timeout")
                .unwrap();
            assert!(buf[..size].starts_with(b"BYE "));
        });
        let result = SipUacClient::new("127.0.0.1")
            .dial(
                &format!("sip:{addr}"),
                "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 41000 RTP/AVP 0 8\r\n",
                Duration::from_secs(2),
            )
            .await;
        match result {
            Err(SipDialError::NoCommonCodec { codec, payload, peer_status }) => {
                assert_eq!(codec, None, "G729 was never in our local offer");
                assert_eq!(payload, None);
                assert_eq!(peer_status, None);
            }
            other => panic!("An unsupported codec must not be shown as connected: {other:?}"),
        }
        peer.await.unwrap();
    }

    #[tokio::test]
    async fn test_uac_dial_and_hangup_mock_terminal() {
        // Запускаем mock терминал на 127.0.0.1
        let mock_server = UdpSocket::bind("127.0.0.1:0").await.expect("bind mock");
        let server_addr = mock_server.local_addr().expect("server addr");

        let client = SipUacClient::new("127.0.0.1");

        // Фоновый таск терминала: отвечает 100 Trying, затем 200 OK, и ожидает BYE
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (len, client_addr) = mock_server.recv_from(&mut buf).await.expect("recv invite");
            let req = String::from_utf8_lossy(&buf[..len]);
            assert!(req.starts_with("INVITE"));

            // 1. Отвечаем 100 Trying
            let trying = concat!(
                "SIP/2.0 100 Trying\r\n",
                "Via: SIP/2.0/UDP 127.0.0.1;branch=z9hG4bKtest\r\n",
                "From: <sip:vks@127.0.0.1>;tag=f123\r\n",
                "To: <sip:peer>\r\n",
                "Call-ID: call-test\r\n",
                "CSeq: 1 INVITE\r\n",
                "Content-Length: 0\r\n\r\n"
            );
            mock_server.send_to(trying.as_bytes(), client_addr).await.expect("send trying");

            // Извлекаем Call-ID из инвайта
            let call_id = req
                .lines()
                .find(|l| l.to_lowercase().starts_with("call-id:"))
                .unwrap()
                .split(':')
                .nth(1)
                .unwrap()
                .trim();

            // 2. Отвечаем 200 OK со SDP
            let sdp = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 45000 RTP/AVP 0\r\n";
            let ok_200 = format!(
                "SIP/2.0 200 OK\r\n\
Via: SIP/2.0/UDP 127.0.0.1;branch=z9hG4bKtest\r\n\
From: <sip:vks@127.0.0.1>;tag=f123\r\n\
To: <sip:peer>;tag=term-tag-1\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 INVITE\r\n\
Contact: <sip:term@127.0.0.1:{}>\r\n\
Content-Type: application/sdp\r\n\
Content-Length: {}\r\n\r\n\
{sdp}",
                server_addr.port(),
                sdp.len()
            );
            mock_server.send_to(ok_200.as_bytes(), client_addr).await.expect("send 200 ok");

            // 3. Получаем ACK
            let (len_ack, _) = mock_server.recv_from(&mut buf).await.expect("recv ack");
            let ack_str = String::from_utf8_lossy(&buf[..len_ack]);
            assert!(ack_str.starts_with("ACK"));

            // 4. Получаем BYE
            let (len_bye, _) = mock_server.recv_from(&mut buf).await.expect("recv bye");
            let bye_str = String::from_utf8_lossy(&buf[..len_bye]);
            assert!(bye_str.starts_with("BYE"));
        });

        let target_uri = format!("sip:{}", server_addr);
        let sdp_offer = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 41000 RTP/AVP 0\r\n";
        let session = client.dial(&target_uri, sdp_offer, Duration::from_secs(3)).await.expect("dial successful");

        assert_eq!(session.to_tag, "term-tag-1");
        assert_eq!(session.remote_audio_port, Some(45000));
        assert_eq!(session.remote_media_ip, "127.0.0.1");

        // Проверяем hangup
        session.hangup().await.expect("hangup successful");
    }

    #[tokio::test]
    async fn test_uac_dial_rejected_486_busy() {
        let mock_server = UdpSocket::bind("127.0.0.1:0").await.expect("bind mock");
        let server_addr = mock_server.local_addr().expect("server addr");

        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (len, client_addr) = mock_server.recv_from(&mut buf).await.expect("recv invite");
            let req = String::from_utf8_lossy(&buf[..len]);
            let call_id = req
                .lines()
                .find(|l| l.to_lowercase().starts_with("call-id:"))
                .unwrap()
                .split(':')
                .nth(1)
                .unwrap()
                .trim();

            let busy = format!(
                "SIP/2.0 486 Busy Here\r\n\
Via: SIP/2.0/UDP 127.0.0.1;branch=z9hG4bKtest\r\n\
From: <sip:vks@127.0.0.1>;tag=f123\r\n\
To: <sip:peer>;tag=b-tag\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 INVITE\r\n\
Content-Length: 0\r\n\r\n"
            );
            mock_server.send_to(busy.as_bytes(), client_addr).await.expect("send 486");
        });

        let client = SipUacClient::new("127.0.0.1");
        let res = client.dial(&format!("sip:{server_addr}"), "v=0\r\n", Duration::from_secs(2)).await;

        match res {
            Err(SipDialError::Rejected(486, _)) => {}
            other => panic!("expected 486 rejection, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn peer_488_maps_to_typed_no_common_codec() {
        let mock_server = UdpSocket::bind("127.0.0.1:0").await.expect("bind mock");
        let server_addr = mock_server.local_addr().expect("server addr");
        tokio::spawn(async move {
            let mut buf = [0u8; 2048];
            let (len, client_addr) = mock_server.recv_from(&mut buf).await.expect("recv invite");
            let req = String::from_utf8_lossy(&buf[..len]);
            let call_id = req
                .lines()
                .find(|l| l.to_lowercase().starts_with("call-id:"))
                .unwrap()
                .split(':')
                .nth(1)
                .unwrap()
                .trim();
            let response = format!(
                "SIP/2.0 488 Not Acceptable Here\r\n\
Via: SIP/2.0/UDP 127.0.0.1;branch=z9hG4bKtest\r\n\
From: <sip:vks@127.0.0.1>;tag=f123\r\n\
To: <sip:peer>;tag=remote\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 INVITE\r\n\
Content-Length: 0\r\n\r\n"
            );
            mock_server.send_to(response.as_bytes(), client_addr).await.expect("send 488");
        });
        let result =
            SipUacClient::new("127.0.0.1").dial(&format!("sip:{server_addr}"), "v=0\r\n", Duration::from_secs(2)).await;
        match result {
            Err(SipDialError::NoCommonCodec { peer_status, codec, payload }) => {
                assert_eq!(peer_status, Some(488));
                assert!(codec.is_none() && payload.is_none());
            }
            other => panic!("expected typed no-common-codec for 488, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_target_addr_varieties() {
        let (c1, a1) = SipUacClient::parse_target_addr("sip:192.168.1.132:5060").unwrap();
        assert_eq!(c1, "192.168.1.132:5060");
        assert_eq!(a1.to_string(), "192.168.1.132:5060");

        let (c2, a2) = SipUacClient::parse_target_addr("192.168.1.132").unwrap();
        assert_eq!(c2, "192.168.1.132");
        assert_eq!(a2.to_string(), "192.168.1.132:5060");

        let (c3, a3) = SipUacClient::parse_target_addr("sip:box@192.168.1.132").unwrap();
        assert_eq!(c3, "box@192.168.1.132");
        assert_eq!(a3.to_string(), "192.168.1.132:5060");

        let (c4, a4) = SipUacClient::parse_target_addr("sip:room101@192.168.1.132:5080;transport=udp").unwrap();
        assert_eq!(c4, "room101@192.168.1.132:5080");
        assert_eq!(a4.to_string(), "192.168.1.132:5080");
    }

    #[tokio::test]
    async fn test_uac_in_dialog_reinvite_returns_local_offer_sdp() {
        let mock_server = UdpSocket::bind("127.0.0.1:0").await.expect("bind mock");
        let server_addr = mock_server.local_addr().expect("server addr");
        let client = SipUacClient::new("127.0.0.1");

        let peer = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            let (len, client_addr) = mock_server.recv_from(&mut buf).await.expect("recv invite");
            let req = String::from_utf8_lossy(&buf[..len]);
            let call_id = req.lines().find_map(|l| l.strip_prefix("Call-ID: ")).unwrap().to_string();
            let from = req.lines().find_map(|l| l.strip_prefix("From: ")).unwrap().to_string();
            let from_tag = from.split("tag=").nth(1).unwrap().to_string();

            let sdp = "v=0\r\nc=IN IP4 192.168.1.95\r\nm=audio 50000 RTP/AVP 0\r\n";
            let ok_200 = format!(
                "SIP/2.0 200 OK\r\n\
Via: SIP/2.0/UDP 127.0.0.1;branch=z9hG4bKtest\r\n\
From: {from}\r\n\
To: <sip:peer>;tag=term-tag-reinvite\r\n\
Call-ID: {call_id}\r\n\
CSeq: 1 INVITE\r\n\
Contact: <sip:term@127.0.0.1:{}>\r\n\
Content-Type: application/sdp\r\n\
Content-Length: {}\r\n\r\n\
{sdp}",
                server_addr.port(),
                sdp.len()
            );
            mock_server.send_to(ok_200.as_bytes(), client_addr).await.expect("send 200 ok");

            let (len_ack, _) = mock_server.recv_from(&mut buf).await.expect("recv ack");
            let ack_str = String::from_utf8_lossy(&buf[..len_ack]);
            assert!(ack_str.starts_with("ACK"));

            // Remote sends in-dialog re-INVITE (e.g. starting presentation)
            let reinvite = format!(
                "INVITE sip:mcu@127.0.0.1 SIP/2.0\r\n\
Via: SIP/2.0/UDP 127.0.0.1:{};branch=z9hG4bKreinvite1\r\n\
From: <sip:peer>;tag=term-tag-reinvite\r\n\
To: <sip:vks@127.0.0.1>;tag={from_tag}\r\n\
Call-ID: {call_id}\r\n\
CSeq: 2 INVITE\r\n\
Contact: <sip:term@127.0.0.1:{}>\r\n\
Content-Type: application/sdp\r\n\
Content-Length: {}\r\n\r\n\
{sdp}",
                server_addr.port(),
                server_addr.port(),
                sdp.len()
            );
            mock_server.send_to(reinvite.as_bytes(), client_addr).await.expect("send reinvite");

            let (len_resp, _) = mock_server.recv_from(&mut buf).await.expect("recv reinvite response");
            let resp_str = String::from_utf8_lossy(&buf[..len_resp]);
            assert!(resp_str.starts_with("SIP/2.0 200 OK"), "must respond 200 OK to re-INVITE");
            assert!(resp_str.contains("m=audio 41000"), "must respond with local offer SDP, not peer SDP");
            assert!(!resp_str.contains("192.168.1.95"), "must not reflect peer IP");
        });

        let target_uri = format!("sip:{}", server_addr);
        let sdp_offer = "v=0\r\nc=IN IP4 127.0.0.1\r\nm=audio 41000 RTP/AVP 0\r\n";
        let session = client.dial(&target_uri, sdp_offer, Duration::from_secs(3)).await.expect("dial successful");

        peer.await.expect("peer task finished");
        session.hangup().await.expect("hangup successful");
    }
}

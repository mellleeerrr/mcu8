//! Async Outbound SIP UAC Client (RFC 3261) for dialing hardware terminals.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::rtp_bridge::parse_remote_media_ports;
use crate::sip_message::{build_ack, build_bye, build_invite, SipResponse};

/// Ошибки исходящего дозвона.
#[derive(Debug, thiserror::Error)]
pub enum SipDialError {
    #[error("Invalid target URI: {0}")]
    InvalidUri(String),
    #[error("Socket IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Call rejected by remote terminal: {0} {1}")]
    Rejected(u16, String),
    #[error("Dial timeout (no response from {0} within {1:?})")]
    Timeout(String, Duration),
    #[error("Invalid or missing SDP answer from remote terminal")]
    InvalidSdpAnswer,
}

/// Активный исходящий SIP диалог.
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
    pub socket: Arc<UdpSocket>,
}

impl ActiveSipSession {
    /// Отправляет запрос BYE удаленному терминалу для завершения сессии.
    pub async fn hangup(&self) -> Result<(), std::io::Error> {
        let bye = build_bye(
            &self.contact_uri,
            &self.local_ip,
            self.local_sip_port,
            &self.call_id,
            &self.from_tag,
            &self.to_tag,
            2,
        );
        let _ = self.socket.send_to(bye.as_bytes(), self.target_addr).await?;
        info!(call_id = %self.call_id, target = %self.target_addr, "SIP BYE sent successfully");
        Ok(())
    }
}

/// Клиент исходящего дозвона SIP UAC.
#[derive(Debug, Clone)]
pub struct SipUacClient {
    local_candidate_ip: String,
}

impl Default for SipUacClient {
    fn default() -> Self {
        Self::new("127.0.0.1")
    }
}

impl SipUacClient {
    pub fn new(local_candidate_ip: impl Into<String>) -> Self {
        Self { local_candidate_ip: local_candidate_ip.into() }
    }

    /// Парсит целевой адрес (e.g. "sip:192.168.1.132:5060" -> "192.168.1.132:5060" или "sip:box@192.168.1.132").
    fn parse_target_addr(raw: &str) -> Result<(String, SocketAddr), SipDialError> {
        let clean = raw.trim().trim_start_matches("sip:").trim_start_matches("sips:");
        let clean = clean.split(';').next().unwrap_or(clean).trim();
        let host_part = clean.split('@').next_back().unwrap_or(clean);
        let target_str = if host_part.contains(':') { host_part.to_string() } else { format!("{host_part}:5060") };

        let addr = target_str.parse::<SocketAddr>().map_err(|_| SipDialError::InvalidUri(raw.to_string()))?;
        Ok((clean.to_string(), addr))
    }

    /// Инициирует исходящий вызов (RFC 3261 INVITE) на указанный адрес терминала.
    pub async fn dial(
        &self,
        target_uri: &str,
        sdp_offer: &str,
        timeout_dur: Duration,
    ) -> Result<ActiveSipSession, SipDialError> {
        let (target_clean, target_addr) = Self::parse_target_addr(target_uri)?;

        // Биндим сокет UAC на эфемерный порт
        let socket = UdpSocket::bind("0.0.0.0:0").await?;
        let local_sip_port = socket.local_addr()?.port();
        let local_ip = self.local_candidate_ip.clone();

        let call_id = format!("call-out-{}", Uuid::new_v4());
        let from_tag = format!("tag-{}", &Uuid::new_v4().simple().to_string()[..8]);
        let branch = format!("br-{}", &Uuid::new_v4().simple().to_string()[..12]);

        let invite_msg =
            build_invite(&target_clean, &local_ip, local_sip_port, &call_id, &from_tag, &branch, sdp_offer);

        debug!(target = %target_addr, call_id = %call_id, "Sending SIP INVITE to terminal");
        socket.send_to(invite_msg.as_bytes(), target_addr).await?;

        // Цикл обработки ответов с таймером
        let arc_socket = Arc::new(socket);
        let dial_fut = Self::wait_for_answer(
            arc_socket.clone(),
            target_addr,
            &target_clean,
            &local_ip,
            local_sip_port,
            &call_id,
            &from_tag,
            &branch,
        );

        match tokio::time::timeout(timeout_dur, dial_fut).await {
            Ok(res) => res,
            Err(_) => {
                warn!(target = %target_addr, "SIP dial timed out after {:?}", timeout_dur);
                Err(SipDialError::Timeout(target_uri.to_string(), timeout_dur))
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn wait_for_answer(
        socket: Arc<UdpSocket>,
        target_addr: SocketAddr,
        target_clean: &str,
        local_ip: &str,
        local_sip_port: u16,
        call_id: &str,
        from_tag: &str,
        branch: &str,
    ) -> Result<ActiveSipSession, SipDialError> {
        let mut buf = [0u8; 4096];

        loop {
            let (len, src) = match socket.recv_from(&mut buf).await {
                Ok(res) => res,
                Err(e) => {
                    // Windows WSAECONNRESET (10054) при ICMP port unreachable
                    debug!(error = %e, "UDP recv_from transient error; waiting for valid response");
                    continue;
                }
            };

            let text = String::from_utf8_lossy(&buf[..len]);
            let Some(resp) = SipResponse::parse(&text) else {
                continue;
            };

            if resp.call_id != Some(call_id) {
                continue;
            }

            debug!(status = resp.status_code, reason = resp.reason, from = %src, "Received SIP response");

            match resp.status_code {
                100..=199 => {
                    // 100 Trying, 180 Ringing, 183 Session Progress — продолжаем ожидать 200 OK
                    continue;
                }
                200 => {
                    // 200 OK — отправляем End-to-End Dialog ACK
                    let to_tag = resp.to_tag.unwrap_or("default-tag");
                    let contact = resp.contact.unwrap_or(target_clean);

                    let ack = build_ack(
                        target_clean,
                        local_ip,
                        local_sip_port,
                        call_id,
                        from_tag,
                        Some(to_tag),
                        branch,
                        true,
                    );
                    let _ = socket.send_to(ack.as_bytes(), target_addr).await;

                    // Разбираем remote media порты из SDP
                    let (remote_ip, remote_audio, remote_video) = parse_remote_media_ports(resp.body);
                    let remote_media_ip = remote_ip.unwrap_or_else(|| target_addr.ip().to_string());

                    info!(
                        call_id = %call_id,
                        target = %target_addr,
                        media_ip = %remote_media_ip,
                        audio_port = ?remote_audio,
                        video_port = ?remote_video,
                        "SIP 200 OK received and ACKed; call confirmed"
                    );

                    return Ok(ActiveSipSession {
                        call_id: call_id.to_string(),
                        local_ip: local_ip.to_string(),
                        local_sip_port,
                        target_addr,
                        contact_uri: contact.to_string(),
                        from_tag: from_tag.to_string(),
                        to_tag: to_tag.to_string(),
                        remote_media_ip,
                        remote_audio_port: remote_audio,
                        remote_video_port: remote_video,
                        socket,
                    });
                }
                300..=699 => {
                    // Ошибка вызова (486 Busy, 404 Not Found, 603 Decline) — отправляем Hop-by-Hop ACK
                    let ack = build_ack(
                        target_clean,
                        local_ip,
                        local_sip_port,
                        call_id,
                        from_tag,
                        resp.to_tag,
                        branch,
                        false,
                    );
                    let _ = socket.send_to(ack.as_bytes(), target_addr).await;
                    return Err(SipDialError::Rejected(resp.status_code, resp.reason.to_string()));
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
}

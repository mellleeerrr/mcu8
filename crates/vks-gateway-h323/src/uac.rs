//! Outbound H.323 UAC client connecting to TCP port 1720 with Fast-Start.

use std::net::SocketAddr;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tracing::info;
use vks_common::Error;

use crate::q931::{
    build_q931_release_complete, build_q931_setup, parse_q931_message, MSG_ALERTING, MSG_CONNECT, MSG_RELEASE_COMPLETE,
};

use std::sync::Arc;
use tokio::sync::Mutex;

/// Active outbound H.323 session.
#[derive(Clone)]
pub struct ActiveH323Session {
    pub call_id: String,
    pub call_ref: u16,
    pub remote_ip: String,
    pub remote_audio_port: Option<u16>,
    pub tcp_stream: Arc<Mutex<Option<TcpStream>>>,
}

impl ActiveH323Session {
    pub async fn hangup(&self) -> Result<(), Error> {
        let mut guard = self.tcp_stream.lock().await;
        if let Some(mut stream) = guard.take() {
            let rel = build_q931_release_complete(self.call_ref);
            let _ = stream.write_all(&rel).await;
            let _ = stream.shutdown().await;
            info!(call_id = %self.call_id, "H.323 session hung up with RELEASE COMPLETE");
        }
        Ok(())
    }
}

pub struct H323UacClient {
    pub local_ip: String,
}

impl H323UacClient {
    pub fn new(local_ip: &str) -> Self {
        Self { local_ip: local_ip.to_string() }
    }

    pub async fn dial(
        &self,
        target_uri: &str,
        local_rtp_port: u16,
        dial_timeout: Duration,
    ) -> Result<ActiveH323Session, Error> {
        let dest = target_uri
            .trim_start_matches("h323:")
            .trim_start_matches("//")
            .split('@')
            .next_back()
            .unwrap_or(target_uri);

        let target_addr: SocketAddr = if dest.contains(':') {
            dest.parse().map_err(|e| Error::InvalidInput(format!("Invalid H.323 target address: {e}")))?
        } else {
            format!("{dest}:1720").parse().map_err(|e| Error::InvalidInput(format!("Invalid H.323 target IP: {e}")))?
        };

        info!(target = %target_addr, "Connecting to H.323 endpoint via TCP 1720");

        let mut stream = timeout(dial_timeout, TcpStream::connect(target_addr))
            .await
            .map_err(|_| Error::Internal(anyhow::anyhow!("H.323 TCP connection timed out")))?
            .map_err(|e| Error::Internal(anyhow::anyhow!("Failed to connect to H.323 endpoint: {e}")))?;

        let local_ipv4: std::net::Ipv4Addr = self.local_ip.parse().unwrap_or(std::net::Ipv4Addr::new(127, 0, 0, 1));
        let call_ref = (uuid::Uuid::new_v4().as_u128() & 0x7fff) as u16;
        let call_id = format!("h323-call-{}", uuid::Uuid::new_v4());

        let setup_pkt = build_q931_setup(call_ref, "vks-mcu", dest, local_ipv4, local_rtp_port);
        stream.write_all(&setup_pkt).await.map_err(|e| Error::Internal(anyhow::anyhow!("Q.931 write error: {e}")))?;

        let mut buf = [0u8; 4096];
        let mut remote_port = None;

        // Wait for ALERTING or CONNECT
        let deadline = tokio::time::Instant::now() + dial_timeout;
        while tokio::time::Instant::now() < deadline {
            let read_fut = stream.read(&mut buf);
            let n = match timeout(Duration::from_millis(1500), read_fut).await {
                Ok(Ok(n)) if n > 0 => n,
                Ok(Ok(_)) => break, // EOF
                Ok(Err(e)) => return Err(Error::Internal(anyhow::anyhow!("Q.931 read error: {e}"))),
                Err(_) => {
                    if tokio::time::Instant::now() >= deadline {
                        break;
                    }
                    continue;
                }
            };

            if let Ok(msg) = parse_q931_message(&buf[..n]) {
                info!(msg_type = msg.message_type, "Received Q.931 response from H.323 endpoint");
                if let Some(port) = msg.remote_media_port {
                    remote_port = Some(port);
                }

                if msg.message_type == MSG_CONNECT || (msg.message_type == MSG_ALERTING && remote_port.is_some()) {
                    break;
                } else if msg.message_type == MSG_RELEASE_COMPLETE {
                    return Err(Error::Conflict("Call rejected by remote H.323 terminal (RELEASE COMPLETE)".into()));
                }
            }
        }

        let final_port = remote_port.unwrap_or(5004);
        Ok(ActiveH323Session {
            call_id,
            call_ref,
            remote_ip: target_addr.ip().to_string(),
            remote_audio_port: Some(final_port),
            tcp_stream: Arc::new(Mutex::new(Some(stream))),
        })
    }
}

//! Live SIP dial against a physical terminal, exercising the full codec matrix.
//!
//! Manual run (network access + free terminal required):
//!
//! ```text
//! VKS_LIVE_SIP=192.168.1.132 VKS_LIVE_LOCAL_IP=192.168.1.93 \
//!   cargo test -p vks-gateway-sip --test live_dial -- --ignored --nocapture
//! ```
//!
//! The test binds the RTP ports it advertises, so the terminal really sends
//! media to it: the number of received audio/video packets is the evidence that
//! the negotiated m-lines are live, not just present in SDP.

use std::time::Duration;

use tokio::net::UdpSocket;
use vks_gateway_sip::{build_plain_sdp, SipUacClient};

/// Bind an even RTP port and its odd RTCP neighbour.
async fn bind_rtp_pair() -> (UdpSocket, u16, UdpSocket) {
    loop {
        let rtp = UdpSocket::bind("0.0.0.0:0").await.expect("bind RTP socket");
        let port = rtp.local_addr().expect("local addr").port();
        if port == 0 || port % 2 != 0 || port == u16::MAX {
            continue;
        }
        if let Ok(rtcp) = UdpSocket::bind(("0.0.0.0", port + 1)).await {
            return (rtp, port, rtcp);
        }
    }
}

fn codec_name(payload: u8) -> &'static str {
    match payload {
        0 => "PCMU",
        8 => "PCMA",
        _ => "?",
    }
}

#[tokio::test]
#[ignore = "live call to a physical SIP terminal; requires network access and a free terminal"]
async fn live_dial_full_codec_matrix() {
    let target = std::env::var("VKS_LIVE_SIP").unwrap_or_else(|_| "192.168.1.132".to_string());
    let local_ip = std::env::var("VKS_LIVE_LOCAL_IP").unwrap_or_else(|_| "192.168.1.93".to_string());
    let require_video = std::env::var("VKS_LIVE_REQUIRE_VIDEO").is_ok_and(|value| value == "1");

    let (audio_rtp, audio_port, _audio_rtcp) = bind_rtp_pair().await;
    let (video_rtp, video_port, _video_rtcp) = bind_rtp_pair().await;
    let offer = build_plain_sdp(&local_ip, audio_port, Some(video_port));
    println!("===== INVITE SDP (full matrix, ports {audio_port}/{video_port}) =====\n{offer}");

    let session = SipUacClient::new(&local_ip)
        .dial(&format!("sip:{target}"), &offer, Duration::from_secs(30))
        .await
        .expect("SIP dial must be answered by the terminal");

    println!("===== 200 OK SDP (verbatim) =====\n{}", session.answer_sdp);
    println!(
        "===== NEGOTIATED =====\n audio: pt={} ({}) port={} peer={}\n video: tx_pt={:?} rx_pt={:?} port={:?} peer={:?}\n dtmf: {:?}",
        session.remote_audio_payload_type,
        codec_name(session.remote_audio_payload_type),
        session.remote_audio_port.unwrap_or(0),
        session.remote_media_ip,
        session.remote_video_payload_type,
        session.local_video_payload_type,
        session.remote_video_port,
        session.remote_video_ip,
        session.remote_dtmf_payload_type,
    );

    let mut audio_packets = 0u32;
    let mut video_packets = 0u32;
    let mut audio_bytes = 0u64;
    let mut video_bytes = 0u64;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut audio_buffer = [0u8; 2048];
    let mut video_buffer = [0u8; 2048];
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            result = audio_rtp.recv_from(&mut audio_buffer) => {
                if let Ok((len, source)) = result {
                    audio_packets += 1;
                    audio_bytes += len as u64;
                    if audio_packets == 1 {
                        println!("first audio RTP from {source}: {len} bytes, pt={}", audio_buffer[1] & 0x7f);
                    }
                }
            }
            result = video_rtp.recv_from(&mut video_buffer) => {
                if let Ok((len, source)) = result {
                    video_packets += 1;
                    video_bytes += len as u64;
                    if video_packets == 1 {
                        println!("first video RTP from {source}: {len} bytes, pt={}", video_buffer[1] & 0x7f);
                    }
                }
            }
        }
    }
    println!("===== LIVE MEDIA (8 s) =====\n audio: {audio_packets} packets / {audio_bytes} bytes\n video: {video_packets} packets / {video_bytes} bytes (marker={})", video_packets > 0);

    session.hangup().await.expect("BYE");
    assert!(audio_packets > 0, "terminal accepted but sent no audio RTP: media is not actually flowing");
    if require_video {
        assert!(video_packets > 0, "VKS_LIVE_REQUIRE_VIDEO=1 but no video RTP arrived");
    }
}

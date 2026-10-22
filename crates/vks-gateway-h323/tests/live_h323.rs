//! Live H.323 connect against a physical terminal (manual, `#[ignore]`).
//!
//! ```text
//! VKS_LIVE_H323=192.168.1.132 VKS_LIVE_LOCAL_IP=192.168.1.93 \
//!   cargo test -p vks-gateway-h323 --test live_h323 -- --ignored --nocapture
//! ```
//!
//! Proves the Rust ↔ Python sidecar path end to end: H.225 CONNECT, H.245
//! master/slave + TCS + OLC for audio (and video when the peer opens it), then
//! real inbound RTP counted on the ports the sidecar advertised.

use std::time::Duration;

use tokio::net::UdpSocket;
use vks_gateway_h323::{H323UacClient, H323VideoProfile};

#[tokio::test]
#[ignore = "live H.323 call to a physical terminal; requires the sidecar venv and network access"]
async fn live_h323_connect() {
    let target = std::env::var("VKS_LIVE_H323").unwrap_or_else(|_| "192.168.1.132".to_string());
    let local_ip = std::env::var("VKS_LIVE_LOCAL_IP").unwrap_or_else(|_| "192.168.1.93".to_string());

    let audio = UdpSocket::bind("0.0.0.0:0").await.expect("bind H.323 audio RTP");
    let audio_port = audio.local_addr().expect("audio addr").port();
    let video = UdpSocket::bind("0.0.0.0:0").await.expect("bind H.323 video RTP");
    let video_port = video.local_addr().expect("video addr").port();

    let profile = H323VideoProfile {
        profile_bits: 64,
        max_bitrate_kbps: std::env::var("VKS_LIVE_H323_BITRATE_KBPS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(512),
        level_idc: std::env::var("VKS_LIVE_H323_LEVEL_IDC").ok().and_then(|value| value.parse().ok()).unwrap_or(31),
    };
    println!("H.323 requested H.241 video profile: {profile:?}");
    let session = H323UacClient::new(&local_ip)
        .dial_with_media_profile(
            &format!("h323:{target}"),
            audio_port,
            Some(video_port),
            Duration::from_secs(45),
            profile,
        )
        .await
        .expect("H.323 CONNECT with H.245 media channels");

    println!(
        "===== H.323 CONNECTED =====\n call_id={} call_ref={}\n peer media: audio {}:{} pt={} (local pt={})\n peer media: video {:?}:{:?} pt={:?} (local pt={:?})",
        session.call_id,
        session.call_ref,
        session.remote_ip,
        session.remote_audio_port.unwrap_or(0),
        session.remote_audio_payload_type,
        session.local_audio_payload_type,
        session.remote_video_ip,
        session.remote_video_port,
        session.remote_video_payload_type,
        session.local_video_payload_type,
    );

    if std::env::var("VKS_LIVE_EXPECT_G722").is_ok_and(|value| value == "1") {
        assert_eq!(
            (session.remote_audio_payload_type, session.local_audio_payload_type),
            (9, 9),
            "live H.323 peer did not select symmetric G.722/PT9; got remote={} local={}",
            session.remote_audio_payload_type,
            session.local_audio_payload_type
        );
    }

    let mut audio_packets = 0u32;
    let mut video_packets = 0u32;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut audio_buffer = [0u8; 2048];
    let mut video_buffer = [0u8; 2048];
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            result = audio.recv_from(&mut audio_buffer) => {
                if let Ok((len, source)) = result {
                    audio_packets += 1;
                    if audio_packets == 1 {
                        let pt = audio_buffer[1] & 0x7f;
                        println!("first H.323 audio RTP from {source}: {len} bytes, pt={pt}");
                        if std::env::var("VKS_LIVE_EXPECT_G722").is_ok_and(|value| value == "1") {
                            assert_eq!(pt, 9, "expected G.722 RTP PT9");
                            assert_eq!(len, 12 + 160, "expected one 20 ms G.722 frame");
                        }
                    }
                }
            }
            result = video.recv_from(&mut video_buffer) => {
                if let Ok((len, source)) = result {
                    video_packets += 1;
                    if video_packets == 1 {
                        println!("first H.323 video RTP from {source}: {len} bytes, pt={}", video_buffer[1] & 0x7f);
                    }
                }
            }
        }
    }
    println!("===== LIVE MEDIA (8 s, H.323) =====\n audio: {audio_packets} packets\n video: {video_packets} packets");

    session.hangup().await.expect("H.323 release");
    assert!(audio_packets > 0, "H.323 call connected but no audio RTP arrived");
}

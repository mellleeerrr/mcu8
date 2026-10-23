//! Real-x265 -> RFC 7798 payload -> depacketizer roundtrip.
//!
//! The payload layer has unit vectors; this test proves the exact NALs the
//! codec worker emits (AUD, VPS/SPS/PPS, IRAP) survive packetization and
//! reassembly without byte changes.

use vks_gateway_sip::h265::{
    is_irap, is_parameter_set, nal_type, packetize_h265, strip_start_code, H265Depacketizer, H265_RTP_MTU,
};
use vks_media_mcu::codec_worker::{FfmpegWorker, SlotConfig, VideoCodec, WorkerConfig, WorkerMode};

fn ffmpeg_available() -> bool {
    std::process::Command::new(std::env::var("VKS_FFMPEG_BIN").unwrap_or_else(|_| "ffmpeg".into()))
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[tokio::test]
async fn x265_annexb_survives_rfc7798_packetization() {
    if !ffmpeg_available() {
        eprintln!("skipping: ffmpeg not installed");
        return;
    }
    let slot =
        SlotConfig { bitrate_bps: 500_000, width: 320, height: 180, fps: 15, audio_bitrate_bps: 64_000, max_queue: 4 };
    let mut encoder = FfmpegWorker::spawn(WorkerMode::VideoEncode(VideoCodec::H265), slot, WorkerConfig::default())
        .await
        .expect("h265 worker");
    let mut frame = vec![0u8; 320 * 180 * 3 / 2];
    for (index, byte) in frame.iter_mut().enumerate() {
        *byte = (index % 251) as u8;
    }
    encoder.write_video(&frame).await.expect("write frame");
    encoder.finish_input().await;
    let mut raw_nals = Vec::new();
    while let Some(nal) = tokio::time::timeout(std::time::Duration::from_secs(10), encoder.read_encoded_video())
        .await
        .expect("read timeout")
        .expect("read error")
    {
        raw_nals.push(nal);
    }
    assert!(!raw_nals.is_empty(), "x265 produced no NAL");
    let nals: Vec<&[u8]> = raw_nals.iter().map(|nal| strip_start_code(nal)).collect();
    let types: Vec<u8> = nals.iter().filter_map(|nal| nal_type(nal)).collect();
    assert!(types.contains(&35), "AUD missing after aud=1: {types:?}");
    for required in [32, 33, 34] {
        assert!(types.contains(&required), "parameter set {required} missing: {types:?}");
    }
    assert!(nals.iter().any(|nal| is_irap(nal)), "no IRAP: {types:?}");
    assert!(nals.iter().any(|nal| is_parameter_set(nal)));

    let mut sequence = 1;
    let packets = packetize_h265(&nals, 99, 0x0102_0304, &mut sequence, 90_000).expect("packetize");
    assert!(packets.iter().all(|packet| packet.len() <= H265_RTP_MTU));
    assert!(packets[0][1] & 0x80 == 0, "marker must not be on the first packet");
    assert!(packets.last().expect("packets")[1] & 0x80 != 0, "marker missing on the last packet");

    let mut receiver = H265Depacketizer::default();
    let mut rebuilt = Vec::new();
    for packet in &packets {
        let seq = u16::from_be_bytes([packet[2], packet[3]]);
        let timestamp = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
        let payload = &packet[12..];
        rebuilt.extend(receiver.push(seq, timestamp, payload).expect("depacketize"));
    }
    let expected: Vec<Vec<u8>> = nals.iter().map(|nal| nal.to_vec()).collect();
    assert_eq!(rebuilt, expected, "RFC 7798 roundtrip changed the NAL stream");
}

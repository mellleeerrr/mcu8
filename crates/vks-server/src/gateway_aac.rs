//! MP4A-LATM audio bridge (AAC-LC) on top of the isolated FFmpeg workers.
//!
//! Unlike the sample-oriented codecs in `gateway_audio`, AAC has a 1024-sample
//! frame: 16 PCM ticks of 960 samples produce 15 RTP packets. The RTP clock is
//! 90 kHz here (1920 ticks/frame), matching the offer. Inbound LOAS frames are
//! decoded in a separate task; outbound PCM is encoded in another, so neither
//! direction can block the 20 ms mixer loop.

use std::collections::VecDeque;
use std::sync::atomic::Ordering::Relaxed;
use std::time::{Duration, Instant};

use vks_gateway_sip::aac_latm::{add_loas, LatmRtpClock, AAC_RTP_CLOCK};
use vks_media_mcu::codec_worker::latm::{LatmDecoder, LatmEncoder, LATM_TICK_SAMPLES};

use super::RtpBridgeSession;

const AAC_PAYLOAD_TYPE: u8 = 113;
/// AAC frames are far below this; a larger payload is corruption, not a frame
/// to truncate.
const MAX_AAC_PAYLOAD: usize = 1200;
const PCM_QUEUE_LIMIT: usize = LATM_TICK_SAMPLES * 8;

fn gain(sample: f32, volume: u32) -> f32 {
    let scaled = sample * volume.min(1000) as f32 / 100.0;
    if scaled.abs() > 1.0 {
        scaled.tanh()
    } else {
        scaled
    }
}

fn ffmpeg_path() -> std::path::PathBuf {
    std::env::var_os("VKS_FFMPEG_BIN").map_or_else(|| std::path::PathBuf::from("ffmpeg"), std::path::PathBuf::from)
}

/// Whether this RTP payload type/codec pair has to use the AAC-LATM bridge.
#[must_use]
/// Builds one AAC RTP packet: one complete `audioMuxElement` per packet,
/// marker=1, 90 kHz timestamp supplied by [`LatmRtpClock`].
fn write_aac_rtp_packet(
    packet: &mut [u8],
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    frame: &[u8],
) -> usize {
    debug_assert!(frame.len() <= MAX_AAC_PAYLOAD);
    vks_gateway_sip::write_rtp_header(&mut packet[..12], payload_type, sequence, timestamp, ssrc, true);
    packet[12..12 + frame.len()].copy_from_slice(frame);
    12 + frame.len()
}

pub(super) fn is_aac_audio(payload_type: u8, codec: Option<&str>) -> bool {
    if payload_type == AAC_PAYLOAD_TYPE {
        return true;
    }
    codec.is_some_and(|name| {
        let normalized = name.trim().to_ascii_uppercase().replace(['.', '-', '_'], "");
        matches!(normalized.as_str(), "AAC" | "AACLATM" | "MP4ALATM")
    })
}

/// Runs the AAC-LATM audio bridge until the call ends.
pub(super) async fn run_aac_bridge(mut session: RtpBridgeSession) {
    let Some(target) = session
        .remote_media_ip
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| std::net::SocketAddr::new(ip, session.remote_audio_port))
    else {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    };
    if target.port() == 0 {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    }
    // Reserve the pool slots atomically; these two children are spawned
    // directly (split pipes), so without the lease preflight would be a no-op.
    let Some(_worker_lease) = crate::transcoder_runtime::pool().and_then(|pool| pool.try_reserve(2)) else {
        tracing::warn!(participant = %session.participant_id_str, "AAC-LATM bridge rejected: no free worker slots");
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    };
    let path = ffmpeg_path();
    let mut encoder = match LatmEncoder::spawn(path.clone(), 64_000) {
        Ok(encoder) => encoder,
        Err(error) => {
            tracing::warn!(participant = %session.participant_id_str, %error, "AAC-LATM encoder unavailable");
            session.media_stats.audio_codec_running.store(false, Relaxed);
            return;
        }
    };
    let mut decoder = match LatmDecoder::spawn(path) {
        Ok(decoder) => decoder,
        Err(error) => {
            tracing::warn!(participant = %session.participant_id_str, %error, "AAC-LATM decoder unavailable");
            encoder.shutdown().await;
            session.media_stats.audio_codec_running.store(false, Relaxed);
            return;
        }
    };
    let mut validator = if session.source_latch {
        super::audio::RtpSource::new_latched(target, session.audio_payload_type)
    } else {
        super::audio::RtpSource::new(target, session.audio_payload_type)
    };
    session.media_stats.audio_codec_running.store(true, Relaxed);
    tracing::info!(
        participant = %session.participant_id_str,
        payload_type = session.audio_payload_type,
        encoder_pid = encoder.pid(),
        decoder_pid = decoder.pid(),
        "AAC-LATM audio bridge started"
    );

    let mut egress = target;
    let ssrc = (uuid::Uuid::new_v4().as_u128() as u32).max(1);
    let mut clock = LatmRtpClock::with_rtp_clock(48_000, AAC_RTP_CLOCK, (uuid::Uuid::new_v4().as_u128() >> 32) as u32);
    let mut sequence = (uuid::Uuid::new_v4().as_u128() as u16).max(1);
    let mut pcm_queue = VecDeque::<f32>::with_capacity(PCM_QUEUE_LIMIT);
    let mut last_sequence: Option<u16> = None;
    let mut input = [0.0f32; LATM_TICK_SAMPLES];
    let mut mixed = [0.0f32; LATM_TICK_SAMPLES];
    let mut pcm = vec![0i16; LATM_TICK_SAMPLES];
    let mut packet = [0u8; 12 + MAX_AAC_PAYLOAD];
    let mut ticker = tokio::time::interval(Duration::from_millis(20));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_rtcp = Instant::now();
    let mut incoming = [0u8; 2048];
    let mut recv_errors = 0u8;
    let mut rejected_logs = 0u8;
    let mut decoded_blocks = 0u64;
    // 3-frame prefill (64 ms) plus a token-bucket pacer at the exact AAC rate
    // (46.875 fps = 15 frames per 16 ticks) avoids the 4-frame/100 ms bursts
    // measured on the live pipe.
    let mut pending_tx: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut ticks = 0u64;
    let mut frames_sent = 0u64;
    let mut first_packet_at: Option<Instant> = None;
    let mut dtmf = VecDeque::<u8>::new();
    let mut dtmf_active: Option<(u8, usize, u32)> = None;
    let mut dtmf_gap = 0u8;
    let mut dtmf_timestamp = 0u32;

    while !session.cancel_flag.load(Relaxed) {
        tokio::select! {
            Some(digits) = session.dtmf_rx.recv() => {
                for character in digits.chars().take(32) {
                    let event = match character {
                        '0'..='9' => character as u8 - b'0',
                        '*' => 10,
                        '#' => 11,
                        'A'..='D' => character as u8 - b'A' + 12,
                        'a'..='d' => character as u8 - b'a' + 12,
                        _ => continue,
                    };
                    if dtmf.len() < 128 { dtmf.push_back(event); }
                }
            }
            result = session.rtp_socket.recv_from(&mut incoming) => {
                let Ok((count, source)) = result else {
                    recv_errors = recv_errors.saturating_add(1);
                    if recv_errors >= 10 { break; }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    continue;
                };
                recv_errors = 0;
                let Some(packet_in) = validator.accept(source, &incoming[..count]) else {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    if rejected_logs < 3 {
                        rejected_logs += 1;
                        tracing::warn!(
                            participant = %session.participant_id_str,
                            source = %source,
                            "AAC RTP rejected by source validator"
                        );
                    }
                    continue;
                };
                if session.source_latch { egress = source; }
                if packet_in.payload.is_empty() || packet_in.payload.len() > MAX_AAC_PAYLOAD {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    continue;
                }
                // Our contract is one complete audioMuxElement per packet
                // (marker=1). RFC 3016 fragments (marker=0) are unsupported and
                // would be fed to add_loas as if complete.
                if !packet_in.marker {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    continue;
                }
                if let Some(last) = last_sequence {
                    let missing = usize::from(packet_in.sequence.wrapping_sub(last).wrapping_sub(1));
                    for _ in 0..missing.min(8) {
                        for _ in 0..1024 {
                            if pcm_queue.len() >= PCM_QUEUE_LIMIT { pcm_queue.pop_front(); }
                            pcm_queue.push_back(0.0);
                        }
                    }
                }
                last_sequence = Some(packet_in.sequence);
                let Ok(loas) = add_loas(packet_in.payload) else {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    continue;
                };
                if !decoder.try_push_loas(loas) {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    continue;
                }
                first_packet_at.get_or_insert_with(Instant::now);
                session.media_stats.audio_rx_packets.fetch_add(1, Relaxed);
                session.media_stats.audio_rx_bytes.fetch_add(count as u64, Relaxed);
                session.media_stats.last_rx_ms.store(
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                    Relaxed,
                );
            }
            _ = ticker.tick() => {
                while let Some(block) = decoder.try_pcm() {
                    decoded_blocks += 1;
                    for sample in block {
                        if pcm_queue.len() >= PCM_QUEUE_LIMIT { pcm_queue.pop_front(); }
                        pcm_queue.push_back(sample);
                    }
                }
                if decoder.failed() {
                    tracing::warn!(participant = %session.participant_id_str, "AAC decoder stopped");
                    break;
                }
                // A live FFmpeg with no PCM (missing/lost SMC, unsupported
                // object type) must not stay "connected" forever: RTP counters
                // keep rising but the participant hears silence.
                if decoded_blocks == 0
                    && first_packet_at.is_some_and(|first| first.elapsed() >= Duration::from_secs(3))
                {
                    tracing::warn!(
                        participant = %session.participant_id_str,
                        "AAC decoder produced no PCM for 3 s; tearing the media bridge down"
                    );
                    break;
                }
                let volume_tx = session.controls.volume_tx.load(Relaxed);
                if session.controls.audio_tx_muted.load(Relaxed) {
                    pcm_queue.clear();
                    input.fill(0.0);
                } else {
                    for sample in &mut input {
                        *sample = gain(pcm_queue.pop_front().unwrap_or(0.0), volume_tx);
                    }
                }
                session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, &input);
                session.mcu_engine.tick_audio_at(session.room_id, super::audio::media_clock_ms());

                mixed.fill(0.0);
                if !session.controls.audio_rx_muted.load(Relaxed)
                    && session.mcu_engine.copy_participant_audio(session.room_id, session.participant_id_val, &mut mixed)
                {
                    let volume_rx = session.controls.volume_rx.load(Relaxed);
                    for (sample, source) in pcm.iter_mut().zip(mixed) {
                        *sample = (gain(source, volume_rx) * 32767.0).clamp(-32768.0, 32767.0) as i16;
                    }
                } else {
                    pcm.fill(0);
                }
                if !encoder.try_push_pcm(&pcm) {
                    tracing::warn!(participant = %session.participant_id_str, "AAC encoder queue closed");
                    break;
                }
                if encoder.failed() {
                    tracing::warn!(participant = %session.participant_id_str, "AAC encoder stopped");
                    break;
                }
                while let Some(frame) = encoder.try_frame() {
                    if frame.len() > MAX_AAC_PAYLOAD {
                        session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                        continue;
                    }
                    if pending_tx.len() >= 64 {
                        pending_tx.pop_front();
                        session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    }
                    pending_tx.push_back(frame);
                }
                ticks += 1;
                let due = ticks.saturating_mul(15) / 16;
                // Keep a 3-frame cushion before the first packet, then send
                // exactly the number of frames the 20 ms timeline owes.
                while frames_sent < due && pending_tx.len()
                    > if ticks < 32 { 2 } else { 0 }
                {
                    let frame = pending_tx.pop_front().expect("pending frame");
                    let timestamp = clock.next_timestamp();
                    let length = write_aac_rtp_packet(
                        &mut packet,
                        session.audio_payload_type,
                        sequence,
                        timestamp,
                        ssrc,
                        &frame,
                    );
                    if session.rtp_socket.send_to(&packet[..length], egress).await.is_ok() {
                        session.media_stats.audio_tx_packets.fetch_add(1, Relaxed);
                        session.media_stats.audio_tx_bytes.fetch_add((12 + frame.len()) as u64, Relaxed);
                        sequence = sequence.wrapping_add(1);
                        frames_sent += 1;
                    }
                }
                // RFC 4733 events keep their own 8 kHz clock; do not advance the
                // AAC audio timestamp while sending them.
                if let Some(payload_type) = session.dtmf_payload_type {
                    if dtmf_gap > 0 {
                        dtmf_gap -= 1;
                    } else {
                        if dtmf_active.is_none() {
                            dtmf_active = dtmf.pop_front().map(|event| (event, 0, dtmf_timestamp));
                        }
                        if let Some((event, step, started)) = dtmf_active {
                            let duration = [160u16, 320, 480, 480, 480][step];
                            let mut event_packet = [0u8; 16];
                            vks_gateway_sip::write_rtp_header(
                                &mut event_packet[..12],
                                payload_type,
                                sequence,
                                started,
                                ssrc,
                                step == 0,
                            );
                            sequence = sequence.wrapping_add(1);
                            event_packet[12] = event;
                            event_packet[13] = if step >= 2 { 0x8a } else { 0x0a };
                            event_packet[14..].copy_from_slice(&duration.to_be_bytes());
                            let _ = session.rtp_socket.send_to(&event_packet, egress).await;
                            if step == 4 {
                                dtmf_active = None;
                                dtmf_gap = 2;
                            } else {
                                dtmf_active = Some((event, step + 1, started));
                            }
                        }
                    }
                } else {
                    dtmf.clear();
                }
                dtmf_timestamp = dtmf_timestamp.wrapping_add(160);
                if last_rtcp.elapsed() >= Duration::from_secs(5) {
                    if let Some(socket) = &session.rtcp_socket {
                        let report = super::audio::rtcp_sender_report(
                            ssrc,
                            clock.timestamp(),
                            session.media_stats.audio_tx_packets.load(Relaxed),
                            session.media_stats.audio_tx_bytes.load(Relaxed),
                        );
                        let _ = socket
                            .send_to(&report, std::net::SocketAddr::new(egress.ip(), egress.port().saturating_add(1)))
                            .await;
                    }
                    last_rtcp = Instant::now();
                }
            }
        }
    }
    session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, &[0.0; LATM_TICK_SAMPLES]);
    session.media_stats.audio_codec_running.store(false, Relaxed);
    encoder.shutdown().await;
    decoder.shutdown().await;
    tracing::info!(participant = %session.participant_id_str, "AAC-LATM audio bridge stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aac_is_detected_by_payload_type_or_canonical_name() {
        assert!(is_aac_audio(113, None));
        assert!(is_aac_audio(96, Some("MP4A-LATM")));
        assert!(is_aac_audio(96, Some("AAC")));
        assert!(!is_aac_audio(96, Some("G722")));
        assert!(!is_aac_audio(9, Some("G722")));
    }

    #[test]
    fn aac_rtp_packet_has_one_element_marker_and_90k_timestamp() {
        let mut packet = [0u8; 1400];
        let frame = [0x20u8, 0x00, 0x11, 0x88];
        let length = write_aac_rtp_packet(&mut packet, 113, 7, 123_456, 0xDEAD_BEEF, &frame);
        assert_eq!(length, 16);
        assert_eq!(packet[0], 0x80, "RTP v2");
        assert_eq!(packet[1] & 0x7f, 113, "MP4A-LATM payload type");
        assert_eq!(packet[1] & 0x80, 0x80, "marker: complete audioMuxElement");
        assert_eq!(&packet[2..4], &7u16.to_be_bytes());
        assert_eq!(&packet[4..8], &123_456u32.to_be_bytes(), "90 kHz clock");
        assert_eq!(&packet[8..12], &0xDEAD_BEEFu32.to_be_bytes());
        assert_eq!(packet[12], 0x20);
        assert_eq!(&packet[12..16], &frame);
    }
}

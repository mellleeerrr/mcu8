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

use vks_gateway_sip::aac_latm::{add_loas, frames_due, LatmRtpClock, AAC_FRAME_SAMPLES, AAC_RTP_CLOCK};
use vks_media_mcu::codec_worker::latm::{LatmDecoder, LatmEncoder, PushOutcome, LATM_TICK_SAMPLES};

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

/// How long the encoder input queue may stay full before the bridge is torn
/// down: a live FFmpeg that no longer consumes PCM is as dead as a dead one.
fn encoder_blocked_too_long(blocked_for: Option<Duration>) -> bool {
    blocked_for.is_some_and(|age| age >= Duration::from_secs(3))
}

/// True when RTP is still arriving (activity under 1 s old) but no PCM block
/// has been decoded for 3 s — a live FFmpeg that stopped producing audio.
fn pcm_liveness_stalled(
    inbound_age: Option<Duration>,
    pcm_age: Option<Duration>,
    first_packet_age: Option<Duration>,
) -> bool {
    let inbound_alive = inbound_age.is_some_and(|age| age < Duration::from_secs(1));
    let pcm_stalled = match pcm_age {
        Some(age) => age >= Duration::from_secs(3),
        None => first_packet_age.is_some_and(|age| age >= Duration::from_secs(3)),
    };
    inbound_alive && pcm_stalled
}

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

/// Whether this RTP payload type/codec pair has to use the AAC-LATM bridge.
/// The negotiated codec identity wins when present: PT113 is a local dynamic
/// assignment, so a peer may legally map it to G.722 or Opus.
#[must_use]
pub(super) fn is_aac_audio(payload_type: u8, codec: Option<&str>) -> bool {
    match codec {
        Some(name) => {
            vks_common::codec_spec::lookup(name).is_some_and(|spec| spec.id == vks_common::codec_spec::CodecId::AacLatm)
        }
        None => payload_type == AAC_PAYLOAD_TYPE,
    }
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
    let mut validator = if session.ip_latch {
        super::audio::RtpSource::new_ip_latched(target, session.audio_payload_type)
    } else if session.source_latch {
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
    let mut last_pcm_at: Option<Instant> = None;
    let mut last_inbound_at: Option<Instant> = None;
    // 3-frame prefill (64 ms) plus a token-bucket pacer at the exact AAC rate
    // (46.875 fps = 15 frames per 16 ticks) avoids the 4-frame/100 ms bursts
    // measured on the live pipe.
    let mut pending_tx: std::collections::VecDeque<Vec<u8>> = std::collections::VecDeque::new();
    let mut ticks = 0u64;
    let mut frames_sent = 0u64;
    let mut encoder_blocked_at: Option<Instant> = None;
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
                if session.source_latch && !session.ip_latch {
                    egress = source;
                }
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
                        for _ in 0..AAC_FRAME_SAMPLES as usize {
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
                // Liveness must see every accepted RTP packet, including the
                // ones rejected because the decoder queue is full: a wedged
                // FFmpeg keeps the queue full, and stale inbound activity
                // would hide exactly the silence this check exists for.
                first_packet_at.get_or_insert_with(Instant::now);
                last_inbound_at = Some(Instant::now());
                session.media_stats.audio_rx_packets.fetch_add(1, Relaxed);
                session.media_stats.audio_rx_bytes.fetch_add(count as u64, Relaxed);
                session.media_stats.last_rx_ms.store(
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                    Relaxed,
                );
                match decoder.try_push_loas(loas) {
                    PushOutcome::Queued => {}
                    PushOutcome::Full => {
                        session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                        continue;
                    }
                    PushOutcome::Closed => break,
                }
            }
            _ = ticker.tick() => {
                while let Some(block) = decoder.try_pcm() {
                    last_pcm_at = Some(Instant::now());
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
                // Rolling liveness: RTP arriving but no PCM for a full 3 s is
                // a dead decoder, not just a slow start.
                let stalled = pcm_liveness_stalled(
                    last_inbound_at.map(|at| at.elapsed()),
                    last_pcm_at.map(|at| at.elapsed()),
                    first_packet_at.map(|at| at.elapsed()),
                );
                if stalled {
                    tracing::warn!(
                        participant = %session.participant_id_str,
                        "AAC decoder produced no PCM for 3 s while RTP arrived; tearing the media bridge down"
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
                session.mcu_engine.tick_audio_at(session.room_id, session.mcu_engine.clock().now_ms());

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
                match encoder.try_push_pcm(&pcm) {
                    PushOutcome::Queued => encoder_blocked_at = None,
                    PushOutcome::Full => {
                        // A transiently slow tick must not tear the call down,
                        // but a wedged-yet-alive encoder would silently drop
                        // every PCM tick: mirror the decoder liveness.
                        let blocked = *encoder_blocked_at.get_or_insert_with(Instant::now);
                        session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                        if encoder_blocked_too_long(Some(blocked.elapsed())) {
                            tracing::warn!(
                                participant = %session.participant_id_str,
                                "AAC encoder accepted no PCM for 3 s; tearing the media bridge down"
                            );
                            break;
                        }
                        // Only the encoder input is backpressured; keep
                        // draining already-encoded frames, pacing, DTMF and
                        // RTCP in this tick.
                    }
                    PushOutcome::Closed => {
                        tracing::warn!(participant = %session.participant_id_str, "AAC encoder queue closed");
                        break;
                    }
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
                let mut pending_due = frames_due(
                    ticks.saturating_mul(LATM_TICK_SAMPLES as u64),
                    frames_sent,
                );
                // Keep a 2-frame cushion before the first packet, then send
                // at most two frames per 20 ms tick (plus one when the encoder
                // delivered a larger burst) so a backlog never leaves as a
                // 4-packet burst on the wire.
                let burst = if pending_tx.len() > 4 { 3 } else { 2 };
                let mut sent_this_tick = 0u32;
                while pending_due > 0
                    && sent_this_tick < burst
                    && pending_tx.len() > if ticks < 32 { 2 } else { 0 }
                {
                    pending_due -= 1;
                    sent_this_tick += 1;
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
                            // Same RFC 4733 series as the sample-oriented bridge:
                            // marker first, E=1 last, 200 ms of 20 ms packets.
                            let event_packet = super::audio::dtmf_event_packet(
                                &super::audio::DTMF_DURATIONS,
                                payload_type,
                                sequence,
                                started,
                                ssrc,
                                event,
                                step,
                            );
                            sequence = sequence.wrapping_add(1);
                            match session.rtp_socket.send_to(&event_packet, egress).await {
                                Ok(_) => {
                                    session.media_stats.dtmf_tx_packets.fetch_add(1, Relaxed);
                                }
                                Err(error) => {
                                    let errors = session.media_stats.dtmf_tx_errors.fetch_add(1, Relaxed) + 1;
                                    if errors <= 3 {
                                        tracing::warn!(participant = %session.participant_id_str, %error, %egress, "failed to send AAC DTMF telephone-event packet");
                                    }
                                }
                            }
                            if step + 1 == super::audio::DTMF_DURATIONS.len() {
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

    #[test]
    fn aac_dispatch_uses_the_codec_identity_over_the_dynamic_pt() {
        assert!(is_aac_audio(113, None));
        assert!(is_aac_audio(96, Some("AAC")));
        assert!(is_aac_audio(96, Some("MP4A-LATM")));
        assert!(is_aac_audio(96, Some("AAC-LATM")));
        assert!(!is_aac_audio(113, Some("G722")));
        assert!(!is_aac_audio(113, Some("OPUS")));
        assert!(!is_aac_audio(113, Some("PCMU")));
    }

    #[test]
    fn encoder_full_turns_into_teardown_only_after_three_seconds() {
        assert!(!encoder_blocked_too_long(None));
        assert!(!encoder_blocked_too_long(Some(Duration::from_millis(2999))));
        assert!(encoder_blocked_too_long(Some(Duration::from_secs(3))));
    }

    #[test]
    fn aac_liveness_detects_a_wedged_decoder_under_full_queue() {
        let secs = Duration::from_secs;
        assert!(!pcm_liveness_stalled(Some(secs(0)), None, Some(secs(0))));
        assert!(pcm_liveness_stalled(Some(secs(0)), None, Some(secs(4))), "no PCM after start");
        assert!(
            pcm_liveness_stalled(Some(secs(0)), Some(secs(4)), Some(secs(30))),
            "mid-call silence while RTP arrives"
        );
        assert!(
            !pcm_liveness_stalled(Some(secs(5)), Some(secs(4)), Some(secs(30))),
            "no inbound RTP: rx watchdog owns this case"
        );
        assert!(!pcm_liveness_stalled(Some(secs(0)), Some(secs(1)), Some(secs(30))));
    }
}

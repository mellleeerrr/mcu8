//! G.711/G.722/G.723.1/G.726/G.728/Opus payload conversion at the negotiated
//! RTP boundary.

use std::sync::atomic::Ordering::Relaxed;

pub(super) fn media_clock_ms() -> u64 {
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
}

fn gain(sample: f32, volume: u32) -> f32 {
    let scaled = sample * volume.min(1000) as f32 / 100.0;
    if scaled.abs() > 1.0 {
        scaled.tanh()
    } else {
        scaled
    }
}

/// Selected RTP audio codec and its PCM domain.  G.711/G.723.1/G.726/G.728 are
/// 8 kHz, G.722 is 16 kHz, Opus is 48 kHz; all use a 20 ms MCU tick, while the
/// RTP timestamp step is 160 (8 kHz family except G.723.1), 240 (G.723.1
/// 30 ms packets) or 960 (Opus).
enum AudioBridgeCodec {
    G711 {
        /// G.711 law (0 = µ-law/PCMU, 8 = A-law/PCMA), independent of the RTP
        /// payload number, which may be a peer-chosen dynamic PT.
        law: u8,
        /// Canonical payload type used by `encode_sample`; the RTP header
        /// still carries the negotiated peer PT.
        encode_payload_type: u8,
    },
    G722(Box<vks_gateway_sip::g722::G722Codec>),
    G726(Box<vks_gateway_sip::g726::G726Codec>),
    G728(Box<vks_gateway_sip::g728::G728Codec>),
    G7231(Box<vks_gateway_sip::g7231::G7231Codec>),
    Opus(Box<vks_gateway_sip::opus::OpusCodec>),
}

impl AudioBridgeCodec {
    fn new(payload_type: u8, codec: Option<&str>) -> Option<Self> {
        // Codec identity is authoritative: a peer may choose any dynamic PT
        // 96-127 for G.726, so the payload number alone is not enough.
        let normalized = codec.map(|name| name.trim().to_ascii_uppercase().replace(['.', '-', '_'], ""));
        match normalized.as_deref() {
            Some("G72616") => {
                Some(Self::G726(Box::new(vks_gateway_sip::g726::G726Codec::new(vks_gateway_sip::g726::G726Rate::R16))))
            }
            Some("G72624") => {
                Some(Self::G726(Box::new(vks_gateway_sip::g726::G726Codec::new(vks_gateway_sip::g726::G726Rate::R24))))
            }
            Some("G72632") => {
                Some(Self::G726(Box::new(vks_gateway_sip::g726::G726Codec::new(vks_gateway_sip::g726::G726Rate::R32))))
            }
            Some("G72640") => {
                Some(Self::G726(Box::new(vks_gateway_sip::g726::G726Codec::new(vks_gateway_sip::g726::G726Rate::R40))))
            }
            Some("G722") => Some(Self::G722(Box::new(vks_gateway_sip::g722::G722Codec::new()))),
            Some("OPUS") => Some(Self::Opus(Box::new(vks_gateway_sip::opus::OpusCodec::new()?))),
            Some("G728") => Some(Self::G728(Box::new(vks_gateway_sip::g728::G728Codec::new()))),
            Some("G723") | Some("G7231") => Some(Self::G7231(Box::new(vks_gateway_sip::g7231::G7231Codec::new()))),
            Some("PCMU") => Some(Self::G711 { law: 0, encode_payload_type: 0 }),
            Some("PCMA") => Some(Self::G711 { law: 8, encode_payload_type: 8 }),
            _ => match payload_type {
                0 | 8 => Some(Self::G711 { law: payload_type, encode_payload_type: payload_type }),
                9 => Some(Self::G722(Box::new(vks_gateway_sip::g722::G722Codec::new()))),
                15 => Some(Self::G728(Box::new(vks_gateway_sip::g728::G728Codec::new()))),
                4 => Some(Self::G7231(Box::new(vks_gateway_sip::g7231::G7231Codec::new()))),
                111 => Some(Self::Opus(Box::new(vks_gateway_sip::opus::OpusCodec::new()?))),
                107..=110 => {
                    let rate = vks_gateway_sip::g726::G726Rate::from_payload_type(payload_type)?;
                    Some(Self::G726(Box::new(vks_gateway_sip::g726::G726Codec::new(rate))))
                }
                _ => None,
            },
        }
    }

    fn frame_samples(&self) -> usize {
        match self {
            Self::G722(_) => 320,
            Self::G728(_) => vks_gateway_sip::g728::FRAME_SAMPLES,
            Self::Opus(_) => vks_gateway_sip::opus::FRAME_SAMPLES,
            // G.723.1 is packetized at 30 ms but consumed at the 20 ms tick.
            Self::G711 { .. } | Self::G726(_) | Self::G7231(_) => 160,
        }
    }

    fn max_payload(&self) -> usize {
        match self {
            Self::G726(codec) => 2 * codec.rate().bytes_20ms(),
            Self::Opus(_) => vks_gateway_sip::opus::MAX_PAYLOAD,
            Self::G728(_) => vks_gateway_sip::g728::PAYLOAD_BYTES,
            Self::G7231(_) => vks_gateway_sip::g7231::PAYLOAD_BYTES,
            Self::G711 { .. } | Self::G722(_) => 480,
        }
    }

    /// Packet-loss concealment for `missing` lost packets.  Opus can recover
    /// the immediately preceding frame from the next packet's in-band FEC;
    /// otherwise it runs up to three PLC frames (bounded so a long network
    /// outage does not enqueue minutes of invented audio).
    fn conceal(&mut self, missing: usize, next_payload: &[u8]) -> Vec<f32> {
        let mut out = Vec::new();
        let Self::Opus(codec) = self else { return out };
        if missing == 1 {
            if let Some(fec) = codec.decode_fec(next_payload) {
                out.extend(fec);
                return out;
            }
        }
        for _ in 0..missing.min(3) {
            let Some(plc) = codec.decode(&[]) else { break };
            out.extend(plc);
        }
        out
    }

    fn is_g723(&self) -> bool {
        matches!(self, Self::G7231(_))
    }

    /// PCM samples per RTP packet.  G.723.1 uses 30 ms (240 samples) while
    /// every other native codec matches the 20 ms MCU tick.
    fn packet_samples(&self) -> usize {
        match self {
            Self::G7231(_) => 240,
            _ => self.frame_samples(),
        }
    }

    /// RTP timestamp increment per 20 ms packet: 48 kHz for Opus, 8 kHz for
    /// the narrowband/wideband codecs (G.722 keeps an 8 kHz RTP clock).
    fn timestamp_step(&self) -> u32 {
        match self {
            Self::Opus(_) => vks_gateway_sip::opus::FRAME_SAMPLES as u32,
            Self::G711 { .. } | Self::G722(_) | Self::G726(_) | Self::G728(_) => 160,
            // G.723.1 packets are 30 ms (240 samples); the MCU tick still
            // consumes/produces 20 ms, so packetization is accumulated.
            Self::G7231(_) => 240,
        }
    }

    fn decode(&mut self, payload_type: u8, payload: &[u8]) -> Option<Vec<f32>> {
        match self {
            // The validator permits PCMU/PCMA interchange, so decode with the
            // packet's actual payload type, never with the session's choice.
            Self::G711 { law, .. } => {
                // 0/8 interchange keeps the packet's law; a dynamic PT keeps
                // the law negotiated via a=rtpmap because its number is not 0/8.
                let law = if matches!(payload_type, 0 | 8) { payload_type } else { *law };
                Some(payload.iter().map(|byte| decode_sample(law, *byte)).collect())
            }
            Self::G722(codec) => Some(
                codec
                    .decode(payload)
                    .into_iter()
                    // oxideav-g722 can overshoot the nominal 14-bit range on
                    // transients; clamp before the MCU mixer sees > 1.0.
                    .map(|sample| (sample as f32 / 8192.0).clamp(-1.0, 1.0))
                    .collect(),
            ),
            Self::G726(codec) => {
                if !codec.rate().accepts_payload_len(payload.len()) {
                    return None;
                }
                Some(codec.decode(payload).into_iter().map(|sample| f32::from(sample) / 32768.0).collect())
            }
            Self::G728(codec) => codec.decode(payload),
            Self::G7231(codec) => codec.decode(payload),
            Self::Opus(codec) => codec.decode(payload),
        }
    }

    fn encode(&mut self, pcm: &[f32]) -> Vec<u8> {
        match self {
            Self::G711 { encode_payload_type, .. } => {
                pcm.iter().map(|sample| encode_sample(*encode_payload_type, *sample)).collect()
            }
            Self::G722(codec) => {
                let pcm: Vec<i32> = pcm.iter().map(|sample| (sample * 8192.0).clamp(-8192.0, 8191.0) as i32).collect();
                codec.encode(&pcm)
            }
            Self::G726(codec) => {
                let pcm: Vec<i16> =
                    pcm.iter().map(|sample| (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16).collect();
                codec.encode(&pcm)
            }
            Self::G728(codec) => codec.encode(pcm).unwrap_or_default(),
            Self::G7231(codec) => codec.encode(pcm).unwrap_or_default(),
            Self::Opus(codec) => codec.encode(pcm).unwrap_or_default(),
        }
    }
}

pub(super) async fn run_audio_bridge(mut session: super::RtpBridgeSession) {
    // AAC-LC is a 1024-sample/90 kHz codec on a 960-sample mixer tick and needs
    // a pair of FFmpeg workers; it cannot share the synchronous sample path.
    if super::aac::is_aac_audio(session.audio_payload_type, session.audio_codec) {
        super::aac::run_aac_bridge(session).await;
        return;
    }
    let Some(target) = session
        .remote_media_ip
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| std::net::SocketAddr::new(ip, session.remote_audio_port))
    else {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    };
    let Some(mut bridge_codec) = AudioBridgeCodec::new(session.audio_payload_type, session.audio_codec) else {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    };
    if target.port() == 0 {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    }
    let mut validator = if session.source_latch {
        RtpSource::new_latched(target, session.audio_payload_type)
    } else {
        RtpSource::new(target, session.audio_payload_type)
    };
    validator.accept_dtmf_payload_type(session.dtmf_payload_type);
    if matches!(bridge_codec, AudioBridgeCodec::Opus(_)) {
        validator.allow_empty_payloads();
    }
    session.media_stats.audio_codec_running.store(true, Relaxed);
    let mut egress = target;
    let expected_audio_pt = session.audio_payload_type;
    let expected_dtmf_pt = session.dtmf_payload_type;
    // Holds one maximum-duration Opus packet (120 ms) so a peer using
    // 40/60/120 ms packetization is not truncated by the 20 ms consumer.
    const AUDIO_QUEUE_LIMIT: usize = vks_gateway_sip::opus::MAX_FRAME_SAMPLES;
    let mut queue = std::collections::VecDeque::<f32>::with_capacity(AUDIO_QUEUE_LIMIT);
    let mut dtmf = std::collections::VecDeque::<u8>::new();
    // Last inbound (event, RTP timestamp) already relayed: repeated packets of
    // the same keypress share the timestamp, the next digit starts a new one.
    let mut last_inbound_dtmf: Option<(u8, u32)> = None;
    let mut dtmf_active: Option<(u8, usize, u32)> = None;
    let mut dtmf_gap = 0;
    let mut sequence = 1u16;
    let mut timestamp = 0u32;
    // Telephone-event keeps the 8 kHz event clock even when Opus audio uses
    // a 48 kHz RTP timestamp.
    let mut dtmf_timestamp = 0u32;
    let mut last_sequence: Option<u16> = None;
    // G.723.1 only: 8 kHz output accumulated until one 30 ms / 240-sample
    // packet is ready.
    let mut g723_accum: Vec<f32> = Vec::with_capacity(480);
    // G.723.1 receives 240 samples every 30 ms but the mixer consumes 160
    // every 20 ms.  Two packets (480 samples) of playout prefill keep the
    // 60 ms arrival/consumption pattern from underrunning every cycle.
    let mut g723_playout_started = false;
    const G723_PREFILL: usize = 480;
    let ssrc = (uuid::Uuid::new_v4().as_u128() as u32).max(1);
    let mut incoming = [0; 2048];
    let mut mixed = [0.0; 960];
    let mut ticker = tokio::time::interval(std::time::Duration::from_millis(20));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_rtcp = std::time::Instant::now();
    let mut recv_errors = 0u8;
    let mut rejected_logs = 0u8;
    while !session.cancel_flag.load(Relaxed) {
        tokio::select! {
            Some(digits) = session.dtmf_rx.recv() => {
                for character in digits.chars().take(32) {
                    let event = match character { '0'..='9' => character as u8 - b'0', '*' => 10, '#' => 11,
                        'A'..='D' => character as u8 - b'A' + 12, 'a'..='d' => character as u8 - b'a' + 12, _ => continue };
                    if dtmf.len() < 128 { dtmf.push_back(event); }
                }
            }
            result = session.rtp_socket.recv_from(&mut incoming) => {
                // A single transient UDP error must not tear down a healthy
                // call; a persistent one must stop the bridge instead of
                // spinning on an always-ready error branch.
                let Ok((count, source)) = result else {
                    recv_errors = recv_errors.saturating_add(1);
                    if recv_errors >= 10 { break; }
                    tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    continue;
                };
                recv_errors = 0;
                let previous_source = validator.target();
                let previous_ssrc = validator.ssrc();
                let Some(packet) = validator.accept(source, &incoming[..count]) else {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    if rejected_logs < 3 {
                        rejected_logs += 1;
                        let header = vks_gateway_sip::parse_rtp_header(&incoming[..count]);
                        let reason = reject_reason(
                            expected_audio_pt,
                            expected_dtmf_pt,
                            source,
                            previous_source,
                            previous_ssrc,
                            header,
                        );
                        tracing::warn!(
                            participant = %session.participant_id_str,
                            reason,
                            expected_payload = expected_audio_pt,
                            received_payload = header.map(|(payload, _, _, _, _)| payload),
                            source = %source,
                            "audio RTP rejected by source validator"
                        );
                    }
                    continue;
                };
                if session.source_latch {
                    egress = source;
                }
                if session.dtmf_payload_type == Some(packet.payload_type) {
                    if let Some(event) = parse_telephone_event(packet.payload) {
                        let key = (event, packet.timestamp);
                        if last_inbound_dtmf != Some(key) {
                            last_inbound_dtmf = Some(key);
                            session.media_stats.dtmf_events_received.fetch_add(1, Relaxed);
                            if let Some(events) = &session.dtmf_events {
                                // Bounded consumer: a blocked relay must never
                                // stall the 20 ms audio tick.
                                let _ = events.send(event);
                            }
                        }
                    }
                    continue;
                }
                // 40 ms G.711/G.722 ptime and 20/30/40 ms G.726 ptime are
                // legal per RFC 4566/3551; never truncate the second half.
                if packet.payload.len() > bridge_codec.max_payload() {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed); continue;
                }
                if let Some(last) = last_sequence {
                    let missing = usize::from(packet.sequence.wrapping_sub(last).wrapping_sub(1));
                    if missing > 0 {
                        let concealed = bridge_codec.conceal(missing, packet.payload);
                        if !concealed.is_empty() {
                            while queue.len() + concealed.len() > AUDIO_QUEUE_LIMIT { queue.pop_front(); }
                            queue.extend(concealed);
                        }
                    }
                }
                last_sequence = Some(packet.sequence);
                let Some(decoded) = bridge_codec.decode(packet.payload_type, packet.payload) else {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    continue;
                };
                // Only complete, decodable frames count as received audio;
                // partial G.726 groups are rejected above and must not feed
                // the "call is alive" watchdog.
                session.media_stats.audio_rx_packets.fetch_add(1, Relaxed);
                session.media_stats.last_rx_ms.store(
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                    Relaxed,
                );
                session.media_stats.audio_rx_bytes.fetch_add(count as u64, Relaxed);
                if session.controls.audio_tx_muted.load(Relaxed) { queue.clear(); continue; }
                while queue.len() + decoded.len() > AUDIO_QUEUE_LIMIT { queue.pop_front(); }
                queue.extend(decoded);
            }
            _ = ticker.tick() => {
                let mut upsampled = [0.0; 960];
                let frame_samples = bridge_codec.frame_samples();
                let mut input = [0.0; 960];
                if session.controls.audio_tx_muted.load(Relaxed) {
                    queue.clear();
                    g723_playout_started = false;
                } else {
                    let volume = session.controls.volume_tx.load(Relaxed);
                    if bridge_codec.is_g723() && !g723_playout_started && queue.len() >= G723_PREFILL {
                        g723_playout_started = true;
                    }
                    let play = !bridge_codec.is_g723() || g723_playout_started;
                    for sample in input.iter_mut().take(frame_samples) {
                        *sample = if play { gain(queue.pop_front().unwrap_or(0.0), volume) } else { 0.0 };
                    }
                }
                if frame_samples == vks_gateway_sip::opus::FRAME_SAMPLES {
                    // Opus runs natively at the 48 kHz MCU mix rate.
                } else if frame_samples == 320 {
                    let mut wide = [0.0; 320];
                    wide.copy_from_slice(&input[..320]);
                    vks_gateway_sip::upsample_16k_to_48k(&wide, &mut upsampled);
                } else {
                    let mut narrow = [0.0; 160];
                    narrow.copy_from_slice(&input[..160]);
                    vks_gateway_sip::upsample_8k_to_48k(&narrow, &mut upsampled);
                }
                let mcu_input = if frame_samples == vks_gateway_sip::opus::FRAME_SAMPLES { &input } else { &upsampled };
                session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, mcu_input);
                session.mcu_engine.tick_audio_at(session.room_id, media_clock_ms());
                let mut packet = [0; 12 + vks_gateway_sip::opus::MAX_PAYLOAD];
                vks_gateway_sip::write_rtp_header(&mut packet[..12], session.audio_payload_type, sequence, timestamp, ssrc, false);
                let volume = session.controls.volume_rx.load(Relaxed);
                let frame_samples = bridge_codec.frame_samples();
                let mut output = [0.0; 960];
                if !session.controls.audio_rx_muted.load(Relaxed)
                    && session.mcu_engine.copy_participant_audio(session.room_id, session.participant_id_val, &mut mixed) {
                    if frame_samples == vks_gateway_sip::opus::FRAME_SAMPLES {
                        output.copy_from_slice(&mixed);
                    } else if frame_samples == 320 {
                        let mut wide = [0.0; 320];
                        vks_gateway_sip::downsample_48k_to_16k(&mixed, &mut wide);
                        output[..320].copy_from_slice(&wide);
                    } else {
                        let mut narrow = [0.0; 160];
                        vks_gateway_sip::downsample_48k_to_8k(&mixed, &mut narrow);
                        output[..160].copy_from_slice(&narrow);
                    }
                }
                for sample in output.iter_mut().take(frame_samples) {
                    *sample = gain(*sample, volume);
                }
                let packet_samples = bridge_codec.packet_samples();
                let mut g723_frame = [0.0f32; 240];
                let mut ready = packet_samples == frame_samples;
                if ready {
                    // The 20 ms tick is exactly one packet.
                } else {
                    g723_accum.extend_from_slice(&output[..frame_samples]);
                    if g723_accum.len() >= packet_samples {
                        g723_frame.copy_from_slice(&g723_accum[..packet_samples]);
                        g723_accum.drain(..packet_samples);
                        ready = true;
                    }
                }
                if ready {
                    let encode_slice: &[f32] = if packet_samples == frame_samples {
                        &output[..frame_samples]
                    } else {
                        &g723_frame[..packet_samples]
                    };
                    let encoded = bridge_codec.encode(encode_slice);
                    // A codec failure must not become a zero-length RTP packet:
                    // skip this frame and keep the timestamp advancing.
                    if !encoded.is_empty() {
                        let payload_len = encoded.len().min(packet.len() - 12);
                        packet[12..12 + payload_len].copy_from_slice(&encoded[..payload_len]);
                        if let Ok(count) = session.rtp_socket.send_to(&packet[..12 + payload_len], egress).await {
                            session.media_stats.audio_tx_packets.fetch_add(1, Relaxed); session.media_stats.audio_tx_bytes.fetch_add(count as u64, Relaxed);
                            sequence = sequence.wrapping_add(1);
                        }
                        // The sampling instant advances even when the socket
                        // temporarily refuses the datagram; the next payload
                        // must carry the later timestamp, not repeat it.
                        timestamp = timestamp.wrapping_add(bridge_codec.timestamp_step());
                    }
                }
                if let Some(payload_type) = expected_dtmf_pt {
                    if dtmf_gap > 0 { dtmf_gap -= 1; }
                    else {
                        if dtmf_active.is_none() { dtmf_active = dtmf.pop_front().map(|event| (event, 0, dtmf_timestamp)); }
                        if let Some((event, step, started)) = dtmf_active {
                            let event_packet =
                                dtmf_event_packet(&DTMF_DURATIONS, payload_type, sequence, started, ssrc, event, step);
                            sequence = sequence.wrapping_add(1);
                            match session.rtp_socket.send_to(&event_packet, egress).await {
                                Ok(_) => {
                                    session.media_stats.dtmf_tx_packets.fetch_add(1, Relaxed);
                                }
                                Err(error) => {
                                    // DTMF нельзя терять молча: раньше `let _ =`
                                    // скрывал закрытый сокет и тон «исчезал».
                                    let errors = session.media_stats.dtmf_tx_errors.fetch_add(1, Relaxed) + 1;
                                    if errors <= 3 {
                                        tracing::warn!(participant = %session.participant_id_str, %error, %egress, "failed to send DTMF telephone-event packet");
                                    } else {
                                        tracing::debug!(participant = %session.participant_id_str, %error, "failed to send DTMF telephone-event packet");
                                    }
                                }
                            }
                            if step + 1 == DTMF_DURATIONS.len() { dtmf_active = None; dtmf_gap = 2; } else { dtmf_active = Some((event, step + 1, started)); }
                        }
                    }
                } else { dtmf.clear(); }
                dtmf_timestamp = dtmf_timestamp.wrapping_add(160);
                if last_rtcp.elapsed() >= std::time::Duration::from_secs(5) {
                    if let Some(socket) = &session.rtcp_socket {
                        let report = rtcp_sender_report(ssrc, timestamp, session.media_stats.audio_tx_packets.load(Relaxed), session.media_stats.audio_tx_bytes.load(Relaxed));
                        let _ = socket.send_to(&report, std::net::SocketAddr::new(egress.ip(), egress.port().saturating_add(1))).await;
                    }
                    last_rtcp = std::time::Instant::now();
                }
            }
        }
    }
    session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, &[0.0; 960]);
    session.media_stats.audio_codec_running.store(false, Relaxed);
    tracing::info!(
        participant = %session.participant_id_str,
        codec = session.audio_codec.unwrap_or_else(|| super::audio_codec_name(session.audio_payload_type)),
        "RTP audio bridge stopped"
    );
}

pub(super) fn rtcp_sender_report(ssrc: u32, timestamp: u32, packets: u64, bytes: u64) -> Vec<u8> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    let mut report = vec![0x80, 200, 0, 6];
    for value in [
        ssrc,
        now.as_secs().wrapping_add(2_208_988_800) as u32,
        ((u64::from(now.subsec_nanos()) << 32) / 1_000_000_000) as u32,
        timestamp,
        packets as u32,
        bytes.saturating_sub(packets * 12) as u32,
    ] {
        report.extend_from_slice(&value.to_be_bytes());
    }
    let mut description = vec![0x81, 202, 0, 0];
    description.extend_from_slice(&ssrc.to_be_bytes());
    description.extend_from_slice(&[1, 7]);
    description.extend_from_slice(b"vks-mcu");
    description.push(0);
    while !description.len().is_multiple_of(4) {
        description.push(0);
    }
    let length = (description.len() / 4 - 1) as u16;
    description[2..4].copy_from_slice(&length.to_be_bytes());
    report.extend(description);
    report
}

pub(super) fn decode_sample(payload_type: u8, byte: u8) -> f32 {
    if payload_type == 8 {
        let value = byte ^ 0x55;
        let segment = (value >> 4) & 7;
        let mut sample = i32::from(value & 15) << 4;
        sample += if segment == 0 { 8 } else { 0x108 };
        if segment > 1 {
            sample <<= segment - 1;
        }
        if value & 0x80 == 0 {
            sample = -sample;
        }
        sample as f32 / 32768.0
    } else {
        vks_gateway_sip::ulaw_to_f32(byte)
    }
}

pub(super) fn encode_sample(payload_type: u8, value: f32) -> u8 {
    let sample = (value.clamp(-1.0, 1.0) * 32767.0) as i16;
    if payload_type != 8 {
        return vks_gateway_sip::rtp_bridge::linear_to_ulaw(sample);
    }
    let mut magnitude = i32::from(sample) >> 3;
    let mask = if magnitude >= 0 {
        0xd5
    } else {
        magnitude = -magnitude - 1;
        0x55
    };
    let limits = [0x1f, 0x3f, 0x7f, 0xff, 0x1ff, 0x3ff, 0x7ff, 0xfff];
    let segment = limits.iter().position(|limit| magnitude <= *limit).unwrap_or(8);
    if segment >= 8 {
        return 0x7f ^ mask;
    }
    let mantissa = if segment < 2 { (magnitude >> 1) & 15 } else { (magnitude >> segment) & 15 };
    (((segment as u8) << 4) | mantissa as u8) ^ mask
}

/// Pins the negotiated RTP endpoint, payload type, SSRC and receive sequence.
pub(super) struct RtpSource {
    target: std::net::SocketAddr,
    payload_type: u8,
    /// RFC 4733 telephone-event PT; accepted in addition to the audio PT and
    /// never fed to the codec decoder.
    dtmf_payload_type: Option<u8>,
    ssrc: Option<u32>,
    sequence: Option<u16>,
    /// Входящий вызов: первый валидный RTP-latch с SDP-target разрешён,
    /// дальше источник фиксируется (NAT/перемаршрутизация порта).
    latch: bool,
    allow_empty: bool,
    /// Видео: переупорядоченные пакеты (seq меньше последнего) не отбрасывать
    /// здесь — их ставит на место входной reorder-буфер (`VideoReorderBuffer`).
    /// Аудио оставляет строгий порядок: у микшера нет своего reorder-буфера.
    reordered: bool,
}

pub(super) struct RtpPayload<'a> {
    pub payload: &'a [u8],
    pub payload_type: u8,
    pub sequence: u16,
    pub timestamp: u32,
    pub marker: bool,
}

impl RtpSource {
    pub(super) fn ssrc(&self) -> Option<u32> {
        self.ssrc
    }
    /// Текущий (возможно, уже залоченный) источник — для диагностики отказа.
    pub(super) fn target(&self) -> std::net::SocketAddr {
        self.target
    }
    pub(super) fn new(target: std::net::SocketAddr, payload_type: u8) -> Self {
        Self {
            target,
            payload_type,
            dtmf_payload_type: None,
            ssrc: None,
            sequence: None,
            latch: false,
            allow_empty: false,
            reordered: false,
        }
    }

    /// Видео-вход: разрешить переупорядоченные последовательности (jitter
    /// bot→MCU). Дубликаты (seq == последний) по-прежнему отбрасываются.
    pub(super) fn accept_reordered(&mut self) {
        self.reordered = true;
    }

    /// Enables RFC 4733 reception alongside the negotiated audio payload.
    pub(super) fn accept_dtmf_payload_type(&mut self, payload_type: Option<u8>) {
        self.dtmf_payload_type = payload_type;
    }

    /// UAS-вызов: разрешает первый валидный пакет с другого source и
    /// фиксирует его; последующие источники отбрасываются.
    pub(super) fn new_latched(target: std::net::SocketAddr, payload_type: u8) -> Self {
        Self { latch: true, ..Self::new(target, payload_type) }
    }

    /// Opus uses a zero-length payload as a lost/PLC indication (RFC 3550
    /// permits empty payloads); other codecs must carry a complete frame.
    pub(super) fn allow_empty_payloads(&mut self) {
        self.allow_empty = true;
    }

    pub(super) fn accept<'a>(&mut self, source: std::net::SocketAddr, packet: &'a [u8]) -> Option<RtpPayload<'a>> {
        if source != self.target {
            if !self.latch || self.ssrc.is_some() {
                return None;
            }
            self.target = source;
        }
        let (pt, seq, ts, ssrc, offset) = vks_gateway_sip::parse_rtp_header(packet)?;
        // G.711 PCMU/PCMA may be sent with either static PT after SDP answer:
        // cross-vendor MCUs (TrueConf) commonly answer PCMU but transmit PCMA.
        // Video/dynamic PTs still require exact match.
        let g711_interchange = matches!(pt, 0 | 8) && matches!(self.payload_type, 0 | 8);
        // RFC 4733 events share the RTP session but use their own PT; they are
        // routed to the DTMF parser instead of the codec (RFC 4733 §2.5).
        let is_dtmf = self.dtmf_payload_type == Some(pt);
        if !is_dtmf && pt != self.payload_type && !g711_interchange {
            return None;
        }
        let padding = if packet[0] & 0x20 != 0 {
            let count = usize::from(*packet.last()?);
            if count == 0 || count > packet.len().checked_sub(offset)? {
                return None;
            }
            count
        } else {
            0
        };
        let end = packet.len().checked_sub(padding)?;
        if end < offset || (end == offset && !self.allow_empty) || self.ssrc.is_some_and(|current| current != ssrc) {
            return None;
        }
        if self.sequence.is_some_and(|last| {
            let distance = seq.wrapping_sub(last);
            if distance == 0 {
                return true;
            }
            // Аудио: пакет «старше последнего» — дубль/реплей, вход закрыт.
            // Видео: это reorder от джиттера, его разложит `VideoReorderBuffer`
            // (иначе при RTT-джиттере вход терял 90 %+ пакетов и видео падало).
            !self.reordered && distance >= 0x8000
        }) {
            return None;
        }
        self.ssrc = Some(ssrc);
        self.sequence = Some(seq);
        Some(RtpPayload {
            payload: &packet[offset..end],
            payload_type: pt,
            sequence: seq,
            timestamp: ts,
            marker: packet[1] & 0x80 != 0,
        })
    }
}

/// Minimal RFC 4733 §2.3 parser: returns the DTMF event code (0-15, `*`=10,
/// `#`=11, A-D=12-15). The end bit and duration are validated but the relay
/// regenerates its own event sequence, so only the digit is returned.
/// Длительности (в 8-кГц сэмплах) последовательных RFC 4733-пакетов одного
/// нажатия: 10 × 160 = 1600 сэмплов = 200 мс тона. Шаг 20 мс совпадает с
/// тиком моста; для Opus/48k телефонный клок остаётся 8 кГц (RFC 4733 §2.1).
pub(super) const DTMF_DURATIONS: [u16; 10] = [160, 320, 480, 640, 800, 960, 1120, 1280, 1440, 1600];

/// Собирает `step`-й 20-мс пакет RFC 4733 для одного нажатия.
///
/// RFC 4733 §2.5.1.2: первый пакет события несёт маркер `M=1`, последний —
/// `E=1`; промежуточные не несут ни того, ни другого. Получатель завершает
/// тон на первом пакете с `E=1` и берёт его `duration` как длительность
/// (RFC 4733 §2.5.2). Раньше `E=1` стоял начиная с третьего пакета, поэтому
/// 200-мс тон обрывался через 60 мс (аудит 07, P1-1).
pub(super) fn dtmf_event_packet(
    durations: &[u16],
    payload_type: u8,
    sequence: u16,
    timestamp: u32,
    ssrc: u32,
    event: u8,
    step: usize,
) -> [u8; 16] {
    debug_assert!(!durations.is_empty() && step < durations.len(), "DTMF step outside the tone series");
    let last = step + 1 == durations.len();
    let mut packet = [0u8; 16];
    vks_gateway_sip::write_rtp_header(&mut packet[..12], payload_type, sequence, timestamp, ssrc, step == 0);
    packet[12] = event;
    packet[13] = if last { 0x8a } else { 0x0a };
    packet[14..].copy_from_slice(&durations[step].to_be_bytes());
    packet
}

/// Причина отказа RTP-валидатора. Раньше все отказы логировались как
/// «codec/PT mismatch», хотя это мог быть чужой source или смена SSRC —
/// политика `source_latch` должна быть видна в логе.
fn reject_reason(
    audio_pt: u8,
    dtmf_pt: Option<u8>,
    source: std::net::SocketAddr,
    previous_source: std::net::SocketAddr,
    previous_ssrc: Option<u32>,
    header: Option<(u8, u16, u32, u32, usize)>,
) -> &'static str {
    let Some((pt, _, _, ssrc, _)) = header else {
        return "malformed_rtp";
    };
    let g711_interchange = matches!(pt, 0 | 8) && matches!(audio_pt, 0 | 8);
    if pt != audio_pt && Some(pt) != dtmf_pt && !g711_interchange {
        return "payload_type_mismatch";
    }
    if source != previous_source {
        return "source_changed";
    }
    if previous_ssrc.is_some_and(|registered| registered != ssrc) {
        return "ssrc_changed";
    }
    "invalid_rtp"
}

fn parse_telephone_event(payload: &[u8]) -> Option<u8> {
    // event(1) + E|R|volume(1) + duration(2); longer payloads carry redundant
    // encodings, which must not change the event identity.
    if payload.len() < 4 {
        return None;
    }
    let event = payload[0];
    if event > 15 {
        return None;
    }
    Some(event)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtp_source_video_accepts_reordered_but_audio_keeps_strict_order() {
        let target = "127.0.0.1:5000".parse().unwrap();
        let mut packet = [0u8; 172];
        // Видео: reorder разрешён (джиттер bot→MCU), дубликаты по-прежнему нет.
        let mut video = RtpSource::new(target, 96);
        video.accept_reordered();
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 96, 10, 0, 42, false);
        assert!(video.accept(target, &packet).is_some());
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 96, 12, 3000, 42, false);
        assert!(video.accept(target, &packet).is_some(), "вперёд");
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 96, 11, 0, 42, false);
        assert!(video.accept(target, &packet).is_some(), "reorder назад: видео принимает");
        assert!(video.accept(target, &packet).is_none(), "точный дубль отброшен");
        // Аудио: строгий порядок, «старше последнего» — реплей/дубль.
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 20, 0, 7, false);
        let mut audio = RtpSource::new(target, 0);
        assert!(audio.accept(target, &packet).is_some());
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 21, 160, 7, false);
        assert!(audio.accept(target, &packet).is_some());
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 20, 0, 7, false);
        assert!(audio.accept(target, &packet).is_none(), "аудио reorder не принимает");
    }

    #[test]
    fn rtp_source_rejects_replay_ssrc_change_and_invalid_padding() {
        let target = "127.0.0.1:5000".parse().unwrap();
        let mut source = RtpSource::new(target, 0);
        let mut packet = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, u16::MAX, 0, 42, false);
        assert!(source.accept(target, &packet).is_some());
        assert!(source.accept(target, &packet).is_none(), "duplicate RTP must not be replayed into mixer");
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 0, 160, 42, false);
        assert!(source.accept(target, &packet).is_some(), "sequence wrap is valid");
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 1, 320, 43, false);
        assert!(source.accept(target, &packet).is_none(), "SSRC is pinned to the established source");
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 1, 320, 42, false);
        packet[0] |= 0x20;
        packet[171] = 0;
        assert!(source.accept(target, &packet).is_none(), "zero padding size is malformed");
    }

    #[test]
    fn latched_inbound_source_accepts_one_nat_port_then_pins_it() {
        let advertised: std::net::SocketAddr = "192.0.2.1:40000".parse().unwrap();
        let nat: std::net::SocketAddr = "198.51.100.7:62000".parse().unwrap();
        let attacker: std::net::SocketAddr = "203.0.113.9:5000".parse().unwrap();
        let mut source = RtpSource::new_latched(advertised, 0);
        let mut packet = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 1, 0, 42, false);
        assert!(source.accept(nat, &packet).is_some(), "first NAT packet latches");
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 2, 160, 42, false);
        assert!(source.accept(attacker, &packet).is_none(), "source is pinned after latch");
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 3, 320, 42, false);
        assert!(source.accept(nat, &packet).is_some(), "latched source keeps flowing");
    }

    #[test]
    fn bridge_codecs_opus_and_g728_use_their_native_frame_sizes() {
        let tone = |len: usize, rate: f32| -> Vec<f32> {
            (0..len).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / rate).sin() * 0.2).collect()
        };
        let mut opus = AudioBridgeCodec::new(111, None).expect("Opus bridge codec");
        assert_eq!(opus.frame_samples(), 960);
        assert_eq!(opus.timestamp_step(), 960);
        let packet = opus.encode(&tone(960, 48_000.0));
        assert!(!packet.is_empty() && packet.len() <= 1275);
        assert_eq!(opus.decode(111, &packet).expect("Opus decode").len(), 960);

        let mut g728 = AudioBridgeCodec::new(15, None).expect("G.728 bridge codec");
        assert_eq!(g728.frame_samples(), 160);
        assert_eq!(g728.timestamp_step(), 160);
        let packet = g728.encode(&tone(160, 8_000.0));
        assert_eq!(packet.len(), 40);
        assert_eq!(g728.decode(15, &packet).expect("G.728 decode").len(), 160);
    }

    #[test]
    fn opus_conceal_produces_plc_frames_and_bounds_long_gaps() {
        let mut codec = AudioBridgeCodec::new(111, None).expect("Opus bridge codec");
        let pcm: Vec<f32> =
            (0..960).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 48_000.0).sin() * 0.2).collect();
        let packet = codec.encode(&pcm);
        assert!(!packet.is_empty());
        assert_eq!(codec.conceal(1, &packet).len(), 960, "one lost packet = one Opus frame");
        assert_eq!(codec.conceal(9, &packet).len(), 3 * 960, "long outages are capped at 3 PLC frames");
        // G.711/G.728 have no loss-concealment path in this bridge.
        let mut g711 = AudioBridgeCodec::new(0, None).expect("PCMU");
        assert!(g711.conceal(1, &[0u8; 160]).is_empty());
    }

    #[test]
    fn bridge_codec_g723_uses_30ms_packets_and_240_samples() {
        let mut codec = AudioBridgeCodec::new(4, None).expect("G.723.1 bridge codec");
        assert_eq!(codec.frame_samples(), 160, "MCU tick stays 20 ms");
        assert_eq!(codec.packet_samples(), 240, "one RTP packet is 30 ms");
        assert_eq!(codec.timestamp_step(), 240);
        let pcm: Vec<f32> =
            (0..240).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin() * 0.2).collect();
        let packet = codec.encode(&pcm);
        assert_eq!(packet.len(), 24);
        assert_eq!(codec.decode(4, &packet).expect("G.723.1 decode").len(), 240);
    }

    #[test]
    fn dynamic_pcma_keeps_alaw_identity_independent_of_pt96() {
        let pcm = [0.25f32; 160];
        let mut pcma = AudioBridgeCodec::new(96, Some("PCMA")).expect("dynamic PCMA");
        let mut pcmu = AudioBridgeCodec::new(96, Some("PCMU")).expect("dynamic PCMU");
        let alaw = pcma.encode(&pcm);
        let ulaw = pcmu.encode(&pcm);
        assert_eq!(alaw.len(), 160);
        assert_ne!(alaw, ulaw, "dynamic PCMA must not be encoded with the µ-law table");
        let decoded = pcma.decode(96, &alaw).expect("decode dynamic PCMA");
        assert_eq!(decoded.len(), 160);
        assert!(decoded.iter().all(|sample| (sample - 0.25).abs() < 0.04), "{decoded:?}");
    }

    #[test]
    fn empty_rtp_payload_is_accepted_only_for_opus() {
        let target: std::net::SocketAddr = "192.0.2.1:40000".parse().unwrap();
        let mut packet = [0u8; 12];
        vks_gateway_sip::write_rtp_header(&mut packet, 111, 1, 0, 42, false);
        let mut g711 = RtpSource::new(target, 0);
        let mut g711_packet = [0u8; 12];
        vks_gateway_sip::write_rtp_header(&mut g711_packet, 0, 1, 0, 42, false);
        assert!(g711.accept(target, &g711_packet).is_none(), "G.711 needs a payload");
        let mut opus = RtpSource::new(target, 111);
        assert!(opus.accept(target, &packet).is_none(), "empty allowed only after opt-in");
        opus.allow_empty_payloads();
        assert!(opus.accept(target, &packet).is_some(), "Opus PLC payload");
    }

    #[test]
    fn accepts_alternate_g711_payload_and_reports_actual_pt() {
        let target: std::net::SocketAddr = "192.0.2.1:40000".parse().unwrap();
        let mut source = RtpSource::new(target, 0);
        let mut packet = [0u8; 172];
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 8, 1, 0, 42, false);
        let accepted = source.accept(target, &packet).expect("PCMA on a PCMU answer must be accepted");
        assert_eq!(accepted.payload_type, 8);
        packet[1] = 0;
        vks_gateway_sip::write_rtp_header(&mut packet[..12], 0, 2, 160, 42, false);
        assert_eq!(source.accept(target, &packet).unwrap().payload_type, 0);
    }

    #[test]
    fn telephone_event_parser_returns_digit_and_ignores_redundancy() {
        // event=5, E=0, volume=10, duration=160.
        assert_eq!(parse_telephone_event(&[5, 0x0a, 0x00, 0xa0]), Some(5));
        // End packet: E=1.
        assert_eq!(parse_telephone_event(&[5, 0x8a, 0x05, 0x60]), Some(5));
        // Redundant encoding appended: first event wins.
        assert_eq!(parse_telephone_event(&[12, 0x0a, 0x00, 0xa0, 9, 0x0a, 0x00, 0xa0]), Some(12));
        // Star/hash map to 10/11 per RFC 4733 §3.2.
        assert_eq!(parse_telephone_event(&[10, 0x0a, 0x00, 0xa0]), Some(10));
        assert_eq!(parse_telephone_event(&[11, 0x0a, 0x00, 0xa0]), Some(11));
        // Out-of-range event and truncated packets are rejected.
        assert_eq!(parse_telephone_event(&[16, 0x0a, 0x00, 0xa0]), None);
        assert_eq!(parse_telephone_event(&[5, 0x0a, 0x00]), None);
    }

    #[test]
    fn source_accepts_telephone_event_only_when_negotiated() {
        let target: std::net::SocketAddr = "192.0.2.10:4000".parse().unwrap();
        let dtmf_packet = {
            let packet = vec![0x80, 101, 0, 1, 0, 0, 0, 1, 0, 0, 0, 5, 5, 0x0a, 0x00, 0xa0];
            packet
        };
        let mut source = RtpSource::new(target, 0);
        assert!(source.accept(target, &dtmf_packet).is_none(), "DTMF must not pass before negotiation");
        source.accept_dtmf_payload_type(Some(101));
        let accepted = source.accept(target, &dtmf_packet).expect("negotiated DTMF passes");
        assert_eq!(accepted.payload_type, 101);
        assert_eq!(parse_telephone_event(accepted.payload), Some(5));
        // A PT that is neither audio nor DTMF is still dropped.
        let mut other = dtmf_packet.clone();
        other[1] = 111;
        assert!(source.accept(target, &other).is_none());
    }

    fn rtp_packet(payload_type: u8, ssrc: u32) -> Vec<u8> {
        let mut packet = [0u8; 12];
        vks_gateway_sip::write_rtp_header(&mut packet, payload_type, 1, 0, ssrc, false);
        packet.to_vec()
    }

    #[test]
    fn reject_reason_distinguishes_pt_source_and_ssrc() {
        let source: std::net::SocketAddr = "192.0.2.10:4000".parse().unwrap();
        let other: std::net::SocketAddr = "192.0.2.11:4000".parse().unwrap();
        let header = |pt: u8, ssrc: u32| vks_gateway_sip::parse_rtp_header(&rtp_packet(pt, ssrc));

        assert_eq!(reject_reason(0, None, source, source, None, None), "malformed_rtp");
        assert_eq!(
            reject_reason(0, None, source, source, None, header(96, 1)),
            "payload_type_mismatch",
            "динамический PT при G.711-аудио"
        );
        assert_ne!(
            reject_reason(0, None, source, source, None, header(8, 1)),
            "payload_type_mismatch",
            "PCMU/PCMA interchange — не мисматч"
        );
        assert_ne!(
            reject_reason(0, Some(101), source, source, None, header(101, 1)),
            "payload_type_mismatch",
            "telephone-event идёт своим PT"
        );
        assert_eq!(reject_reason(0, None, other, source, None, header(0, 1)), "source_changed");
        assert_eq!(reject_reason(0, None, source, source, Some(7), header(0, 9)), "ssrc_changed");
    }

    /// RFC 4733-получатель: тон длится от первого пакета до первого `E=1`;
    /// длительность берётся из поля duration этого пакета (в 8-кГц сэмплах).
    fn receiver_tone_ms(packets: &[[u8; 16]]) -> u32 {
        for packet in packets {
            let duration = u16::from_be_bytes([packet[14], packet[15]]);
            if packet[13] & 0x80 != 0 {
                return u32::from(duration) / 8;
            }
        }
        0
    }

    #[test]
    fn telephone_event_tone_is_200_ms_marker_first_end_last() {
        let packets: Vec<[u8; 16]> = (0..DTMF_DURATIONS.len())
            .map(|step| dtmf_event_packet(&DTMF_DURATIONS, 101, 7 + step as u16, 0, 42, 5, step))
            .collect();
        assert_eq!(packets.len(), 10, "10 пакетов по 20 мс");
        for (step, packet) in packets.iter().enumerate() {
            assert_eq!(packet[0] >> 6, 2, "RTP v2");
            assert_eq!(packet[12], 5, "event=5");
            let marker = packet[1] & 0x80 != 0;
            assert_eq!(marker, step == 0, "маркер только на первом пакете (step {step})");
            let end = packet[13] & 0x80 != 0;
            assert_eq!(end, step + 1 == packets.len(), "E=1 только на последнем пакете (step {step})");
            let duration = u16::from_be_bytes([packet[14], packet[15]]);
            assert_eq!(duration, 160 * (step as u16 + 1), "+20 мс на пакет");
            assert_eq!(u32::from_be_bytes(packet[4..8].try_into().unwrap()), 0, "ts0 фиксирован");
        }
        assert_eq!(receiver_tone_ms(&packets), 200, "получатель видит 200 мс, а не 60");
    }

    #[test]
    fn renumbered_dynamic_payload_resolves_by_codec_identity() {
        // G.722 offered as PT9 but answered on PT96: identity wins, the wire PT
        // is remembered for the payload header.
        let codec = AudioBridgeCodec::new(96, Some("G722")).expect("G722 identity must resolve");
        assert_eq!(codec.frame_samples(), 320);
        assert!(AudioBridgeCodec::new(96, None).is_none(), "bare dynamic PT is ambiguous");
        assert!(AudioBridgeCodec::new(96, Some("G729")).is_none(), "unbridgeable identity must fail");
        // Static and dynamic G.726-32 agree on the same codec.
        assert_eq!(AudioBridgeCodec::new(2, Some("G726-32")).map(|c| c.frame_samples()), Some(160));
    }

    #[test]
    fn telephone_event_end_bit_on_third_packet_would_truncate_tone() {
        let packets: Vec<[u8; 16]> = (0..10)
            .map(|step| {
                let mut packet = dtmf_event_packet(&DTMF_DURATIONS, 101, step as u16, 0, 42, 5, step);
                // Регресс-контроль: старое поведение (E=1 с третьего пакета).
                packet[13] = if step >= 2 { 0x8a } else { 0x0a };
                packet
            })
            .collect();
        assert_eq!(receiver_tone_ms(&packets), 60, "старый баг давал 60 мс");
    }
}

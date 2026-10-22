//! G.711 payload conversion at the negotiated RTP boundary.

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

pub(super) async fn run_audio_bridge(mut session: super::RtpBridgeSession) {
    let Some(target) = session
        .remote_media_ip
        .parse::<std::net::IpAddr>()
        .ok()
        .map(|ip| std::net::SocketAddr::new(ip, session.remote_audio_port))
    else {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    };
    if ![0, 8].contains(&session.audio_payload_type) || target.port() == 0 {
        session.media_stats.audio_codec_running.store(false, Relaxed);
        return;
    }
    let mut validator = if session.source_latch {
        RtpSource::new_latched(target, session.audio_payload_type)
    } else {
        RtpSource::new(target, session.audio_payload_type)
    };
    session.media_stats.audio_codec_running.store(true, Relaxed);
    let mut egress = target;
    let mut queue = std::collections::VecDeque::<f32>::with_capacity(960);
    let mut dtmf = std::collections::VecDeque::<u8>::new();
    let mut dtmf_active: Option<(u8, usize, u32)> = None;
    let mut dtmf_gap = 0;
    let mut sequence = 1u16;
    let mut timestamp = 0u32;
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
                let Some(packet) = validator.accept(source, &incoming[..count]) else {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed);
                    if rejected_logs < 3 {
                        rejected_logs += 1;
                        let header = vks_gateway_sip::parse_rtp_header(&incoming[..count]);
                        tracing::warn!(
                            participant = %session.participant_id_str,
                            expected_payload = session.audio_payload_type,
                            received_payload = header.map(|(payload, _, _, _, _)| payload),
                            source = %source,
                            "audio RTP rejected by source validator (codec/PT mismatch)"
                        );
                    }
                    continue;
                };
                if session.source_latch {
                    egress = source;
                }
                if packet.payload.len() > 480 {
                    session.media_stats.rejected_packets.fetch_add(1, Relaxed); continue;
                }
                session.media_stats.audio_rx_packets.fetch_add(1, Relaxed);
                session.media_stats.last_rx_ms.store(
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64,
                    Relaxed,
                );
                session.media_stats.audio_rx_bytes.fetch_add(count as u64, Relaxed);
                if session.controls.audio_tx_muted.load(Relaxed) { queue.clear(); continue; }
                while queue.len() + packet.payload.len() > 960 { queue.pop_front(); }
                queue.extend(packet.payload.iter().map(|byte| decode_sample(packet.payload_type, *byte)));
            }
            _ = ticker.tick() => {
                let mut input = [0.0; 160];
                if session.controls.audio_tx_muted.load(Relaxed) { queue.clear(); }
                else {
                    let volume = session.controls.volume_tx.load(Relaxed);
                    for sample in &mut input { *sample = gain(queue.pop_front().unwrap_or(0.0), volume); }
                }
                let mut upsampled = [0.0; 960];
                vks_gateway_sip::upsample_8k_to_48k(&input, &mut upsampled);
                session.mcu_engine.set_audio_slice(session.room_id, session.participant_id_val, &upsampled);
                session.mcu_engine.tick_audio_at(session.room_id, media_clock_ms());
                let mut output = [0.0; 160];
                if !session.controls.audio_rx_muted.load(Relaxed)
                    && session.mcu_engine.copy_participant_audio(session.room_id, session.participant_id_val, &mut mixed) {
                    vks_gateway_sip::downsample_48k_to_8k(&mixed, &mut output);
                }
                let mut packet = [0; 172];
                vks_gateway_sip::write_rtp_header(&mut packet[..12], session.audio_payload_type, sequence, timestamp, ssrc, false);
                sequence = sequence.wrapping_add(1);
                let volume = session.controls.volume_rx.load(Relaxed);
                for (encoded, sample) in packet[12..].iter_mut().zip(output) { *encoded = encode_sample(session.audio_payload_type, gain(sample, volume)); }
                if let Ok(count) = session.rtp_socket.send_to(&packet, egress).await {
                    session.media_stats.audio_tx_packets.fetch_add(1, Relaxed); session.media_stats.audio_tx_bytes.fetch_add(count as u64, Relaxed);
                }
                if let Some(payload_type) = session.dtmf_payload_type {
                    if dtmf_gap > 0 { dtmf_gap -= 1; }
                    else {
                        if dtmf_active.is_none() { dtmf_active = dtmf.pop_front().map(|event| (event, 0, timestamp)); }
                        if let Some((event, step, started)) = dtmf_active {
                            let duration = [160u16, 320, 480, 480, 480][step];
                            let mut event_packet = [0; 16];
                            vks_gateway_sip::write_rtp_header(&mut event_packet[..12], payload_type, sequence, started, ssrc, step == 0);
                            sequence = sequence.wrapping_add(1); event_packet[12] = event; event_packet[13] = if step >= 2 { 0x8a } else { 0x0a };
                            event_packet[14..].copy_from_slice(&duration.to_be_bytes());
                            let _ = session.rtp_socket.send_to(&event_packet, egress).await;
                            if step == 4 { dtmf_active = None; dtmf_gap = 2; } else { dtmf_active = Some((event, step + 1, started)); }
                        }
                    }
                } else { dtmf.clear(); }
                timestamp = timestamp.wrapping_add(160);
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
    tracing::info!(participant = %session.participant_id_str, "G711 RTP bridge stopped");
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
    ssrc: Option<u32>,
    sequence: Option<u16>,
    /// Входящий вызов: первый валидный RTP-latch с SDP-target разрешён,
    /// дальше источник фиксируется (NAT/перемаршрутизация порта).
    latch: bool,
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
    pub(super) fn new(target: std::net::SocketAddr, payload_type: u8) -> Self {
        Self { target, payload_type, ssrc: None, sequence: None, latch: false }
    }

    /// UAS-вызов: разрешает первый валидный пакет с другого source и
    /// фиксирует его; последующие источники отбрасываются.
    pub(super) fn new_latched(target: std::net::SocketAddr, payload_type: u8) -> Self {
        Self { latch: true, ..Self::new(target, payload_type) }
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
        if pt != self.payload_type && !g711_interchange {
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
        if end <= offset || self.ssrc.is_some_and(|current| current != ssrc) {
            return None;
        }
        if self.sequence.is_some_and(|last| {
            let distance = seq.wrapping_sub(last);
            distance == 0 || distance >= 0x8000
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

#[cfg(test)]
mod tests {
    use super::*;

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
}

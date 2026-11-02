//! Opus (RFC 6716 / RFC 7587) RTP payload conversion.
//!
//! The MCU mix is 48 kHz, which is Opus' native rate, so this codec needs no
//! resampling: one 20 ms frame is exactly 960 mono samples and the RTP
//! timestamp advances by 960.  `rusty-opus` is a pure-Rust, allocation-free
//! streaming implementation (BSD-3-Clause, no FFI); we keep it behind this
//! wrapper so the bridge never depends on its API directly.

use rusty_opus::{Application, OpusDecoder, OpusEncoder};

/// 20 ms at 48 kHz — the MCU tick and the preferred packet duration.
pub const FRAME_SAMPLES: usize = 960;
/// Longest legal Opus packet (120 ms at 48 kHz).  RFC 7587 allows 2.5–120 ms;
/// decoding into the maximum lets a peer choose any of them.
pub const MAX_FRAME_SAMPLES: usize = 5760;
/// Smallest legal Opus packet duration (2.5 ms at 48 kHz).
const MIN_FRAME_SAMPLES: usize = 120;

fn is_legal_frame_len(samples: usize) -> bool {
    (MIN_FRAME_SAMPLES..=MAX_FRAME_SAMPLES).contains(&samples) && samples.is_multiple_of(MIN_FRAME_SAMPLES)
}
/// RFC 7587: one 20 ms Opus packet must fit into 1275 octets (`maxaveragebitrate`
/// safety limit; the codec itself can emit up to 1275 for 20 ms).
pub const MAX_PAYLOAD: usize = 1275;
/// Mono encoder/decoder; the SDP still advertises `opus/48000/2` because the
/// channel count there is the codec's maximum, with `stereo=0` selecting mono.
pub const CHANNELS: usize = 1;

pub struct OpusCodec {
    encoder: OpusEncoder,
    decoder: OpusDecoder,
}

impl OpusCodec {
    #[must_use]
    pub fn new() -> Option<Self> {
        let mut encoder = OpusEncoder::new(48_000, CHANNELS, Application::Voip).ok()?;
        encoder.bitrate_bps = 64_000;
        encoder.complexity = 6;
        encoder.use_inband_fec = true;
        encoder.use_dtx = true;
        encoder.packet_loss_perc = 10;
        let decoder = OpusDecoder::new(48_000, CHANNELS).ok()?;
        Some(Self { encoder, decoder })
    }

    /// Encodes one 960-sample mono frame directly into `out`. Returns the number of bytes written.
    pub fn encode_into(&mut self, pcm: &[f32], out: &mut [u8]) -> Option<usize> {
        if pcm.len() < FRAME_SAMPLES || out.len() < MAX_PAYLOAD {
            return None;
        }
        self.encoder.encode(&pcm[..FRAME_SAMPLES], FRAME_SAMPLES, out).ok()
    }

    /// Encodes one 960-sample mono frame.  Returns `None` on a codec error
    /// (never a truncated packet: the caller drops the frame instead).
    #[must_use]
    pub fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        let mut packet = vec![0u8; MAX_PAYLOAD];
        let len = self.encode_into(pcm, &mut packet)?;
        packet.truncate(len);
        Some(packet)
    }

    /// Recovers the previous frame from the in-band FEC carried by `next`
    /// (RFC 6716 §4.5.3).  Used by the bridge when exactly one packet is lost;
    /// the recovered duration matches the packet that carried the FEC.
    #[must_use]
    pub fn decode_fec(&mut self, next: &[u8]) -> Option<Vec<f32>> {
        if next.is_empty() || next.len() > MAX_PAYLOAD {
            return None;
        }
        // FEC always conceals the previous 20 ms frame; variable-duration
        // packets fall back to PLC in the bridge.
        let mut pcm = vec![0.0f32; FRAME_SAMPLES];
        let produced = self.decoder.decode_fec(next, FRAME_SAMPLES, &mut pcm).ok()?;
        (produced == FRAME_SAMPLES).then_some(pcm)
    }

    /// Decodes one Opus packet directly into `out`. Returns the number of samples decoded.
    pub fn decode_into(&mut self, payload: &[u8], out: &mut [f32]) -> Option<usize> {
        if payload.len() > MAX_PAYLOAD || out.len() < MAX_FRAME_SAMPLES {
            return None;
        }
        if payload.is_empty() {
            let produced = self.decoder.decode(&[], FRAME_SAMPLES, out).ok()?;
            return (produced == FRAME_SAMPLES).then_some(produced);
        }
        let produced = self.decoder.decode(payload, MAX_FRAME_SAMPLES, out).ok()?;
        if !is_legal_frame_len(produced) {
            return None;
        }
        Some(produced)
    }

    /// Decodes one Opus packet of any RFC 7587 duration (2.5–120 ms).  An
    /// empty payload is not a valid Opus packet; it is accepted defensively as
    /// a PLC indication for a lost frame and produces one 20 ms PLC frame.
    #[must_use]
    pub fn decode(&mut self, payload: &[u8]) -> Option<Vec<f32>> {
        let mut pcm = vec![0.0f32; MAX_FRAME_SAMPLES];
        let produced = self.decode_into(payload, &mut pcm)?;
        pcm.truncate(produced);
        Some(pcm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize, hz: f32) -> Vec<f32> {
        (0..samples).map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / 48_000.0).sin() * 0.25).collect()
    }

    #[test]
    fn roundtrip_has_correct_length_and_audible_energy() {
        let mut codec = OpusCodec::new().expect("opus codec");
        let pcm = tone(FRAME_SAMPLES, 440.0);
        let packet = codec.encode(&pcm).expect("encode");
        assert!(!packet.is_empty() && packet.len() <= MAX_PAYLOAD, "{}", packet.len());
        let decoded = codec.decode(&packet).expect("decode");
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let energy: f32 = decoded.iter().map(|sample| sample * sample).sum();
        assert!(energy > 1.0, "decoded silence: {energy}");
    }

    #[test]
    fn empty_payload_is_plc_and_oversize_is_rejected() {
        let mut codec = OpusCodec::new().expect("opus codec");
        let plc = codec.decode(&[]).expect("empty payload must produce PLC");
        assert_eq!(plc.len(), FRAME_SAMPLES);
        assert!(codec.decode(&vec![0u8; MAX_PAYLOAD + 1]).is_none());
    }

    #[test]
    fn decodes_all_legal_opus_packet_durations() {
        let mut encoder = rusty_opus::OpusEncoder::new(48_000, 1, rusty_opus::Application::Audio).unwrap();
        let mut codec = OpusCodec::new().expect("opus codec");
        for samples in [480usize, 1920, 3840, 4800] {
            let pcm = tone(samples, 440.0);
            let mut packet = vec![0u8; MAX_PAYLOAD];
            let len = encoder.encode(&pcm, samples, &mut packet).unwrap();
            let decoded = codec.decode(&packet[..len]).expect("non-20ms Opus packet");
            assert_eq!(decoded.len(), samples, "{samples} samples");
        }
    }
}

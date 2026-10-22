//! G.723.1 dual-rate speech codec over RTP (RFC 3551 §4.5.3, PT4).
//!
//! 30 ms / 240 samples at 8 kHz.  The bridge uses the 6.3 kbit/s MP-MLQ
//! (24-octet) rate; the decoder also accepts 5.3 kbit/s ACELP (20 octets) so a
//! peer selecting the same PT with the other rate is not dropped.

use oxideav_g7231::encoder::{SpecEncoder, SynthesisState};
use oxideav_g7231::linepack::PackedRate;

/// 30 ms at 8 kHz.
pub const FRAME_SAMPLES: usize = 240;
/// MP-MLQ 6.3 kbit/s.
pub const PAYLOAD_BYTES: usize = 24;
/// ACELP 5.3 kbit/s.
pub const ACELP_PAYLOAD_BYTES: usize = 20;

pub struct G7231Codec {
    encoder: SpecEncoder,
    decoder: SynthesisState,
}

impl G7231Codec {
    #[must_use]
    pub fn new() -> Self {
        Self { encoder: SpecEncoder::new(PackedRate::High), decoder: SynthesisState::new() }
    }

    #[must_use]
    pub fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        if pcm.len() < FRAME_SAMPLES {
            return None;
        }
        let mut input = [0i16; FRAME_SAMPLES];
        for (slot, sample) in input.iter_mut().zip(&pcm[..FRAME_SAMPLES]) {
            *slot = (sample * 32768.0).round().clamp(-32768.0, 32767.0) as i16;
        }
        let payload = self.encoder.encode_frame(&input);
        (payload.len() == PAYLOAD_BYTES).then_some(payload)
    }

    #[must_use]
    pub fn decode(&mut self, payload: &[u8]) -> Option<Vec<f32>> {
        let decoded = match payload.len() {
            PAYLOAD_BYTES => self.decoder.decode_mpmlq(payload).ok()?,
            ACELP_PAYLOAD_BYTES => self.decoder.decode_acelp(payload).ok()?,
            _ => return None,
        };
        Some(decoded.iter().map(|sample| f32::from(*sample) / 32768.0).collect())
    }
}

impl Default for G7231Codec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_is_240_samples_and_24_octets() {
        let mut codec = G7231Codec::new();
        let pcm: Vec<f32> =
            (0..FRAME_SAMPLES).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin() * 0.25).collect();
        let payload = codec.encode(&pcm).expect("encode");
        assert_eq!(payload.len(), PAYLOAD_BYTES);
        let decoded = codec.decode(&payload).expect("decode");
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let energy: f32 = decoded.iter().map(|sample| sample * sample).sum();
        assert!(energy > 0.01, "decoded silence: {energy}");
        // Truncated/oversized payloads are refused before the decoder.
        assert!(codec.decode(&payload[..PAYLOAD_BYTES - 1]).is_none());
        assert!(codec.decode(&[0u8; 25]).is_none());
    }

    #[test]
    fn decodes_acelp_20_octet_frames_too() {
        let mut encoder = SpecEncoder::new(PackedRate::Low);
        let pcm: [i16; FRAME_SAMPLES] =
            std::array::from_fn(|i| ((i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin() * 8000.0) as i16);
        let payload = encoder.encode_frame(&pcm);
        assert_eq!(payload.len(), ACELP_PAYLOAD_BYTES);
        let mut codec = G7231Codec::new();
        let decoded = codec.decode(&payload).expect("ACELP decode");
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        assert!(decoded.iter().any(|sample| sample.abs() > 0.001));
    }
}

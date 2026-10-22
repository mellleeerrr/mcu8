//! G.722 (ITU-T G.722, 64 kbit/s SB-ADPCM) encoder/decoder wrapper.
//!
//! RTP carries G.722 with clock rate 8000 (RFC 3551 §4.5.2) even though the
//! codec itself samples at 16 kHz: one octet per 8 kHz tick, two PCM samples
//! per octet, 20 ms = 160 octets = 320 samples. Callers must keep that
//! distinction (timestamp +160, payload 160 bytes) or Polycom/TrueConf will
//! reject the stream. The 160-octet / 20 ms frame is the only cadence this
//! adapter emits; 320 bytes would be a 40 ms packet and is never produced.

use oxideav_g722::{Decoder, Encoder, Mode};

/// Stateful G.722 codec pair for one direction of one call.
#[derive(Debug)]
pub struct G722Codec {
    encoder: Encoder,
    decoder: Decoder,
}

impl Default for G722Codec {
    fn default() -> Self {
        Self::new()
    }
}

impl G722Codec {
    /// Creates a 64 kbit/s G.722 codec pair.
    #[must_use]
    pub fn new() -> Self {
        Self { encoder: Encoder::new(), decoder: Decoder::new(Mode::Mode1) }
    }

    /// Encodes 16 kHz PCM samples (14-bit uniform scale) into G.722 octets.
    pub fn encode(&mut self, samples: &[i32]) -> Vec<u8> {
        self.encoder.encode(samples)
    }

    /// Decodes G.722 octets into 16 kHz PCM samples (14-bit uniform scale).
    pub fn decode(&mut self, payload: &[u8]) -> Vec<i32> {
        let mut out = vec![0_i32; payload.len() * 2];
        self.decoder.decode_into(payload, &mut out);
        out
    }
}

/// Converts 16-bit signed PCM to the 14-bit uniform scale used by G.722.
#[must_use]
pub fn pcm16_to_g722(sample: i16) -> i32 {
    i32::from(sample) >> 2
}

/// Converts a 14-bit G.722 PCM sample back to 16-bit signed PCM.
#[must_use]
pub fn g722_to_pcm16(sample: i32) -> i16 {
    (sample.clamp(-8192, 8191) << 2) as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_preserves_tone_energy() {
        let mut codec = G722Codec::new();
        let mut input = Vec::with_capacity(320);
        for i in 0..320 {
            let value = (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 16_000.0).sin();
            input.push((value * 6000.0) as i32);
        }
        let encoded = codec.encode(&input);
        assert_eq!(encoded.len(), 160, "20 ms G.722 must be 160 octets");
        let decoded = codec.decode(&encoded);
        assert_eq!(decoded.len(), 320);
        let before: f64 = input.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        let after: f64 = decoded.iter().map(|&s| f64::from(s) * f64::from(s)).sum();
        assert!(after > before * 0.3, "decoded energy collapsed: {before} -> {after}");
    }

    #[test]
    fn pcm16_scale_roundtrip_is_stable() {
        for sample in [-32768_i16, -1000, 0, 1000, 32767] {
            let encoded = pcm16_to_g722(sample);
            assert_eq!(g722_to_pcm16(encoded), sample & !3);
        }
    }
}

//! G.728 LD-CELP (16 kbit/s) RTP payload conversion (RFC 3551 §4.5.5).
//!
//! The codec works on 5-sample vectors (0.625 ms, 10-bit index); one 20 ms
//! RTP packet is 32 vectors = 40 octets and 160 samples at the 8 kHz clock.
//! `oxideav-g728` is the MIT, pure-Rust Annex G fixed-point implementation; the
//! wrapper keeps its float API and byte packing out of the bridge.  Official
//! ITU conformance vectors are not staged in this repository, so bit-exactness
//! against them is not independently re-established here.

use oxideav_g728::{Decoder, Encoder, FRAME_LEN};

/// 20 ms at 8 kHz.
pub const FRAME_SAMPLES: usize = 160;
/// 32 vectors × 10 bits.
pub const PAYLOAD_BYTES: usize = 40;
const VECTORS_PER_FRAME: usize = FRAME_SAMPLES / FRAME_LEN;
/// Float codec scale (±4095 per the G.728 fixed-point range).
const SCALE: f64 = 4096.0;

pub struct G728Codec {
    encoder: Encoder,
    decoder: Decoder,
}

impl G728Codec {
    #[must_use]
    pub fn new() -> Self {
        Self { encoder: Encoder::new(), decoder: Decoder::new() }
    }

    #[must_use]
    pub fn encode(&mut self, pcm: &[f32]) -> Option<Vec<u8>> {
        if pcm.len() < FRAME_SAMPLES {
            return None;
        }
        let mut vectors = [[0.0f64; FRAME_LEN]; VECTORS_PER_FRAME];
        for (index, chunk) in pcm[..FRAME_SAMPLES].chunks_exact(FRAME_LEN).enumerate() {
            for (slot, sample) in vectors[index].iter_mut().zip(chunk) {
                *slot = f64::from(*sample) * SCALE;
            }
        }
        let bytes = self.encoder.encode_buffer(&vectors).ok()?;
        (bytes.len() == PAYLOAD_BYTES).then_some(bytes)
    }

    #[must_use]
    pub fn decode(&mut self, payload: &[u8]) -> Option<Vec<f32>> {
        if payload.len() != PAYLOAD_BYTES {
            return None;
        }
        let samples = self.decoder.decode_bytes(payload).ok()?;
        if samples.len() != FRAME_SAMPLES {
            return None;
        }
        Some(samples.into_iter().map(|sample| (sample / SCALE) as f32).collect())
    }
}

impl Default for G728Codec {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_is_160_samples_and_40_octets() {
        let mut codec = G728Codec::new();
        let pcm: Vec<f32> =
            (0..FRAME_SAMPLES).map(|i| (i as f32 * 2.0 * std::f32::consts::PI * 440.0 / 8000.0).sin() * 0.25).collect();
        let payload = codec.encode(&pcm).expect("encode");
        assert_eq!(payload.len(), PAYLOAD_BYTES);
        let decoded = codec.decode(&payload).expect("decode");
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        let energy: f32 = decoded.iter().map(|sample| sample * sample).sum();
        assert!(energy > 0.01, "decoded silence: {energy}");
        assert!(codec.decode(&payload[..PAYLOAD_BYTES - 1]).is_none());
    }
}

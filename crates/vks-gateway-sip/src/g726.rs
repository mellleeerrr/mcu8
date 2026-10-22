//! G.726 ADPCM RTP payloads (RFC 3551 §4.5.4), all four rates.
//!
//! The RTP payload has **no** header: 2/3/4/5-bit code words are packed
//! LSB-first for every rate (`G726-16/-24/-32/-40`), the first code word
//! occupying the least significant free bits of the first octet.  One
//! 20 ms frame is 160 samples → 40/60/80/100 octets; 40 ms is 320 samples
//! → 80/120/160/200 octets.  The RTP clock is 8000 Hz, timestamp +160 for
//! both ptime variants (a 40 ms packet carries two 20 ms ticks).

use oxideav_adpcm::g726::{BitOrder, BitPacker, BitUnpacker, Rate, State};

/// ITU-T G.726 operating rate, mapped to its SDP/RTP payload type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum G726Rate {
    /// 16 kbit/s, 2 bits/sample, PT 107.
    R16,
    /// 24 kbit/s, 3 bits/sample, PT 108.
    R24,
    /// 32 kbit/s, 4 bits/sample, PT 109 (G.721 successor).
    R32,
    /// 40 kbit/s, 5 bits/sample, PT 110.
    R40,
}

impl G726Rate {
    #[must_use]
    pub const fn payload_type(self) -> u8 {
        match self {
            Self::R16 => 107,
            Self::R24 => 108,
            Self::R32 => 109,
            Self::R40 => 110,
        }
    }

    #[must_use]
    pub const fn from_payload_type(payload_type: u8) -> Option<Self> {
        match payload_type {
            107 => Some(Self::R16),
            108 => Some(Self::R24),
            109 => Some(Self::R32),
            110 => Some(Self::R40),
            _ => None,
        }
    }

    /// RFC 3551 encoding name (`G726-16` … `G726-40`).
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::R16 => "G726-16",
            Self::R24 => "G726-24",
            Self::R32 => "G726-32",
            Self::R40 => "G726-40",
        }
    }

    #[must_use]
    pub const fn rate(self) -> Rate {
        match self {
            Self::R16 => Rate::R16,
            Self::R24 => Rate::R24,
            Self::R32 => Rate::R32,
            Self::R40 => Rate::R40,
        }
    }

    #[must_use]
    pub const fn bits(self) -> u8 {
        self.rate().bits()
    }

    #[must_use]
    pub const fn bytes_20ms(self) -> usize {
        20 * self.bits() as usize
    }

    /// Number of octets in one complete code-word group: 1 for the 2/4-bit
    /// rates (4/2 codes), 3 for 24 kbit/s (8 codes), 5 for 40 kbit/s (8 codes).
    #[must_use]
    pub const fn group_bytes(self) -> usize {
        match self.bits() {
            2 | 4 => 1,
            3 => 3,
            _ => 5,
        }
    }

    /// Accept a whole number of code groups between 20 and 40 ms (20/30/40 ms
    /// for standard ptime values).  Partial groups would leave residual bits
    /// in the unpacker and desync the stream.
    #[must_use]
    pub const fn accepts_payload_len(self, len: usize) -> bool {
        len > 0 && len.is_multiple_of(self.group_bytes()) && len <= 2 * self.bytes_20ms()
    }
}

/// Stateful G.726 encoder/decoder pair for one RTP direction.
pub struct G726Codec {
    rate: G726Rate,
    encoder: State,
    decoder: State,
    packer: BitPacker,
    unpacker: BitUnpacker,
}

impl G726Codec {
    #[must_use]
    pub fn new(rate: G726Rate) -> Self {
        // RFC 3551 §4.5.4: LSB-first for all G.726 rates.  The codec
        // crate's `MsbFirst` default and "network/RTP" doc comment are
        // wrong for this payload format; ffmpeg calls this order `g726le`.
        Self {
            rate,
            encoder: State::new(rate.rate()),
            decoder: State::new(rate.rate()),
            packer: BitPacker::new(BitOrder::LsbFirst),
            unpacker: BitUnpacker::new(BitOrder::LsbFirst),
        }
    }

    #[must_use]
    pub const fn rate(&self) -> G726Rate {
        self.rate
    }

    /// Encodes 160 (20 ms) or 320 (40 ms) 16-bit PCM samples.
    #[must_use]
    pub fn encode(&mut self, pcm: &[i16]) -> Vec<u8> {
        let bits = self.rate.bits();
        let mut output = Vec::with_capacity(pcm.len() * bits as usize / 8);
        for &sample in pcm {
            let code = self.encoder.encode_i16(sample);
            self.packer.push(u32::from(code), bits, &mut output);
        }
        // Frames from the bridge are exactly 160/320 samples, so all code
        // groups are complete and this is a no-op; it only protects a future
        // caller that passes a non-aligned final chunk.
        self.packer.flush(&mut output);
        output
    }

    /// Decodes one or more complete code-word groups (20/30/40 ms frames)
    /// into 16-bit PCM.
    #[must_use]
    pub fn decode(&mut self, payload: &[u8]) -> Vec<i16> {
        let bits = self.rate.bits();
        let mut codes = Vec::with_capacity(payload.len() * 8 / bits as usize);
        self.unpacker.feed(payload, bits, &mut codes);
        codes.into_iter().map(|code| self.decoder.decode_i16(code)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(samples: usize, amplitude: f32) -> Vec<i16> {
        (0..samples)
            .map(|index| {
                let phase = 2.0 * std::f32::consts::PI * 440.0 * index as f32 / 8000.0;
                (phase.sin() * amplitude).round() as i16
            })
            .collect()
    }

    #[test]
    fn payload_lengths_match_rfc_3551_for_20_and_40_ms() {
        for (rate, bytes_20ms) in [(G726Rate::R16, 40), (G726Rate::R24, 60), (G726Rate::R32, 80), (G726Rate::R40, 100)]
        {
            let mut codec = G726Codec::new(rate);
            let short = codec.encode(&tone(160, 12_000.0));
            let long = codec.encode(&tone(320, 12_000.0));
            assert_eq!(short.len(), bytes_20ms, "{rate:?} 20 ms");
            assert_eq!(long.len(), 2 * bytes_20ms, "{rate:?} 40 ms");
            assert!(rate.accepts_payload_len(short.len()));
            assert!(rate.accepts_payload_len(long.len()));
            assert!(rate.accepts_payload_len(bytes_20ms * 3 / 2), "30 ms is a complete code-group frame");
            if rate.group_bytes() > 1 {
                assert!(!rate.accepts_payload_len(bytes_20ms + 1), "partial group must be rejected");
            }
            assert!(!rate.accepts_payload_len(2 * bytes_20ms + rate.group_bytes()), "80 ms exceeds bridge cap");
        }
    }

    #[test]
    fn roundtrip_all_rates_is_not_silent() {
        for rate in [G726Rate::R16, G726Rate::R24, G726Rate::R32, G726Rate::R40] {
            let pcm = tone(320, 10_000.0);
            let mut codec = G726Codec::new(rate);
            let encoded = codec.encode(&pcm);
            let decoded = codec.decode(&encoded);
            assert_eq!(decoded.len(), pcm.len(), "{rate:?}");
            let energy: f64 = decoded.iter().map(|sample| f64::from(*sample).powi(2)).sum();
            assert!(energy > 1.0e7, "{rate:?} decoded silence");
        }
    }

    #[test]
    fn g726_32_uses_lsb_first_like_rfc_3551() {
        // RFC 3551 §4.5.4: code 1 occupies bits 0..3 of octet 0, code 2
        // bits 4..7, etc.  1..8 therefore packs to 21 43 65 87.
        let mut packer = BitPacker::new(BitOrder::LsbFirst);
        let mut output = Vec::new();
        for code in 1u32..=8 {
            packer.push(code, 4, &mut output);
        }
        assert_eq!(output, vec![0x21, 0x43, 0x65, 0x87]);
    }

    /// External interoperability probe: FFmpeg's `g726le` is the RFC 3551
    /// LSB-first stream.  `g726` (MSB-first) is AAL2-style and must not be
    /// used.  Ignored by default because it depends on the host ffmpeg.
    #[test]
    #[ignore = "requires ffmpeg; external g726le conformance probe"]
    fn ffmpeg_g726le_decodes_our_payloads_at_all_rates() {
        let ffmpeg = std::process::Command::new("ffmpeg").arg("-version").output();
        if ffmpeg.is_err() {
            eprintln!("ffmpeg not installed; skipping external G.726 probe");
            return;
        }
        let dir = std::env::temp_dir().join(format!("vks-g726-probe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for rate in [G726Rate::R16, G726Rate::R24, G726Rate::R32, G726Rate::R40] {
            let mut codec = G726Codec::new(rate);
            let encoded = codec.encode(&tone(320, 12_000.0));
            let input = dir.join(format!("{}.g726", rate.payload_type()));
            let output = dir.join(format!("{}.s16le", rate.payload_type()));
            std::fs::write(&input, &encoded).unwrap();
            let status = std::process::Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "g726le"])
                .args(["-code_size", &rate.bits().to_string()])
                .args(["-ar", "8000", "-ac", "1", "-i"])
                .arg(&input)
                .args(["-f", "s16le", "-ac", "1"])
                .arg(&output)
                .status()
                .expect("spawn ffmpeg");
            assert!(status.success(), "{rate:?}: ffmpeg rejected our payload");
            let pcm = std::fs::read(&output).unwrap();
            assert_eq!(pcm.len(), 320 * 2, "{rate:?}: 320 samples expected");
            let energy: f64 =
                pcm.chunks_exact(2).map(|bytes| f64::from(i16::from_le_bytes([bytes[0], bytes[1]])).powi(2)).sum();
            assert!(energy > 1.0e7, "{rate:?}: ffmpeg decoded silence");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn payload_type_and_name_mapping_is_total() {
        for rate in [G726Rate::R16, G726Rate::R24, G726Rate::R32, G726Rate::R40] {
            assert_eq!(G726Rate::from_payload_type(rate.payload_type()), Some(rate));
            assert_eq!(rate.name(), format!("G726-{}", rate.bits() * 8));
        }
        assert_eq!(G726Rate::from_payload_type(106), None);
        assert_eq!(G726Rate::from_payload_type(111), None);
    }
}

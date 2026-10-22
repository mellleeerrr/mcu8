//! Raw-frame placeholder types and codec abstractions.
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): `Frame` — placeholder
//! raw-frame types (width, height, opaque byte payload) that stand in for
//! real decoded pixel frames. `VideoEncoder` / `VideoDecoder` — codec
//! abstractions with deterministic mock impls; the real codecs
//! (H.264/VP8, ADR-0004) sit behind the `oxideav` feature.
//!
//! Everything here is `unsafe`-free and fully deterministic.

use thiserror::Error;

/// Canvas the MCU composites onto: fixed 1920x1080 (ARCHITECTURE §2,
/// DEVELOPMENT_PLAN Фаза 3: N video → 1920×1080 canvas).
pub const CANVAS_WIDTH: u32 = 1920;
pub const CANVAS_HEIGHT: u32 = 1080;

/// A raw video frame (placeholder for a decoded pixel frame).
///
/// `data` is opaque bytes: in the mock pipeline it is the raw (unencoded)
/// payload; in the real pipeline it would be decoded pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    /// Native width (px).
    pub width: u32,
    /// Native height (px).
    pub height: u32,
    /// Opaque payload (placeholder for decoded pixels).
    pub data: Vec<u8>,
}

impl Frame {
    /// Whether the buffer is a complete planar YUV 4:2:0 frame (Y, U, V).
    /// Legacy one-byte-per-pixel frames remain supported as grayscale.
    pub fn is_yuv420p(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width.is_multiple_of(2)
            && self.height.is_multiple_of(2)
            && self.data.len() as u64 == self.pixel_count() * 3 / 2
    }
    /// Creates a frame with the given size and payload.
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self { width, height, data }
    }

    /// Creates a frame filled with a single repeated byte value
    /// (deterministic test payload).
    pub fn filled(width: u32, height: u32, value: u8) -> Self {
        Self { width, height, data: vec![value; width as usize * height as usize] }
    }

    /// Pixel count implied by the size (w * h).
    pub fn pixel_count(&self) -> u64 {
        u64::from(self.width) * u64::from(self.height)
    }

    /// Zero-size frame with an empty payload.
    pub fn empty() -> Self {
        Self { width: 0, height: 0, data: Vec::new() }
    }
}

/// Codec error (decode failures, size mismatch, …).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum CodecError {
    /// The decoded payload size does not match the declared frame size.
    #[error("payload size {payload} does not match declared frame size {declared}")]
    SizeMismatch {
        /// Declared pixel/byte size from the header.
        declared: u64,
        /// Actual payload size.
        payload: u64,
    },
    /// The byte sequence is not a valid encoded frame.
    #[error("invalid encoded frame")]
    InvalidFrame,
}

/// Video encoder abstraction (stands in for H.264 / VP8 encoders).
///
/// The mock impl is deterministic: the encoded bytes embed the frame
/// dimensions plus its payload. The real encoder (ADR-0004) sits behind
/// the `oxideav` feature.
pub trait VideoEncoder: Send + Sync + 'static {
    /// Encodes a frame into a byte sequence.
    fn encode(&self, frame: &Frame) -> Vec<u8>;
}

/// Video decoder abstraction (stands in for H.264 / VP8 decoders).
pub trait VideoDecoder: Send + Sync + 'static {
    /// Decodes a byte sequence into a [`Frame`].
    fn decode(&self, bytes: &[u8]) -> Result<Frame, CodecError>;
}

/// Header layout of the mock codec:
///
/// ```text
/// [0..4)  magic   0x56 0x4b 0x53 0x46 ("VKSF")
/// [4..8)  width   u32 LE
/// [8..12) height  u32 LE
/// [12..)  payload (raw frame bytes)
/// ```
const MAGIC: [u8; 4] = [0x56, 0x4b, 0x53, 0x46];
const HEADER_LEN: usize = 12;

/// Deterministic mock encoder: header + payload, byte-for-byte
/// reproducible for the same input.
#[derive(Debug, Clone, Copy, Default)]
pub struct MockVideoEncoder;

impl VideoEncoder for MockVideoEncoder {
    fn encode(&self, frame: &Frame) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + frame.data.len());
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&frame.width.to_le_bytes());
        out.extend_from_slice(&frame.height.to_le_bytes());
        out.extend_from_slice(&frame.data);
        out
    }
}

/// Deterministic mock decoder: the inverse of [`MockVideoEncoder`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MockVideoDecoder;

impl VideoDecoder for MockVideoDecoder {
    fn decode(&self, bytes: &[u8]) -> Result<Frame, CodecError> {
        if bytes.len() < HEADER_LEN || bytes[..4] != MAGIC {
            return Err(CodecError::InvalidFrame);
        }
        let width = u32::from_le_bytes(bytes[4..8].try_into().expect("4 bytes"));
        let height = u32::from_le_bytes(bytes[8..12].try_into().expect("4 bytes"));
        let payload = &bytes[HEADER_LEN..];
        let declared = u64::from(width) * u64::from(height);
        // The mock payload may carry any size; only reject on mismatch
        // when the payload is non-empty and disagrees with w*h.
        if !payload.is_empty() && declared != 0 && payload.len() as u64 != declared {
            return Err(CodecError::SizeMismatch { declared, payload: payload.len() as u64 });
        }
        Ok(Frame { width, height, data: payload.to_vec() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_is_deterministic() {
        let enc = MockVideoEncoder;
        let f = Frame::filled(4, 3, 7);
        assert_eq!(enc.encode(&f), enc.encode(&f));
    }

    #[test]
    fn roundtrip_preserves_size_and_payload() {
        let f = Frame::filled(64, 48, 0xAB);
        let enc = MockVideoEncoder;
        let dec = MockVideoDecoder;
        let decoded = dec.decode(&enc.encode(&f)).unwrap();
        assert_eq!(decoded.width, f.width);
        assert_eq!(decoded.height, f.height);
        assert_eq!(decoded.data, f.data);
    }

    #[test]
    fn roundtrip_empty_frame() {
        let f = Frame::empty();
        let enc = MockVideoEncoder;
        let dec = MockVideoDecoder;
        let decoded = dec.decode(&enc.encode(&f)).unwrap();
        assert_eq!(decoded, f);
    }

    #[test]
    fn decode_rejects_invalid() {
        let dec = MockVideoDecoder;
        assert_eq!(dec.decode(&[]), Err(CodecError::InvalidFrame));
        assert_eq!(dec.decode(&[1, 2, 3]), Err(CodecError::InvalidFrame));
        let bad_magic = [0x00, 0x00, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0];
        assert_eq!(dec.decode(&bad_magic), Err(CodecError::InvalidFrame));
    }

    #[test]
    fn decode_rejects_size_mismatch() {
        let f = Frame::filled(2, 2, 9); // w*h = 4, payload = 4
        let mut bytes = MockVideoEncoder.encode(&f);
        bytes.push(0xFF); // payload now 5 != 4
        assert_eq!(MockVideoDecoder.decode(&bytes), Err(CodecError::SizeMismatch { declared: 4, payload: 5 }));
    }
}

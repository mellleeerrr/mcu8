//! Raw-frame placeholder types.
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): `Frame` — placeholder raw-frame type
//! (width, height, opaque byte payload) that stands in for real decoded
//! pixel frames.
//!
//! Everything here is `unsafe`-free and fully deterministic.

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

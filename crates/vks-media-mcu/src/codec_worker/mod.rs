//! Isolated codec workers (T06b): FFmpeg subprocesses for codecs that the
//! native gateway/MCU path cannot handle in-process.
//!
//! The worker boundary is a process, not a thread: a malformed RTP payload or
//! a codec crash kills one child, never the MCU. Each worker owns one
//! long-lived FFmpeg pipeline (`pipe:0 -> pipe:1`) and exchanges fixed-size
//! raw frames:
//!
//! * audio — 20 ms of mono `s16le` at 48 kHz (960 samples / 1920 bytes);
//! * video — one `yuv420p` frame of the configured slot resolution.
//!
//! Codec support is probed from the host FFmpeg at startup; a missing
//! encoder/decoder is reported as [`WorkerError::Unavailable`] instead of
//! silently degrading.

pub mod ffmpeg;

pub use ffmpeg::{FfmpegCapabilities, FfmpegWorker, WorkerConfig, WorkerError, WorkerMode, WorkerPool, WorkerStats};

/// Fixed PCM format used across the worker boundary.
pub const WORKER_SAMPLE_RATE: u32 = 48_000;
/// Mono, 16-bit little-endian PCM.
/// 20 ms at 48 kHz.
pub const AUDIO_FRAME_SAMPLES: usize = 960;
/// Bytes of one 20 ms `s16le` frame.
pub const AUDIO_FRAME_BYTES: usize = AUDIO_FRAME_SAMPLES * 2;
/// Product canvas cap shared by encoder and decoder slots (3840×2160 fits).
pub const MAX_VIDEO_WIDTH: u16 = 4096;
/// Product canvas cap shared by encoder and decoder slots.
pub const MAX_VIDEO_HEIGHT: u16 = 2160;

/// Audio codecs that can be bridged through an isolated FFmpeg worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCodec {
    /// MPEG-4 AAC in ADTS framing (`aac`/`adts` in FFmpeg).
    Aac,
    /// MPEG-4 AAC in LATM/LOAS framing (`MP4A-LATM` on the SIP side).
    AacLatm,
    /// ITU-T G.729: FFmpeg can decode raw G.729 but does not encode it.
    G729,
}

impl AudioCodec {
    /// FFmpeg decoder name (LATM bitstreams are decoded by `aac_latm`, while
    /// their container is parsed by the `loas` demuxer).
    #[must_use]
    pub const fn decoder_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::AacLatm => "aac_latm",
            Self::G729 => "g729",
        }
    }

    /// FFmpeg demuxer name for raw RTP payload concatenation.
    #[must_use]
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::Aac => "aac",
            Self::AacLatm => "loas",
            Self::G729 => "g729",
        }
    }

    /// FFmpeg output container for the encoder direction.
    #[must_use]
    pub const fn ffmpeg_container(self) -> &'static str {
        match self {
            Self::Aac => "adts",
            Self::AacLatm => "latm",
            Self::G729 => "g729",
        }
    }

    /// PCM samples one decoded codec frame expands to at 48 kHz.
    #[must_use]
    pub const fn decoded_samples_48k(self) -> usize {
        match self {
            // AAC-LC: 1024 samples at its native rate, resampled to 48 kHz.
            Self::Aac | Self::AacLatm => 1024,
            // G.729: 80 samples / 10 ms at 8 kHz -> 480 at 48 kHz.
            Self::G729 => 480,
        }
    }

    /// FFmpeg encoder name, if the host can encode this codec.
    #[must_use]
    pub const fn ffmpeg_encoder(self) -> Option<&'static str> {
        match self {
            // The native FFmpeg `aac` encoder is always available.
            Self::Aac | Self::AacLatm => Some("aac"),
            // No G.729 encoder in FFmpeg; the worker is decode-only.
            Self::G729 => None,
        }
    }
}

/// Video codecs that can be bridged through an isolated FFmpeg worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VideoCodec {
    /// ITU-T H.261 (QCIF/CIF).
    H261,
    /// ITU-T H.263 baseline.
    H263,
    /// ITU-T H.265 / HEVC (via libx265 on the host).
    H265,
}

impl VideoCodec {
    /// FFmpeg raw demuxer/decoder name.
    #[must_use]
    pub const fn ffmpeg_name(self) -> &'static str {
        match self {
            Self::H261 => "h261",
            Self::H263 => "h263",
            Self::H265 => "hevc",
        }
    }

    /// FFmpeg encoder name.
    #[must_use]
    pub const fn ffmpeg_encoder(self) -> &'static str {
        match self {
            Self::H261 => "h261",
            Self::H263 => "h263",
            Self::H265 => "libx265",
        }
    }

    /// Picture formats accepted by the **FFmpeg encoder** for this codec.
    #[must_use]
    ///
    /// This is an ENCODER constraint, not a decoder one: FFmpeg's `h263`
    /// encoder accepts exactly 128x96/176x144/352x288/704x576/1408x1152,
    /// while its `h263` decoder also handles H.263+ (320x240, 640x480).
    pub const fn supports_encode_size(self, width: u16, height: u16) -> bool {
        if width == 0 || height == 0 || !width.is_multiple_of(2) || !height.is_multiple_of(2) {
            return false;
        }
        match self {
            // H.261 defines exactly QCIF and CIF.
            Self::H261 => matches!((width, height), (176, 144) | (352, 288)),
            // FFmpeg's H.263 encoder accepts exactly these picture formats.
            Self::H263 => matches!((width, height), (128, 96) | (176, 144) | (352, 288) | (704, 576) | (1408, 1152)),
            // HEVC/x265 requires at least 16x16 macroblock granularity.
            // x265 requires at least 16x16; a multiple of 16 is NOT required
            // (x265 accepts 18x18, 100x100, 176x144). Even dimensions are
            // required by the `rawvideo`/`yuv420p` framing, not by the codec.
            Self::H265 => width >= 16 && height >= 16 && width <= MAX_VIDEO_WIDTH && height <= MAX_VIDEO_HEIGHT,
        }
    }

    /// Size sanity for the **decode** direction: even dimensions, non-zero and
    /// within the MCU canvas. The decoder scales the incoming picture to the
    /// slot resolution, so the source size is not constrained by the codec.
    pub const fn supports_decode_size(self, width: u16, height: u16) -> bool {
        width > 0
            && height > 0
            && width.is_multiple_of(2)
            && height.is_multiple_of(2)
            && width <= MAX_VIDEO_WIDTH
            && height <= MAX_VIDEO_HEIGHT
    }
}

/// Per-slot transcode settings: each participant/slot can have its own
/// bitrate and resolution instead of one global profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotConfig {
    /// Target video bitrate in bit/s (audio workers use `audio_bitrate_bps`).
    pub bitrate_bps: u32,
    /// Target video width.
    pub width: u16,
    /// Target video height.
    pub height: u16,
    /// Target video frame rate.
    pub fps: u8,
    /// AAC target bitrate (audio workers only).
    pub audio_bitrate_bps: u32,
    /// Advisory queue depth for the caller's bounded channel (the worker
    /// itself is a synchronous pipe and does not queue frames).
    pub max_queue: usize,
}

impl SlotConfig {
    /// A safe 720p30 default.
    #[must_use]
    pub const fn default_720p() -> Self {
        Self { bitrate_bps: 1_500_000, width: 1280, height: 720, fps: 30, audio_bitrate_bps: 64_000, max_queue: 30 }
    }

    fn validate_geometry(&self) -> Result<(), WorkerError> {
        if self.max_queue == 0 {
            return Err(WorkerError::InvalidConfig("max_queue must be positive".into()));
        }
        if self.width == 0 || self.height == 0 {
            return Err(WorkerError::InvalidConfig("width/height must be positive".into()));
        }
        Ok(())
    }

    /// Validates ranges shared by all codecs.
    pub fn validate(&self) -> Result<(), WorkerError> {
        if !(32_000..=20_000_000).contains(&self.bitrate_bps) {
            return Err(WorkerError::InvalidConfig(format!("video bitrate {} outside 32k..=20M", self.bitrate_bps)));
        }
        if !(1..=60).contains(&self.fps) {
            return Err(WorkerError::InvalidConfig(format!("fps {} outside 1..=60", self.fps)));
        }
        self.validate_geometry()
    }

    /// Validates this slot for an **audio** worker.
    ///
    /// `audio_bitrate_bps` has meaning only here; for a video slot the field
    /// is unused and must not be range-checked.
    pub fn validate_audio(&self) -> Result<(), WorkerError> {
        self.validate()?;
        if !(6_000..=256_000).contains(&self.audio_bitrate_bps) {
            return Err(WorkerError::InvalidConfig(format!(
                "audio bitrate {} outside 6k..=256k",
                self.audio_bitrate_bps
            )));
        }
        Ok(())
    }

    /// Validates this slot for a **video encoder** worker.
    pub fn validate_for(&self, codec: VideoCodec) -> Result<(), WorkerError> {
        self.validate()?;
        if !codec.supports_encode_size(self.width, self.height) {
            return Err(WorkerError::InvalidConfig(format!(
                "{} encoder does not support {}x{}",
                codec.ffmpeg_name(),
                self.width,
                self.height
            )));
        }
        Ok(())
    }

    /// Validates this slot for a **video decoder** worker.
    pub fn validate_decode(&self, codec: VideoCodec) -> Result<(), WorkerError> {
        self.validate_geometry()?;
        if !codec.supports_decode_size(self.width, self.height) {
            return Err(WorkerError::InvalidConfig(format!(
                "{} decoder does not support {}x{}",
                codec.ffmpeg_name(),
                self.width,
                self.height
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_config_rejects_out_of_range_values() {
        let mut slot = SlotConfig::default_720p();
        slot.bitrate_bps = 1_000;
        assert!(slot.validate().is_err());
        slot.bitrate_bps = 1_500_000;
        slot.fps = 0;
        assert!(slot.validate().is_err());
        slot.fps = 30;
        slot.max_queue = 0;
        assert!(slot.validate().is_err());
    }

    #[test]
    fn codec_sizes_are_enforced() {
        let mut slot = SlotConfig::default_720p();
        assert!(slot.validate_for(VideoCodec::H265).is_ok());
        assert!(slot.validate_for(VideoCodec::H261).is_err());
        slot.width = 352;
        slot.height = 288;
        assert!(slot.validate_for(VideoCodec::H261).is_ok());
        slot.width = 320;
        slot.height = 240;
        assert!(slot.validate_for(VideoCodec::H263).is_err(), "FFmpeg rejects 320x240 H.263");
        slot.width = 350;
        slot.height = 286;
        assert!(slot.validate_for(VideoCodec::H263).is_err());
        slot.width = 128;
        slot.height = 96;
        assert!(slot.validate_for(VideoCodec::H263).is_ok());
        assert!(!VideoCodec::H265.supports_encode_size(8, 8));
        assert!(VideoCodec::H265.supports_encode_size(16, 16));
        assert!(VideoCodec::H265.supports_decode_size(3840, 2160));
        assert!(!VideoCodec::H265.supports_decode_size(4098, 2162));
        assert!(!VideoCodec::H265.supports_decode_size(u16::MAX, u16::MAX));
        let mut huge = SlotConfig::default_720p();
        huge.width = u16::MAX;
        huge.height = u16::MAX;
        assert!(huge.validate_decode(VideoCodec::H265).is_err(), "huge decode slot must be rejected before allocating");
    }
}

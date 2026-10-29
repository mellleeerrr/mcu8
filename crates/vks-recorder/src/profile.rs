//! Recording profile: resolution/fps/bitrate model (T09a).
//!
//! Bitrate follows the MCU master plan: `W * H * fps * 0.085` bit/s, i.e.
//! 1280x720@30 -> ~2350 kbps. Values are clamped to the vendor-visible range
//! (64..8192 kbps, 15..60 fps, even frame dimensions).

/// Recorder target profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecorderProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

/// Bits per pixel-second coefficient from `ARCHITECTURE_MASTER_PLAN`.
pub const BITRATE_BPP: f64 = 0.085;
pub const MIN_BITRATE_KBPS: u32 = 64;
pub const MAX_BITRATE_KBPS: u32 = 8192;
pub const MIN_FPS: u32 = 15;
pub const MAX_FPS: u32 = 60;
pub const DEFAULT_WIDTH: u32 = 1280;
pub const DEFAULT_HEIGHT: u32 = 720;
pub const DEFAULT_FPS: u32 = 30;

impl Default for RecorderProfile {
    fn default() -> Self {
        Self { width: DEFAULT_WIDTH, height: DEFAULT_HEIGHT, fps: DEFAULT_FPS }
    }
}

impl RecorderProfile {
    /// Builds a clamped profile; odd dimensions become the previous even value.
    pub fn new(width: u32, height: u32, fps: u32) -> Self {
        Self {
            width: (width.clamp(2, 3840) / 2) * 2,
            height: (height.clamp(2, 2160) / 2) * 2,
            fps: fps.clamp(MIN_FPS, MAX_FPS),
        }
    }

    /// Target bitrate in kbps for this profile.
    pub fn bitrate_kbps(&self) -> u32 {
        let raw = f64::from(self.width) * f64::from(self.height) * f64::from(self.fps) * BITRATE_BPP / 1000.0;
        (raw.round() as u32).clamp(MIN_BITRATE_KBPS, MAX_BITRATE_KBPS)
    }

    /// Seconds of recording that fit into `free_bytes` at this bitrate.
    pub fn estimate_seconds(&self, free_bytes: u64) -> u64 {
        let bytes_per_sec = u64::from(self.bitrate_kbps()) * 1000 / 8;
        if bytes_per_sec == 0 {
            return 0;
        }
        free_bytes / bytes_per_sec
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_matches_the_spec_bitrate() {
        let profile = RecorderProfile::default();
        assert_eq!(profile.bitrate_kbps(), 2350, "720p30 must yield the documented 2350 kbps");
        assert_eq!(profile.estimate_seconds(0), 0);
        // 1 GiB at 2350 kbps ~ 3656 s.
        let hour = profile.estimate_seconds(1024 * 1024 * 1024);
        assert!((3600..3700).contains(&hour), "unexpected estimate {hour}");
    }

    #[test]
    fn profile_clamps_dimensions_and_fps() {
        let profile = RecorderProfile::new(1921, 1081, 120);
        assert_eq!((profile.width, profile.height, profile.fps), (1920, 1080, MAX_FPS));
        assert!(profile.bitrate_kbps() <= MAX_BITRATE_KBPS);
        let tiny = RecorderProfile::new(1, 1, 1);
        assert_eq!((tiny.width, tiny.height), (2, 2));
        assert_eq!(tiny.bitrate_kbps(), MIN_BITRATE_KBPS);
    }
}

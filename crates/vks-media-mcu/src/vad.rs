//! Voice activity detection (speaker detection).
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): RMS + zero-crossing on 20 ms frames
//! (960 samples @ 48 kHz) with a 500 ms debounce (a state change is
//! committed only after the condition has been sustained for 500 ms).
//!
//! Deterministic: the caller drives the frame stream; no wall clock.

/// Frame size in samples: 20 ms @ 48 kHz = 960 samples.
pub const FRAME_SAMPLES: usize = 960;

/// Frame duration in milliseconds (20 ms).
pub const FRAME_MS: u64 = 20;

/// Debounce window in milliseconds (500 ms).
pub const DEBOUNCE_MS: u64 = 500;

/// VAD configuration.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct VadConfig {
    /// RMS threshold above which a frame is considered "loud".
    pub rms_threshold: f32,
    /// Zero-crossing-rate threshold above which a frame is considered
    /// tonal/speech-like.
    pub zcr_threshold: f32,
    /// Debounce window (ms) the candidate state must be sustained before
    /// the VAD state changes.
    pub debounce_ms: u64,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self { rms_threshold: 0.01, zcr_threshold: 0.0, debounce_ms: DEBOUNCE_MS }
    }
}

/// The committed VAD state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VadState {
    /// The speaker is silent (committed).
    #[default]
    Inactive,
    /// The speaker is active (committed).
    Active,
}

/// Voice activity detector.
///
/// Feed 20 ms frames in order via [`Vad::feed_frame`]; read the committed
/// state via [`Vad::state`].
#[derive(Debug, Clone)]
pub struct Vad {
    config: VadConfig,
    /// Committed state.
    state: VadState,
    /// The candidate state under debouncing (None = matching committed).
    pending: Option<VadState>,
    /// Elapsed ms the pending state has been sustained.
    pending_ms: u64,
}

impl Default for Vad {
    fn default() -> Self {
        Self::new(VadConfig::default())
    }
}

impl Vad {
    /// Creates a VAD with the given configuration.
    pub fn new(config: VadConfig) -> Self {
        Self { config, state: VadState::Inactive, pending: None, pending_ms: 0 }
    }

    /// The committed VAD state.
    pub fn state(&self) -> VadState {
        self.state
    }

    /// Whether the VAD state is currently active (speaker is speaking).
    pub fn is_speaking(&self) -> bool {
        self.state == VadState::Active
    }

    /// Classifies one 20 ms frame and advances the debounced state.
    ///
    /// A frame is "loud" when its RMS exceeds `rms_threshold` AND its
    /// zero-crossing rate exceeds `zcr_threshold`. When the classification
    /// disagrees with the committed state it becomes the *pending* state;
    /// after `debounce_ms` of sustained pending state it is committed.
    pub fn feed_frame(&mut self, frame: &[f32]) -> VadState {
        let loud = Self::frame_is_loud(frame, self.config.rms_threshold, self.config.zcr_threshold);
        let candidate = if loud { VadState::Active } else { VadState::Inactive };

        if candidate != self.state {
            // The candidate differs from the committed state: (re)start the
            // debounce timer for it.
            if self.pending != Some(candidate) {
                self.pending = Some(candidate);
                self.pending_ms = FRAME_MS;
            } else {
                self.pending_ms += FRAME_MS;
            }
            if self.pending_ms >= self.config.debounce_ms {
                self.state = candidate;
                self.pending = None;
                self.pending_ms = 0;
            }
        } else {
            // Committed state matches the candidate: cancel any pending
            // opposite candidate (hysteresis — no flap).
            self.pending = None;
            self.pending_ms = 0;
        }
        self.state
    }

    /// True when the frame is "loud": RMS above threshold AND zero-crossing
    /// rate above threshold.
    pub fn frame_is_loud(frame: &[f32], rms_threshold: f32, zcr_threshold: f32) -> bool {
        if frame.is_empty() {
            return false;
        }
        let mut sum_sq = 0.0f32;
        let mut crossings = 0usize;
        for (i, s) in frame.iter().enumerate() {
            sum_sq += s * s;
            if i > 0 {
                let prev = frame[i - 1];
                if (prev < 0.0 && *s >= 0.0) || (prev >= 0.0 && *s < 0.0) {
                    crossings += 1;
                }
            }
        }
        let rms = (sum_sq / frame.len() as f32).sqrt();
        let zcr = crossings as f32 / frame.len() as f32;
        // «Громкий» = RMS выше порога И ZCR не ниже порога. ZCR — фильтр
        // от чистого DC (zcr=0): DC «громкий» по RMS, но не речь; при
        // zcr_threshold=0 DC проходит, при >0 — нет.
        rms > rms_threshold && zcr >= zcr_threshold
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 960-sample 8 kHz-ish square-ish wave (crosses zero regularly).
    fn loud_frame() -> Vec<f32> {
        (0..FRAME_SAMPLES).map(|i| f32::from(i % 16 < 8) * 2.0 - 1.0).collect()
    }

    /// A silent (all-zeros) 960-sample frame.
    fn silent_frame() -> Vec<f32> {
        vec![0.0f32; FRAME_SAMPLES]
    }

    /// A frame that is loud by RMS but never crosses zero (DC tone) —
    /// fails the ZCR gate when `zcr_threshold > 0`.
    fn dc_frame() -> Vec<f32> {
        vec![0.5f32; FRAME_SAMPLES]
    }

    #[test]
    fn loud_frames_activate_after_debounce() {
        let mut vad = Vad::new(VadConfig { zcr_threshold: 0.01, ..VadConfig::default() });
        // 24 frames × 20 ms = 480 ms < 500 ms → still inactive.
        for _ in 0..24 {
            assert_eq!(vad.feed_frame(&loud_frame()), VadState::Inactive);
        }
        // 25th frame → 500 ms sustained → committed Active.
        assert_eq!(vad.feed_frame(&loud_frame()), VadState::Active);
        // Stays active on further loud frames.
        assert_eq!(vad.feed_frame(&loud_frame()), VadState::Active);
    }

    #[test]
    fn silent_frames_stay_inactive() {
        let mut vad = Vad::new(VadConfig::default());
        for _ in 0..50 {
            assert_eq!(vad.feed_frame(&silent_frame()), VadState::Inactive);
        }
    }

    #[test]
    fn no_flap_on_threshold_crossing_noise() {
        let mut vad = Vad::new(VadConfig { zcr_threshold: 0.01, ..VadConfig::default() });
        // Reach Active with sustained loud frames.
        for _ in 0..25 {
            vad.feed_frame(&loud_frame());
        }
        assert_eq!(vad.state(), VadState::Active);
        // Single silent frame among loud ones: pending Inactive restarts
        // on the next loud frame → no flap.
        vad.feed_frame(&silent_frame());
        assert_eq!(vad.state(), VadState::Active, "1 silent frame must not deactivate");
        for _ in 0..25 {
            vad.feed_frame(&loud_frame());
        }
        assert_eq!(vad.state(), VadState::Active);
        // Only 500 ms of sustained silence deactivates.
        for _ in 0..24 {
            vad.feed_frame(&silent_frame());
            assert_eq!(vad.state(), VadState::Active);
        }
        vad.feed_frame(&silent_frame());
        assert_eq!(vad.state(), VadState::Inactive);
    }

    #[test]
    fn zcr_gate_rejects_dc() {
        let cfg = VadConfig { rms_threshold: 0.01, zcr_threshold: 0.1, debounce_ms: DEBOUNCE_MS };
        let mut vad = Vad::new(cfg);
        let dc = dc_frame();
        // 25 sustained "loud-by-RMS-only" frames — still Inactive (ZCR gate).
        for _ in 0..25 {
            assert_eq!(vad.feed_frame(&dc), VadState::Inactive);
        }
    }

    #[test]
    fn frame_is_loud_helpers() {
        let loud = loud_frame();
        let silent = silent_frame();
        assert!(Vad::frame_is_loud(&loud, 0.01, 0.01));
        assert!(!Vad::frame_is_loud(&silent, 0.01, 0.01));
        assert!(!Vad::frame_is_loud(&[], 0.0, 0.0));
        // Pure DC is loud by RMS, not by ZCR.
        let dc = dc_frame();
        assert!(Vad::frame_is_loud(&dc, 0.01, 0.0));
        assert!(!Vad::frame_is_loud(&dc, 0.01, 0.1));
    }

    #[test]
    fn debounce_window_is_configurable() {
        let mut vad = Vad::new(VadConfig { debounce_ms: 60, zcr_threshold: 0.01, ..VadConfig::default() });
        // 3 frames × 20 ms = 60 ms → Active.
        for _ in 0..2 {
            assert_eq!(vad.feed_frame(&loud_frame()), VadState::Inactive);
        }
        assert_eq!(vad.feed_frame(&loud_frame()), VadState::Active);
    }
}

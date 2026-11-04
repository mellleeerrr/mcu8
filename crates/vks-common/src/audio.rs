//! Common audio processing utilities: gain, soft-clipping, sanitization, and PCM conversions.

/// Soft-clipping transfer function: linear response for amplitudes within ±1.0,
/// smooth hyperbolic tangent saturation above ±1.0 to prevent hard digital clipping.
///
/// Note: preserves existing MCU mixer behavior where values exceeding 1.0 smoothly saturate.
#[inline(always)]
#[must_use]
pub fn soft_clip(sample: f32) -> f32 {
    if sample.abs() <= 1.0 {
        sample
    } else {
        sample.tanh()
    }
}

/// Replaces non-finite values (`NaN`, `+inf`, `-inf`) with silence (0.0).
/// Prevents poisoned audio samples from corrupting the entire room mix.
#[inline(always)]
#[must_use]
pub fn sanitize_sample(sample: f32) -> f32 {
    if sample.is_finite() {
        sample
    } else {
        0.0
    }
}

/// Applies volume scaling (0..=1000%, where 100 = 100% / 1.0x gain) followed by soft-clipping.
#[inline(always)]
#[must_use]
pub fn apply_gain(sample: f32, volume: u32) -> f32 {
    let scaled = sample * (volume.min(1000) as f32 / 100.0);
    soft_clip(scaled)
}

/// Alias for [`apply_gain`] matching legacy gateway naming.
#[inline(always)]
#[must_use]
pub fn gain(sample: f32, volume: u32) -> f32 {
    apply_gain(sample, volume)
}

/// Converts signed 16-bit PCM sample to normalized 32-bit float [-1.0, 1.0).
#[inline(always)]
#[must_use]
pub fn pcm16_to_f32(sample: i16) -> f32 {
    sample as f32 / 32768.0
}

/// Converts normalized 32-bit float sample [-1.0, 1.0] to signed 16-bit PCM with rounding and saturation.
#[inline(always)]
#[must_use]
pub fn f32_to_pcm16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * 32767.0).round() as i16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_clip_preserves_linear_range() {
        assert_eq!(soft_clip(0.0), 0.0);
        assert_eq!(soft_clip(0.5), 0.5);
        assert_eq!(soft_clip(-0.5), -0.5);
        assert_eq!(soft_clip(1.0), 1.0);
        assert_eq!(soft_clip(-1.0), -1.0);
    }

    #[test]
    fn soft_clip_saturates_above_one() {
        let saturated = soft_clip(2.0);
        assert!((saturated - 2.0f32.tanh()).abs() < 1e-6);
        assert!(saturated > 0.0 && saturated < 1.0);
    }

    #[test]
    fn sanitize_sample_filters_nan_and_infinity() {
        assert_eq!(sanitize_sample(f32::NAN), 0.0);
        assert_eq!(sanitize_sample(f32::INFINITY), 0.0);
        assert_eq!(sanitize_sample(f32::NEG_INFINITY), 0.0);
        assert_eq!(sanitize_sample(0.75), 0.75);
    }

    #[test]
    fn apply_gain_scales_and_clips() {
        assert_eq!(apply_gain(0.5, 100), 0.5);
        assert_eq!(apply_gain(0.5, 200), 1.0);
        assert_eq!(apply_gain(0.5, 0), 0.0);
        let clipped = apply_gain(0.8, 200);
        assert!((clipped - 1.6f32.tanh()).abs() < 1e-6);
    }

    #[test]
    fn pcm16_f32_conversions() {
        assert_eq!(pcm16_to_f32(0), 0.0);
        assert_eq!(f32_to_pcm16(0.0), 0);
        assert_eq!(f32_to_pcm16(1.0), 32767);
        assert_eq!(f32_to_pcm16(-1.0), -32767);
    }
}

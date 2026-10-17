//! Audio mixer: N PCM input buffers (f32, 48 kHz) → 1 mixed output.
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): N Opus → 1 mixed (PCM sum, AGC = 1/√N,
//! soft-clip). The Opus decode step is out of scope of this module (the
//! inputs are already decoded PCM); the real Opus codec sits behind the
//! `oxideav` feature.
//!
//! The mix is deterministic:
//! 1. Sum the inputs sample-wise (missing samples of shorter buffers are
//!    treated as silence).
//! 2. Apply the AGC gain `1/√N` (N = number of non-empty inputs; N = 0 →
//!    silence).
//! 3. Soft-clip at ±1.0 with `tanh`, so the output never exceeds ±1.0.
//!
//! No `unsafe`.

/// Sample rate of the mixed audio (Hz) — 48 kHz per DEVELOPMENT_PLAN.
pub const SAMPLE_RATE_HZ: u32 = 48_000;

/// Audio mixer.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioMixer;

impl AudioMixer {
    /// Mixes N input buffers into a single output buffer.
    ///
    /// - `inputs`: one slice per participant (f32 PCM, 48 kHz, mono).
    /// - `output_len`: number of samples of the result (the longest input
    ///   is not required — shorter inputs are zero-padded, longer ones
    ///   truncated).
    ///
    /// Gain = `1/√N` where `N` is the count of non-empty inputs; with zero
    /// non-empty inputs the output is all zeros.
    pub fn mix(&self, inputs: &[&[f32]], output_len: usize) -> Vec<f32> {
        let n = inputs.iter().filter(|b| !b.is_empty()).count();
        let gain = if n == 0 { 0.0 } else { 1.0 / (n as f32).sqrt() };

        let mut out = vec![0.0f32; output_len];
        for i in 0..output_len {
            let mut sum = 0.0f32;
            for buf in inputs {
                if i < buf.len() {
                    sum += buf[i];
                }
            }
            let mixed = sum * gain;
            // Soft-clip: pass through inside ±1.0, tanh-clip outside (never exceeds ±1).
            out[i] = if mixed.abs() <= 1.0 { mixed } else { mixed.tanh() };
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_inputs_sum_over_sqrt2() {
        let a = [0.5f32, 0.0, -0.25];
        let b = [0.25f32, 0.5, 0.0];
        let out = AudioMixer.mix(&[&a, &b], 3);
        let expected_gain = 1.0f32 / 2.0f32.sqrt();
        assert!((out[0] - 0.75 * expected_gain).abs() < 1e-6);
        assert!((out[1] - 0.5 * expected_gain).abs() < 1e-6);
        assert!((out[2] + 0.25 * expected_gain).abs() < 1e-6);
    }

    #[test]
    fn zero_inputs_silence() {
        let out = AudioMixer.mix(&[], 8);
        assert_eq!(out, vec![0.0f32; 8]);
    }

    #[test]
    fn all_max_soft_clipped_to_one() {
        // N identical +1.0 inputs: sum = N, gain = 1/√N → N/√N = √N → tanh(√N).
        // N=4 → tanh(2) ≈ 0.964 (clipped, ≤ 1.0).
        let a = [1.0f32; 48];
        let out = AudioMixer.mix(&[&a, &a, &a, &a], 48);
        assert!(out.iter().all(|v| v.abs() <= 1.0), "soft-clip must not exceed ±1");
        assert!((out[0] - 2.0f32.tanh()).abs() < 1e-6, "N=4 → tanh(2)");
    }

    #[test]
    fn single_input_gain_one() {
        let a = [0.3f32, -0.3, 0.9];
        let out = AudioMixer.mix(&[&a], 3);
        assert!((out[0] - 0.3).abs() < 1e-6, "N=1 → gain 1/√1 = 1");
        assert!((out[2] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn negative_all_max_soft_clipped_to_minus_one() {
        let a = [-1.0f32; 4];
        let out = AudioMixer.mix(&[&a, &a, &a, &a], 4);
        assert!(out.iter().all(|v| v.abs() <= 1.0));
        assert!((out[0] - (-2.0f32.tanh())).abs() < 1e-6, "N=4 → tanh(-2)");
    }

    #[test]
    fn short_inputs_zero_padded() {
        let a = [0.5f32];
        let b = [0.5f32, 0.5, 0.5];
        let out = AudioMixer.mix(&[&a, &b], 3);
        // sample 0: (0.5+0.5)/√2; samples 1..2: 0.5/√2
        let g = 1.0f32 / 2.0f32.sqrt();
        assert!((out[0] - 1.0 * g).abs() < 1e-6);
        assert!((out[1] - 0.5 * g).abs() < 1e-6);
        assert!((out[2] - 0.5 * g).abs() < 1e-6);
    }

    #[test]
    fn empty_buffers_do_not_count_for_gain() {
        let a = [0.4f32, 0.4];
        let empty: [f32; 0] = [];
        let out = AudioMixer.mix(&[&a, &empty, &empty], 2);
        assert!((out[0] - 0.4).abs() < 1e-6, "N counts non-empty only");
    }
}

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
    #[inline]
    fn sanitize_sample(s: f32) -> f32 {
        if s.is_finite() {
            s
        } else {
            0.0
        }
    }

    #[inline]
    fn soft_clip(mixed: f32) -> f32 {
        if mixed.abs() <= 1.0 {
            mixed
        } else {
            mixed.tanh()
        }
    }

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
                    sum += Self::sanitize_sample(buf[i]);
                }
            }
            let mixed = sum * gain;
            // Soft-clip: pass through inside ±1.0, tanh-clip outside (never exceeds ±1).
            out[i] = Self::soft_clip(mixed);
        }
        out
    }

    /// Mix-minus (N-1): produces N output buffers, where output[k] contains the mix of
    /// all inputs EXCEPT inputs[k].
    ///
    /// Complexity: O(N * L) via single global accumulation and per-participant subtraction.
    /// Gain = 1 / √(N_other) where N_other is the count of other non-empty inputs.
    pub fn mix_minus(&self, inputs: &[&[f32]], output_len: usize) -> Vec<Vec<f32>> {
        let n_active = inputs.iter().filter(|b| !b.is_empty()).count();

        // 1. Precompute global sum S[0..output_len] with NaN/Inf sanitization.
        let mut sum = vec![0.0f32; output_len];
        for buf in inputs {
            for (i, s) in buf.iter().take(output_len).enumerate() {
                sum[i] += Self::sanitize_sample(*s);
            }
        }

        // 2. Compute mix-minus for each participant.
        inputs
            .iter()
            .map(|own_buf| {
                let own_non_empty = !own_buf.is_empty();
                let other_count = if own_non_empty { n_active.saturating_sub(1) } else { n_active };

                if other_count == 0 {
                    return vec![0.0f32; output_len];
                }

                let gain = 1.0f32 / (other_count as f32).sqrt();
                let mut out = vec![0.0f32; output_len];
                for i in 0..output_len {
                    let own_sample = if i < own_buf.len() { Self::sanitize_sample(own_buf[i]) } else { 0.0 };
                    let sample_minus = sum[i] - own_sample;
                    let mixed = sample_minus * gain;
                    out[i] = Self::soft_clip(mixed);
                }
                out
            })
            .collect()
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

    #[test]
    fn mix_minus_two_participants_hear_each_other_without_self_echo() {
        let a = [0.6f32, 0.2];
        let b = [0.4f32, 0.8];
        let outputs = AudioMixer.mix_minus(&[&a, &b], 2);
        assert_eq!(outputs.len(), 2);
        // Participant A hears ONLY participant B (N_other = 1 -> gain = 1.0).
        assert!((outputs[0][0] - 0.4).abs() < 1e-6, "A hears B");
        assert!((outputs[0][1] - 0.8).abs() < 1e-6);
        // Participant B hears ONLY participant A (N_other = 1 -> gain = 1.0).
        assert!((outputs[1][0] - 0.6).abs() < 1e-6, "B hears A");
        assert!((outputs[1][1] - 0.2).abs() < 1e-6);
    }

    #[test]
    fn mix_minus_single_participant_gets_silence() {
        let a = [0.9f32, -0.5];
        let outputs = AudioMixer.mix_minus(&[&a], 2);
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0], vec![0.0f32, 0.0f32]);
    }

    #[test]
    fn mix_minus_nan_sanitized() {
        let bad = [f32::NAN, f32::INFINITY];
        let good = [0.5f32, 0.5f32];
        let outputs = AudioMixer.mix_minus(&[&bad, &good], 2);
        assert_eq!(outputs.len(), 2);
        // Participant with bad input hears participant with good input safely.
        assert!((outputs[0][0] - 0.5).abs() < 1e-6);
        assert!((outputs[0][1] - 0.5).abs() < 1e-6);
        // Participant with good input hears 0.0 (bad input was sanitized).
        assert!((outputs[1][0] - 0.0).abs() < 1e-6);
        assert!((outputs[1][1] - 0.0).abs() < 1e-6);
    }
}

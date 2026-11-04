//! Audio mixer: N PCM input buffers (f32, 48 kHz) → 1 mixed output.
//!
//! Фаза 3 (DEVELOPMENT_PLAN.md): N Opus → 1 mixed (PCM sum, AGC = 1/√N,
//! soft-clip). The Opus decode step is out of scope of this module (the
//! inputs are already decoded PCM); Opus/RTP wiring lives in the server and
//! SFU, not here.
//!
//! The mix is deterministic:
//! 1. Sum the inputs sample-wise (missing samples of shorter buffers are
//!    treated as silence).
//! 2. Apply the AGC gain `1/√N` (N = number of non-empty inputs; N = 0 →
//!    silence).
//! 3. Soft-clip at ±1.0 with `tanh`, so the output never exceeds ±1.0.
//!
//! No `unsafe`.

/// Piecewise-rational soft-knee limiter for integer PCM (preventing harsh clipping
/// distortion and arithmetic overflow without transcendental math).
#[inline(always)]
pub fn soft_limit_sample(x: i32) -> i16 {
    const K: i32 = 24576; // -2.5 dBFS knee
    const H: i32 = 8191; // Headroom to 32767
    let abs_x = x.saturating_abs();
    if abs_x <= K {
        x as i16
    } else {
        let delta = (abs_x - K) as i64;
        let h = H as i64;
        let sat = K + ((delta * h) / (delta + h)) as i32;
        if x < 0 {
            (-sat) as i16
        } else {
            sat as i16
        }
    }
}

/// Sample rate of the mixed audio (Hz) — 48 kHz per DEVELOPMENT_PLAN.
pub const SAMPLE_RATE_HZ: u32 = 48_000;
/// Standard 20 ms audio frame samples at 48 kHz.
pub const FRAME_SAMPLES_20MS: usize = 960;

use vks_common::audio::{sanitize_sample, soft_clip};

/// Audio mixer.
#[derive(Debug, Clone, Copy, Default)]
pub struct AudioMixer;

impl AudioMixer {
    /// In-place mix into a caller-provided slice. Zero heap allocation.
    pub fn mix_into(&self, inputs: &[&[f32]], out: &mut [f32]) {
        out.fill(0.0);
        let n_active = inputs.iter().filter(|b| !b.is_empty()).count();
        if n_active == 0 {
            return;
        }
        for buf in inputs {
            for (i, s) in buf.iter().take(out.len()).enumerate() {
                out[i] += sanitize_sample(*s);
            }
        }
        let full_gain = 1.0f32 / (n_active as f32).sqrt();
        for sample in out.iter_mut() {
            *sample = soft_clip(*sample * full_gain);
        }
    }

    /// Single-pass calculation of both room mix and per-participant mix-minus into
    /// pre-allocated slices without any heap allocation.
    ///
    /// - `full_out`: Output slice for the room mix.
    /// - `minus_outs`: Mutable slices for each participant's mix-minus.
    pub fn mix_and_mix_minus_into(
        &self,
        inputs: &[&[f32]],
        full_out: &mut [f32],
        minus_outs: &mut [&mut [f32]],
    ) {
        debug_assert_eq!(inputs.len(), minus_outs.len(), "minus_outs slice count must match inputs count");
        let output_len = full_out.len();
        full_out.fill(0.0);
        let n_active = inputs.iter().filter(|b| !b.is_empty()).count();

        // 1. Single accumulation pass directly into full_out (unscaled sum S[i])
        for buf in inputs {
            for (i, s) in buf.iter().take(output_len).enumerate() {
                full_out[i] += sanitize_sample(*s);
            }
        }

        // 2. Per-participant mix-minus pass (reading S[i] from full_out BEFORE full_gain/soft_clip)
        for (k, own_buf) in inputs.iter().enumerate() {
            let out = &mut minus_outs[k];
            let own_non_empty = !own_buf.is_empty();
            let other_count = if own_non_empty { n_active.saturating_sub(1) } else { n_active };

            if other_count == 0 {
                out.fill(0.0);
                continue;
            }

            let gain = 1.0f32 / (other_count as f32).sqrt();
            let len = output_len.min(out.len());
            for i in 0..len {
                let own_sample = if i < own_buf.len() { sanitize_sample(own_buf[i]) } else { 0.0 };
                let sample_minus = full_out[i] - own_sample;
                out[i] = soft_clip(sample_minus * gain);
            }
        }

        // 3. Final in-place scaling & soft-clipping of global room mix
        let full_gain = if n_active == 0 { 0.0 } else { 1.0f32 / (n_active as f32).sqrt() };
        for sample in full_out.iter_mut() {
            *sample = soft_clip(*sample * full_gain);
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
        let mut out = vec![0.0f32; output_len];
        self.mix_into(inputs, &mut out);
        out
    }

    /// Mix-minus (N-1): produces N output buffers, where `output[k]` contains the mix of
    /// all inputs EXCEPT `inputs[k]`.
    pub fn mix_minus(&self, inputs: &[&[f32]], output_len: usize) -> Vec<Vec<f32>> {
        self.mix_and_mix_minus(inputs, output_len).1
    }

    /// Single-pass calculation of both the room's global audio mix and per-participant
    /// mix-minus in O(N * L) with a single accumulation pass.
    ///
    /// Returns `(full_mix, per_participant_minus)`.
    pub fn mix_and_mix_minus(&self, inputs: &[&[f32]], output_len: usize) -> (Vec<f32>, Vec<Vec<f32>>) {
        let mut full_mix = vec![0.0f32; output_len];
        let mut minus_vecs: Vec<Vec<f32>> = (0..inputs.len()).map(|_| vec![0.0f32; output_len]).collect();
        let mut minus_slices: Vec<&mut [f32]> = minus_vecs.iter_mut().map(|v| v.as_mut_slice()).collect();
        self.mix_and_mix_minus_into(inputs, &mut full_mix, &mut minus_slices);
        (full_mix, minus_vecs)
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

    #[test]
    fn mix_and_mix_minus_consistency() {
        let a = [0.6f32, 0.2];
        let b = [0.4f32, 0.8];
        let (full, minus) = AudioMixer.mix_and_mix_minus(&[&a, &b], 2);
        let separate_full = AudioMixer.mix(&[&a, &b], 2);
        let separate_minus = AudioMixer.mix_minus(&[&a, &b], 2);
        assert_eq!(full, separate_full);
        assert_eq!(minus, separate_minus);
    }

    #[test]
    fn soft_limit_sample_behavior() {
        // Below knee (24576): linear 1:1
        assert_eq!(soft_limit_sample(0), 0);
        assert_eq!(soft_limit_sample(10000), 10000);
        assert_eq!(soft_limit_sample(-10000), -10000);
        assert_eq!(soft_limit_sample(24576), 24576);
        assert_eq!(soft_limit_sample(-24576), -24576);

        // Above knee: smooth rational saturation without overflow (asymptote is 24576 + 8191 = 32767)
        assert!(soft_limit_sample(30000) > 24576);
        assert!(soft_limit_sample(100000) >= 31500);
        assert!(soft_limit_sample(-100000) <= -31500);
        assert!(soft_limit_sample(i32::MIN) <= -32000);
        assert!(soft_limit_sample(i32::MAX) >= 32000);
    }

    #[test]
    fn mix_into_stack_buffer() {
        let a = [0.5f32; 960];
        let b = [0.25f32; 960];
        let mut out = [0.0f32; 960];
        AudioMixer.mix_into(&[&a, &b], &mut out);
        let expected = 0.75f32 / 2.0f32.sqrt();
        assert!((out[0] - expected).abs() < 1e-6);
        assert!((out[959] - expected).abs() < 1e-6);
    }
}

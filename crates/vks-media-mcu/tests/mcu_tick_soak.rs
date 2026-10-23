//! MCU engine tick soak/SLO: real audio mix + video compose, measured by the
//! `vks_mcu_tick_duration_seconds` histogram.
//!
//! Runs in-process (no HTTP server, live DB, RTP or SIP sockets) and only
//! measures engine work. Timing gates are meaningless in debug/shared-CI
//! builds, so the test is `#[ignore]` and refuses to run in a debug profile:
//!
//! ```text
//! VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS=60 VKS_SOAK_WARMUP=2 \
//!   VKS_SOAK_OUT="$PWD/docs/progress/evidence/t14b-mcu-tick-soak/soak-60s-4p.json" \
//!   cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture
//! ```
//!
//! Env: `VKS_SOAK_SECONDS` (measured window, default 20), `VKS_SOAK_WARMUP`
//! (discarded warm-up, default 2), `VKS_SOAK_PARTICIPANTS` (default 4),
//! `VKS_SOAK_OUT` (default `target/t14b-mcu-tick-soak.json`). Invalid values are
//! hard errors rather than silently falling back.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use vks_common::metrics::MCU_TICK_BUCKETS;
use vks_core::{RoomId, Track, TrackKind};
use vks_media_mcu::{Frame, McuEngine};

const AUDIO_PERIOD_MS: u64 = 20;
const VIDEO_PERIOD_MS: u64 = 33;
const AUDIO_SLO_BOUND: f64 = 0.002;
const VIDEO_FRAME_BUDGET: f64 = 0.033;

#[derive(Debug, Clone, Default)]
struct PhaseSnapshot {
    count: u64,
    sum: f64,
    buckets: Vec<(f64, u64)>,
}

impl PhaseSnapshot {
    fn delta(&self, before: &Self) -> Self {
        let buckets = self
            .buckets
            .iter()
            .zip(before.buckets.iter())
            .map(|((bound, after), (_, before))| (*bound, after.saturating_sub(*before)))
            .collect();
        Self { count: self.count.saturating_sub(before.count), sum: (self.sum - before.sum).max(0.0), buckets }
    }

    /// Exact cumulative ratio at `bound` (the SLO gate; no interpolation).
    fn cumulative_ratio(&self, bound: f64) -> Option<f64> {
        if self.count == 0 {
            return None;
        }
        let seen = self.buckets.iter().find(|(upper, _)| (*upper - bound).abs() < 1e-12).map(|(_, seen)| *seen)?;
        Some(seen as f64 / self.count as f64)
    }

    /// Bucket containing the p99 and a clearly-labelled interpolated point.
    fn p99(&self) -> Option<(f64, f64, f64)> {
        if self.count == 0 {
            return None;
        }
        let target = (self.count as f64 * 0.99).ceil() as u64;
        let mut previous_count = 0u64;
        let mut previous_bound = 0.0f64;
        for (bound, seen) in &self.buckets {
            if *seen >= target {
                let bucket_count = seen.saturating_sub(previous_count).max(1);
                let rank = target.saturating_sub(previous_count);
                let frac = rank as f64 / bucket_count as f64;
                let estimate = previous_bound + (bound - previous_bound) * frac;
                return Some((estimate, previous_bound, *bound));
            }
            previous_count = *seen;
            previous_bound = *bound;
        }
        self.buckets.last().map(|(bound, _)| (*bound, *bound, *bound))
    }
}

fn histogram_snapshot() -> BTreeMap<String, PhaseSnapshot> {
    let mut phases = BTreeMap::new();
    let family = prometheus::gather().into_iter().find(|family| family.name() == "vks_mcu_tick_duration_seconds");
    let Some(family) = family else { return phases };
    for metric in family.get_metric() {
        let phase = metric
            .get_label()
            .iter()
            .find(|label| label.name() == "phase")
            .map_or("", |label| label.value())
            .to_string();
        let histogram = metric.get_histogram();
        phases.insert(
            phase,
            PhaseSnapshot {
                count: histogram.get_sample_count(),
                sum: histogram.get_sample_sum(),
                buckets: histogram
                    .get_bucket()
                    .iter()
                    .map(|bucket| (bucket.upper_bound(), bucket.cumulative_count()))
                    .collect(),
            },
        );
    }
    phases
}

fn env_usize(name: &str, default: usize) -> usize {
    match std::env::var(name) {
        Ok(value) => value
            .trim()
            .parse::<usize>()
            .unwrap_or_else(|error| panic!("{name}={value:?} is not a non-negative integer: {error}")),
        Err(_) => default,
    }
}

fn command_output(program: &str, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn cpu_model() -> String {
    std::fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("model name"))
                .and_then(|line| line.split_once(':'))
                .map(|(_, value)| value.trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn sine_pcm() -> Vec<f32> {
    // Near full scale so the mixer's tanh soft-clip branch is exercised; the
    // sum of four 0.8-amplitude inputs scaled by 1/sqrt(N) crosses 1.0.
    (0..960).map(|i| ((i as f32) * 0.05).sin() * 0.8).collect()
}

#[test]
#[ignore = "release-only timing gate: cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored"]
fn mcu_tick_soak_audio_slo_and_video_budget() {
    const { assert!(!cfg!(debug_assertions), "timing gates require --release") };
    // This integration-test binary has exactly one test, so setting the env here
    // is safe; `mcu_tick_metrics_enabled()` is a process-wide `OnceLock` and the
    // first read must happen after this line.
    std::env::set_var("VKS_MCU_TICK_METRICS", "1");

    let seconds = env_usize("VKS_SOAK_SECONDS", 20);
    let warmup = env_usize("VKS_SOAK_WARMUP", 2);
    let participants = env_usize("VKS_SOAK_PARTICIPANTS", 4);
    assert!((5..=3600).contains(&seconds), "VKS_SOAK_SECONDS must be 5..=3600, got {seconds}");
    assert!(warmup >= 1 && warmup < seconds, "VKS_SOAK_WARMUP must be >=1 and < seconds");
    assert!((1..=32).contains(&participants), "VKS_SOAK_PARTICIPANTS must be 1..=32");
    let out = std::env::var("VKS_SOAK_OUT").unwrap_or_else(|_| "target/t14b-mcu-tick-soak.json".into());

    let engine = McuEngine::new();
    let room = RoomId::new();
    engine.ensure_room(room);
    let pids: Vec<_> = (0..participants).map(|_| vks_core::ParticipantId::new()).collect();
    for (index, pid) in pids.iter().enumerate() {
        engine.bridge_participant(
            room,
            *pid,
            vec![Track::new(*pid, TrackKind::Video), Track::new(*pid, TrackKind::Audio)],
        );
        // Frames are decoded inputs, not per-tick workload here: set them once so
        // harness allocation does not perturb pacing. `Arc<Frame>` is stored.
        let fill = (20 + index * 15) as u8;
        engine.set_participant_frame(room, *pid, Frame::filled(1280, 720, fill));
    }
    let pcm = sine_pcm();

    let started = Instant::now();
    let total = Duration::from_secs(seconds as u64);
    let warmup_ms = warmup as u64 * 1000;
    let mut next_audio = started;
    let mut next_video = started;
    let mut audio_attempts = 0u64;
    let mut audio_ticks = 0u64;
    let mut video_attempts = 0u64;
    let mut video_ticks = 0u64;
    let mut measured = false;
    let mut baseline = BTreeMap::new();
    let mut baseline_dropped = 0u64;

    while started.elapsed() < total {
        let now = Instant::now();
        let now_ms = now.duration_since(started).as_millis() as u64;
        // Snapshot after the warm-up (crossfade/layout settle + CPU cache warm).
        if !measured && now_ms >= warmup_ms {
            baseline = histogram_snapshot();
            baseline_dropped = engine.stats(room).map_or(0, |stats| stats.frames_dropped);
            measured = true;
        }
        if !measured {
            // Keep the engine busy during warm-up too, but do not count it.
            if now >= next_audio {
                for pid in &pids {
                    engine.set_audio_slice(room, *pid, &pcm);
                }
                // Two calls per period: the second must be throttled and must not
                // be recorded. This is the throttle-exclusion check in production
                // code, exercised on every tick.
                let _ = engine.tick_audio_at(room, now_ms);
                let _ = engine.tick_audio_at(room, now_ms);
                // Measured rate from the actual tick, not a fixed grid: firing a
                // late grid slot would land <18 ms after the previous tick and be
                // throttled, silently lowering the audio cadence.
                next_audio = now + Duration::from_millis(AUDIO_PERIOD_MS);
            }
            if now >= next_video {
                let _ = engine.tick_video_at(room, now_ms);
                next_video = now + Duration::from_millis(VIDEO_PERIOD_MS);
            }
            std::thread::sleep(Duration::from_micros(200));
            continue;
        }
        if now >= next_audio {
            for pid in &pids {
                engine.set_audio_slice(room, *pid, &pcm);
            }
            audio_attempts += 2;
            if engine.tick_audio_at(room, now_ms) {
                audio_ticks += 1;
            }
            if engine.tick_audio_at(room, now_ms) {
                audio_ticks += 1;
            }
            next_audio = now + Duration::from_millis(AUDIO_PERIOD_MS);
        }
        if now >= next_video {
            video_attempts += 1;
            if engine.tick_video_at(room, now_ms).is_some() {
                video_ticks += 1;
            }
            next_video = now + Duration::from_millis(VIDEO_PERIOD_MS);
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    if !measured {
        baseline = histogram_snapshot();
        baseline_dropped = engine.stats(room).map_or(0, |stats| stats.frames_dropped);
    }
    let measured_end_ms = started.elapsed().as_millis() as u64;
    let measured_seconds = measured_end_ms.saturating_sub(warmup_ms).max(1) as f64 / 1000.0;

    let after = histogram_snapshot();
    let dropped = engine.stats(room).map_or(0, |stats| stats.frames_dropped) - baseline_dropped;
    // Admin rebuild paths (`clear_participant_frame`) call `process_video` directly
    // and must not be recorded as ticks.
    let before_admin = histogram_snapshot();
    engine.clear_participant_frame(room, pids[0]);
    engine.set_participant_frame(room, pids[0], Frame::filled(1280, 720, 20));
    let after_admin = histogram_snapshot();
    let admin_pollution = after_admin
        .get("video")
        .map_or(0, |snapshot| snapshot.count)
        .saturating_sub(before_admin.get("video").map_or(0, |snapshot| snapshot.count));

    let audio =
        after.get("audio").cloned().unwrap_or_default().delta(&baseline.get("audio").cloned().unwrap_or_default());
    let video =
        after.get("video").cloned().unwrap_or_default().delta(&baseline.get("video").cloned().unwrap_or_default());
    let audio_ratio = audio.cumulative_ratio(AUDIO_SLO_BOUND);
    let video_ratio = video.cumulative_ratio(VIDEO_FRAME_BUDGET);
    let audio_p99 = audio.p99();
    let video_p99 = video.p99();
    let effective_video_fps = video_ticks as f64 / measured_seconds;

    let report = serde_json::json!({
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "rustc": command_output("rustc", &["-V"]),
        "git_sha": command_output("git", &["rev-parse", "HEAD"]),
        "git_dirty": command_output("git", &["status", "--porcelain"])
            .lines()
            .any(|line| {
                line.contains("crates/vks-common/src/metrics.rs")
                    || line.contains("crates/vks-media-mcu/src/compositor.rs")
                    || line.contains("crates/vks-media-mcu/tests/mcu_tick_soak.rs")
            }),
        "cpu_model": cpu_model(),
        "arch": std::env::consts::ARCH,
        "seconds": seconds,
        "warmup_seconds": warmup,
        "measured_seconds": measured_seconds,
        "participants": participants,
        "audio": {
            "attempts": audio_attempts,
            "successful_ticks": audio_ticks,
            "histogram_count": audio.count,
            "le_2ms_ratio": audio_ratio,
            "p99_seconds_estimate": audio_p99.map(|(estimate, _, _)| estimate),
            "p99_bucket_seconds": audio_p99.map(|(_, lower, upper)| [lower, upper]),
            "sum_seconds": audio.sum,
            "mean_seconds": (audio.count > 0).then(|| audio.sum / audio.count as f64),
        },
        "video": {
            "attempts": video_attempts,
            "successful_ticks": video_ticks,
            "histogram_count": video.count,
            "engine_frames_dropped_delta": dropped,
            "effective_fps": effective_video_fps,
            "le_2ms_ratio": video_ratio_for(&video, AUDIO_SLO_BOUND),
            "le_33ms_ratio": video_ratio,
            "p99_seconds_estimate": video_p99.map(|(estimate, _, _)| estimate),
            "p99_bucket_seconds": video_p99.map(|(_, lower, upper)| [lower, upper]),
            "sum_seconds": video.sum,
            "mean_seconds": (video.count > 0).then(|| video.sum / video.count as f64),
        },
        "admin_rebuild_recorded_ticks": admin_pollution,
        "histogram_buckets_seconds": MCU_TICK_BUCKETS,
        "slo": {
            "audio_engine_work_p99_le_2ms": audio_ratio.is_some_and(|ratio| ratio >= 0.99),
            "video_compose_p99_le_33ms_budget": video_ratio.is_some_and(|ratio| ratio >= 0.99),
            "video_compose_p99_le_2ms": video_ratio_for(&video, AUDIO_SLO_BOUND).is_some_and(|ratio| ratio >= 0.99),
            "note": "audio is engine-work p99, not end-to-end latency; the video 2 ms target is deferred to S20",
        },
        "command": "VKS_MCU_TICK_METRICS=1 cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture",
        "notes": "In-process engine only: no RTP, SIP, decode/encode, resize_i420, WebRTC fan-out or live DB. Single room, so no rooms-mutex contention between rooms.",
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize soak report");
    if let Some(parent) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(parent).expect("create soak report directory");
    }
    std::fs::write(&out, &text).expect("write soak report");
    println!("{text}");

    // Gates run after the report is written so a failure still leaves diagnostics.
    assert!(audio.count > 0, "audio histogram is empty; zero samples is a false pass ({out})");
    assert!(video.count > 0, "video histogram is empty; zero samples is a false pass ({out})");
    assert_eq!(audio.count, audio_ticks, "audio histogram must count exactly the successful ticks");
    assert_eq!(video.count, video_ticks, "video histogram must count exactly the rendered ticks");
    assert_eq!(
        video_attempts.saturating_sub(video_ticks),
        dropped,
        "every throttled video attempt must be visible as frames_dropped"
    );
    assert_eq!(admin_pollution, 0, "admin rebuild paths must not be recorded as ticks");
    assert!(
        audio_ticks as f64 >= measured_seconds * 45.0,
        "audio cadence too low: {audio_ticks} ticks in {measured_seconds:.1}s"
    );
    assert!(
        effective_video_fps >= 28.0,
        "video render cadence too low: {effective_video_fps:.1} fps (attempts {video_attempts}, ticks {video_ticks})"
    );
    assert!(
        audio_ratio.is_some_and(|ratio| ratio >= 0.99),
        "audio engine work: only {:.4}% of ticks <= 2 ms (count {})",
        audio_ratio.unwrap_or(0.0) * 100.0,
        audio.count
    );
    assert!(
        video_ratio.is_some_and(|ratio| ratio >= 0.99),
        "video compose: only {:.4}% of ticks <= 33 ms budget (count {})",
        video_ratio.unwrap_or(0.0) * 100.0,
        video.count
    );
}

fn video_ratio_for(video: &PhaseSnapshot, bound: f64) -> Option<f64> {
    video.cumulative_ratio(bound)
}

//! MCU engine tick soak/SLO: real audio mix + video compose, measured by the
//! `vks_mcu_tick_duration_seconds` histogram.
//!
//! Runs in-process (no HTTP server, live DB, RTP or SIP sockets) and only
//! measures engine work. The measured window has two sequential phases: audio
//! alone at 20 ms, then video alone at 34 ms (above the engine's 30 ms video
//! throttle). They are not run concurrently, because today's global room mutex
//! would let a 1280×720 I420 compose block the audio tick and distort its
//! cadence; per-room actors and multi-room concurrency are S20. Timing gates are
//! meaningless in debug builds, so the test is `#[ignore]` and exits early in a
//! debug profile (it still compiles, so `cargo test --workspace` stays green):
//!
//! ```text
//! VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS=60 VKS_SOAK_WARMUP=3 \
//!   VKS_SOAK_OUT="$PWD/docs/progress/evidence/t14b-mcu-tick-soak/soak-60s-4p.json" \
//!   cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture
//! ```
//!
//! Env: `VKS_SOAK_SECONDS` (measured window, default 20), `VKS_SOAK_WARMUP`
//! (discarded warm-up, default 3), `VKS_SOAK_PARTICIPANTS` (default 4),
//! `VKS_SOAK_OUT` (default `target/t14b-mcu-tick-soak.json`). Invalid values are
//! hard errors rather than silently falling back.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde::Serialize;
use vks_common::metrics::MCU_TICK_BUCKETS;
use vks_core::{ParticipantId, RoomId, Track, TrackKind};
use vks_media_mcu::{Frame, McuEngine};

/// Audio tasks are due every 20 ms (50 Hz); video at 34 ms because the engine
/// drops ticks closer than 30 ms. The wall-clock period-loss gate below is the
/// cadence guard.
const AUDIO_PERIOD_MS: u64 = 20;
const VIDEO_PERIOD_MS: u64 = 34;
const AUDIO_SLO_BOUND: f64 = 0.002;
const VIDEO_FRAME_BUDGET: f64 = 0.033;
const FRAME_WIDTH: u32 = 1280;
const FRAME_HEIGHT: u32 = 720;

#[derive(Debug, Clone, Default)]
struct PhaseSnapshot {
    count: u64,
    sum: f64,
    buckets: Vec<(f64, u64)>,
}

impl PhaseSnapshot {
    fn delta(&self, before: &Self) -> Self {
        assert!(
            self.buckets.len() == before.buckets.len()
                && self.buckets.iter().zip(&before.buckets).all(|((a, _), (b, _))| a == b),
            "histogram bucket layout changed between snapshots"
        );
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
        // The last finite bucket is below the target rank: a p99 above the
        // histogram range is not a point estimate, so report it as unknown.
        None
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
        let histogram = metric.get_histogram().as_ref().expect("histogram payload");
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

/// `git status --porcelain` raw output; `None` means git itself failed. An
/// empty string is a clean tree and must not be confused with "unknown".
fn git_status_porcelain() -> Option<String> {
    std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
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

/// Complete planar YUV 4:2:0 frame: a 2-D luma gradient plus non-constant U/V
/// planes, so the compositor's luma and chroma paths both do real work and a
/// row/column mapping error cannot hide behind a flat plane.
fn i420_frame(width: u32, height: u32, seed: u32) -> Frame {
    let (w, h) = (width as usize, height as usize);
    let mut data = Vec::with_capacity(w * h * 3 / 2);
    for y in 0..h {
        for x in 0..w {
            data.push((((x as u32) * 3 + (y as u32) * 5 + seed * 7) % 220 + 20) as u8);
        }
    }
    let chroma = (w / 2) * (h / 2);
    for plane in 0..2u32 {
        for i in 0..chroma {
            data.push((((i as u32) * (5 + plane * 2) + seed * (11 + plane * 2)) % 200 + 20) as u8);
        }
    }
    let frame = Frame::new(width, height, data);
    assert!(frame.is_yuv420p(), "soak frames must exercise the real I420 compositor path");
    frame
}

#[derive(Debug, Default)]
struct Tally {
    periods: u64,
    missed_periods: u64,
    attempts: u64,
    ticks: u64,
    first_successes: u64,
    throttled: u64,
    second_call_successes: u64,
}

/// Per-phase JSON block, shared by audio and video so a new field cannot drift
/// between the two copies.
#[derive(Serialize)]
struct PhaseReport {
    periods: u64,
    missed_periods: u64,
    scheduled_periods_lower_bound: u64,
    attempts: u64,
    successful_ticks: u64,
    first_successes: u64,
    throttled: u64,
    second_call_successes: u64,
    rate: f64,
    histogram_count: u64,
    le_2ms_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    le_33ms_ratio: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine_frames_dropped_delta: Option<u64>,
    p99_seconds_estimate: Option<f64>,
    p99_bucket_seconds: Option<[f64; 2]>,
    sum_seconds: f64,
    mean_seconds: Option<f64>,
    buckets_cumulative: Vec<(f64, u64)>,
}

/// Named inputs for [`phase_report`]: two adjacent `Option<f64>` positional
/// arguments would silently transpose if swapped.
struct PhaseReportInput<'a> {
    tally: &'a Tally,
    delta: &'a PhaseSnapshot,
    scheduled: u64,
    rate: f64,
    le_2ms: Option<f64>,
    le_33ms: Option<f64>,
    dropped: Option<u64>,
}

fn phase_report(input: PhaseReportInput<'_>) -> PhaseReport {
    let PhaseReportInput { tally, delta, scheduled, rate, le_2ms, le_33ms, dropped } = input;
    let p99 = delta.p99();
    PhaseReport {
        periods: tally.periods,
        missed_periods: tally.missed_periods,
        scheduled_periods_lower_bound: scheduled,
        attempts: tally.attempts,
        successful_ticks: tally.ticks,
        first_successes: tally.first_successes,
        throttled: tally.throttled,
        second_call_successes: tally.second_call_successes,
        rate,
        histogram_count: delta.count,
        le_2ms_ratio: le_2ms,
        le_33ms_ratio: le_33ms,
        engine_frames_dropped_delta: dropped,
        p99_seconds_estimate: p99.map(|(estimate, _, _)| estimate),
        p99_bucket_seconds: p99.map(|(_, lower, upper)| [lower, upper]),
        sum_seconds: delta.sum,
        mean_seconds: (delta.count > 0).then(|| delta.sum / delta.count as f64),
        buckets_cumulative: delta.buckets.clone(),
    }
}

/// One measured window driven by the same sleep-to-deadline scheduler for both
/// phases. `prepare` runs once per period (audio slice update); `attempt` runs
/// the real engine tick. Every period makes two attempts at the same engine
/// timestamp, so the second one must be throttled. Returns the tally and the
/// measured wall-clock duration.
fn run_window(
    period: Duration,
    seconds: usize,
    epoch: Instant,
    mut prepare: impl FnMut(),
    mut attempt: impl FnMut(u64) -> bool,
) -> (Tally, f64) {
    let start = Instant::now();
    let deadline = start + Duration::from_secs(seconds as u64);
    let mut tally = Tally::default();
    let mut next = start;
    while Instant::now() < deadline {
        let now = Instant::now();
        if now >= next {
            let now_ms = epoch.elapsed().as_millis() as u64;
            prepare();
            tally.periods += 1;
            tally.attempts += 2;
            if attempt(now_ms) {
                tally.ticks += 1;
                tally.first_successes += 1;
            } else {
                tally.throttled += 1;
            }
            if attempt(now_ms) {
                tally.ticks += 1;
                tally.second_call_successes += 1;
            } else {
                tally.throttled += 1;
            }
            let due = next + period;
            let after = Instant::now();
            if due <= after {
                // The engine took longer than one period: count every grid slot
                // between `due` and the first future one, then align `next` to
                // that grid slot.
                let behind = after.duration_since(due);
                let skipped = 1 + (behind.as_nanos() / period.as_nanos()) as u64;
                tally.missed_periods += skipped;
                next = due + period * u32::try_from(skipped).unwrap_or(u32::MAX);
            } else {
                next = due;
            }
        }
        let sleep = next.min(deadline).saturating_duration_since(Instant::now());
        if !sleep.is_zero() {
            std::thread::sleep(sleep);
        }
    }
    (tally, start.elapsed().as_secs_f64())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[test]
#[ignore = "release-only timing gate: cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored"]
fn mcu_tick_soak_audio_slo_and_video_budget() {
    // The test must compile in debug (otherwise `cargo test --workspace` fails),
    // but a debug run measures nothing useful: skip instead of asserting.
    if cfg!(debug_assertions) {
        eprintln!("timing gates require --release: measurement skipped");
        return;
    }
    // This integration-test binary has exactly one test, so setting the env here
    // is safe; `mcu_tick_metrics_enabled()` is a process-wide `OnceLock` and the
    // first read must happen after this line.
    std::env::set_var("VKS_MCU_TICK_METRICS", "1");

    let seconds = env_usize("VKS_SOAK_SECONDS", 20);
    let warmup = env_usize("VKS_SOAK_WARMUP", 3);
    let participants = env_usize("VKS_SOAK_PARTICIPANTS", 4);
    assert!((5..=604_800).contains(&seconds), "VKS_SOAK_SECONDS must be 5..=604800, got {seconds}");
    assert!(warmup >= 1 && warmup < seconds, "VKS_SOAK_WARMUP must be >=1 and < seconds, got {warmup} for {seconds}");
    // The admin-unbridge step indexes pids[1], so one participant cannot exercise
    // the two-sided admin gate.
    assert!((2..=32).contains(&participants), "VKS_SOAK_PARTICIPANTS must be 2..=32, got {participants}");
    let out = std::env::var("VKS_SOAK_OUT").unwrap_or_else(|_| "target/t14b-mcu-tick-soak.json".into());
    assert!(!out.trim().is_empty(), "VKS_SOAK_OUT must not be empty");

    let audio_seconds = seconds / 2;
    let video_seconds = seconds - audio_seconds;

    let engine = McuEngine::new();
    let room = RoomId::new();
    engine.ensure_room(room);
    let pids: Vec<_> = (0..participants).map(|_| ParticipantId::new()).collect();
    for (index, pid) in pids.iter().enumerate() {
        engine.bridge_participant(
            room,
            *pid,
            vec![Track::new(*pid, TrackKind::Video), Track::new(*pid, TrackKind::Audio)],
        );
        // Decoded inputs, not per-tick workload: set them once so harness
        // allocation cannot perturb pacing.
        assert!(
            engine.set_participant_frame(room, *pid, i420_frame(FRAME_WIDTH, FRAME_HEIGHT, index as u32)),
            "I420 frame must be accepted for {pid}"
        );
    }
    let pcm = sine_pcm();
    let epoch = Instant::now();

    // Warm both engine paths before any baseline snapshot; both measured phases
    // then use their own histogram delta, so warm-up samples cannot leak in.
    let warmup_start = Instant::now();
    let mut next_audio = warmup_start;
    let mut next_video = warmup_start;
    while warmup_start.elapsed() < Duration::from_secs(warmup as u64) {
        let now = Instant::now();
        let now_ms = epoch.elapsed().as_millis() as u64;
        if now >= next_audio {
            for pid in &pids {
                engine.set_audio_slice(room, *pid, &pcm);
            }
            let _ = engine.tick_audio_at(room, now_ms);
            next_audio += Duration::from_millis(AUDIO_PERIOD_MS);
        }
        if now >= next_video {
            let _ = engine.tick_video_at(room, now_ms);
            next_video += Duration::from_millis(VIDEO_PERIOD_MS);
        }
        std::thread::sleep(Duration::from_micros(500));
    }

    // Let one full period pass since the last warm-up tick so the first measured
    // tick cannot be rejected by the 18 ms audio throttle.
    std::thread::sleep(Duration::from_millis(AUDIO_PERIOD_MS));
    let audio_baseline = histogram_snapshot();
    let (audio, audio_measured_seconds) = run_window(
        Duration::from_millis(AUDIO_PERIOD_MS),
        audio_seconds,
        epoch,
        || {
            for pid in &pids {
                engine.set_audio_slice(room, *pid, &pcm);
            }
        },
        |now_ms| engine.tick_audio_at(room, now_ms),
    );
    let audio_after = histogram_snapshot();

    let video_baseline = histogram_snapshot();
    let dropped_baseline = engine.stats(room).map_or(0, |stats| stats.frames_dropped);
    let (video, video_measured_seconds) = run_window(
        Duration::from_millis(VIDEO_PERIOD_MS),
        video_seconds,
        epoch,
        || {},
        |now_ms| engine.tick_video_at(room, now_ms).is_some(),
    );
    let video_after = histogram_snapshot();
    let dropped = engine.stats(room).map_or(0, |stats| stats.frames_dropped).saturating_sub(dropped_baseline);

    // Admin rebuilds call `process_video` directly and must stay out of the
    // histogram: clear/unbridge are the two public admin paths that re-render.
    let before_admin = histogram_snapshot();
    let rendered_before_admin = engine.stats(room).map_or(0, |stats| stats.frames_rendered);
    engine.clear_participant_frame(room, pids[0]);
    assert!(engine.set_participant_frame(room, pids[0], i420_frame(FRAME_WIDTH, FRAME_HEIGHT, 999)));
    engine.unbridge_participant(room, pids[1]);
    assert!(!engine.has_participant(room, pids[1]), "unbridge must remove the participant");
    let after_admin = histogram_snapshot();
    let admin_renders =
        engine.stats(room).map_or(0, |stats| stats.frames_rendered).saturating_sub(rendered_before_admin);
    assert_eq!(admin_renders, 2, "admin clear+unbridge must re-render exactly two frames");
    let admin_pollution = after_admin
        .get("video")
        .map_or(0, |snapshot| snapshot.count)
        .saturating_sub(before_admin.get("video").map_or(0, |snapshot| snapshot.count));

    let audio_delta = audio_after
        .get("audio")
        .cloned()
        .unwrap_or_default()
        .delta(&audio_baseline.get("audio").cloned().unwrap_or_default());
    let video_delta = video_after
        .get("video")
        .cloned()
        .unwrap_or_default()
        .delta(&video_baseline.get("video").cloned().unwrap_or_default());
    let audio_ratio = audio_delta.cumulative_ratio(AUDIO_SLO_BOUND);
    let video_budget_ratio = video_delta.cumulative_ratio(VIDEO_FRAME_BUDGET);
    let video_slo_ratio = video_delta.cumulative_ratio(AUDIO_SLO_BOUND);
    let audio_hz = audio.first_successes as f64 / audio_measured_seconds;
    let effective_video_fps = video.first_successes as f64 / video_measured_seconds;
    // Conservative wall-clock lower bound on scheduled periods in the measured
    // window (floored). A skipped period consumes one slot of the 1% budget:
    // roughly more than 1% fails, while edge rounding can hide a single slot.
    let audio_scheduled = (audio_measured_seconds * 1000.0 / AUDIO_PERIOD_MS as f64).floor() as u64;
    let video_scheduled = (video_measured_seconds * 1000.0 / VIDEO_PERIOD_MS as f64).floor() as u64;

    let report = serde_json::json!({
        "schema_version": 2,
        "timestamp_unix": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_secs()),
        "rustc": command_output("rustc", &["-V"]),
        "git_sha": command_output("git", &["rev-parse", "HEAD"]),
        // `null` when git is unavailable, otherwise any worktree change means
        // dirty. The authoritative artifact is run from a clean worktree.
        "git_dirty": git_status_porcelain().map(|status| !status.trim().is_empty()),
        "cpu_model": cpu_model(),
        "arch": std::env::consts::ARCH,
        "seconds": seconds,
        "warmup_seconds": warmup,
        "audio_window_seconds": audio_seconds,
        "video_window_seconds": video_seconds,
        "audio_measured_seconds": audio_measured_seconds,
        "video_measured_seconds": video_measured_seconds,
        "cwd": std::env::current_dir().ok().map(|path| path.display().to_string()),
        "git_toplevel": command_output("git", &["rev-parse", "--show-toplevel"]),
        "target_dir": std::env::var("CARGO_TARGET_DIR").ok(),
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "participants": participants,
        "frame": {"width": FRAME_WIDTH, "height": FRAME_HEIGHT, "format": "I420"},
        "audio": phase_report(PhaseReportInput {
            tally: &audio,
            delta: &audio_delta,
            scheduled: audio_scheduled,
            rate: audio_hz,
            le_2ms: audio_ratio,
            le_33ms: None,
            dropped: None,
        }),
        "video": phase_report(PhaseReportInput {
            tally: &video,
            delta: &video_delta,
            scheduled: video_scheduled,
            rate: effective_video_fps,
            le_2ms: video_slo_ratio,
            le_33ms: video_budget_ratio,
            dropped: Some(dropped),
        }),
        "admin_rebuild_renders": admin_renders,
        "admin_rebuild_recorded_ticks": admin_pollution,
        "histogram_buckets_seconds": MCU_TICK_BUCKETS,
        "slo": {
            "audio_engine_work_p99_le_2ms": audio_ratio.is_some_and(|ratio| ratio >= 0.99),
            "video_compose_p99_le_33ms_budget": video_budget_ratio.is_some_and(|ratio| ratio >= 0.99),
            "video_compose_p99_le_2ms": video_slo_ratio.is_some_and(|ratio| ratio >= 0.99),
            "note": "audio is engine-work p99, not end-to-end latency; the video 2 ms target is deferred to S20",
        },
        "command": format!(
            "VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS={seconds} VKS_SOAK_WARMUP={warmup} \
             VKS_SOAK_PARTICIPANTS={participants} VKS_SOAK_OUT={} \
             cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture",
            shell_quote(&out)
        ),
        "notes": "In-process engine only: no RTP, SIP, decode/encode, resize_i420, WebRTC fan-out or live DB. Audio and video are measured in separate sequential windows in one room: today's global room mutex would let video compose block the audio tick, and per-room actors/multi-room concurrency are S20. Frames are real 1280x720 I420. Each period makes one deliberate same-timestamp duplicate call that must be throttled; video throttles increment engine frames_dropped.",
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize soak report");
    if let Some(parent) = std::path::Path::new(&out).parent() {
        std::fs::create_dir_all(parent).expect("create soak report directory");
    }
    std::fs::write(&out, &text).expect("write soak report");
    println!("{text}");

    // Gates run after the report is written so a failure still leaves diagnostics.
    assert!(audio_delta.count > 0, "audio histogram is empty; zero samples is a false pass ({out})");
    assert!(video_delta.count > 0, "video histogram is empty; zero samples is a false pass ({out})");
    assert_eq!(audio_delta.count, audio.ticks, "audio histogram must count exactly the successful ticks");
    assert_eq!(video_delta.count, video.ticks, "video histogram must count exactly the rendered ticks");
    assert_eq!(audio.second_call_successes, 0, "same-timestamp audio duplicate must be throttled");
    assert_eq!(video.second_call_successes, 0, "same-timestamp video duplicate must be throttled");
    // Wall-clock period-loss gate: a skipped slot lowers first successes while
    // `scheduled` stays fixed. First successes also include 18 ms audio-throttle
    // rejects on early wakes; up to 1% of all such losses is tolerated. The floored
    // lower bound rejects a clearly larger loss (edge rounding can hide one slot).
    assert!(
        audio.first_successes * 100 >= audio_scheduled * 99,
        "audio lost more than 1% of scheduled periods: {} of {audio_scheduled}",
        audio.first_successes
    );
    assert!(
        video.first_successes * 100 >= video_scheduled * 99,
        "video lost more than 1% of scheduled periods: {} of {video_scheduled}",
        video.first_successes
    );
    assert_eq!(video.throttled, dropped, "every throttled video attempt must be visible as frames_dropped");
    assert_eq!(admin_pollution, 0, "admin rebuild paths must not be recorded as ticks");
    assert!(
        audio_ratio.is_some_and(|ratio| ratio >= 0.99),
        "audio engine work: only {:.4}% of ticks <= 2 ms (count {})",
        audio_ratio.unwrap_or(0.0) * 100.0,
        audio_delta.count
    );
    assert!(
        video_budget_ratio.is_some_and(|ratio| ratio >= 0.99),
        "video compose: only {:.4}% of ticks <= 33 ms budget (count {})",
        video_budget_ratio.unwrap_or(0.0) * 100.0,
        video_delta.count
    );
}

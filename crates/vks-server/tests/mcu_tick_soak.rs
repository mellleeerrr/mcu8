//! MCU tick SLO soak: real audio mix + video compose through `McuEngine`,
//! measured by the `vks_mcu_tick_duration_seconds` histogram.
//!
//! This deliberately runs in-process (no HTTP server, no live DB, no SIP
//! sockets): it proves the cost of the engine work itself. It is `#[ignore]`
//! because timing gates are meaningless in debug/shared-CI builds:
//!
//! ```text
//! VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS=60 \
//!     cargo test --release --test mcu_tick_soak -- --ignored --nocapture
//! ```
//!
//! Env: `VKS_SOAK_SECONDS` (default 20), `VKS_SOAK_OUT` (default
//! `target/t14-mcu-tick-soak.json`), `VKS_SOAK_PARTICIPANTS` (default 4).

use std::sync::Arc;
use std::time::{Duration, Instant};

use vks_core::{RoomId, Track, TrackKind};
use vks_media_mcu::{Frame, McuEngine};

fn frame(width: u32, height: u32, fill: u8) -> Frame {
    Frame::new(width, height, vec![fill; (width * height * 3 / 2) as usize])
}

fn percentile_from_buckets(cumulative: &[(f64, u64)], count: u64, q: f64) -> f64 {
    if count == 0 {
        return 0.0;
    }
    let target = (count as f64 * q).ceil() as u64;
    let mut previous = 0.0;
    for (bound, seen) in cumulative {
        if *seen >= target {
            // Linear interpolation inside the bucket, matching the Prometheus
            // estimator; the exact `le=0.002` ratio is the gate, this is context.
            let bucket_count = seen.saturating_sub(previous.max(0.0) as u64).max(1);
            let rank = target.saturating_sub(previous.max(0.0) as u64);
            let frac = rank as f64 / bucket_count as f64;
            let lower = if previous == 0.0 { 0.0 } else { previous };
            return lower + (bound - lower) * frac;
        }
        previous = *bound;
    }
    *cumulative.last().map(|(b, _)| b).unwrap_or(&0.0)
}

#[test]
#[ignore = "release-only timing gate: cargo test --release --test mcu_tick_soak -- --ignored"]
fn mcu_tick_soak_meets_p99_2ms() {
    std::env::set_var("VKS_MCU_TICK_METRICS", "1");
    let seconds: u64 = std::env::var("VKS_SOAK_SECONDS").ok().and_then(|v| v.parse().ok()).unwrap_or(20);
    let participants: usize = std::env::var("VKS_SOAK_PARTICIPANTS").ok().and_then(|v| v.parse().ok()).unwrap_or(4);
    let out = std::env::var("VKS_SOAK_OUT").unwrap_or_else(|_| "target/t14-mcu-tick-soak.json".into());

    let engine = Arc::new(McuEngine::new());
    let room = RoomId::new();
    engine.ensure_room(room);
    let pids: Vec<_> = (0..participants).map(|_| vks_core::ParticipantId::new()).collect();
    for (index, pid) in pids.iter().enumerate() {
        engine.bridge_participant(
            room,
            *pid,
            vec![Track::new(*pid, TrackKind::Video), Track::new(*pid, TrackKind::Audio)],
        );
        engine.set_participant_frame(room, *pid, frame(1280, 720, (index + 1) as u8));
    }

    let tick = Duration::from_millis(20);
    let video_every = 33u64;
    let started = Instant::now();
    let mut now_ms = 0u64;
    let mut next_audio = Instant::now();
    let mut next_video = Instant::now();
    let mut audio_ticks = 0u64;
    let mut video_ticks = 0u64;
    let audio: Vec<f32> = (0..960).map(|i| ((i as f32) * 0.05).sin() * 0.1).collect();
    while started.elapsed() < Duration::from_secs(seconds) {
        let now = Instant::now();
        if now >= next_audio {
            for pid in &pids {
                engine.set_audio_slice(room, *pid, &audio);
                engine.tick_audio_at(room, now_ms);
            }
            audio_ticks += 1;
            now_ms += 20;
            next_audio += tick;
        }
        if now >= next_video {
            for pid in &pids {
                engine.set_participant_frame(room, *pid, frame(1280, 720, 7));
            }
            engine.tick_video_at(room, now_ms);
            video_ticks += 1;
            next_video += Duration::from_millis(video_every);
        }
        std::thread::sleep(Duration::from_millis(2));
    }

    let families = prometheus::gather();
    let family = families
        .iter()
        .find(|f| f.name() == "vks_mcu_tick_duration_seconds")
        .expect("tick histogram must be registered when VKS_MCU_TICK_METRICS=1");
    let mut phases = std::collections::BTreeMap::new();
    for metric in family.get_metric() {
        let phase = metric.get_label().iter().find(|l| l.name() == "phase").map_or("", |l| l.value()).to_string();
        let histogram = metric.get_histogram();
        let count = histogram.get_sample_count();
        assert!(count > 0, "phase {phase} recorded no samples; empty p99 is a false pass");
        let cumulative: Vec<(f64, u64)> =
            histogram.get_bucket().iter().map(|b| (b.upper_bound(), b.cumulative_count())).collect();
        let le_2ms = cumulative
            .iter()
            .find(|(bound, _)| (*bound - 0.002).abs() < 1e-12)
            .map(|(_, seen)| *seen)
            .expect("2 ms bucket must exist");
        let ratio = le_2ms as f64 / count as f64;
        let p99 = percentile_from_buckets(&cumulative, count, 0.99);
        assert!(ratio >= 0.99, "phase {phase}: only {:.4}% of ticks <= 2 ms (count {count})", ratio * 100.0);
        phases.insert(phase, (count, ratio, p99, histogram.get_sample_sum()));
    }
    assert!(phases.contains_key("audio"), "audio ticks missing: {phases:?}");
    assert!(phases.contains_key("video"), "video ticks missing: {phases:?}");

    let report = serde_json::json!({
        "profile": if cfg!(debug_assertions) { "debug" } else { "release" },
        "seconds": seconds,
        "participants": participants,
        "audio_ticks": audio_ticks,
        "video_ticks": video_ticks,
        "phases": phases.iter().map(|(phase, (count, ratio, p99, sum))| {
            (phase.clone(), serde_json::json!({
                "count": count,
                "le_2ms_ratio": ratio,
                "p99_seconds_estimate": p99,
                "sum_seconds": sum,
            }))
        }).collect::<serde_json::Map<_, _>>(),
        "notes": "In-process engine only: no RTP, SIP, decode/encode, resize or network. Not a full media SLO.",
    });
    let text = serde_json::to_string_pretty(&report).expect("serialize soak report");
    std::fs::write(&out, &text).expect("write soak report");
    println!("{text}");
}

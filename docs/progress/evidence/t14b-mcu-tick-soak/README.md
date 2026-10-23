# T14 — MCU tick SLO / soak (2026-10-23)

## What is measured

`vks_mcu_tick_duration_seconds{phase="audio"|"video"}` is recorded inside
`RoomState::tick_audio_at` / `tick_video_at` around the actual engine work
(audio mix + VAD / video compose), excluding network, input cloning before the
throttle and composite cloning after it. The metric is registered only when
`VKS_MCU_TICK_METRICS=1`; a default deployment does not even expose the family.

## Harness

`crates/vks-media-mcu/tests/mcu_tick_soak.rs` (ignored timing test, release
only) creates one MCU room, bridges N participants with audio+video tracks,
feeds 960-sample PCM and 1280×720 I420 frames, and drives the real engine at
20 ms (audio) / 33 ms (video) cadence for the configured duration. It then
reads the Prometheus histogram and writes a JSON report.

```bash
VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS=60 \
  VKS_SOAK_OUT=docs/progress/evidence/t14/soak-60s-4p.json \
  cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture
```

Env: `VKS_SOAK_SECONDS` (default 20), `VKS_SOAK_PARTICIPANTS` (default 4),
`VKS_SOAK_OUT` (default `target/t14-mcu-tick-soak.json`).

## Result (60 s, 4 participants, release, 1 room)

| phase | samples | exact `le=2 ms` ratio | p99 (bucket interpolation) | sum |
|---|---:|---:|---:|---:|
| audio mix | 3000 | **1.000** | **0.22 ms** | 0.196 s |
| video compose | 1182 | 0.000 | **19.8 ms** | 12.13 s |

- Audio SLO **p99 ≤ 2 ms is met** with a wide margin (0.22 ms p99, 65 µs mean).
- Video compose p99 is ~20 ms at 4×720p inputs onto the fixed 1920×1080 canvas.
  It is within the 33 ms frame budget but **not within 2 ms**; the 2 ms target
  for video needs SIMD/zero-copy/parallel-tile work (next slice).

Compositor optimisation included in this slice: replaced per-pixel `f64`
division with 16.16 fixed-point stepping, hoisted YUV checks, row slices and
built the canvas in one allocation. Before: video p99 ≈ 80 ms (30 s run, same
load); after: ≈ 20 ms.

## Honest limits

- In-process engine only: no RTP/jitter/loss, no SIP/H.323 signalling, no
  decode/encode, no `resize_i420`, no WebRTC fan-out, no live DB.
- One room/one process does not model global `rooms` mutex contention,
  allocator pressure or CPU frequency scaling across 6–8 rooms.
- This is not a full media SLO and not a substitute for 24–72 h live soak.

## 24–72 h procedure

1. Enable `VKS_MCU_TICK_METRICS=1` on the target service, restart.
2. Drive real SIP/H.323 calls (or the in-process harness for engine-only soak)
   for the required duration; scrape `/metrics` every 15 s into Prometheus.
3. Gate on the exact ratio, not the estimator:
   `sum(rate(vks_mcu_tick_duration_seconds_bucket{phase="audio",le="0.002"}[5m])) / sum(rate(vks_mcu_tick_duration_seconds_count{phase="audio"}[5m])) >= 0.99`.
4. Watch RSS/fd trend of the server process, `vks_mcu_tick_duration_seconds_count`
   growth, 5xx rate, and `media_bridge_dead` teardowns.
5. Keep `VKS_MCU_TICK_METRICS` off in production until the overhead is accepted
   (it is two `Instant::now` calls per successful tick and atomic observes).

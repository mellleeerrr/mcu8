# T14b — MCU engine tick SLO / soak (2026-10-23)

## Scope

`vks_mcu_tick_duration_seconds{phase="audio"|"video"}` measures **engine work**
inside `RoomState::tick_audio_at` / `tick_video_at`: audio mix + mix-minus + VAD,
or video `slot_rects` + `Compositor::compose`. It excludes RTP/jitter/loss, SIP
and H.323 signalling, decode/encode, `resize_i420`, WebRTC fan-out, the
composite clone, input cloning before the throttle, lock wait and the live DB.
The audio number is therefore an **audio engine-work** p99, not end-to-end
latency. The histogram is opt-in (`VKS_MCU_TICK_METRICS=1`, default off); with
the flag unset the family is absent from `/metrics` (covered by
`crates/vks-common/tests/metrics_default_off.rs`), and startup materialises
zero-count children instead of a synthetic `observe(0.0)` sample.

## Harness

`crates/vks-media-mcu/tests/mcu_tick_soak.rs` is a release-only `#[ignore]`
test. It creates one MCU room, bridges four participants with audio+video
tracks, feeds 960-sample near-full-scale PCM (exercises the mixer's `tanh`
soft-clip), sets four 1280×720 I420 frames once, and drives the real engine
from the **wall clock**: audio once per 20 ms plus one same-instant duplicate
call (must be throttled and must not be recorded), video every 33 ms. A 3 s
warm-up is discarded. The report is written before the gates so a failure still
leaves diagnostics. Gates assert: histogram count equals successful ticks;
throttled video attempts equal `stats().frames_dropped`; an admin
`clear_participant_frame` rebuild records no tick; audio ≥45 ticks/s and video
≥28 fps; exact audio `le=2 ms` ratio ≥0.99; exact video `le=33 ms` ratio ≥0.99.
An empty histogram or a debug build fails hard.

```bash
# from the repository root
VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS=60 VKS_SOAK_WARMUP=3 \
  VKS_SOAK_OUT="$PWD/docs/progress/evidence/t14b-mcu-tick-soak/soak-60s-4p.json" \
  cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture
```

Env: `VKS_SOAK_SECONDS` (5..=3600, default 20), `VKS_SOAK_WARMUP` (default 2),
`VKS_SOAK_PARTICIPANTS` (1..=32, default 4), `VKS_SOAK_OUT` (default
`target/t14b-mcu-tick-soak.json`). Invalid values fail instead of silently
falling back.

## Result — 60 s window (57.0 s measured), 4 participants, release

Run `cb57aa2`, dirty=false, Intel Xeon E5-2699 v3, rustc 1.94.0. Full machine
report: [`soak-60s-4p.json`](soak-60s-4p.json).

| phase | samples | exact `le=2ms` | p99 bucket (estimate) | mean |
|---|---:|---:|---:|---:|
| audio engine work | 2566 | **1.000** | (0.1, 0.25] ms (0.249 ms) | 0.130 ms |
| video compose | 1720 | 0.000 | (10, 20] ms (19.6 ms) | 9.87 ms |

Video effective render cadence **30.17 fps**, exact `le=33 ms` ratio **1.000**,
`frames_dropped` delta 0. Audio exact `le=2 ms` ratio **1.000**.

**Verdict:** the audio engine-work p99 ≤2 ms SLO is met with a wide margin. Video
compose is within the 33 ms frame budget but **not** within 2 ms; its p99 is only
known to be in (10, 20] ms because that is the histogram bucket width. The
video 2 ms target is deferred to S20 (SIMD/zero-copy/parallel tiles and
per-room actors). No claim is made that the full media tick meets 2 ms.

Compositor work in this slice: exact integer nearest-neighbour mapping
(`fx = dx*fw/dw` via incremental quotient/remainder instead of per-pixel `f64`
division), integer letterbox geometry, row slices, single-allocation canvas.
Golden gradient/letterbox tests pin the mapping. This reduced compose cost
substantially, but no archived "before" JSON is claimed here: the measurement
above is a single reproducible after-state run.

## Honest limits

- In-process engine only: no RTP/jitter/loss/PLC, SIP/H.323, decode/encode,
  `resize_i420`, WebRTC fan-out, live DB or network.
- One room in one process: no contention on the global `rooms` mutex between
  rooms, no allocator/page-fault pressure or CPU frequency scaling at 6–8 rooms.
  Lock wait and input cloning happen **outside** the histogram by design.
- Audio load is four participants; the signal is near full scale but not a
  stress corpus.
- 2566/1720 samples are enough for the exact rank gates, not for a precise p99
  point estimate; the report labels the estimate and its bucket interval.
- This artifact does not prove that `/metrics` was scraped on the live service.
  The live service currently has no active media rooms, so a live API-only soak
  would record zero samples; the default-off behaviour is verified separately.

## 24–72 h procedure

### A. Live service (real SIP/H.323 media)

1. Set `VKS_MCU_TICK_METRICS=1` in the unit environment, restart deliberately,
   verify `/readyz` and that `/metrics` exposes the family with count 0.
2. Drive real SIP/H.323 calls/rooms for the required duration; scrape
   `/metrics` every 15 s and sample server RSS/fd count.
3. Gate on the exact cumulative ratio (not `histogram_quantile`) with a
   minimum-count guard:
   ```promql
   sum(rate(vks_mcu_tick_duration_seconds_bucket{phase="audio",le="0.002"}[5m]))
   /
   sum(rate(vks_mcu_tick_duration_seconds_count{phase="audio"}[5m])) >= 0.99
   and
   sum(rate(vks_mcu_tick_duration_seconds_count{phase="audio"}[5m])) > 1
   ```
   Repeat with `phase="video",le="0.033"` for the frame-budget gate.
4. Watch `_count` growth against expected tick cadence, 5xx/WS error rate,
   RSS/fd slope, and count `media_bridge_dead` terminations in the log.
5. Keep the flag off by default until the canary accepts the overhead (two
   `Instant::now` calls + atomic observes per successful tick).

### B. Engine-only (this harness)

Run the command above with `VKS_SOAK_SECONDS=259200` (72 h) for a long engine
soak; snapshot the JSON periodically. This path has no `/metrics` endpoint and
no server process, so RSS/fd and Prometheus scraping belong to procedure A.

## Artifacts

- `soak-60s-4p.json` — machine report (provenance, buckets, attempts, gates).
- `SUMMARY.md` — short outcome and review history.
- `SHA256SUMS` — hashes; verify from the repository root with
  `sha256sum -c docs/progress/evidence/t14b-mcu-tick-soak/SHA256SUMS`.

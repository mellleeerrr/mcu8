# T14b — MCU engine tick SLO / soak (2026-10-23)

## Scope

`vks_mcu_tick_duration_seconds{phase="audio"|"video"}` measures **engine work**
inside `RoomState::tick_audio_at` / `tick_video_at`: audio mix + mix-minus + VAD,
or video `slot_rects` + `Compositor::compose`. It excludes RTP/jitter/loss, SIP
and H.323 signalling, decode/encode, `resize_i420`, WebRTC fan-out, the
composite hand-off, input cloning before the throttle, lock wait and the live DB.
The audio number is therefore an **audio engine-work** p99, not end-to-end
latency. The histogram is opt-in (`VKS_MCU_TICK_METRICS=1`, default off); with
the flag unset the family is absent from `/metrics`. At startup with the flag on,
both phases exist at count=0/sum=0 — no synthetic `observe(0.0)` sample.

## Harness

`crates/vks-media-mcu/tests/mcu_tick_soak.rs` is a release-only `#[ignore]`
test (it compiles in debug and exits early, so `cargo test --workspace` stays
green). One room, four participants, real **1280×720 I420** frames with 2-D luma and
non-constant U/V planes, feeding 960-sample near-full-scale PCM (exercises
the mixer's `tanh` soft-clip). The measured window has two **sequential**
phases: audio alone at 20 ms, then video alone at 34 ms. They are not concurrent
because today's global room mutex would let a 15–27 ms compose block the audio tick;
per-room actors and multi-room concurrency are S20. A 3 s warm-up is discarded.

Run timing gates on an idle or pinned host: the wall-clock loss gate is
load-sensitive (an unpinned 20 s run under heavy CI load can legitimately exceed
the 1% budget). Each period makes one deliberate **same-timestamp duplicate**
call that must be throttled. The report counts attempts, first successes, throttled calls,
`second_call_successes` (must be 0), histogram count and engine
`frames_dropped` (video), so the throttle/drop gates are not vacuous. After the
windows, `clear_participant_frame` + `unbridge_participant` are invoked and the
histogram count must not change. The report is written before assertions so a
failure still leaves diagnostics; an empty histogram fails hard.

```bash
# from the repository root; for git_dirty=false run from a clean worktree at the
# recorded commit (the shared checkout can have concurrent edits):
git worktree add --detach /var/tmp/vks-t14b-clean <commit>
cd /var/tmp/vks-t14b-clean
CARGO_TARGET_DIR="$OLDPWD/target" \
VKS_MCU_TICK_METRICS=1 VKS_SOAK_SECONDS=60 VKS_SOAK_WARMUP=3 \
VKS_SOAK_PARTICIPANTS=4 VKS_SOAK_OUT="$OLDPWD/docs/progress/evidence/t14b-mcu-tick-soak/soak-60s-4p.json" \
  cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored --nocapture
```

Env: `VKS_SOAK_SECONDS` (5..=604800, default 20), `VKS_SOAK_WARMUP` (default 3),
`VKS_SOAK_PARTICIPANTS` (1..=32, default 4), `VKS_SOAK_OUT` (default
`target/t14b-mcu-tick-soak.json`). Invalid values fail instead of silently
falling back.

## Result — 60 s window (30 s per phase), 4 participants, release

Run `2f5ba5e`, clean worktree (`git_dirty=false`), Intel Xeon E5-2699 v3,
rustc 1.94.0. Machine report: [`soak-60s-4p.json`](soak-60s-4p.json). Reproducers
should write `VKS_SOAK_OUT` to `/tmp`; the command above overwrites the canonical
artifact.

| phase | samples | exact `le=2ms` | p99 bucket (estimate) | mean | rate |
|---|---:|---:|---:|---:|---:|
| audio engine work | 1500 | **1.000** | (0.1, 0.25] ms (0.249 ms) | 0.141 ms | 50.0 Hz |
| video compose (I420) | 883 | 0.000 | (10, 20] ms (20.0 ms) | 15.31 ms | 29.43 fps |

Video exact `le=33 ms` ratio **1.000**, engine `frames_dropped` delta 883 (all
883 are the deliberate duplicate/throttle probes; first successes 883), audio
exact `le=2 ms` ratio **1.000**, admin rebuilds rendered 2 frames and recorded
0 ticks. All 1500 wall-clock audio periods produced their first tick; the audio
histogram count equals the tick count (1500).

**Verdict:** the audio engine-work p99 ≤2 ms SLO is met with a wide margin. Video
compose fits the 33 ms frame budget in this workload but **not** the 2 ms target;
its p99 is only known to be in (20, 33] ms because that is the histogram bucket
width. The video 2 ms target is deferred to S20 (SIMD/zero-copy/parallel tiles,
per-room actors). No claim is made that the full media tick meets 2 ms.

Compositor work in this slice: exact integer nearest-neighbour mapping
(`fx = floor(dx*fw/dw)` via incremental quotient/remainder), integer letterbox
geometry, row slices, single-allocation canvas. Golden tests pin 2-D luma,
non-constant U/V planes and PiP letterbox-bar behaviour. The timer wraps only
`process_video`; the harness's 34 ms idle gap is not measured. Frames are static
and warmed during the warm-up window, but a long idle gap can still leave
cold-cache/page-fault effects in the first frames. The 33 ms figure is a budget
margin for this harness, not a warm-cache per-frame cost; no back-to-back
speedup or before/after claim is made. No archived "before" JSON is
claimed; this is a single reproducible after-state measurement.

## Honest limits

- In-process engine only: no RTP/jitter/loss/PLC, SIP/H.323, decode/encode,
  `resize_i420`, WebRTC fan-out, live DB or network.
- One room in one process; audio and video measured **sequentially**. The global
  `rooms` mutex, allocator/page-fault pressure and 6–8-room contention are not
  measured; S20 owns per-room actors and the multi-room p99.
- Audio load is four participants; the signal is near full scale but not a
  stress corpus.
- 1500/883 samples support the exact rank gates, not a precise p99 point; the
  report labels the estimate and its bucket interval.
- This artifact does not prove that `/metrics` was scraped on the live service;
  procedure A below is how to obtain one. Default-off absence is covered by
  `crates/vks-common/tests/metrics_default_off.rs`, and the env-on zero-count
  startup by `crates/vks-common/tests/metrics_tick_zero.rs`.

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
5. Keep the flag off by default until the canary accepts the overhead (one
   `Instant::now` + histogram observe per successful phase tick).

### B. Engine-only (this harness)

Run the command above with `VKS_SOAK_SECONDS=259200` (72 h; the cap is 604800 s)
for one long engine soak. The report is written when the window ends, so for
periodic snapshots run shorter segments (for example 12 × 6 h) with distinct
`VKS_SOAK_OUT` paths, or use procedure A for a live scrape. Use a distinct
`VKS_SOAK_OUT` for a long run: the canonical 60 s artifact must not be
overwritten. This path has no `/metrics` endpoint and no server process, so
RSS/fd and Prometheus scraping belong to procedure A.

## Artifacts

- `soak-60s-4p.json` — machine report (provenance, buckets, attempts, gates).
- `SUMMARY.md` — short outcome and review history.
- `SHA256SUMS` — hashes; verify from the repository root with
  `sha256sum -c docs/progress/evidence/t14b-mcu-tick-soak/SHA256SUMS`.

# vks-media-mcu

The MCU engine for Phase 3 (DEVELOPMENT_PLAN.md). It implements the
`vks_core::MediaEngine` port in MCU mode: a server-side compositor that
takes N video tracks, composites them onto a 1920×1080 canvas, mixes N
audio tracks into one, and serves a single composite track to each
subscriber (MCU/SFU transparency, ARCHITECTURE §2).

## Modules

| Module | Role |
|---|---|
| `engine` | `McuEngine` — impl `MediaEngine` (MCU); in-memory room state wiring compositor + mixer + VAD + layout bridge. Pluggable mock frame/audio sources drive the tests. |
| `frame` | `Frame` / `CompositeFrame` placeholder raw-frame types; `VideoEncoder` / `VideoDecoder` abstractions with deterministic mock codecs (stand-in for H.264/VP8; real codecs behind the `oxideav` feature, ADR-0004). |
| `compositor` | N input frames + per-slot `PixelRect`s → a 1920×1080 `CompositeFrame`. Default: nearest-neighbour uniform-fit (letterbox, centered). The real bilinear (libyuv) compositor is feature-gated. |
| `audio_mixer` | N PCM buffers (f32, 48 kHz) → 1 mixed output: sample-wise sum × `1/√N` (AGC), tanh soft-clip at ±1.0. |
| `vad` | Speaker detection: RMS + zero-crossing on 20 ms frames (960 samples @ 48 kHz), 500 ms debounce (hysteresis — no flapping). |
| `layout_bridge` | `vks-layout::Layout` → pixel `Rect`s with a 300 ms crossfade on layout changes (linear interpolation over the window, snap after). |

## Composite pipeline

```text
N publishers (video + audio tracks)
        │  add_publisher
        ▼
   room state ── apply_layout → vks_layout::Layout
        │                    │
        ▼                    ▼
  mock frame source     LayoutBridge (300 ms crossfade)
  (per-slot frames)      → per-slot Rects
        │                  │
        └──────┬───────────┘
               ▼
         Compositor (N frames + 1920×1080 canvas,
          nearest-neighbour letterbox)
          │
        CompositeFrame (1920×1080)
          │  (real build: H.264 1080p30 2.5 Mbps CBR, ADR-0004)
          ▼
      1 composite track per subscriber
```

Audio: each publisher's 20 ms PCM frame is summed with AGC `1/√N` and
soft-clipped; the mix is also fed to the VAD for speaker detection.

Frame rate is fixed at 30 fps (drop/repeat, no interpolation): the
engine advances the crossfade clock ~33 ms per tick.

## How it maps to `MediaEngine` + layout

- `McuEngine::mode()` → `RoomMode::Mcu`.
- `create_room` / `destroy_room` — room lifecycle in the engine.
- `add_publisher` / `remove_publisher` — register a participant's tracks.
- `set_layout` — the core [`vks_core::LayoutDescriptor`] is converted
  into a `vks_layout::Layout` (one slot per participant, computed by
  `GridLayout` on the 1920×1080 canvas) and applied through the
  LayoutBridge (300 ms crossfade).
- `set_room_capabilities` — stores the room's codec capabilities.
- `list_publishers` / `list_subscribers` — active lists.

Subscribers all receive the single composite track (that is the MCU/SFU
transparency invariant: the client does not know which mode serves it).

## Mocked vs real

- **Mocked (default build):** video encode/decode (deterministic mock
  codec with a header), frame contents (pluggable sources), the
  compositor scaler (nearest-neighbour instead of bilinear).
- **Real (behind `oxideav` feature, ADR-0004):** oxideav-scene codecs and
  the bilinear compositor. The default build is dependency-light so the
  workspace baseline stays green.

## Invariants

- No `unsafe` (CODE_STYLE §6).
- Deterministic: same input sequence → same composite.
- Fixed 30 fps (drop/repeat, no interpolation).

## Build

```text
cargo build -p vks-media-mcu                 # default (dependency-light)
cargo build -p vks-media-mcu --features oxideav   # real compositor/codec
cargo test -p vks-media-mcu
```

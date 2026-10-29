//! `vks-media-mcu` — MCU engine (compositor).
//!
//! Phase 3 (DEVELOPMENT_PLAN.md): server-side MCU — composites N video
//! tracks onto a 1920×1080 canvas, mixes N audio tracks into one, and
//! serves a single composite track per subscriber (MCU/SFU transparency,
//! ARCHITECTURE §2).
//!
//! ## Modules
//! - [`engine`] — [`McuEngine`], impl `vks_core::MediaEngine` (MCU);
//!   in-memory MCU wiring the compositor + mixer + VAD + layout bridge.
//! - [`frame`] — `Frame` placeholder type (raw pixel buffer).
//! - [`compositor`] — N frames + slot rects → 1920×1080 canvas
//!   (nearest-neighbour letterbox; the real bilinear compositor is
//!   feature-gated).
//! - [`audio_mixer`] — N PCM (f32, 48 kHz) → 1 mixed output
//!   (sum × 1/√N AGC, tanh soft-clip).
//! - [`vad`] — speaker detection (RMS + zero-crossing, 20 ms frames,
//!   500 ms debounce).
//! - [`layout_bridge`] — `vks-layout::Layout` → pixel rects with a 300 ms
//!   crossfade.
//!
//! ## What is mocked vs real
//! - Mocked (default build): video codec (encode/decode), frame
//!   contents, the compositor scaler (nearest-neighbor instead of
//!   bilinear). Frame source and audio source are pluggable in the
//!   engine so tests drive them.
//! - Real: FFmpeg workers (H.264/H.265) и собственный композитор;
//!   oxideav-scene не используется (нет ни скейлера, ни композитора).
//!
//! ## Invariants
//! - No `unsafe` in the engine; the only `unsafe` is the isolated
//!   `libc::kill(-pgid)` in `codec_worker` (ADR-0028, `// SAFETY:`).
//! - Deterministic: same input sequence → same composite.
//! - Fixed 30 fps pipeline (drop/repeat, no interpolation).
//!
//! Tree: vks-core (MediaEngine port), vks-layout (geometry).

pub mod audio_mixer;
pub mod codec_worker;
pub mod compositor;
pub mod engine;
pub mod farm;
pub mod frame;
pub mod layout_bridge;
pub mod vad;

// Re-exports (flat API).
pub use audio_mixer::{AudioMixer, SAMPLE_RATE_HZ};
pub use codec_worker::{
    AudioCodec, FfmpegCapabilities, FfmpegWorker, SlotConfig, VideoCodec, WorkerConfig, WorkerError, WorkerMode,
    WorkerPool,
};
pub use compositor::{Compositor, CANVAS_SIZE};
pub use engine::{group_bitrate_kbps, EventSink, LayoutSnapshot, McuEngine, RoomEvent, GROUP_BITRATE_CEILING_PCT};
pub use farm::{
    AccessUnit, EncoderFarm, EncoderKey, EncoderSpec, FarmCodec, FarmError, GroupStat, GroupViewer, ViewerRtp,
};
pub use frame::Frame;
pub use layout_bridge::{LayoutBridge, PixelRect, SlotKey, SlotRects, CROSSFADE_MS};
pub use vad::{Vad, VadConfig, VadState, DEBOUNCE_MS, FRAME_MS, FRAME_SAMPLES};

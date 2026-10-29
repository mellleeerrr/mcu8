//! `vks-client-sdk` — SDK для видеоконференций (Rust + WASM).
//!
//! Единый API `Conference::join()` + signaling (WS/WebTransport) + media
//! renderers (SFU/MCU через vks-layout) + WebRTC glue.
//!
//! Основание: `docs/architecture/ARCHITECTURE.md` §233-243 (SDK-дерево),
//! `docs/DEVELOPMENT_PLAN.md` Фаза 2 (vks-client-sdk), `docs/rules/FRONTEND_RULES.md`
//! §7 (SDK API), Q8 (WebRTC in WASM).
//!
//! Цели: `wasm32-unknown-unknown` (браузер) + native (десктоп/мобильные).

pub mod conference;
pub mod layout;
pub mod media_engine;
pub mod render_surface;
pub mod signaling_client;
pub mod sync_helper;
pub mod types;
pub mod webrtc_glue;

pub use conference::{Conference, ConferenceConfig, ConferenceState};
pub use layout::normalized_layout_from_proto;
pub use media_engine::MediaEngine;
pub use types::{ConferenceEvent, QualityReport};

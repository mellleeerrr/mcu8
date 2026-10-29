//! `vks-gateway-h323` — H.323-шлюз: H.323 → WebRTC (ADR-0007, H323Plus FFI).
//!
//! Фаза 6 (DEVELOPMENT_PLAN.md):
//!
//! - H.323 → WebRTC bridge: вызов «приземляется» в WebRTC-комнату
//!   (participant = gateway, для WebRTC-клиентов прозрачно —
//!   ARCHITECTURE §2);
//! - Codec negotiation (H.323 capability → WebRTC SDP):
//!   H.263/H.264 → VP8/VP9, G.711/G.729 → Opus (транскодирование при
//!   mismatch);
//! - **FFI-граница** (H323Plus, BACKEND_RULES §6.2): одна точка,
//!   panic hook (не роняет процесс), watchdog (FFI call > 5s → restart).
//!
//! Текущая волна: self-contained testable core — [`codec`] (таблица
//! соответствий) и [`call`] (H.323-вызов FSM + in-memory шлюз). Транспорт —
//! Python-сайдкар на pycrate; нативный Q.931/H.225/H.245 — отдельный эпик (см.
//! docs/audit/2026-10-29/02_h323_p6.md); дефолтная сборка dep-light, без `unsafe`.
//!
//! ## Module
//! - [`codec`] — H.323 ↔ WebRTC codec negotiation (deterministic map).
//! - [`call`] — H.323 call FSM (H.225/Q.931) and in-memory gateway.
//!
//! Dependency tree: vks-core, vks-common (ARCHITECTURE §3).

/// Symmetric static RTP payload types the H.323 single-codec bridge supports.
pub const H323_NATIVE_AUDIO_PTS: &[u8] = &[0, 4, 8, 9];

pub mod call;
pub mod codec;
pub mod q931;
pub mod uac;
pub mod uas;

pub use call::{H323Call, H323CallState, H323Gateway};
pub use codec::{H323Codec, H323CodecMap, NegotiatedCodec, WebRtcCodec};
pub use q931::{
    build_q931_release_complete, encode_tpkt, parse_q931_message, Q931Message, MSG_ALERTING, MSG_CALL_PROCEEDING,
    MSG_CONNECT, MSG_RELEASE_COMPLETE, MSG_SETUP,
};
pub use uac::{ActiveH323Session, H323UacClient, H323VideoProfile};
pub use uas::{read_event, H323EventStream, H323Uas, H323UasHandle};

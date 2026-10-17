//! `vks-gateway-sip` — SIP-шлюз: SIP ↔ WebRTC (ADR-0006).
//!
//! Фаза 6 (DEVELOPMENT_PLAN.md):
//!
//! - SIP UAS (rvoip): приём SIP-вызовов;
//! - «приземление» вызова в WebRTC-комнату (participant = gateway,
//!   для WebRTC-клиентов прозрачно — ARCHITECTURE §2);
//! - Codec negotiation (SIP SDP → WebRTC SDP): G.711 ↔ Opus,
//!   H.264 ↔ VP8, транскодирование при mismatch;
//! - DTLS: gateway = DTLS server (RFC 8831, ULP); ICE: gateway = ICE-lite
//!   (в полной сборке через rvoip/str0m; в этой волне — mock-реестр).
//!
//! Текущая волна: self-contained testable core — [`codec`] (таблица
//! соответствий) и in-memory UAS [`call`] с mock-реестром комнат.
//! Тяжёлая зависимость rvoip — за опциональным feature `rvoip`;
//! дефолтная сборка dep-light (без FFI, без `unsafe`).
//!
//! ## Module
//! - [`codec`] — SIP ↔ WebRTC codec negotiation (deterministic map).
//! - [`call`] — SIP call FSM (Idle/Ringing/Active/Terminated) и
//!   in-memory SIP gateway (UAS) + mock room registry.
//!
//! Dependency tree: vks-core, vks-common (ARCHITECTURE §3).

pub mod call;
pub mod codec;

pub use call::{CallState, SipCall, SipGateway};
pub use codec::{CodecMap, NegotiatedCodec, SipCodec, WebRtcCodec};

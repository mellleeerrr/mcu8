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
pub mod rtp_bridge;
pub mod sip_message;
pub mod uac;

pub use call::{CallState, SipCall, SipGateway};
pub use codec::{CodecMap, NegotiatedCodec, SipCodec, WebRtcCodec};
pub use rtp_bridge::{
    build_offer, build_plain_sdp, decode_ulaw, downsample_48k_to_8k, encode_ulaw, negotiate_media,
    negotiate_media_detailed, parse_remote_media_ports, parse_rtp_header, ulaw_to_f32, upsample_8k_to_48k,
    write_rtp_header, CodecProfile, NegotiatedMedia, NegotiationFailure, OfferCodec, PortPair, RtpPortPool,
    AUDIO_OFFER, VIDEO_OFFER,
};
pub use sip_message::{build_ack, build_bye, build_cancel, build_invite, SipResponse};
pub use uac::{ActiveSipSession, SipDialError, SipUacClient};

//! `vks-signaling` — сигнализация VKS.
//!
//! Фаза 1: **Session FSM** (SIGNALING_PROTOCOL §11) + **envelope-кодек**
//! (§1, поверх `vks-proto::SignalingMessage`).
//!
//! Фаза 2: **транспорты** — `ws.rs` (WebSocket, основной R5) и `wt.rs`
//! (WebTransport/QUIC) — как трейты + in-memory mock-impl'ы; **`sdp.rs`**
//! (SDP offer/answer + ICE, §4) и **`handler.rs`** (диспетчер сообщений,
//! привязанный к Session FSM).
//!
//! T13: `webtransport.rs` is the **real** native WebTransport endpoint
//! (wtransport/QUIC/TLS 1.3), feature-gated by `webtransport`. The mock
//! `wt.rs` stays for transport-agnostic handler tests; it is not a listener.
//!
//! Источник: `docs/architecture/SIGNALING_PROTOCOL.md`.
//! Дерево: vks-core, vks-proto, vks-common (ARCHITECTURE.md §3).

pub mod envelope;
pub mod handler;
pub mod sdp;
pub mod session;
#[cfg(feature = "webtransport")]
pub mod webtransport;
pub mod ws;
pub mod wt;

pub use envelope::Envelope;
pub use handler::{HandlerConfig, SignalingHandler};
pub use sdp::{Media, MediaKind, Sdp};
pub use session::{InvalidTransition, Session, SessionEvent, SessionState};
#[cfg(feature = "webtransport")]
pub use webtransport::{WtConfig, WtError, WtIdentity, WtServer, WtTokenVerifier, WT_PATH};
pub use ws::{MockWs, WsSession};
pub use wt::{MockWt, WebTransportSession};

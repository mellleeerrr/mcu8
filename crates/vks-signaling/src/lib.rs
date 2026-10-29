//! `vks-signaling` — сигнализация VKS.
//!
//! Фаза 1: **Session FSM** (SIGNALING_PROTOCOL §11) + **envelope-кодек**
//! (§1, поверх `vks-proto::SignalingMessage`).
//!
//! Фаза 2: **транспорты** — `ws.rs` (WebSocket, основной R5) и `wt.rs`
//! (WebTransport/QUIC, обязательный второй R5) — как трейты + in-memory
//! mock-impl'ы; **`sdp.rs`** (SDP offer/answer + ICE, §4) и **`handler.rs`**
//! (диспетчер сообщений, привязанный к Session FSM).
//!
//! Источник: `docs/architecture/SIGNALING_PROTOCOL.md`.
//! Дерево: vks-core, vks-proto, vks-common, vks-auth (ARCHITECTURE.md §3).

pub mod envelope;
pub mod handler;
pub mod sdp;
pub mod session;
pub mod ws;
pub mod wt;

pub use envelope::Envelope;
pub use handler::{HandlerConfig, SignalingHandler};
pub use sdp::{Media, MediaKind, Sdp};
pub use session::{InvalidTransition, Session, SessionEvent, SessionState};
pub use ws::{MockWs, WsSession};
pub use wt::{MockWt, WebTransportSession};

//! `vks-media-sfu` — SFU-движок (str0m).
//!
//! **Фаза 2** (DEVELOPMENT_PLAN.md): наполнен по `docs/architecture/
//! MEDIA_PIPELINE.md` §2. Модули:
//!
//! - [`engine`] — [`SfuEngine`], impl `vks_core::MediaEngine` (SFU);
//!   in-memory SFU с loopback-раном.
//! - [`forwarder`] — полный loop, forward-тер N+1 (N; 1-to-1 = 1
//!   подписатель; full-mesh по умолчанию).
//! - [`simulcast`] — 3 пространственных layer (sim/Med/High)
//!   hsim hysteresis.
//! - [`bwe`] — TWCC bandwidth estimator (sliding window).
//! - [`dtls`] — DTLS-SRTP session state (str0m, optional feature).
//! - [`congestion`] — congestion ladder (degrade/recovery).
//! - [`ice`] — ICE-lite (host candidates only, RFC 8445 §2).
//! - [`codec`] — codec feature flags (ADR-0017).
//!
//! **MediaEngine-порт:** `SfuEngine` реализует `vks_core::MediaEngine`
//! (SFU). Клиентский SDK работает с трейтом — MCU/SFU-прозрачность
//! (ARCHITECTURE.md §2).
//!
//! Дерево: vks-core, vks-proto, vks-common (ARCHITECTURE.md §3).

pub mod bwe;
pub mod codec;
pub mod congestion;
pub mod dtls;
pub mod engine;
pub mod forwarder;
pub mod ice;
pub mod simulcast;
pub mod udp_loop;

// Re-exports (flat API).
pub use bwe::{BweConfig, BweEstimator, BweResult, TwccReportedPacket};
pub use codec::{CodecEntry, CodecKind, CodecRegistry};
pub use congestion::{CongestionConfig, CongestionLadder, CongestionLevel, LadderStep};
pub use dtls::{CipherSuite, DtlsConfig, DtlsRole, DtlsSession, DtlsState, SrtpKeyingMaterial};
pub use engine::SfuEngine;
pub use forwarder::{ForwardTarget, Forwarder, ForwarderConfig, RecordingSink, RtpPacket, Sink};
pub use ice::{HostCandidate, IceLite, LocalAddrProvider, LoopbackProvider, StaticAddrs, Transport};
pub use simulcast::{Selection, SimulcastConfig, SimulcastLayer, SimulcastSelector};
pub use udp_loop::{UdpMediaLoop, UdpMediaStats};

/// str0m (optional, feature `str0m`) — re-export для фаз3 (реальный
/// DTLS handshake + SRTP). Без feature крейт компилируется.
#[cfg(feature = "str0m")]
pub use str0m;

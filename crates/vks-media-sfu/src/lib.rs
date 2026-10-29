//! `vks-media-sfu` — настоящая WebRTC-медиаплоскость SFU (str0m).
//!
//! Плоскость — только DTLS/SRTP-сессия на каждого браузера: ICE (host
//! candidates, ICE-lite на сервере), DTLS, SRTP/SRTCP, NACK/RTX, jitter
//! buffer, TWCC/REMB — всё внутри `str0m`. Серверного plaintext-форвардера,
//! UDP-медиалупа, BWE/simulcast-легаси больше нет (T11c).
//!
//! - [`webrtc`] — sans-I/O обёртка над `str0m` ([`WebRtcPeer`]): приём
//!   датаграмм, реальный SDP-answer, per-source egress mids (T11b).
//! - [`webrtc_room`] — реестр реальных пиров ([`WebRtcRoomRegistry`]),
//!   UDP-сокет и таймер на каждого пира, per-publisher keyframe gate.
//! - [`registry`] — [`MediaRegistry`], roster комнат/паблишеров/подписчиков
//!   для сигналинга (`TrackAdded`, раскладка, подписки); медиа через него не
//!   идёт.
//!
//! **MediaEngine-порт:** [`MediaRegistry`] реализует `vks_core::MediaEngine`
//! — сервер работает с трейтом, а не с транспортом.
//!
//! Дерево: vks-core, vks-common.

pub mod registry;
#[cfg(feature = "str0m")]
pub mod webrtc;
#[cfg(feature = "str0m")]
pub mod webrtc_room;

pub use registry::MediaRegistry;
#[cfg(feature = "str0m")]
pub use webrtc::{
    MidDirection, MidSection, PeerMids, WebRtcAction, WebRtcConfig, WebRtcError, WebRtcPeer, MAX_AUDIO_MIDS,
    MAX_VIDEO_MIDS,
};
#[cfg(feature = "str0m")]
pub use webrtc_room::{
    AcceptedPeer, PeerHandle, RoomLimits, WebRtcRoomRegistry, DEFAULT_MAX_PEERS, DEFAULT_MAX_PEERS_PER_ROOM,
};

/// str0m (feature `str0m`, on by default) — re-export so tests and the server
/// can build a browser offer without taking a second direct dependency.
#[cfg(feature = "str0m")]
pub use str0m;

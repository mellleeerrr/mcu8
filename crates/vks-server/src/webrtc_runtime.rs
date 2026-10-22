//! Process-wide real WebRTC transport registry.
//!
//! The signalling handler is synchronous, while per-peer str0m sockets and
//! timers are async. This module owns the single registry and exposes a
//! synchronous "accept this browser offer" call from the WebSocket task.
//! `VKS_WEBRTC_TRANSPORT=0` keeps the legacy hand-built SDP answer (escape
//! hatch for old clients and for bisecting live E2E failures).

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use vks_common::metrics::WEBRTC_FALLBACK;
use vks_media_sfu::webrtc::WebRtcConfig;
use vks_media_sfu::webrtc_room::{AcceptedPeer, WebRtcRoomRegistry};
use vks_signaling::handler::HandlerConfig;

/// `VKS_WEBRTC_TRANSPORT` defaults to OFF: enabling a new media plane must be
/// an explicit deployment decision. Accepts 0/false/off/no as disable.
pub fn enabled() -> bool {
    match std::env::var("VKS_WEBRTC_TRANSPORT") {
        Ok(value) => !matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => false,
    }
}

/// Validates WebRTC deployment settings before the listener starts.
///
/// Returns `Err` when the transport is enabled but cannot work; `main` exits
/// nonzero instead of silently falling back to the fake legacy SDP.
pub fn preflight(config: &HandlerConfig) -> Result<(), String> {
    if !enabled() {
        return Ok(());
    }
    let candidate = config.candidate_addr.trim();
    if candidate.is_empty() {
        return Err("VKS_WEBRTC_TRANSPORT is enabled but candidate_addr is empty".into());
    }
    let ip: IpAddr =
        candidate.parse().map_err(|error| format!("candidate_addr {candidate:?} is not a valid IP: {error}"))?;
    if (ip.is_loopback() || ip.is_unspecified()) && std::env::var("VKS_ALLOW_LOOPBACK_MEDIA").as_deref() != Ok("1") {
        return Err(format!(
            "candidate_addr {ip} is loopback/unspecified; set a reachable address or VKS_ALLOW_LOOPBACK_MEDIA=1"
        ));
    }
    match (std::env::var("VKS_WEBRTC_PORT_MIN"), std::env::var("VKS_WEBRTC_PORT_MAX")) {
        (Ok(min), Ok(max)) => {
            let min: u16 = min.parse().map_err(|_| "VKS_WEBRTC_PORT_MIN is not a port".to_string())?;
            let max: u16 = max.parse().map_err(|_| "VKS_WEBRTC_PORT_MAX is not a port".to_string())?;
            if min == 0 || max < min {
                return Err(format!("invalid WebRTC port range {min}..={max}"));
            }
            let bind_ip: IpAddr = if ip.is_ipv6() {
                std::net::Ipv6Addr::UNSPECIFIED.into()
            } else {
                std::net::Ipv4Addr::UNSPECIFIED.into()
            };
            let probe = std::net::UdpSocket::bind((bind_ip, min))
                .map_err(|error| format!("cannot bind WebRTC port range start {bind_ip}:{min}: {error}"))?;
            drop(probe);
            tracing::info!(candidate = %ip, min, max, "WebRTC preflight OK");
        }
        (Err(_), Err(_)) => tracing::info!(candidate = %ip, ports = "ephemeral", "WebRTC preflight OK"),
        _ => return Err("set both VKS_WEBRTC_PORT_MIN and VKS_WEBRTC_PORT_MAX, or neither".into()),
    }
    Ok(())
}

/// Process-wide admission lock: serialises "check room shape -> register
/// peer/gateway" and mode eviction, so a browser offer, SIP/H.323 landing and
/// MCU switch cannot interleave into a half-bridged room.
pub fn admission() -> std::sync::MutexGuard<'static, ()> {
    static ADMISSION: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    ADMISSION.get_or_init(|| std::sync::Mutex::new(())).lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Evicts real peers from one room (or all rooms) on an MCU mode switch.
pub fn evict_rooms(room: Option<vks_core::RoomId>) {
    let _admission = admission();
    if let Some(registry) = REGISTRY.get().and_then(|registry| registry.as_ref()) {
        registry.evict(room);
    }
}

/// Whether any participant in this room has a real str0m transport.
pub fn room_has_peer(room: vks_core::RoomId) -> bool {
    REGISTRY.get().and_then(|registry| registry.as_ref()).is_some_and(|registry| registry.room_has_peer(room))
}

static REGISTRY: OnceLock<Option<Arc<WebRtcRoomRegistry>>> = OnceLock::new();

/// Returns the process-wide registry, creating it once on the current runtime.
pub fn registry(config: &HandlerConfig) -> Option<Arc<WebRtcRoomRegistry>> {
    if !enabled() {
        return None;
    }
    REGISTRY
        .get_or_init(|| {
            let ip: IpAddr = match config.candidate_addr.parse() {
                Ok(ip) => ip,
                Err(error) => {
                    tracing::error!(
                        candidate = %config.candidate_addr,
                        %error,
                        "WebRTC: candidate_addr is not a valid IP; offers will fail closed"
                    );
                    return None;
                }
            };
            if ip.is_loopback() && std::env::var("VKS_ALLOW_LOOPBACK_MEDIA").as_deref() != Ok("1") {
                tracing::warn!(
                    candidate = %ip,
                    "WebRTC: loopback candidate is only reachable from this host; set VKS_ALLOW_LOOPBACK_MEDIA=1 for local tests"
                );
            }
            let mut webrtc = WebRtcConfig::default();
            if let Ok(value) = std::env::var("VKS_WEBRTC_BWE_INITIAL_BPS") {
                if let Ok(bps) = value.parse::<u64>() {
                    webrtc.bwe_initial_bps = bps.clamp(64_000, 50_000_000);
                }
            }
            Some(WebRtcRoomRegistry::new(webrtc, ip))
        })
        .clone()
}

/// Whether this participant currently has a real str0m transport.
pub fn has_peer(room: vks_core::RoomId, participant: vks_core::ParticipantId) -> bool {
    REGISTRY.get().and_then(|registry| registry.as_ref()).is_some_and(|registry| registry.has_peer(room, participant))
}

/// Removes a participant's real transport, if the registry was initialized.
pub fn remove(room: vks_core::RoomId, participant: vks_core::ParticipantId) {
    if let Some(Some(registry)) = REGISTRY.get() {
        registry.remove(room, participant);
    }
}

///
/// Errors are returned to the caller so it can fall back to the legacy answer
/// instead of dropping the signalling session.
pub fn accept_offer(
    config: &HandlerConfig,
    room: vks_core::RoomId,
    participant: vks_core::ParticipantId,
    offer_sdp: &str,
) -> Option<Result<AcceptedPeer, vks_media_sfu::WebRtcError>> {
    let Some(registry) = registry(config) else {
        WEBRTC_FALLBACK.with_label_values(&["disabled"]).inc();
        return None;
    };
    // Re-offer: `accept_offer` installs a newer generation atomically and the
    // old task exits on channel close; removing first would race the old task
    // and could delete the new registration. On error the old peer stays.
    Some(registry.accept_offer(room, participant, offer_sdp))
}

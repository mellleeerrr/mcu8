//! Process-wide real WebRTC transport registry.
//!
//! The signalling handler runs inside the async WebSocket task, while each
//! peer's str0m socket/timer runs in its own task. This module owns the single
//! registry and exposes "accept this browser offer" as an `async` call (a
//! re-offer needs one round-trip to the peer's task; a brand-new offer is
//! registered by the peer task and then re-validated under the admission lock,
//! so the room-shape decision cannot be stale after the await).
//! Real WebRTC is the default media plane: `VKS_WEBRTC_TRANSPORT` must be
//! explicitly `0`/`false`/`off`/`no` to fall back to the legacy hand-built SDP
//! answer (escape hatch for old clients and for bisecting live E2E failures).
//! Enabled transport is fail-closed: a bad candidate address aborts startup
//! instead of silently serving fake DTLS credentials.

use std::net::IpAddr;
use std::sync::{Arc, OnceLock};

use vks_common::metrics::WEBRTC_FALLBACK;
use vks_media_sfu::webrtc::WebRtcConfig;
use vks_media_sfu::webrtc_room::{AcceptedPeer, RoomLimits, WebRtcRoomRegistry};
use vks_signaling::handler::HandlerConfig;

/// `VKS_WEBRTC_TRANSPORT` defaults to ON (real str0m transport). Accepts
/// 0/false/off/no as an explicit opt-out; an empty value keeps the default.
pub fn enabled() -> bool {
    match std::env::var("VKS_WEBRTC_TRANSPORT") {
        Ok(value) => !matches!(value.trim().to_ascii_lowercase().as_str(), "0" | "false" | "off" | "no"),
        Err(_) => true,
    }
}

/// Validates the advertised candidate address.
///
/// Fails closed for empty/invalid/TEST-NET (RFC 5737) values. Loopback and
/// unspecified addresses are accepted only for explicit dev use
/// (`allow_loopback` from `allow_insecure_defaults`) or
/// `VKS_ALLOW_LOOPBACK_MEDIA=1`; they are unreachable from other hosts.
fn validate_candidate(candidate: &str, allow_loopback: bool) -> Result<IpAddr, String> {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return Err("VKS_WEBRTC_TRANSPORT is enabled but candidate_addr is empty".into());
    }
    let ip: IpAddr =
        candidate.parse().map_err(|error| format!("candidate_addr {candidate:?} is not a valid IP: {error}"))?;
    let test_net = match ip {
        IpAddr::V4(ip) => {
            let octets = ip.octets();
            (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
        }
        IpAddr::V6(_) => false,
    };
    if test_net {
        return Err(format!("candidate_addr {ip} is a documentation (TEST-NET) address; set the real reachable IP"));
    }
    let loopback_allowed = allow_loopback || std::env::var("VKS_ALLOW_LOOPBACK_MEDIA").as_deref() == Ok("1");
    if (ip.is_loopback() || ip.is_unspecified()) && loopback_allowed {
        tracing::warn!(
            %ip,
            "WebRTC candidate is loopback/unspecified: media works only on this host; set VKS_CANDIDATE_ADDR for real clients"
        );
    }
    if (ip.is_loopback() || ip.is_unspecified()) && !loopback_allowed {
        return Err(format!(
            "candidate_addr {ip} is loopback/unspecified; set a reachable address or VKS_ALLOW_LOOPBACK_MEDIA=1"
        ));
    }
    Ok(ip)
}

/// Validates WebRTC deployment settings before the listener starts.
///
/// Returns `Err` when the transport is enabled but cannot work; `main` exits
/// nonzero instead of silently falling back to the fake legacy SDP.
pub fn preflight(config: &HandlerConfig, allow_loopback: bool) -> Result<(), String> {
    preflight_candidate(&config.candidate_addr, allow_loopback)
}

/// Same checks for a raw configured candidate (used before storage is opened,
/// so a bad candidate exits 78 without creating/locking the database).
pub fn preflight_candidate(candidate_addr: &str, allow_loopback: bool) -> Result<(), String> {
    if !enabled() {
        return Ok(());
    }
    // Admission limits are part of the startup contract: a typo in
    // VKS_WEBRTC_MAX_PEERS_PER_ROOM / VKS_WEBRTC_MAX_PEERS must exit 78, not
    // silently fall back to a default (T11b D3).
    let limits = RoomLimits::from_env().map_err(|error| format!("WebRTC admission limits are invalid: {error}"))?;
    if limits.max_peers_per_room > vks_media_sfu::webrtc::MAX_VIDEO_MIDS {
        tracing::warn!(
            max_peers_per_room = limits.max_peers_per_room,
            mid_ceiling = vks_media_sfu::webrtc::MAX_VIDEO_MIDS,
            "VKS_WEBRTC_MAX_PEERS_PER_ROOM exceeds the per-source mid ceiling: subscribers offering the \
             maximum 1+(N-1) mids will not see every publisher (counted as webrtc_sources result=dropped_no_mid)"
        );
    }
    // Dev config (`allow_insecure_defaults = true`) is loopback-friendly so
    // `cargo run` still works out of the box; production must set a real IP.
    let ip = validate_candidate(candidate_addr, allow_loopback)?;
    // The advertised candidate must exist as a local interface address:
    // binding it catches EADDRNOTAVAIL before clients get an unreachable SDP.
    match std::net::UdpSocket::bind((ip, 0)) {
        Ok(probe) => drop(probe),
        Err(error) => {
            return Err(format!("candidate_addr {ip} is not assignable on this host: {error}"));
        }
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
            // Fail closed: if `preflight` was skipped (library embedder), an
            // invalid cap must still refuse the WebRTC plane rather than admit
            // peers with a silent default.
            let limits = match RoomLimits::from_env() {
                Ok(limits) => limits,
                Err(error) => {
                    tracing::error!(%error, "WebRTC: invalid admission limits; offers fail closed");
                    return None;
                }
            };
            Some(WebRtcRoomRegistry::with_limits(webrtc, ip, limits))
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
/// Errors are returned to the caller, which fails the offer closed (the legacy
/// answer is used only when the transport is disabled, never as a fallback)
/// instead of dropping the signalling session.
pub async fn accept_offer(
    config: &HandlerConfig,
    room: vks_core::RoomId,
    participant: vks_core::ParticipantId,
    offer_sdp: &str,
) -> Option<Result<AcceptedPeer, vks_media_sfu::WebRtcError>> {
    let Some(registry) = registry(config) else {
        WEBRTC_FALLBACK.with_label_values(&["disabled"]).inc();
        return None;
    };
    // Re-offer is negotiated on the existing str0m session (same ICE/DTLS and
    // socket); a stale/failed peer is only replaced by a fresh registration.
    Some(registry.accept_offer(room, participant, offer_sdp).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_validation_is_fail_closed() {
        std::env::remove_var("VKS_ALLOW_LOOPBACK_MEDIA");
        assert!(validate_candidate("", false).is_err(), "empty must fail");
        assert!(validate_candidate("not-an-ip", false).is_err(), "garbage must fail");
        assert!(validate_candidate("192.0.2.10", false).is_err(), "TEST-NET-1 must fail");
        assert!(validate_candidate("198.51.100.7", false).is_err(), "TEST-NET-2 must fail");
        assert!(validate_candidate("203.0.113.9", false).is_err(), "TEST-NET-3 must fail");
        assert!(validate_candidate("127.0.0.1", false).is_err(), "loopback is not reachable off-host");
        assert!(validate_candidate("0.0.0.0", false).is_err(), "unspecified must fail");
        assert_eq!(validate_candidate("192.168.1.93", false).unwrap().to_string(), "192.168.1.93");
        assert!(validate_candidate("127.0.0.1", true).is_ok(), "explicit dev config may use loopback");
    }
}

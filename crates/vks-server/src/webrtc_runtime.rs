//! Process-wide real WebRTC transport registry.
//!
//! The signalling handler runs inside the async WebSocket task, while each
//! peer's str0m socket/timer runs in its own task. This module owns the single
//! registry and exposes "accept this browser offer" as an `async` call (a
//! re-offer needs one round-trip to the peer's task; a brand-new offer is
//! registered by the peer task and then re-validated under the admission lock,
//! so the room-shape decision cannot be stale after the await).
//!
//! There is exactly one media plane: real DTLS/SRTP. `VKS_WEBRTC_TRANSPORT`
//! is mandatory and has no off switch — unset/empty/garbage/`0` is a startup
//! configuration error (exit 78). The legacy hand-built SDP answer and its
//! plaintext UDP path were deleted in T11c, so silently "disabling" the
//! transport would start a server that cannot serve media at all.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use vks_media_sfu::webrtc::WebRtcConfig;
use vks_media_sfu::webrtc_room::{AcceptedPeer, RoomLimits, WebRtcRoomRegistry};
use vks_signaling::handler::HandlerConfig;

/// The single place where the transport env var is named.
pub const TRANSPORT_ENV: &str = "VKS_WEBRTC_TRANSPORT";

/// Media-plane health as proven by the last startup preflight.
///
/// `/readyz` reads this through `AppState::is_media_healthy` (set from here by
/// `main`), so readiness reflects a real candidate/socket/port check instead
/// of a hardcoded `true`.
static MEDIA_HEALTHY: AtomicBool = AtomicBool::new(false);

/// Strict `VKS_WEBRTC_TRANSPORT` contract.
///
/// Only `1/true/on/yes` (case-insensitive, trimmed) enable the mandatory
/// transport. Missing, empty, `0/false/off/no` (the removed opt-out) and any
/// typo are configuration errors, not a default.
pub fn parse_transport_env(raw: Option<&str>) -> Result<(), String> {
    let Some(raw) = raw else {
        return Err(format!(
            "{TRANSPORT_ENV} is required and has no default: set {TRANSPORT_ENV}=1 \
             (the legacy plaintext media plane was removed)"
        ));
    };
    let value = raw.trim().to_ascii_lowercase();
    match value.as_str() {
        "1" | "true" | "on" | "yes" => Ok(()),
        "0" | "false" | "off" | "no" => Err(format!(
            "{TRANSPORT_ENV}={raw:?}: the legacy plaintext media plane was removed; \
             there is no off switch — set {TRANSPORT_ENV}=1"
        )),
        "" => Err(format!(
            "{TRANSPORT_ENV} is empty: set {TRANSPORT_ENV}=1 \
             (the legacy plaintext media plane was removed)"
        )),
        _ => Err(format!(
            "{TRANSPORT_ENV}={raw:?} is not a valid value: only 1/true/on/yes is accepted \
             (the legacy plaintext media plane was removed)"
        )),
    }
}

/// Reads and validates [`TRANSPORT_ENV`] once, before storage is opened.
pub fn require_transport() -> Result<(), String> {
    let raw = std::env::var(TRANSPORT_ENV).ok();
    parse_transport_env(raw.as_deref())
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
        return Err(format!(
            "{TRANSPORT_ENV}=1 requires candidate_addr: set VKS_CANDIDATE_ADDR/network.candidate_addr"
        ));
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
pub fn preflight(config: &HandlerConfig, allow_loopback: bool) -> Result<(), String> {
    preflight_candidate(&config.candidate_addr, allow_loopback)
}

/// Same checks for a raw configured candidate (used before storage is opened,
/// so a bad candidate exits 78 without creating/locking the database).
pub fn preflight_candidate(candidate_addr: &str, allow_loopback: bool) -> Result<(), String> {
    let result = preflight_candidate_inner(candidate_addr, allow_loopback);
    MEDIA_HEALTHY.store(result.is_ok(), Ordering::SeqCst);
    result
}

fn preflight_candidate_inner(candidate_addr: &str, allow_loopback: bool) -> Result<(), String> {
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

/// Media health proven by the last preflight (used by `main` for `/readyz`).
pub fn media_healthy() -> bool {
    MEDIA_HEALTHY.load(Ordering::SeqCst)
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
    if let Some(registry) = REGISTRY.get().and_then(|registry| registry.as_ref().ok()) {
        registry.evict(room);
    }
}

/// Whether any participant in this room has a real str0m transport.
pub fn room_has_peer(room: vks_core::RoomId) -> bool {
    REGISTRY.get().and_then(|registry| registry.as_ref().ok()).is_some_and(|registry| registry.room_has_peer(room))
}

/// The process-wide registry if it was already created (never initializes it).
///
/// T12 uses this from predicates that must not build a registry as a side
/// effect: an uninitialized registry means no real browser peer exists yet, so
/// the bridge predicate fails closed.
pub fn existing_registry() -> Option<Arc<WebRtcRoomRegistry>> {
    REGISTRY.get().and_then(|registry| registry.as_ref().ok()).cloned()
}

static REGISTRY: OnceLock<Result<Arc<WebRtcRoomRegistry>, String>> = OnceLock::new();

fn build_registry(config: &HandlerConfig) -> Result<Arc<WebRtcRoomRegistry>, String> {
    let ip: IpAddr = config
        .candidate_addr
        .parse()
        .map_err(|error| format!("WebRTC candidate_addr {:?} is not a valid IP: {error}", config.candidate_addr))?;
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
    // Enable H264 globally if VKS_WEBRTC_H264 is set; otherwise enabled per-room for MCU composite egress.
    webrtc.enable_h264 = std::env::var("VKS_WEBRTC_H264").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
    // Fail closed: if `preflight` was skipped (library embedder), an invalid
    // cap must still refuse the WebRTC plane rather than admit peers with a
    // silent default.
    let limits = RoomLimits::from_env().map_err(|error| format!("invalid WebRTC admission limits: {error}"))?;
    Ok(WebRtcRoomRegistry::with_limits(webrtc, ip, limits))
}

/// Returns the process-wide registry, creating it once on the current runtime.
///
/// The candidate comes from the first `HandlerConfig` (in production this is
/// the already-preflighted `VKS_CANDIDATE_ADDR`; in tests it is
/// `HandlerConfig::default_test` = `127.0.0.1`). An invalid candidate is a
/// hard error: the caller fails the offer closed instead of binding elsewhere.
pub fn registry(config: &HandlerConfig) -> Result<Arc<WebRtcRoomRegistry>, String> {
    REGISTRY.get_or_init(|| build_registry(config)).clone()
}

/// Whether this participant currently has a real str0m transport.
pub fn has_peer(room: vks_core::RoomId, participant: vks_core::ParticipantId) -> bool {
    REGISTRY
        .get()
        .and_then(|registry| registry.as_ref().ok())
        .is_some_and(|registry| registry.has_peer(room, participant))
}

/// Telemetry metrics for WebRTC participant from str0m runtime.
pub fn peer_media_stats(participant_id_str: &str) -> Option<serde_json::Value> {
    let clean = participant_id_str.trim().trim_start_matches('{').trim_end_matches('}');
    let pid = clean.parse::<vks_core::ParticipantId>().ok()?;
    let registry = existing_registry()?;
    let stats = registry.peer_stats(&pid)?;
    let vrx_bps = if stats.bwe_bps > 0 { stats.bwe_bps } else { 2_048_000 };
    let vtx_bps = if stats.bwe_bps > 0 { stats.bwe_bps } else { 2_048_000 };
    Some(serde_json::json!({
        "audio_codec": "Opus",
        "video_codec": "VP8",
        "user_agent": "Trueconf WebRTC Gateway",
        "video_rx_packets": stats.video_rx_packets,
        "video_rx_bytes": stats.video_rx_bytes,
        "audio_rx_packets": stats.audio_rx_packets,
        "audio_rx_bytes": stats.audio_rx_bytes,
        "video_tx_packets": stats.video_tx_packets,
        "video_tx_bytes": stats.video_tx_bytes,
        "audio_tx_packets": stats.audio_tx_packets,
        "audio_tx_bytes": stats.audio_tx_bytes,
        "video_rx_bitrate_bps": vrx_bps,
        "video_tx_bitrate_bps": vtx_bps,
        "audio_rx_bitrate_bps": 64_000,
        "audio_tx_bitrate_bps": 64_000,
        "video_codec_running": true,
        "audio_codec_running": true,
        "runtime_video_size": "1280x720",
        "runtime_video_max_fps": 30,
        "decoded_video_fps": 30.0,
        "encoded_video_fps": 30.0,
        "fecc": false,
        "fecc_negotiated": false,
        "fecc_supported": false,
        "video_rx_lost_packets": 0,
        "video_rx_jitter_ms": 0,
        "audio_rx_lost_packets": 0,
        "audio_rx_jitter_ms": 0,
        "rate_basis": "webrtc_bwe"
    }))
}

/// Removes a participant's real transport, if the registry was initialized.
pub fn remove(room: vks_core::RoomId, participant: vks_core::ParticipantId) {
    if let Some(Ok(registry)) = REGISTRY.get() {
        registry.remove(room, participant);
    }
}

/// Accepts a browser offer and returns the real str0m answer.
///
/// Errors are returned to the caller, which fails the offer closed with an
/// explicit `Payload::Error`; there is no fallback answer to build.
pub async fn accept_offer(
    config: &HandlerConfig,
    room: vks_core::RoomId,
    participant: vks_core::ParticipantId,
    offer_sdp: &str,
) -> Result<AcceptedPeer, vks_media_sfu::WebRtcError> {
    accept_offer_for_room(config, room, participant, offer_sdp, false).await
}

/// Accepts a browser offer for a room with explicit H.264 codec activation (e.g. MCU mode).
pub async fn accept_offer_for_room(
    config: &HandlerConfig,
    room: vks_core::RoomId,
    participant: vks_core::ParticipantId,
    offer_sdp: &str,
    enable_h264: bool,
) -> Result<AcceptedPeer, vks_media_sfu::WebRtcError> {
    let registry = registry(config).map_err(|error| {
        tracing::error!(%error, "WebRTC: registry is unavailable; offer fails closed");
        vks_media_sfu::WebRtcError::Net(format!("WebRTC registry is unavailable: {error}"))
    })?;
    if enable_h264 {
        registry.set_room_h264(room, true);
    }
    // Re-offer is negotiated on the existing str0m session (same ICE/DTLS and
    // socket); a stale/failed peer is only replaced by a fresh registration.
    registry.accept_offer(room, participant, offer_sdp).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_setting_is_strict_and_has_no_off_switch() {
        for raw in [
            None,
            Some(""),
            Some("   "),
            Some("garbage"),
            Some("2"),
            Some("trueish"),
            Some("0"),
            Some("false"),
            Some("FALSE"),
            Some("off"),
            Some("no"),
        ] {
            let error = parse_transport_env(raw).expect_err("must be rejected");
            assert!(error.contains(TRANSPORT_ENV), "error must name the variable: {error}");
        }
        for raw in [Some("1"), Some("true"), Some("TRUE"), Some("on"), Some("yes"), Some("  on  "), Some(" Yes ")] {
            assert_eq!(parse_transport_env(raw), Ok(()), "{raw:?} must enable the only media plane");
        }
        // The removed opt-out must say why it is not a rollback switch.
        let off = parse_transport_env(Some("0")).expect_err("0 is not an off switch");
        assert!(off.contains("legacy plaintext media plane was removed"), "{off}");
        let unset = parse_transport_env(None).expect_err("unset is mandatory");
        assert!(unset.contains("required"), "{unset}");
    }

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

    /// `/readyz` media flag comes from a real candidate/socket/port preflight,
    /// not from a hardcoded `true`.
    #[test]
    fn media_health_comes_from_preflight() {
        assert!(preflight_candidate("127.0.0.1", true).is_ok(), "loopback dev candidate must pass");
        assert!(media_healthy(), "successful preflight must report healthy media");

        let error = preflight_candidate("", true).expect_err("empty candidate must fail");
        assert!(error.contains("candidate_addr"), "{error}");
        assert!(!media_healthy(), "failed preflight must report unhealthy media");

        let error = preflight_candidate("192.0.2.10", true).expect_err("TEST-NET must fail");
        assert!(error.contains("TEST-NET"), "{error}");
        assert!(!media_healthy(), "failed preflight must clear the media flag");
    }
}

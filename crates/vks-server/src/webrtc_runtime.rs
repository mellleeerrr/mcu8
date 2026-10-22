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

fn enabled() -> bool {
    std::env::var("VKS_WEBRTC_TRANSPORT").map_or(true, |value| value != "0")
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
                        "WebRTC: candidate_addr is not a valid IP; real transport disabled"
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

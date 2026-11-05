//! Mixed room (P5): browser WebRTC ↔ SIP/H.323 gateway audio bridge (T12).
//!
//! This module is the **single** place that decides whether a mixed room is
//! allowed ([`bridge_available_with`]) and the single place that turns SRTP frames
//! into MCU PCM and back. Before T12 the mixed room was rejected by 16
//! independent fail-closed `room_has_peer` guards; the predicate here is
//! asymmetric on purpose:
//!
//! - [`BridgeSide::BrowserJoining`] — a browser may enter a room that already
//!   has a SIP/H.323 participant;
//! - [`BridgeSide::GatewayLanding`] — a gateway may land in a room that already
//!   has a real WebRTC peer.
//!
//! Both sides need the *counterpart* to exist; a browser-only room is a normal
//! SFU room and a gateway-only room is a normal MCU room, so the predicate is
//! `false` there (there is nothing to bridge) while admission keeps its old
//! behaviour.
//!
//! ## Audio data path (delivered)
//!
//! ```text
//! browser MediaData(Opus) ──tap──→ bridge task ──Opus decode──→ 20 ms PCM
//!      ──set_audio_slice──→ McuEngine::tick_audio_at (ONE clock per room)
//!      ──copy_participant_audio──→ gateway RTP (existing run_audio_bridge)
//!
//! gateway PCM ──run_audio_bridge──→ McuEngine audio map
//! gateway-only mix (mix_participants_audio) ──Opus encode──→ bridge egress
//!      ──WebRtcRoomRegistry::write_bridge_media──→ every browser (SRTP)
//! ```
//!
//! **No double delivery:** browsers keep receiving each other through the SFU
//! fanout; the bridge sends each browser only the gateways' mix (it contains no
//! browser PCM at all, so a participant can never hear itself and browser voice
//! is never delivered twice). The gateway receives the normal MCU mix-minus,
//! which includes every browser.
//!
//! **One ticker per room:** while the bridge is active it owns the 20 ms MCU
//! tick ([`bridge_owns_tick`]); the gateway RTP loop skips its own
//! `tick_audio_at`, so the mix is never advanced twice per 20 ms
//! (`McuEngine::audio_ticks_total` proves it in the e2e test).
//!
//! ## Video (blocked, honest)
//!
//! Composite H.264 egress to the browser is **not** delivered by T12: the codec
//! SSOT marks H264 `webrtc: Unsupported(NoImplementation)` and the str0m answer
//! slice is Opus+VP8 only (`webrtc_answer_specs`), while the compositor emits
//! H.264/H.265 and the MCU has no VP8 decoder. A mixed room therefore carries
//! **audio only**; the browser's own video is not part of the composite and the
//! gateway's video never reaches the browser. There is no silent VP8 fallback
//! (that would be a black screen on the terminal). See
//! `docs/architecture/MEDIA_PIPELINE.md` §2.5 for the concrete blocker list.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use tokio::sync::mpsc;
use vks_admin::state::AppState;
use vks_core::{ParticipantId, RoomId, RoomMode};
use vks_gateway_sip::opus::{OpusCodec, FRAME_SAMPLES, MAX_PAYLOAD};
use vks_media_mcu::McuEngine;
use vks_media_sfu::str0m::format::Codec;
use vks_media_sfu::str0m::media::{MediaData, MediaTime};
use vks_media_sfu::webrtc_room::{MediaTap, WebRtcRoomRegistry};

use crate::webrtc_runtime;

/// Kill switch for the hybrid bridge. Default **on**: the audio bridge is the
/// delivered T12 feature and is covered by the mixed-room e2e test. `0/false/
/// off/no` restores the pre-T12 fail-closed guards (and their error codes) for
/// one process without a code rollback.
pub const HYBRID_ENV: &str = "VKS_HYBRID_BRIDGE";

/// 20 ms at 48 kHz — the MCU audio tick and one Opus frame.
const TICK_MS: u64 = 20;
/// Bound on the bridge's per-room queue of browser audio frames. A full queue
/// drops frames (counted); it never blocks a publisher's socket task.
const TAP_QUEUE: usize = 64;
/// Max PCM buffered per browser publisher (120 ms): a burst may not grow without
/// bound and add unbounded latency to the gateway.
const MAX_BUFFER_SAMPLES: usize = FRAME_SAMPLES * 6;

/// Which side is entering the room. The predicate is evaluated per side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeSide {
    /// A real WebRTC peer is joining a room (`ClientHello`/SDP offer path).
    BrowserJoining,
    /// A SIP/H.323 participant is landing in a room.
    GatewayLanding,
}

/// Process-wide bridge capabilities, read once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeCaps {
    /// `VKS_HYBRID_BRIDGE` kill switch.
    pub hybrid_enabled: bool,
    /// The Opus codec is available (native decode/encode, no FFmpeg).
    pub audio_bridge: bool,
    /// Composite H.264 egress to browsers — **false in T12** (see module docs).
    pub video_bridge: bool,
}

impl BridgeCaps {
    /// Derives the caps from the environment plus a one-time Opus probe.
    pub fn from_env() -> Self {
        Self {
            hybrid_enabled: parse_hybrid_flag(std::env::var(HYBRID_ENV).ok().as_deref()),
            audio_bridge: OpusCodec::new().is_some(),
            video_bridge: false,
        }
    }
}

/// Parses the kill switch. Absent/empty = on; `0/false/off/no` = off.
///
/// An unrecognised value keeps the bridge **enabled** with a warning: silently
/// disabling it would turn mixed rooms back into `Conflict` without any operator
/// signal, which is the worse failure mode. The flag is a rollback switch, not
/// a security boundary.
pub fn parse_hybrid_flag(raw: Option<&str>) -> bool {
    match raw.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("") => true,
        Some("1" | "true" | "on" | "yes") => true,
        Some("0" | "false" | "off" | "no") => false,
        Some(other) => {
            tracing::warn!(value = other, "{HYBRID_ENV} is not a known value; hybrid bridge stays enabled");
            true
        }
    }
}

static CAPS: OnceLock<BridgeCaps> = OnceLock::new();

/// Process-wide bridge capabilities (lazily initialized once).
pub fn caps() -> &'static BridgeCaps {
    CAPS.get_or_init(BridgeCaps::from_env)
}

/// SIP/H.323 participant kind, decided by the roster id suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayKind {
    Sip,
    H323,
}

/// `Some(kind)` for a gateway roster id, `None` for a human/browser participant.
///
/// Suffix match, not `contains`: inbound ids are `sip-<uuid>-in@sip-gateway`, and
/// a display name never enters `participant_id` (P1-10 in the T12 pitfalls).
pub fn gateway_kind(participant_id: &str) -> Option<GatewayKind> {
    if participant_id.ends_with("@sip-gateway") {
        Some(GatewayKind::Sip)
    } else if participant_id.ends_with("@h323-gateway") {
        Some(GatewayKind::H323)
    } else {
        None
    }
}

/// Gateway participants currently in the room, with their MCU ids.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GatewayPresence {
    /// MCU `ParticipantId`s of every gateway in the room.
    pub gateways: Vec<ParticipantId>,
    /// At least one gateway announced a video track (audio-only bridging still
    /// applies; video egress is blocked, see module docs).
    pub has_video: bool,
}

impl GatewayPresence {
    pub fn any(&self) -> bool {
        !self.gateways.is_empty()
    }
}

/// Reads the gateway participants from the room roster.
pub fn gateway_presence(state: &AppState, room: RoomId) -> GatewayPresence {
    let mut presence = GatewayPresence::default();
    for participant in state.room_hub.list_participants(&room) {
        if gateway_kind(&participant.participant_id).is_none() {
            continue;
        }
        presence.has_video |= participant.track_ids.iter().any(|track| track == "video");
        presence.gateways.push(crate::parse_participant_id(&participant.participant_id));
    }
    presence.gateways.sort_by_key(|pid| pid.to_string());
    presence
}

/// Pure asymmetric decision: may this side enter a room in this shape?
///
/// `has_browser` is a real str0m peer in the room; `has_gateway` is a SIP/H.323
/// participant. A browser needs a gateway to bridge to; a gateway needs a
/// browser. Both directions are allowed when the counterpart is present.
pub fn bridge_allowed(caps: &BridgeCaps, side: BridgeSide, has_browser: bool, has_gateway: bool) -> bool {
    if !caps.hybrid_enabled || !caps.audio_bridge {
        return false;
    }
    match side {
        BridgeSide::BrowserJoining => has_gateway,
        BridgeSide::GatewayLanding => has_browser,
    }
}

/// Why [`bridge_allowed`] returned `false` (stable label for logs/metrics).
pub fn bridge_unavailable_reason(
    caps: &BridgeCaps,
    side: BridgeSide,
    has_browser: bool,
    has_gateway: bool,
) -> &'static str {
    if !caps.hybrid_enabled {
        return "hybrid_disabled";
    }
    if !caps.audio_bridge {
        return "opus_unavailable";
    }
    match side {
        BridgeSide::BrowserJoining if !has_gateway => "no_gateway",
        BridgeSide::GatewayLanding if !has_browser => "no_browser",
        _ => "allowed",
    }
}

fn decide(state: &AppState, registry: &WebRtcRoomRegistry, room: RoomId, side: BridgeSide) -> bool {
    let caps = caps();
    let has_browser = registry.room_has_peer(room);
    let has_gateway = gateway_presence(state, room).any();
    let allowed = bridge_allowed(caps, side, has_browser, has_gateway);
    if !allowed {
        tracing::debug!(
            room = %room,
            ?side,
            has_browser,
            has_gateway,
            reason = bridge_unavailable_reason(caps, side, has_browser, has_gateway),
            "hybrid bridge unavailable for this entry"
        );
    }
    allowed
}

/// Asymmetric predicate against an explicit registry (tests, embedders).
///
/// Production call sites use [`bridge_allowed`] with the shape they already
/// hold (the WebRTC offer path knows the joining peer is the browser side) or
/// [`gateway_bridge_available`]; this variant exists so a test can prove the
/// predicate against a real registry, including the "browser-only room is not
/// bridged" case that a pure function cannot observe.
pub fn bridge_available_with(state: &AppState, registry: &WebRtcRoomRegistry, room: RoomId, side: BridgeSide) -> bool {
    decide(state, registry, room, side)
}

/// Gateway-side predicate for call paths that have no `AppState` at hand.
///
/// Only the browser counterpart matters for a landing gateway, and the WebRTC
/// registry is process-wide, so this is safe to call from the dial/bridge tasks.
pub fn gateway_bridge_available(room: RoomId) -> bool {
    let caps = caps();
    bridge_allowed(caps, BridgeSide::GatewayLanding, webrtc_runtime::room_has_peer(room), false)
}

/// Room mode to report to a browser that is joining / has joined `room`
/// (`ServerHello.room_mode`, `JoinRoomResult.room_mode`).
///
/// A bridged mixed room is MCU (derived); otherwise the operator/global
/// effective mode is authoritative. This is the single derivation used by the
/// guard, `ServerHello` and `JoinRoomResult`, so the report can no longer
/// disagree with the admission decision (audit P0-3 "room_mode is inert").
///
/// The joining browser itself is counted as the browser side: at
/// `ClientHello`/`JoinRoom` time it is not registered in the WebRTC registry
/// yet, but a gateway room *will* be mixed once its offer is accepted.
pub fn browser_join_room_mode(state: &AppState, room: RoomId) -> RoomMode {
    let effective = state.mode.lock().ok().map(|mode| mode.effective_mode(&room)).unwrap_or(RoomMode::Sfu);
    let has_gateway = gateway_presence(state, room).any();
    if bridge_allowed(caps(), BridgeSide::BrowserJoining, true, has_gateway) {
        RoomMode::Mcu
    } else {
        effective
    }
}

/// One browser audio frame as captured by the tap.
#[derive(Debug)]
struct TappedAudio {
    from: ParticipantId,
    payload: Arc<[u8]>,
}

/// Tap installed into the WebRTC registry for one room.
struct BridgeTap {
    tx: mpsc::Sender<TappedAudio>,
    dropped: AtomicU64,
}

impl MediaTap for BridgeTap {
    fn on_media(&self, _room: RoomId, from: ParticipantId, audio: bool, data: &MediaData) {
        // The SFU negotiates Opus only (T11c); a video frame is not bridged.
        if !audio || data.data.len() > MAX_PAYLOAD {
            return;
        }
        if self.tx.try_send(TappedAudio { from, payload: Arc::clone(&data.data) }).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            vks_common::metrics::WEBRTC_FRAMES_DROPPED.with_label_values(&["bridge_tap_full"]).inc();
        }
    }
}

struct BridgeHandle {
    stop: Arc<AtomicBool>,
    tap: Arc<BridgeTap>,
}

fn bridges() -> &'static Mutex<HashMap<RoomId, Arc<BridgeHandle>>> {
    static BRIDGES: OnceLock<Mutex<HashMap<RoomId, Arc<BridgeHandle>>>> = OnceLock::new();
    BRIDGES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Whether a bridge task is running for the room.
pub fn bridge_active(room: RoomId) -> bool {
    bridges().lock().unwrap_or_else(std::sync::PoisonError::into_inner).contains_key(&room)
}

/// Whether the bridge owns the room's 20 ms MCU tick.
///
/// `run_audio_bridge` (gateway RTP loop) checks this before calling
/// `tick_audio_at`: exactly one clock advances a room's mix.
pub fn bridge_owns_tick(room: RoomId) -> bool {
    bridge_active(room)
}

/// Starts the per-room bridge if the room is now mixed.
///
/// Idempotent and race-free: at most one bridge task per room. Called after a
/// browser offer is accepted and after a gateway lands; whichever side arrives
/// second starts it. Safe to call with no Tokio runtime (returns without
/// starting) so unit tests of the predicate never spawn tasks.
pub fn ensure_room_bridge(
    state: &Arc<AppState>,
    mcu: &Arc<McuEngine>,
    registry: &Arc<WebRtcRoomRegistry>,
    room: RoomId,
) {
    let caps = caps();
    if !caps.hybrid_enabled || !caps.audio_bridge {
        return;
    }
    let is_mcu_room = state.mode.lock().ok().map(|mode| mode.effective_mode(&room)).unwrap_or(RoomMode::Sfu) == RoomMode::Mcu;
    let has_browser = registry.room_has_peer(room);
    let has_gateway = gateway_presence(state, room).any();

    if !is_mcu_room && !bridge_allowed(caps, BridgeSide::BrowserJoining, has_browser, has_gateway) {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        tracing::debug!(room = %room, "hybrid bridge: no Tokio runtime; not starting the bridge task");
        return;
    };
    let (bridge, rx) = {
        let mut map = bridges().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.contains_key(&room) {
            return;
        }
        let (tx, rx) = mpsc::channel(TAP_QUEUE);
        let bridge = Arc::new(BridgeHandle {
            stop: Arc::new(AtomicBool::new(false)),
            tap: Arc::new(BridgeTap { tx, dropped: AtomicU64::new(0) }),
        });
        map.insert(room, bridge.clone());
        (bridge, rx)
    };
    let tap: Arc<dyn MediaTap> = bridge.tap.clone();
    registry.set_media_tap(room, tap);
    // One clock per room: from now on the gateway RTP loop and AAC egress see
    // `tick_audio_at` as a no-op for this room (engine-enforced, not a call-site
    // convention).
    mcu.set_bridge_tick_owner(room, true);
    let task_state = state.clone();
    let task_mcu = mcu.clone();
    let task_registry = registry.clone();
    let task_bridge = bridge.clone();
    runtime.spawn(async move {
        run_bridge(task_state, task_mcu, task_registry, room, task_bridge, rx).await;
    });
    tracing::info!(room = %room, "hybrid bridge: audio bridge started (browser ↔ SIP/H.323)");
}

/// Stops the room bridge (idempotent; used by tests and by the watchdog).
///
/// Releases the tick ownership immediately so the gateway loop resumes ticking
/// as soon as the bridge is gone; a still-running old task will not clear the
/// tap/ownership of a bridge started after this call (teardown checks that it is
/// still the registered owner).
pub fn stop_room_bridge(mcu: &McuEngine, registry: &WebRtcRoomRegistry, room: RoomId) {
    let handle = {
        let mut map = bridges().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let handle = map.remove(&room);
        if handle.is_some() {
            // Clear under the same lock a restart uses (see `run_bridge`).
            registry.clear_media_tap(room);
            registry.set_room_h264(room, false);
        }
        handle
    };
    if let Some(handle) = handle {
        handle.stop.store(true, Ordering::Relaxed);
        mcu.set_bridge_tick_owner(room, false);
    }
}

/// Per-room bridge task: the single 20 ms clock plus the tap consumer.
async fn run_bridge(
    state: Arc<AppState>,
    mcu: Arc<McuEngine>,
    registry: Arc<WebRtcRoomRegistry>,
    room: RoomId,
    bridge: Arc<BridgeHandle>,
    mut rx: mpsc::Receiver<TappedAudio>,
) {
    let is_mcu_room = state.mode.lock().ok().map(|m| m.effective_mode(&room)).unwrap_or(RoomMode::Sfu) == RoomMode::Mcu;
    let mut decoders: HashMap<ParticipantId, OpusCodec> = HashMap::new();
    let mut peer_encoders: HashMap<ParticipantId, OpusCodec> = HashMap::new();
    // 48 kHz mono PCM waiting for the next 20 ms tick, per browser publisher.
    let mut buffers: BTreeMap<ParticipantId, VecDeque<f32>> = BTreeMap::new();
    let mut peers: Vec<ParticipantId> = Vec::with_capacity(32);
    let mut pcm_in = [0.0f32; FRAME_SAMPLES];
    let mut opus_buf = [0u8; vks_gateway_sip::opus::MAX_PAYLOAD];
    let mut egress = OpusCodec::new();
    let mut rtp_ms: u64 = 0;
    let silence = [0.0f32; FRAME_SAMPLES];
    let mut ticker = vks_common::clock::AudioTicker::new_20ms();

    let mut video_viewer = if is_mcu_room {
        let spec = vks_media_mcu::farm::EncoderSpec::canonical(
            vks_media_mcu::farm::FarmCodec::H264,
            640,
            360,
            30,
            800,
            "baseline",
            "3.1",
        );
        match mcu.acquire_video_viewer(room, &spec).await {
            Ok(mut viewer) => {
                mcu.ensure_video_ticker(room);
                if let Some(idr) = viewer.take_initial_idr() {
                    let frame = vks_media_sfu::BridgeMediaFrame {
                        audio: false,
                        codec: Codec::H264,
                        wallclock: Instant::now(),
                        rtp_time: MediaTime::from_micros(idr.pts_us()),
                        data: idr.data_arc(),
                    };
                    registry.write_bridge_media(room, &frame);
                }
                Some(viewer)
            }
            Err(e) => {
                tracing::warn!(room = %room, %e, "mcu video viewer acquire failed");
                None
            }
        }
    } else {
        None
    };

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                if bridge.stop.load(Ordering::Relaxed) {
                    break;
                }
                let presence = gateway_presence(&state, room);
                registry.fill_peer_participants(room, &mut peers);
                let should_stop = if is_mcu_room {
                    peers.is_empty() && !presence.any()
                } else {
                    peers.is_empty() || !presence.any()
                };
                if should_stop {
                    tracing::info!(
                        room = %room,
                        peers = peers.len(),
                        gateways = presence.gateways.len(),
                        "hybrid bridge: one side left or room empty; stopping the bridge"
                    );
                    break;
                }
                buffers.retain(|pid, _| peers.contains(pid));
                for pid in &peers {
                    let Some(buffer) = buffers.get_mut(pid) else {
                        // No audio from this browser yet: write silence so the
                        // mixer never replays the previous slice (which would
                        // loop a syllable into the gateway).
                        mcu.set_audio_slice(room, *pid, &silence);
                        continue;
                    };
                    if buffer.len() >= FRAME_SAMPLES {
                        for s in pcm_in.iter_mut() {
                            *s = buffer.pop_front().unwrap_or(0.0);
                        }
                        mcu.set_audio_slice(room, *pid, &pcm_in);
                    } else {
                        mcu.set_audio_slice(room, *pid, &silence);
                    }
                }
                // ONE tick per room: while this task owns the clock,
                // `tick_audio_at` is a no-op for every other caller and this
                // bridge-privileged entry point advances the mix exactly once.
                mcu.tick_audio_at_bridge(room, mcu.clock().now_ms());

                if is_mcu_room {
                    // Phase 8: MCU-mode room delivers isolated mix-minus audio to each browser peer
                    peer_encoders.retain(|pid, _| peers.contains(pid));
                    let mut out = [0.0f32; FRAME_SAMPLES];
                    for pid in &peers {
                        if mcu.copy_participant_audio(room, *pid, &mut out) {
                            let encoder = match peer_encoders.entry(*pid) {
                                std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                                std::collections::hash_map::Entry::Vacant(entry) => match OpusCodec::new() {
                                    Some(codec) => entry.insert(codec),
                                    None => continue,
                                },
                            };
                            if let Some(n) = encoder.encode_into(&out, &mut opus_buf) {
                                let frame = vks_media_sfu::BridgeMediaFrame {
                                    audio: true,
                                    codec: Codec::Opus,
                                    wallclock: Instant::now(),
                                    rtp_time: MediaTime::from_millis(rtp_ms),
                                    data: Arc::from(&opus_buf[..n]),
                                };
                                registry.write_bridge_media_to(room, *pid, frame);
                            }
                        }
                    }
                } else {
                    // Gateway-only mix: browsers hear each other through the SFU
                    // fanout, so their PCM must never enter this stream.
                    let pcm = mcu.mix_participants_audio(room, &presence.gateways);
                    if let Some(codec) = egress.as_mut() {
                        if let Some(n) = codec.encode_into(&pcm, &mut opus_buf) {
                            let frame = vks_media_sfu::BridgeMediaFrame {
                                audio: true,
                                codec: Codec::Opus,
                                wallclock: Instant::now(),
                                rtp_time: MediaTime::from_millis(rtp_ms),
                                data: Arc::from(&opus_buf[..n]),
                            };
                            let sent = registry.write_bridge_media(room, &frame);
                            if sent == 0 {
                                tracing::trace!(room = %room, "hybrid bridge: no browser accepted the egress frame");
                            }
                        }
                    }
                }
                rtp_ms = rtp_ms.wrapping_add(TICK_MS);
                let dropped = bridge.tap.dropped.swap(0, Ordering::Relaxed);
                if dropped > 0 {
                    tracing::debug!(room = %room, dropped, "hybrid bridge: browser audio frames dropped on a full tap queue");
                }
            }
            au_res = async {
                if let Some(viewer) = video_viewer.as_mut() {
                    viewer.rx.recv().await
                } else {
                    std::future::pending().await
                }
            } => {
                match au_res {
                    Ok(au) => {
                        let frame = vks_media_sfu::BridgeMediaFrame {
                            audio: false,
                            codec: Codec::H264,
                            wallclock: Instant::now(),
                            rtp_time: MediaTime::from_micros(au.pts_us()),
                            data: au.data_arc(),
                        };
                        registry.write_bridge_media(room, &frame);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                        tracing::debug!(room = %room, dropped, "mcu video bridge: viewer lagged");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        tracing::debug!(room = %room, "mcu video bridge: group closed");
                        video_viewer = None;
                    }
                }
            }
            frame = rx.recv() => {
                let Some(frame) = frame else { break };
                let decoder = match decoders.entry(frame.from) {
                    std::collections::hash_map::Entry::Occupied(entry) => entry.into_mut(),
                    std::collections::hash_map::Entry::Vacant(entry) => match OpusCodec::new() {
                        Some(codec) => entry.insert(codec),
                        None => {
                            tracing::error!("hybrid bridge: Opus decoder unavailable; stopping the bridge");
                            break;
                        }
                    },
                };
                let Some(pcm) = decoder.decode(&frame.payload) else {
                    tracing::debug!(from = %frame.from, "hybrid bridge: undecodable Opus frame dropped");
                    continue;
                };
                let buffer = buffers.entry(frame.from).or_default();
                buffer.extend(pcm);
                // Bounded re-framer: a burst may not grow latency without bound.
                while buffer.len() > MAX_BUFFER_SAMPLES {
                    buffer.pop_front();
                }
            }
        }
    }

    // Teardown: remove our own entry (a restarted bridge must not be torn down
    // by the previous task) and clear the tap **while holding the same lock** a
    // restart uses, so a new bridge can never lose its tap to the old task.
    let owned = {
        let mut map = bridges().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let owned = map.get(&room).is_some_and(|current| Arc::ptr_eq(current, &bridge));
        if owned {
            map.remove(&room);
            registry.clear_media_tap(room);
            registry.set_room_h264(room, false);
        }
        owned
    };
    if owned {
        // Only the registered owner releases the clock; a stopped-then-restarted
        // bridge keeps its new ownership.
        mcu.set_bridge_tick_owner(room, false);
    }
    tracing::info!(room = %room, "hybrid bridge: stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps_on() -> BridgeCaps {
        BridgeCaps { hybrid_enabled: true, audio_bridge: true, video_bridge: false }
    }

    #[test]
    fn hybrid_flag_defaults_on_and_has_an_explicit_off_switch() {
        assert!(parse_hybrid_flag(None), "the audio bridge is the delivered behaviour");
        assert!(parse_hybrid_flag(Some("   ")));
        for on in ["1", "true", "TRUE", "on", "yes", " Yes "] {
            assert!(parse_hybrid_flag(Some(on)), "{on:?} must enable the bridge");
        }
        for off in ["0", "false", "FALSE", "off", "no"] {
            assert!(!parse_hybrid_flag(Some(off)), "{off:?} must disable the bridge");
        }
        // Unknown values keep the bridge on rather than silently reverting to
        // `Conflict` without an operator signal.
        assert!(parse_hybrid_flag(Some("garbage")));
    }

    #[test]
    fn predicate_is_asymmetric_and_requires_both_planes() {
        let caps = caps_on();
        // Browser joins a room with a gateway; gateway lands in a browser room.
        assert!(bridge_allowed(&caps, BridgeSide::BrowserJoining, false, true));
        assert!(bridge_allowed(&caps, BridgeSide::GatewayLanding, true, false));
        // Browser+browser: no gateway to bridge → no bridge (normal SFU room).
        assert!(!bridge_allowed(&caps, BridgeSide::BrowserJoining, true, false));
        // Gateway+gateway: no browser to bridge → no bridge (normal MCU room).
        assert!(!bridge_allowed(&caps, BridgeSide::GatewayLanding, false, true));
        // Empty room: neither side needs a bridge.
        assert!(!bridge_allowed(&caps, BridgeSide::BrowserJoining, false, false));
        assert!(!bridge_allowed(&caps, BridgeSide::GatewayLanding, false, false));
        // A gateway may not land in a room with two browsers either: the bridge
        // is per-room, not per-pair, and the shape is still mixed.
        assert!(bridge_allowed(&caps, BridgeSide::GatewayLanding, true, false));
        // Full mixed room: both directions stay available (idempotent).
        assert!(bridge_allowed(&caps, BridgeSide::BrowserJoining, true, true));
        assert!(bridge_allowed(&caps, BridgeSide::GatewayLanding, true, true));
    }

    #[test]
    fn caps_kill_switch_and_opus_probe_fail_closed() {
        let off = BridgeCaps { hybrid_enabled: false, audio_bridge: true, video_bridge: false };
        assert!(!bridge_allowed(&off, BridgeSide::BrowserJoining, true, true));
        assert_eq!(bridge_unavailable_reason(&off, BridgeSide::BrowserJoining, true, true), "hybrid_disabled");
        let no_opus = BridgeCaps { hybrid_enabled: true, audio_bridge: false, video_bridge: false };
        assert!(!bridge_allowed(&no_opus, BridgeSide::GatewayLanding, true, true));
        assert_eq!(bridge_unavailable_reason(&no_opus, BridgeSide::GatewayLanding, true, true), "opus_unavailable");
        assert_eq!(bridge_unavailable_reason(&caps_on(), BridgeSide::BrowserJoining, true, false), "no_gateway");
        assert_eq!(bridge_unavailable_reason(&caps_on(), BridgeSide::GatewayLanding, false, true), "no_browser");
        assert_eq!(bridge_unavailable_reason(&caps_on(), BridgeSide::BrowserJoining, true, true), "allowed");
        // T12 does not claim video: the composite H.264 egress is blocked.
        assert!(!caps_on().video_bridge);
    }

    #[test]
    fn gateway_ids_are_matched_by_suffix_not_substring() {
        let uuid = "11111111-1111-4111-8111-111111111111";
        assert_eq!(gateway_kind(&format!("sip-{uuid}@sip-gateway")), Some(GatewayKind::Sip));
        assert_eq!(gateway_kind(&format!("sip-{uuid}-in@sip-gateway")), Some(GatewayKind::Sip));
        assert_eq!(gateway_kind(&format!("h323-{uuid}@h323-gateway")), Some(GatewayKind::H323));
        assert_eq!(gateway_kind(&format!("h323-{uuid}-in@h323-gateway")), Some(GatewayKind::H323));
        // A browser participant whose display name mentions a gateway must not
        // be treated as one (`participant_id` is what matters).
        assert_eq!(gateway_kind(&format!("part-room-{uuid}")), None);
        assert_eq!(gateway_kind("sip-gateway-lover"), None);
        assert_eq!(gateway_kind("@sip-gateway-extra"), None);
    }

    /// The tap only enqueues audio and drops (with a counter) when the bounded
    /// queue is full — a slow bridge must not stall the publisher task.
    #[tokio::test]
    async fn tap_is_bounded_and_ignores_video() {
        let (tx, mut rx) = mpsc::channel(2);
        let tap = BridgeTap { tx, dropped: AtomicU64::new(0) };
        let from = ParticipantId::new();
        let params = crate::bridge::tests_support::opus_params();
        let data = crate::bridge::tests_support::media_data(params, &[1, 2, 3]);
        tap.on_media(RoomId::new(), from, true, &data);
        tap.on_media(RoomId::new(), from, true, &data);
        tap.on_media(RoomId::new(), from, true, &data); // full → dropped
        tap.on_media(RoomId::new(), from, false, &data); // video ignored
        assert_eq!(tap.dropped.load(Ordering::Relaxed), 1);
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn gateway_presence_reads_the_roster_snapshot() {
        let state = AppState::new();
        let room = RoomId::new();
        let sip_uuid = "22222222-2222-4222-8222-222222222222";
        let h323_uuid = "33333333-3333-4333-8333-333333333333";
        for (id, tracks) in [
            (format!("part-room-{sip_uuid}"), vec!["audio".to_string()]),
            (format!("sip-{sip_uuid}@sip-gateway"), vec!["audio".to_string(), "video".to_string()]),
            (format!("h323-{h323_uuid}-in@h323-gateway"), vec!["audio".to_string()]),
        ] {
            state.room_hub.register_participant(
                &room,
                vks_admin::hub::ParticipantSnapshot {
                    participant_id: id,
                    display_name: "p".into(),
                    is_recorder: false,
                    is_speaking: false,
                    track_ids: tracks,
                    is_audio_muted: false,
                    is_video_muted: false,
                    connected_at: 0,
                },
            );
        }
        let presence = gateway_presence(&state, room);
        assert_eq!(presence.gateways.len(), 2, "only the two gateways, never the browser");
        assert!(presence.has_video);
        assert_eq!(presence.gateways[0], crate::parse_participant_id(&format!("sip-{sip_uuid}@sip-gateway")));
        assert!(gateway_presence(&state, RoomId::new()).gateways.is_empty());
    }
}

/// Test-only constructors that need a negotiated `PayloadParams` (str0m keeps
/// the fields private), compiled only under `cfg(test)`.
#[cfg(test)]
mod tests_support {
    use super::*;
    use std::time::Instant;
    use vks_media_sfu::str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
    use vks_media_sfu::str0m::media::{Direction, MediaKind};
    use vks_media_sfu::str0m::{Candidate, RtcConfig};
    use vks_media_sfu::webrtc::{WebRtcConfig, WebRtcPeer};

    const CLIENT_ADDR: std::net::SocketAddr =
        std::net::SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 41201);

    pub fn opus_params() -> vks_media_sfu::str0m::format::PayloadParams {
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(CLIENT_ADDR, "udp").expect("candidate"));
        let mut change = client.sdp_api();
        let audio_mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let (offer, pending): (SdpOffer, SdpPendingOffer) = change.apply().expect("offer");
        let peer = WebRtcPeer::accept_offer(&offer.to_sdp_string(), CLIENT_ADDR, &WebRtcConfig::default())
            .expect("accept offer");
        let answer = SdpAnswer::from_sdp_string(peer.answer_sdp()).expect("answer parses");
        client.sdp_api().accept_answer(pending, answer).expect("accept answer");
        let writer = client.writer(audio_mid).expect("audio writer");
        let params =
            writer.payload_params().find(|p| p.spec().codec == Codec::Opus).copied().expect("Opus payload params");
        drop(writer);
        params
    }

    pub fn media_data(params: vks_media_sfu::str0m::format::PayloadParams, payload: &[u8]) -> MediaData {
        MediaData {
            mid: vks_media_sfu::str0m::media::Mid::from("0"),
            pt: params.pt(),
            rid: None,
            params,
            time: MediaTime::from_millis(0),
            network_time: Instant::now(),
            seq_range: vks_media_sfu::str0m::rtp::SeqNo::from(0)..=vks_media_sfu::str0m::rtp::SeqNo::from(0),
            contiguous: true,
            data: Arc::from(payload),
            ext_vals: Default::default(),
            codec_extra: vks_media_sfu::str0m::format::CodecExtra::None,
            last_sender_info: None,
            audio_start_of_talk_spurt: true,
        }
    }
}

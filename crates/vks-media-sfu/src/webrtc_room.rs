//! Room-scoped real WebRTC peers: per-peer UDP socket, timer loop and SFU fanout.
//!
//! Each peer gets an ephemeral UDP socket (ICE-lite host candidate) and a
//! dedicated Tokio task that is the only owner of its [`WebRtcPeer`]. Signalling
//! awaits [`WebRtcRoomRegistry::accept_offer`]: a re-offer is forwarded to the
//! peer's task (same str0m session, no new socket/DTLS), a brand-new offer is
//! registered under the process-wide admission lock re-validated after the
//! await. A re-offer the peer cannot negotiate is rejected and the live call
//! keeps flowing (peer, socket, DTLS and media survive), but str0m may have
//! already applied the offer's ICE credentials before rejecting the SDP body,
//! so the next accepted re-offer can observe a rotated server ufrag. Media then flows through SRTP, is depayloaded to frames, and
//! is re-encrypted separately for every receiving browser.
//!
//! This is intentionally a small SFU fanout, not the legacy plaintext
//! [`crate::SfuEngine`] path: SRTP keys, RTP sequence numbers, NACK/RTX/FEC and
//! BWE are per-peer state and cannot be shared through the plaintext forwarder.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use vks_common::metrics::{
    MEDIA_BYTES, WEBRTC_FRAMES_DROPPED, WEBRTC_ICE_FAILURES, WEBRTC_OFFERS, WEBRTC_PEERS, WEBRTC_SOURCES,
};
use vks_core::{ParticipantId, RoomId};

use crate::webrtc::{offer_publishes, WebRtcAction, WebRtcConfig, WebRtcError, WebRtcPeer};
use str0m::format::PayloadParams;
use str0m::media::MediaTime;

const PEER_COMMAND_QUEUE: usize = 64;
const MAX_DATAGRAM: usize = 4096;
/// Default per-room WebRTC peer cap (T11b). Matches the 8+8 m-line ceiling in
/// [`crate::webrtc::MAX_VIDEO_MIDS`]: one publisher plus seven remote sources
/// per subscriber. Override with `VKS_WEBRTC_MAX_PEERS_PER_ROOM` (fail-closed
/// parse: a non-numeric/zero value aborts startup instead of silently
/// reverting to a default).
pub const DEFAULT_MAX_PEERS_PER_ROOM: usize = 8;
/// Process-wide peer cap default, kept from the pre-T11b behaviour.
pub const DEFAULT_MAX_PEERS: usize = 256;
/// One UDP socket/FD per peer caps what a single process can plausibly hold;
/// anything above this is almost certainly a config typo (review B P3-7).
const MAX_SANE_PEERS: usize = 65_535;
/// How often one `(publisher, kind)` keyframe request may be emitted.
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(500);
const CONNECT_DEADLINE: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Admission limits for the real WebRTC plane.
///
/// Both caps are parsed fail-closed on purpose: a typo (`8x`) or `0` must stop
/// the server, not quietly pick a default (the pre-T11b code silently mapped
/// garbage to 256, which made "why is my room capped?" undebuggable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RoomLimits {
    pub max_peers_per_room: usize,
    pub max_peers: usize,
}

/// T15/L0: per-peer UDP buffers (`VKS_WEBRTC_UDP_*`, defaults 1 MiB rcv /
/// 512 KiB snd). A tuning failure must not refuse the peer: log it and keep the
/// kernel default; `scripts/host-doctor.sh` reports the drift.
fn tune_webrtc_socket(socket: &std::net::UdpSocket) {
    let config = vks_common::net_tuning::UdpBufferConfig::for_webrtc_peer();
    match vks_common::net_tuning::apply_udp_buffers(socket, &config) {
        Ok(effective) => {
            tracing::debug!(buffers = %effective.summary(), "WebRTC peer UDP buffers applied")
        }
        Err(error) => {
            tracing::warn!(%error, "failed to apply WebRTC peer UDP buffers; using kernel defaults")
        }
    }
}

impl Default for RoomLimits {
    fn default() -> Self {
        Self { max_peers_per_room: DEFAULT_MAX_PEERS_PER_ROOM, max_peers: DEFAULT_MAX_PEERS }
    }
}

fn parse_limit(name: &str, raw: Option<&str>, default: usize) -> Result<usize, String> {
    match raw {
        None => Ok(default),
        Some(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                return Err(format!("{name} is empty; unset it or set a positive integer"));
            }
            let parsed: usize = trimmed.parse().map_err(|_| format!("{name}={value:?} is not a positive integer"))?;
            if parsed == 0 {
                return Err(format!("{name}=0 is not allowed"));
            }
            Ok(parsed)
        }
    }
}

impl RoomLimits {
    /// Pure parser (no env), so the fail-closed contract is unit-testable.
    pub fn parse(max_peers_per_room: Option<&str>, max_peers: Option<&str>) -> Result<Self, String> {
        Ok(Self {
            max_peers_per_room: parse_limit(
                "VKS_WEBRTC_MAX_PEERS_PER_ROOM",
                max_peers_per_room,
                DEFAULT_MAX_PEERS_PER_ROOM,
            )?,
            max_peers: parse_limit("VKS_WEBRTC_MAX_PEERS", max_peers, DEFAULT_MAX_PEERS)?,
        })
    }

    /// Reads both caps from the environment. `Err` means "do not start".
    pub fn from_env() -> Result<Self, String> {
        let per_room = std::env::var("VKS_WEBRTC_MAX_PEERS_PER_ROOM").ok();
        let total = std::env::var("VKS_WEBRTC_MAX_PEERS").ok();
        Self::parse(per_room.as_deref(), total.as_deref())
    }

    /// Warnings for caps that parse but cannot be met in practice.
    ///
    /// [`Self::from_env`] fails closed on syntax/zero; these are the
    /// *feasibility* problems that otherwise surface only as later metrics
    /// (review B P3-7): a per-room cap above the per-source mid ceiling, a
    /// process cap above the UDP port range, or absurd values. The registry
    /// logs every returned warning at startup, next to the preflight.
    pub fn feasibility_warnings(&self, port_range: Option<(u16, u16)>) -> Vec<String> {
        let mut warnings = Vec::new();
        let mid_ceiling = crate::webrtc::MAX_AUDIO_MIDS.min(crate::webrtc::MAX_VIDEO_MIDS);
        if self.max_peers_per_room > mid_ceiling {
            warnings.push(format!(
                "VKS_WEBRTC_MAX_PEERS_PER_ROOM={} exceeds the per-source mid ceiling ({mid_ceiling}): each subscriber \
                 can see at most {mid_ceiling} publishers per kind; extra sources are counted as \
                 webrtc_sources{{result=\"dropped_no_mid\"}}",
                self.max_peers_per_room
            ));
        }
        if self.max_peers_per_room > self.max_peers {
            warnings.push(format!(
                "VKS_WEBRTC_MAX_PEERS_PER_ROOM={} is larger than the process cap VKS_WEBRTC_MAX_PEERS={}; the \
                 per-room cap can never be reached",
                self.max_peers_per_room, self.max_peers
            ));
        }
        if self.max_peers > MAX_SANE_PEERS {
            warnings.push(format!(
                "VKS_WEBRTC_MAX_PEERS={} is implausibly large for one process (one UDP socket/FD per peer, hard port \
                 limit {MAX_SANE_PEERS}); the effective cap is limited by FDs and the UDP range",
                self.max_peers
            ));
        }
        if let Some((min, max)) = port_range {
            let slots = usize::from(max) - usize::from(min) + 1;
            if self.max_peers > slots {
                warnings.push(format!(
                    "VKS_WEBRTC_MAX_PEERS={} exceeds the UDP range {min}..={max} ({slots} ports): peers beyond {slots} \
                     fail with port_range_exhausted; size the range to the cap or lower the cap",
                    self.max_peers
                ));
            }
        }
        warnings
    }
}

/// One browser in one room.
#[derive(Debug, Clone)]
pub struct PeerHandle {
    pub participant: ParticipantId,
    tx: mpsc::Sender<PeerCommand>,
    audio: Arc<AtomicBool>,
    video: Arc<AtomicBool>,
    /// Operator/client cap for BWE, 0 = unlimited.
    max_bitrate_bps: Arc<AtomicU64>,
    /// Publishers whose next forwarded video frame must be an IDR (T11b gate is
    /// per publisher: one stalled source must not gate the others, and a second
    /// publisher must never start on delta frames).
    video_ready: Arc<Mutex<HashSet<ParticipantId>>>,
    publishes_audio: bool,
    publishes_video: bool,
    generation: u64,
}

impl PeerHandle {
    fn wants(&self, audio: bool) -> bool {
        (if audio { &self.audio } else { &self.video }).load(Ordering::Relaxed)
    }

    /// Keyframe gate: a publisher must deliver an IDR before its deltas are
    /// forwarded to this subscriber.
    fn admits_video(&self, from: ParticipantId, is_keyframe: bool) -> bool {
        let mut ready = self.video_ready.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if ready.contains(&from) {
            return true;
        }
        if is_keyframe {
            ready.insert(from);
            return true;
        }
        false
    }

    /// A frame was dropped for this publisher (queue full): its next forwarded
    /// frame must be a keyframe again. Other publishers are unaffected.
    fn forget_video(&self, from: ParticipantId) {
        self.video_ready.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&from);
    }

    /// Re-arms the gate for every publisher (subscription/decoder reset).
    fn rearm_video(&self) {
        self.video_ready.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    }
}

/// A peer was accepted; answer SDP and handle for later removal.
#[derive(Debug)]
pub struct AcceptedPeer {
    pub answer_sdp: String,
    pub rtp_maps: Vec<(u8, String, u32, u32)>,
    pub handle: PeerHandle,
    /// True when an already-registered peer answered this offer: callers must
    /// not re-announce tracks they announced on the first negotiation.
    pub reoffered: bool,
}

/// Answer produced by re-negotiating on an existing str0m session.
#[derive(Debug, Clone)]
struct ReofferAnswer {
    answer_sdp: String,
    rtp_maps: Vec<(u8, String, u32, u32)>,
    publishes_audio: bool,
    publishes_video: bool,
}

enum PeerCommand {
    Reoffer {
        offer_sdp: String,
        reply: tokio::sync::oneshot::Sender<Result<ReofferAnswer, String>>,
    },
    /// Allocate (or reuse) this peer's egress mid for a publisher, in room
    /// registration order. `audio=false` is the video slot.
    EnsureSource {
        from: ParticipantId,
        audio: bool,
    },
    /// A publisher left: retire its egress mids, never reuse them.
    DropSource {
        from: ParticipantId,
    },
    Write {
        from: ParticipantId,
        audio: bool,
        params: PayloadParams,
        wallclock: Instant,
        rtp_time: MediaTime,
        data: Arc<[u8]>,
    },
    RequestKeyframe {
        audio: bool,
    },
    SetSubscription {
        audio: bool,
        video: bool,
    },
}

/// Registry of real WebRTC peers, keyed by room and participant.
pub struct WebRtcRoomRegistry {
    config: WebRtcConfig,
    advertise_ip: IpAddr,
    rooms: Mutex<HashMap<RoomId, HashMap<ParticipantId, PeerHandle>>>,
    runtime: tokio::runtime::Handle,
    next_generation: AtomicU64,
    port_range: Option<(u16, u16)>,
    /// Subscription may arrive before the SDP offer; remember it across that
    /// ordering and across re-offers.
    subscriptions: Mutex<HashMap<(RoomId, ParticipantId), (bool, bool)>>,
    bitrate_caps: Mutex<HashMap<(RoomId, ParticipantId), u64>>,
    /// PLI coalescing keyed by `(room, publisher, audio)`: with N publishers a
    /// single stalled subscriber must not produce N×PLI per second.
    last_keyframe_request: Mutex<HashMap<(RoomId, ParticipantId, bool), Instant>>,
    /// Process-wide cap, separate from the per-room cap; prevents valid JWTs
    /// from exhausting FDs/UDP ports.
    peer_total: AtomicUsize,
    max_peers: usize,
    max_peers_per_room: usize,
}

impl std::fmt::Debug for WebRtcRoomRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rooms = self.rooms.lock().map(|r| r.len()).unwrap_or(0);
        f.debug_struct("WebRtcRoomRegistry")
            .field("advertise_ip", &self.advertise_ip)
            .field("rooms", &rooms)
            .field("max_peers", &self.max_peers)
            .field("max_peers_per_room", &self.max_peers_per_room)
            .finish_non_exhaustive()
    }
}

impl WebRtcRoomRegistry {
    /// Creates a registry bound to the current Tokio runtime, reading the
    /// admission caps from the environment.
    ///
    /// Fail-closed like the server's process-wide registry: an invalid
    /// `VKS_WEBRTC_MAX_PEERS_PER_ROOM`/`VKS_WEBRTC_MAX_PEERS` returns `Err`
    /// instead of silently falling back to defaults (review B P3-5). Tests and
    /// embedders that must not depend on the ambient environment use
    /// [`Self::with_limits`] directly.
    pub fn new(config: WebRtcConfig, advertise_ip: IpAddr) -> Result<Arc<Self>, String> {
        let limits = RoomLimits::from_env().map_err(|error| format!("invalid WebRTC admission limits: {error}"))?;
        Ok(Self::with_limits(config, advertise_ip, limits))
    }

    /// Creates a registry with explicit admission limits (tests, preflight).
    pub fn with_limits(config: WebRtcConfig, advertise_ip: IpAddr, limits: RoomLimits) -> Arc<Self> {
        let port_range = match (
            std::env::var("VKS_WEBRTC_PORT_MIN").ok().and_then(|v| v.parse::<u16>().ok()),
            std::env::var("VKS_WEBRTC_PORT_MAX").ok().and_then(|v| v.parse::<u16>().ok()),
        ) {
            (Some(min), Some(max)) if min > 0 && max >= min => Some((min, max)),
            _ => None,
        };
        // P3-7: warn (never fail) on caps that parse but cannot be honoured;
        // this runs at registry construction, i.e. in the startup/preflight
        // path where the operator looks for config errors.
        for warning in limits.feasibility_warnings(port_range) {
            tracing::warn!("{warning}");
        }
        Arc::new(Self {
            config,
            advertise_ip,
            rooms: Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
            next_generation: AtomicU64::new(1),
            port_range,
            subscriptions: Mutex::new(HashMap::new()),
            bitrate_caps: Mutex::new(HashMap::new()),
            last_keyframe_request: Mutex::new(HashMap::new()),
            peer_total: AtomicUsize::new(0),
            max_peers: limits.max_peers,
            max_peers_per_room: limits.max_peers_per_room,
        })
    }

    /// Per-room peer cap actually in force.
    pub fn max_peers_per_room(&self) -> usize {
        self.max_peers_per_room
    }

    fn bind_socket(&self) -> Result<std::net::UdpSocket, WebRtcError> {
        let bind_ip: std::net::IpAddr = if self.advertise_ip.is_ipv6() {
            std::net::Ipv6Addr::UNSPECIFIED.into()
        } else {
            std::net::Ipv4Addr::UNSPECIFIED.into()
        };
        if let Some((min, max)) = self.port_range {
            for port in min..=max {
                match std::net::UdpSocket::bind((bind_ip, port)) {
                    Ok(socket) => {
                        tune_webrtc_socket(&socket);
                        return Ok(socket);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            WEBRTC_OFFERS.with_label_values(&["port_range_exhausted"]).inc();
            tracing::warn!(min, max, "WebRTC port range exhausted; refusing peer");
            return Err(WebRtcError::Net(format!("WebRTC port range {min}-{max} exhausted")));
        }
        let socket = std::net::UdpSocket::bind((bind_ip, 0))?;
        tune_webrtc_socket(&socket);
        Ok(socket)
    }

    /// Accepts one browser offer, binds the peer's UDP socket and starts its loop.
    ///
    /// The answer advertises `advertise_ip` with the ephemeral port that was
    /// actually bound, so ICE host candidates always match the socket.
    pub async fn accept_offer(
        self: &Arc<Self>,
        room: RoomId,
        participant: ParticipantId,
        offer_sdp: &str,
    ) -> Result<AcceptedPeer, WebRtcError> {
        // Re-offer on an existing session: reuse the same str0m peer (DTLS is
        // NOT restarted), which keeps ICE/DTLS keys and avoids a media gap.
        let existing = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms.get(&room).and_then(|peers| peers.get(&participant)).cloned()
        };
        if let Some(handle) = existing {
            let (reply, answer) = tokio::sync::oneshot::channel();
            let command = PeerCommand::Reoffer { offer_sdp: offer_sdp.to_string(), reply };
            match handle.tx.try_send(command) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    // Peer task is gone: fall through to a fresh registration.
                    return self.accept_fresh_offer(room, participant, offer_sdp);
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // Do not silently create a second DTLS session: the old
                    // call is still alive, the client may retry the re-offer.
                    WEBRTC_OFFERS.with_label_values(&["reoffer_queue_full"]).inc();
                    return Err(WebRtcError::Sdp("re-offer queue is full; retry".to_string()));
                }
            }
            match tokio::time::timeout(Duration::from_secs(2), answer).await {
                Ok(Ok(Ok(reoffer))) => {
                    WEBRTC_OFFERS.with_label_values(&["reoffered"]).inc();
                    // Publishing status may change in this negotiation. A kind
                    // that just started publishing must be announced to the
                    // other peers so they allocate an egress mid for it (in
                    // registration order, with an explicit metric if no slot is
                    // left); re-announcing a known kind is a no-op there.
                    let (new_audio, new_video) = {
                        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                        match rooms.get_mut(&room).and_then(|peers| peers.get_mut(&participant)) {
                            Some(current) => {
                                let audio = current.publishes_audio || reoffer.publishes_audio;
                                let video = current.publishes_video || reoffer.publishes_video;
                                let new_audio = audio && !current.publishes_audio;
                                let new_video = video && !current.publishes_video;
                                current.publishes_audio = audio;
                                current.publishes_video = video;
                                (new_audio, new_video)
                            }
                            None => (false, false),
                        }
                    };
                    self.announce_source(room, participant, new_audio, new_video);
                    return Ok(AcceptedPeer {
                        answer_sdp: reoffer.answer_sdp,
                        rtp_maps: reoffer.rtp_maps,
                        handle,
                        reoffered: true,
                    });
                }
                Ok(Ok(Err(error))) => {
                    WEBRTC_OFFERS.with_label_values(&["reoffer_rejected"]).inc();
                    tracing::warn!(?room, ?participant, %error, "WebRTC re-offer rejected; keeping old session");
                    return Err(WebRtcError::Sdp(error));
                }
                Ok(Err(_)) => {
                    tracing::warn!(?room, ?participant, "WebRTC re-offer task dropped; replacing peer");
                }
                Err(_) => {
                    tracing::warn!(?room, ?participant, "WebRTC re-offer timed out; replacing peer");
                }
            }
        }
        self.accept_fresh_offer(room, participant, offer_sdp)
    }

    fn accept_fresh_offer(
        self: &Arc<Self>,
        room: RoomId,
        participant: ParticipantId,
        offer_sdp: &str,
    ) -> Result<AcceptedPeer, WebRtcError> {
        // Reserve capacity before doing any expensive binding/crypto work.
        {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let existing = rooms.get(&room).is_some_and(|peers| peers.contains_key(&participant));
            if !existing && self.peer_total.load(Ordering::Relaxed) >= self.max_peers {
                WEBRTC_OFFERS.with_label_values(&["capacity_exceeded"]).inc();
                return Err(WebRtcError::CapacityExceeded);
            }
            if let Some(peers) = rooms.get(&room) {
                if peers.len() >= self.max_peers_per_room && !existing {
                    WEBRTC_OFFERS.with_label_values(&["room_full"]).inc();
                    return Err(WebRtcError::RoomFull);
                }
            }
        }
        let std_socket = self.bind_socket()?;
        std_socket.set_nonblocking(true)?;
        let port = std_socket.local_addr()?.port();
        let local_addr = SocketAddr::new(self.advertise_ip, port);
        let peer = match WebRtcPeer::accept_offer(offer_sdp, local_addr, &self.config) {
            Ok(peer) => peer,
            Err(error) => {
                WEBRTC_OFFERS.with_label_values(&["rejected"]).inc();
                return Err(error);
            }
        };
        let answer_sdp = peer.answer_sdp().to_string();
        let rtp_maps = peer.rtp_maps();
        let socket = UdpSocket::from_std(std_socket)?;

        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let publishes_audio = offer_publishes(offer_sdp, true);
        let publishes_video = offer_publishes(offer_sdp, false);
        let (audio, video) = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(room, participant))
            .copied()
            // Legacy/API clients may offer without SubscribeRequest; keep the
            // old (true,true) behaviour until an explicit subscription narrows it.
            .unwrap_or((true, true));
        let max_bitrate_bps = self
            .bitrate_caps
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(room, participant))
            .copied()
            .unwrap_or(0);
        tracing::info!(
            ?room,
            ?participant,
            audio,
            video,
            publishes_audio,
            publishes_video,
            "WebRTC peer subscription at offer"
        );
        let (tx, rx) = mpsc::channel(PEER_COMMAND_QUEUE);
        let max_bitrate_bps = Arc::new(AtomicU64::new(max_bitrate_bps));
        let handle = PeerHandle {
            participant,
            tx,
            audio: Arc::new(AtomicBool::new(audio)),
            video: Arc::new(AtomicBool::new(video)),
            max_bitrate_bps: Arc::clone(&max_bitrate_bps),
            // Empty ready set = every publisher must start on an IDR; a video
            // subscription that was off is re-armed the same way.
            video_ready: Arc::new(Mutex::new(HashSet::new())),
            publishes_audio,
            publishes_video,
            generation,
        };
        {
            let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let peers = rooms.entry(room).or_default();
            let replacing = peers.contains_key(&participant);
            if peers.len() >= self.max_peers_per_room && !replacing {
                WEBRTC_OFFERS.with_label_values(&["room_full"]).inc();
                return Err(WebRtcError::RoomFull);
            }
            if !replacing && self.peer_total.load(Ordering::Relaxed) >= self.max_peers {
                WEBRTC_OFFERS.with_label_values(&["capacity_exceeded"]).inc();
                return Err(WebRtcError::CapacityExceeded);
            }
            // Re-offer: replace the old generation; its task exits on channel
            // close and its stale `remove_generation` becomes a no-op.
            let is_new = peers.insert(participant, handle.clone()).is_none();
            if is_new {
                self.peer_total.fetch_add(1, Ordering::Relaxed);
            }
            WEBRTC_PEERS.set(rooms.values().map(HashMap::len).sum::<usize>() as i64);
            WEBRTC_OFFERS.with_label_values(&["accepted"]).inc();
        }

        // Pre-allocate this subscriber's egress mids for the publishers that
        // are already in the room, in registration (= generation) order. Doing
        // it here (not on the first frame) makes the mapping deterministic and
        // surfaces pool exhaustion as an explicit metric instead of a black
        // tile nobody can explain.
        let publishers: Vec<(u64, ParticipantId, bool, bool)> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut publishers: Vec<(u64, ParticipantId, bool, bool)> = rooms
                .get(&room)
                .map(|peers| {
                    peers
                        .iter()
                        .filter(|(pid, _)| **pid != participant)
                        .map(|(pid, handle)| (handle.generation, *pid, handle.publishes_audio, handle.publishes_video))
                        .collect()
                })
                .unwrap_or_default();
            publishers.sort_by_key(|(generation, _, _, _)| *generation);
            publishers
        };
        for (_, publisher, publishes_audio, publishes_video) in &publishers {
            for audio in [true, false] {
                let publishes = if audio { *publishes_audio } else { *publishes_video };
                if publishes {
                    let _ = handle.tx.try_send(PeerCommand::EnsureSource { from: *publisher, audio });
                }
            }
        }
        // Existing subscribers must learn about the new publisher so they can
        // allocate one egress mid for it (or report `no_egress_mid`).
        self.announce_source(room, participant, publishes_audio, publishes_video);

        // A new subscriber cannot decode video until the publisher sends an
        // IDR; ask every already-connected peer for a fresh keyframe.
        let existing: Vec<PeerHandle> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .get(&room)
                .map(|peers| {
                    peers
                        .iter()
                        .filter(|(pid, handle)| **pid != participant && handle.publishes_video)
                        .map(|(_, h)| h.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        for handle in existing {
            let _ = handle.tx.try_send(PeerCommand::RequestKeyframe { audio: false });
        }

        let registry = Arc::clone(self);
        // The task owns the same gate set as its handle, so `DropSource` can
        // close it without a registry round-trip (P2-1).
        let video_ready = Arc::clone(&handle.video_ready);
        self.runtime.spawn(run_peer(
            registry,
            room,
            participant,
            generation,
            socket,
            peer,
            rx,
            video_ready,
            max_bitrate_bps,
        ));
        Ok(AcceptedPeer { answer_sdp, rtp_maps, handle, reoffered: false })
    }

    /// Tells every other peer in the room that `from` now publishes these
    /// kinds, so each subscriber allocates its egress mid for it.
    fn announce_source(&self, room: RoomId, from: ParticipantId, audio: bool, video: bool) {
        if !audio && !video {
            return;
        }
        let targets: Vec<PeerHandle> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .get(&room)
                .map(|peers| peers.iter().filter(|(pid, _)| **pid != from).map(|(_, handle)| handle.clone()).collect())
                .unwrap_or_default()
        };
        for target in targets {
            if audio {
                let _ = target.tx.try_send(PeerCommand::EnsureSource { from, audio: true });
            }
            if video {
                let _ = target.tx.try_send(PeerCommand::EnsureSource { from, audio: false });
            }
        }
    }

    /// Removes a peer and closes its command channel; the task exits after
    /// flushing its last datagrams and dropping its `Rtc` (DTLS close).
    pub fn remove(&self, room: RoomId, participant: ParticipantId) {
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(room, participant));
        self.bitrate_caps.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(room, participant));
        self.last_keyframe_request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(key_room, publisher, _), _| !(*key_room == room && *publisher == participant));
        let (removed, survivors) = {
            let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut survivors = Vec::new();
            let mut removed = false;
            if let Some(peers) = rooms.get_mut(&room) {
                if peers.remove(&participant).is_some() {
                    self.peer_total.fetch_sub(1, Ordering::Relaxed);
                    removed = true;
                }
                survivors = peers.values().cloned().collect();
                if peers.is_empty() {
                    rooms.remove(&room);
                }
                WEBRTC_PEERS.set(rooms.values().map(HashMap::len).sum::<usize>() as i64);
            }
            (removed, survivors)
        };
        if !removed {
            return;
        }
        // Retire the departed publisher's egress mids in every surviving peer:
        // the mid must never be reused for a different publisher (a new RTP
        // clock domain under a live decoder). Close the per-publisher keyframe
        // gate synchronously, before the command is queued, so a frame cannot
        // slip through into the reopened gate in the meantime.
        for survivor in survivors {
            survivor.forget_video(participant);
            let _ = survivor.tx.try_send(PeerCommand::DropSource { from: participant });
        }
    }

    /// Exits a peer task without deleting a newer generation of the same
    /// participant (re-offer/ICE restart race).
    ///
    /// A dead task is a departed publisher (the signalling session may still be
    /// alive, e.g. ICE/DTLS death): survivors must be told to retire its
    /// source exactly like [`Self::remove`] does, the PLI coalescing window
    /// must be cleared, and the per-publisher keyframe gate must be closed so a
    /// reconnect with the same `ParticipantId` waits for a fresh IDR (P2-1).
    fn remove_generation(&self, room: RoomId, participant: ParticipantId, generation: u64) {
        let (removed, survivors) = {
            let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut removed = false;
            let mut survivors = Vec::new();
            if let Some(peers) = rooms.get_mut(&room) {
                if peers.get(&participant).is_some_and(|handle| handle.generation == generation) {
                    peers.remove(&participant);
                    self.peer_total.fetch_sub(1, Ordering::Relaxed);
                    removed = true;
                }
                survivors = peers.values().cloned().collect();
                if peers.is_empty() {
                    rooms.remove(&room);
                }
            }
            if removed {
                WEBRTC_PEERS.set(rooms.values().map(HashMap::len).sum::<usize>() as i64);
            }
            (removed, survivors)
        };
        // A stale generation (already replaced by a re-offer) must not touch
        // the new generation's coalescing state.
        if !removed {
            return;
        }
        self.last_keyframe_request
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|(key_room, publisher, _), _| !(*key_room == room && *publisher == participant));
        for survivor in survivors {
            survivor.forget_video(participant);
            let _ = survivor.tx.try_send(PeerCommand::DropSource { from: participant });
        }
    }

    /// Number of registered browser peers in a room (tests/observability).
    pub fn peer_count(&self, room: RoomId) -> usize {
        self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&room).map_or(0, HashMap::len)
    }

    /// Applies `SubscribeRequest` semantics to the peer fanout: a subscriber
    /// that asked only for audio does not receive video frames, and vice versa.
    pub fn set_subscription(&self, room: RoomId, participant: ParticipantId, audio: bool, video: bool) {
        // Remember it even if the SDP offer has not arrived yet; otherwise a
        // client that subscribes before negotiating would silently receive
        // nothing.
        self.subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert((room, participant), (audio, video));
        let handle = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms.get(&room).and_then(|peers| peers.get(&participant)).cloned()
        };
        let Some(handle) = handle else { return };
        handle.audio.store(audio, Ordering::Relaxed);
        handle.video.store(video, Ordering::Relaxed);
        if video {
            // Arm the gate synchronously, before any queued Write can slip in
            // ahead of the SetSubscription command. Every publisher has to
            // re-deliver an IDR before its deltas resume.
            handle.rearm_video();
        }
        let _ = handle.tx.try_send(PeerCommand::SetSubscription { audio, video });
    }

    /// Maximum egress bitrate requested by the subscriber (0 = unlimited).
    pub fn set_max_bitrate(&self, room: RoomId, participant: ParticipantId, bps: u64) {
        self.bitrate_caps.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert((room, participant), bps);
        let handle = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms.get(&room).and_then(|peers| peers.get(&participant)).cloned()
        };
        if let Some(handle) = handle {
            handle.max_bitrate_bps.store(bps, Ordering::Relaxed);
        }
    }

    /// Removes real peers, either one room or all rooms (mode switch to MCU).
    pub fn evict(&self, room: Option<RoomId>) {
        let victims: Vec<(RoomId, ParticipantId)> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .iter()
                .filter(|(id, _)| room.is_none_or(|wanted| **id == wanted))
                .flat_map(|(id, peers)| peers.keys().map(|participant| (*id, *participant)).collect::<Vec<_>>())
                .collect()
        };
        for (room, participant) in victims {
            self.remove(room, participant);
        }
    }

    /// Whether at least one real peer is registered in the room.
    pub fn room_has_peer(&self, room: RoomId) -> bool {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&room)
            .is_some_and(|peers| !peers.is_empty())
    }

    fn targets_for(&self, room: RoomId, from: ParticipantId, audio: bool) -> Vec<PeerHandle> {
        let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut targets: Vec<PeerHandle> = rooms
            .get(&room)
            .map(|peers| {
                peers
                    .iter()
                    .filter(|(pid, handle)| **pid != from && handle.wants(audio))
                    .map(|(_, h)| h.clone())
                    .collect()
            })
            .unwrap_or_default();
        // Deterministic fanout order (room registration order): a legacy peer
        // with a single egress mid must always pick the same publisher, never a
        // random one from HashMap iteration.
        targets.sort_by_key(|handle| handle.generation);
        targets
    }

    /// Whether a participant has a live browser transport.
    pub fn has_peer(&self, room: RoomId, participant: ParticipantId) -> bool {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&room)
            .is_some_and(|peers| peers.contains_key(&participant))
    }

    /// Asks a publisher's browser for a keyframe, coalesced per
    /// `(room, publisher, audio)`: N subscribers dropping the same source
    /// produce at most one PLI per window instead of N.
    fn request_keyframe_from(&self, room: RoomId, publisher: ParticipantId, audio: bool) {
        {
            let now = Instant::now();
            let mut last = self.last_keyframe_request.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let key = (room, publisher, audio);
            if last.get(&key).is_some_and(|previous| now.duration_since(*previous) < KEYFRAME_REQUEST_INTERVAL) {
                return;
            }
            last.insert(key, now);
        }
        let handle = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms.get(&room).and_then(|peers| peers.get(&publisher)).cloned()
        };
        if let Some(handle) = handle {
            if handle.publishes_video {
                let _ = handle.tx.try_send(PeerCommand::RequestKeyframe { audio });
            }
        }
    }

    /// Fallback broadcast used when a browser PLI cannot be attributed to one
    /// publisher (egress mid not mapped yet); the mapped path is
    /// [`Self::request_keyframe_from`].
    fn request_keyframe(&self, room: RoomId, from: ParticipantId, audio: bool) {
        let targets: Vec<PeerHandle> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .get(&room)
                .map(|peers| {
                    peers
                        .iter()
                        .filter(|(pid, handle)| **pid != from && handle.publishes_video)
                        .map(|(_, h)| h.clone())
                        .collect()
                })
                .unwrap_or_default()
        };
        for target in targets {
            let _ = target.tx.try_send(PeerCommand::RequestKeyframe { audio });
        }
    }

    async fn forward(&self, room: RoomId, from: ParticipantId, data: str0m::media::MediaData) {
        self.enqueue_frame(
            room,
            from,
            data.params.spec().codec.is_audio(),
            data.is_keyframe(),
            data.params,
            data.network_time,
            data.time,
            Arc::clone(&data.data),
        );
    }

    /// Fanout of one depayloaded frame to every subscribed peer (sync: the
    /// publisher's task must never await a slow subscriber).
    #[allow(clippy::too_many_arguments)]
    fn enqueue_frame(
        &self,
        room: RoomId,
        from: ParticipantId,
        audio: bool,
        is_keyframe: bool,
        params: PayloadParams,
        wallclock: Instant,
        rtp_time: MediaTime,
        data: Arc<[u8]>,
    ) {
        let targets = self.targets_for(room, from, audio);
        if targets.is_empty() {
            return;
        }
        for target in targets {
            // T11b keyframe gate, per publisher: a subscriber that has not seen
            // this publisher's IDR yet must not receive its deltas (they would
            // corrupt the decoder), and asking for the IDR is coalesced.
            if !audio && !target.admits_video(from, is_keyframe) {
                WEBRTC_FRAMES_DROPPED.with_label_values(&["awaiting_keyframe"]).inc();
                self.request_keyframe_from(room, from, false);
                continue;
            }
            // A slow browser must never stall the publisher or other peers.
            if target
                .tx
                .try_send(PeerCommand::Write { from, audio, params, wallclock, rtp_time, data: Arc::clone(&data) })
                .is_err()
            {
                WEBRTC_FRAMES_DROPPED.with_label_values(&["queue_full"]).inc();
                tracing::debug!(participant = ?target.participant, "webrtc fanout queue full; dropping frame");
                // The next delivered frame may be a delta after a gap: wait
                // for a fresh keyframe instead of corrupting the decoder.
                if !audio {
                    target.forget_video(from);
                }
                // Ask the publisher for a fresh keyframe so the slow
                // subscriber can resynchronise after its queue drains.
                self.request_keyframe_from(room, from, audio);
            } else {
                MEDIA_BYTES
                    .with_label_values(&[if audio { "opus" } else { "vp8" }, "downlink"])
                    .inc_by(data.len() as u64);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_peer(
    registry: Arc<WebRtcRoomRegistry>,
    room: RoomId,
    participant: ParticipantId,
    generation: u64,
    socket: UdpSocket,
    mut peer: WebRtcPeer,
    mut rx: mpsc::Receiver<PeerCommand>,
    // Shared with `PeerHandle::video_ready`: the task closes this subscriber's
    // per-publisher gate when it retires a source.
    video_ready: Arc<Mutex<HashSet<ParticipantId>>>,
    max_bitrate_bps: Arc<AtomicU64>,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let created_at = Instant::now();
    let mut last_seen = created_at;
    // Sources this peer cannot show because every egress mid is taken/retired;
    // counted once per source so the metric reads "one invisible publisher".
    let mut no_egress_mid: HashSet<(ParticipantId, bool)> = HashSet::new();
    loop {
        if !peer.is_connected() && created_at.elapsed() > CONNECT_DEADLINE {
            tracing::warn!(?participant, "WebRTC peer did not connect before deadline");
            WEBRTC_ICE_FAILURES.with_label_values(&["connect_deadline"]).inc();
            break;
        }
        if last_seen.elapsed() > IDLE_TIMEOUT {
            WEBRTC_ICE_FAILURES.with_label_values(&["idle_timeout"]).inc();
            tracing::info!(?participant, "WebRTC peer idle; evicting");
            break;
        }
        let deadline = peer.next_timeout();
        let sleep = async move {
            match deadline {
                Some(at) => {
                    let at = tokio::time::Instant::from_std(at);
                    let now = tokio::time::Instant::now();
                    if at > now {
                        tokio::time::sleep_until(at).await;
                    } else {
                        // Past deadlines must not spin the runtime at 1 kHz.
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                }
                None => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        };
        tokio::select! {
            received = socket.recv_from(&mut buf) => {
                let Ok((len, source)) = received else { break };
                last_seen = Instant::now();
                // The socket is bound to 0.0.0.0; str0m needs the advertised
                // local candidate address to associate an ICE-lite Binding
                // Request with the right local candidate.
                let destination = peer.local_addr();
                let actions = match peer.handle_receive(Instant::now(), source, destination, &buf[..len]) {
                    Ok(actions) => actions,
                    Err(error) => {
                        tracing::warn!(?error, ?participant, "WebRTC datagram rejected; closing peer");
                        break;
                    }
                };
                if !dispatch(&registry, room, participant, &socket, &mut peer, &max_bitrate_bps, actions).await {
                    break;
                }
            }
            command = rx.recv() => {
                let Some(command) = command else { break };
                let now = Instant::now();
                let actions = match command {
                    PeerCommand::EnsureSource { from, audio } => {
                        if peer.ensure_source(from, audio) {
                            WEBRTC_SOURCES.with_label_values(&["allocated"]).inc();
                            tracing::debug!(?participant, ?from, audio, "webrtc egress mid allocated");
                        } else if no_egress_mid.insert((from, audio)) {
                            // Policy (T11b D4): a legacy 1+1 browser has one
                            // egress mid per kind, so with 3+ publishers one is
                            // not visible to it. That is an explicit, counted
                            // degradation, never two publishers glued into one
                            // mid (two Opus clock domains would garble audio).
                            WEBRTC_SOURCES.with_label_values(&["dropped_no_mid"]).inc();
                            tracing::warn!(
                                ?participant,
                                ?from,
                                audio,
                                "webrtc: no free egress mid for this publisher; the peer will not see it \
                                 (client pre-offer needs 1+N-1 mids, or raise VKS_WEBRTC_MAX_PEERS_PER_ROOM)"
                            );
                        }
                        continue;
                    }
                    PeerCommand::DropSource { from } => {
                        peer.retire_source(from);
                        // Close this subscriber's keyframe gate for the retired
                        // publisher: a reconnect with the same ParticipantId
                        // starts a new RTP clock domain and must wait for a
                        // fresh IDR, like any other new source (P2-1).
                        video_ready
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&from);
                        no_egress_mid.retain(|(publisher, _)| *publisher != from);
                        WEBRTC_SOURCES.with_label_values(&["retired"]).inc();
                        continue;
                    }
                    PeerCommand::Write { from, audio, params, wallclock, rtp_time, data } => {
                        match peer.mid_for_source(from, audio) {
                            Some(mid) => match peer.write_media_matched(mid, params, wallclock, rtp_time, data) {
                                Ok(actions) => Ok(actions),
                                Err(WebRtcError::UnknownMid) => {
                                    // A rejected m-line (port 0) or a mid whose
                                    // writer went away is a dropped frame, not a
                                    // reason to tear down the transport (T11b).
                                    WEBRTC_FRAMES_DROPPED.with_label_values(&["no_egress_mid"]).inc();
                                    continue;
                                }
                                Err(error) => Err(error),
                            },
                            None => {
                                WEBRTC_FRAMES_DROPPED.with_label_values(&["no_egress_mid"]).inc();
                                if no_egress_mid.insert((from, audio)) {
                                    WEBRTC_SOURCES.with_label_values(&["dropped_no_mid"]).inc();
                                    tracing::warn!(
                                        ?participant,
                                        ?from,
                                        audio,
                                        "webrtc: publisher has no egress mid in this peer's SDP; frames dropped"
                                    );
                                }
                                continue;
                            }
                        }
                    }
                    PeerCommand::Reoffer { offer_sdp, reply } => {
                        let result: Result<(String, Vec<WebRtcAction>), WebRtcError> =
                            peer.accept_reoffer(&offer_sdp);
                        match result {
                            Ok((answer_sdp, mut actions)) => {
                                let _ = reply.send(Ok(ReofferAnswer {
                                    answer_sdp,
                                    rtp_maps: peer.rtp_maps(),
                                    publishes_audio: offer_publishes(&offer_sdp, true),
                                    publishes_video: offer_publishes(&offer_sdp, false),
                                }));
                                // Local ICE/DTLS actions from the renegotiation
                                // must be transmitted like any other output.
                                let now = Instant::now();
                                let mut tail = match peer.handle_timeout(now) {
                                    Ok(tail) => tail,
                                    Err(error) => {
                                        tracing::warn!(?error, ?participant, "WebRTC re-offer timer failed");
                                        break;
                                    }
                                };
                                actions.append(&mut tail);
                                Ok(actions)
                            }
                            Err(WebRtcError::InvalidOffer(message)) => {
                                // The offered SDP is not something this peer can
                                // renegotiate (e.g. getDisplayMedia adds a second
                                // video m-line, or a data channel). The live call
                                // keeps flowing (peer/socket/DTLS/media), though
                                // str0m may already have applied the offer's ICE
                                // credentials: reject this re-offer only.
                                let _ = reply.send(Err(message.clone()));
                                tracing::warn!(%message, ?participant, "WebRTC re-offer rejected; keeping live session");
                                continue;
                            }
                            Err(error) => {
                                let _ = reply.send(Err(error.to_string()));
                                tracing::warn!(?error, ?participant, "WebRTC re-offer failed; closing peer");
                                break;
                            }
                        }
                    }
                    PeerCommand::RequestKeyframe { audio } => {
                        // Only the browser's own publish m-line can be asked for
                        // a keyframe; an egress-only (`sendonly`) mid has no
                        // source and used to be picked by `mid_for_kind`.
                        let result = match peer.publish_mid(audio) {
                            Some(mid) => peer.request_keyframe(mid),
                            None => Ok(Vec::new()),
                        };
                        if let Err(error) = result {
                            // A recvonly/camera-off peer has no receiver source;
                            // a keyframe request must never kill its transport.
                            tracing::debug!(?error, ?participant, "WebRTC keyframe request ignored");
                            continue;
                        }
                        result
                    }
                    PeerCommand::SetSubscription { audio, video } => {
                        // The gate was re-armed synchronously in
                        // `WebRtcRoomRegistry::set_subscription`, before this
                        // command was queued, so no stale Write can slip in.
                        tracing::trace!(?participant, audio, video, "webrtc subscription updated");
                        continue;
                    }
                };
                let actions = match actions {
                    Ok(actions) => actions,
                    Err(error) => {
                        tracing::warn!(?error, ?participant, "WebRTC write failed; closing peer");
                        break;
                    }
                };
                let actions = match peer.handle_timeout(now) {
                    Ok(mut tail) => {
                        let mut actions = actions;
                        actions.append(&mut tail);
                        actions
                    }
                    Err(error) => {
                        tracing::warn!(?error, ?participant, "WebRTC timer failed; closing peer");
                        break;
                    }
                };
                if !dispatch(&registry, room, participant, &socket, &mut peer, &max_bitrate_bps, actions).await {
                    break;
                }
            }
            _ = sleep => {
                let actions = match peer.handle_timeout(Instant::now()) {
                    Ok(actions) => actions,
                    Err(error) => {
                        tracing::warn!(?error, ?participant, "WebRTC timer failed; closing peer");
                        break;
                    }
                };
                if !dispatch(&registry, room, participant, &socket, &mut peer, &max_bitrate_bps, actions).await {
                    break;
                }
            }
        }
    }

    registry.remove_generation(room, participant, generation);
    tracing::debug!(?participant, "webrtc peer task stopped");
}

async fn dispatch(
    registry: &Arc<WebRtcRoomRegistry>,
    room: RoomId,
    participant: ParticipantId,
    socket: &UdpSocket,
    peer: &mut WebRtcPeer,
    max_bitrate_bps: &AtomicU64,
    mut actions: Vec<WebRtcAction>,
) -> bool {
    // Apply local-only actions before fanout. BWE must actually steer the
    // pacer; a connected peer must immediately ask the publisher for an IDR.
    let mut extra = Vec::new();
    for action in &actions {
        match action {
            WebRtcAction::Bitrate(bps) => {
                let cap = max_bitrate_bps.load(Ordering::Relaxed);
                let applied = if cap == 0 { *bps } else { (*bps).min(cap) };
                peer.set_desired_bitrate(applied);
            }
            WebRtcAction::Connected => {
                tracing::info!(?participant, "WebRTC peer connected (ICE+DTLS+SRTP)");
                // Ask for an IDR on the browser's own publish m-line, never on
                // an egress-only section (which has no source at all).
                if let Some(mid) = peer.publish_mid(false) {
                    if let Ok(mut tail) = peer.request_keyframe(mid) {
                        extra.append(&mut tail);
                    }
                }
            }
            _ => {}
        }
    }
    actions.append(&mut extra);
    for action in actions {
        match action {
            WebRtcAction::Send { destination, contents, .. } => {
                // Send failures (ICMP unreachable, transient ENOBUFS) are not
                // fatal for a UDP media session.
                let _ = socket.send_to(&contents, destination).await;
            }
            WebRtcAction::Media(data) => {
                registry.forward(room, participant, *data).await;
            }
            WebRtcAction::Connected => {}
            WebRtcAction::IceState(state) => {
                tracing::info!(?participant, ?state, "WebRTC ICE state");
                if matches!(state, str0m::IceConnectionState::Disconnected) {
                    WEBRTC_ICE_FAILURES.with_label_values(&[&format!("{state:?}")]).inc();
                }
            }
            WebRtcAction::Bitrate(bps) => {
                tracing::debug!(bps, ?participant, "webrtc BWE estimate applied");
            }
            WebRtcAction::KeyframeRequest { audio, source } => match source {
                // T11b: an egress mid names exactly one publisher, so the PLI
                // goes to that publisher only (coalesced) instead of to every
                // publisher in the room.
                Some(publisher) => registry.request_keyframe_from(room, publisher, audio),
                None => registry.request_keyframe(room, participant, audio),
            },
            WebRtcAction::Closed => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
    use str0m::format::Codec;
    use str0m::media::{Direction, MediaKind};
    use str0m::{Candidate, RtcConfig};

    const CLIENT_ADDR: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 41101);

    /// Real browser offer with `audio_sections`/`video_sections` m-lines (the
    /// first of each kind is sendrecv, the rest are pre-offered recvonly slots).
    fn browser_offer(audio_sections: usize, video_sections: usize) -> String {
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(CLIENT_ADDR, "udp").expect("candidate"));
        let mut change = client.sdp_api();
        for index in 0..audio_sections {
            let direction = if index == 0 { Direction::SendRecv } else { Direction::RecvOnly };
            change.add_media(MediaKind::Audio, direction, None, None, None);
        }
        for index in 0..video_sections {
            let direction = if index == 0 { Direction::SendRecv } else { Direction::RecvOnly };
            change.add_media(MediaKind::Video, direction, None, None, None);
        }
        let (offer, _pending): (SdpOffer, SdpPendingOffer) = change.apply().expect("offer");
        offer.to_sdp_string()
    }

    /// A negotiated VP8 `PayloadParams`, obtained from a real peer instead of a
    /// hand-built struct (its fields are private in str0m).
    fn video_params() -> PayloadParams {
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(CLIENT_ADDR, "udp").expect("candidate"));
        let mut change = client.sdp_api();
        let video_mid = change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        let (offer, pending): (SdpOffer, SdpPendingOffer) = change.apply().expect("offer");
        let peer = WebRtcPeer::accept_offer(&offer.to_sdp_string(), CLIENT_ADDR, &WebRtcConfig::default())
            .expect("accept offer");
        let answer = SdpAnswer::from_sdp_string(peer.answer_sdp()).expect("answer parses");
        client.sdp_api().accept_answer(pending, answer).expect("accept answer");
        let writer = client.writer(video_mid).expect("video writer");
        let params = writer
            .payload_params()
            .find(|params| params.spec().codec == Codec::Vp8)
            .copied()
            .expect("VP8 payload params");
        params
    }

    fn handle(participant: ParticipantId) -> (PeerHandle, mpsc::Receiver<PeerCommand>) {
        handle_with(participant, 1)
    }

    fn handle_with(participant: ParticipantId, generation: u64) -> (PeerHandle, mpsc::Receiver<PeerCommand>) {
        let (tx, rx) = mpsc::channel(8);
        (
            PeerHandle {
                participant,
                tx,
                audio: Arc::new(AtomicBool::new(true)),
                video: Arc::new(AtomicBool::new(true)),
                max_bitrate_bps: Arc::new(AtomicU64::new(0)),
                video_ready: Arc::new(Mutex::new(HashSet::new())),
                publishes_audio: true,
                publishes_video: true,
                generation,
            },
            rx,
        )
    }

    fn test_limits(per_room: usize) -> RoomLimits {
        RoomLimits { max_peers_per_room: per_room, max_peers: DEFAULT_MAX_PEERS }
    }

    /// Hermetic constructor: tests never read the ambient
    /// `VKS_WEBRTC_MAX_PEERS*` environment (that path is unit-tested via
    /// `RoomLimits::parse`, which is pure).
    fn default_registry() -> Arc<WebRtcRoomRegistry> {
        WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), RoomLimits::default())
    }

    #[tokio::test]
    async fn fanout_respects_participant_exclusion_and_audio_only() {
        let registry = default_registry();
        let room = RoomId::new();
        let publisher = ParticipantId::new();
        let audio_sub = ParticipantId::new();
        let video_sub = ParticipantId::new();
        let (publisher_handle, _publisher_rx) = handle(publisher);
        let (audio_handle, _audio_rx) = handle(audio_sub);
        let (video_handle, _video_rx) = handle(video_sub);
        {
            let mut rooms = registry.rooms.lock().unwrap();
            let peers = rooms.entry(room).or_default();
            peers.insert(publisher, publisher_handle);
            peers.insert(audio_sub, audio_handle);
            peers.insert(video_sub, video_handle);
        }
        let audio_targets = registry.targets_for(room, publisher, true);
        assert_eq!(audio_targets.len(), 2);
        let video_targets = registry.targets_for(room, publisher, false);
        assert_eq!(video_targets.len(), 2);

        registry.set_subscription(room, audio_sub, true, false);
        let audio_targets = registry.targets_for(room, publisher, true);
        assert_eq!(audio_targets.len(), 2, "audio-only subscriber still receives audio");
        let video_targets = registry.targets_for(room, publisher, false);
        assert_eq!(video_targets.len(), 1, "audio-only subscriber is excluded from video");
        assert_ne!(video_targets[0].participant, audio_sub);

        registry.set_subscription(room, video_sub, false, false);
        assert_eq!(registry.targets_for(room, publisher, true).len(), 1);
        assert!(registry.targets_for(room, publisher, false).is_empty());
        assert_eq!(registry.peer_count(room), 3);
        assert!(registry.has_peer(room, publisher));
        registry.remove(room, publisher);
        registry.remove(room, publisher);
        assert_eq!(registry.peer_count(room), 2);
        assert!(!registry.has_peer(room, publisher));
    }

    #[tokio::test]
    async fn stale_task_cannot_remove_a_newer_generation() {
        let registry = default_registry();
        let room = RoomId::new();
        let participant = ParticipantId::new();
        let (mut peer, _rx) = handle(participant);
        peer.generation = 2;
        registry.rooms.lock().unwrap().entry(room).or_default().insert(participant, peer);
        // The old task (generation 1) exits late and must not delete gen 2.
        registry.remove_generation(room, participant, 1);
        assert!(registry.has_peer(room, participant));
        registry.remove_generation(room, participant, 2);
        assert!(!registry.has_peer(room, participant));
    }

    #[tokio::test]
    async fn peers_are_admitted_up_to_the_per_room_cap() {
        let registry =
            WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), test_limits(3));
        assert_eq!(registry.max_peers_per_room(), 3);
        let room = RoomId::new();
        let offer = browser_offer(1, 1);
        for expected in 1..=3 {
            registry
                .accept_offer(room, ParticipantId::new(), &offer)
                .await
                .unwrap_or_else(|error| panic!("peer #{expected} must be admitted, got {error:?}"));
            assert_eq!(registry.peer_count(room), expected, "three peers, not two");
        }
        let error = registry.accept_offer(room, ParticipantId::new(), &offer).await.expect_err("cap + 1 is room_full");
        assert!(matches!(error, WebRtcError::RoomFull), "unexpected: {error:?}");
        assert_eq!(registry.peer_count(room), 3, "rejected peer must not be registered");
    }

    #[test]
    fn room_limits_default_to_eight_and_parse_fail_closed() {
        assert_eq!(RoomLimits::default(), RoomLimits { max_peers_per_room: 8, max_peers: DEFAULT_MAX_PEERS });
        assert_eq!(RoomLimits::parse(None, None).expect("defaults"), RoomLimits::default());
        assert_eq!(
            RoomLimits::parse(Some(" 4 "), Some("32")).expect("valid limits"),
            RoomLimits { max_peers_per_room: 4, max_peers: 32 }
        );
        for invalid in ["8x", "", " ", "0", "-1", "1.5"] {
            assert!(RoomLimits::parse(Some(invalid), None).is_err(), "per-room cap {invalid:?} must fail closed");
            assert!(RoomLimits::parse(None, Some(invalid)).is_err(), "global cap {invalid:?} must fail closed");
        }
    }

    #[tokio::test]
    async fn port_range_exhaustion_is_not_room_full() {
        let mut registry =
            WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), test_limits(8));
        let blocker = std::net::UdpSocket::bind(("127.0.0.1", 0)).expect("bind blocker");
        let port = blocker.local_addr().expect("local addr").port();
        Arc::get_mut(&mut registry).expect("unique registry").port_range = Some((port, port));
        let error = registry
            .accept_offer(RoomId::new(), ParticipantId::new(), "v=0\r\n")
            .await
            .expect_err("exhausted port range must fail");
        assert!(matches!(error, WebRtcError::Net(_)), "port exhaustion is a transport error, not room_full: {error:?}");
        drop(blocker);
    }

    #[tokio::test]
    async fn removing_a_publisher_retires_its_source_in_every_subscriber() {
        let registry =
            WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), test_limits(8));
        let room = RoomId::new();
        let publisher = ParticipantId::new();
        let (publisher_handle, _publisher_rx) = handle(publisher);
        let (first, mut first_rx) = handle(ParticipantId::new());
        let (second, mut second_rx) = handle(ParticipantId::new());
        {
            let mut rooms = registry.rooms.lock().unwrap();
            let peers = rooms.entry(room).or_default();
            peers.insert(publisher, publisher_handle);
            peers.insert(first.participant, first);
            peers.insert(second.participant, second);
        }
        registry.remove(room, publisher);
        for rx in [&mut first_rx, &mut second_rx] {
            match rx.try_recv() {
                Ok(PeerCommand::DropSource { from }) => assert_eq!(from, publisher),
                _ => panic!("every survivor must be told to retire the departed publisher"),
            }
        }
        assert!(!registry.has_peer(room, publisher));
    }

    #[tokio::test]
    async fn fanout_order_is_registration_order_not_hashmap_order() {
        let registry =
            WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), test_limits(8));
        let room = RoomId::new();
        let publisher = ParticipantId::new();
        let late = ParticipantId::new();
        let early = ParticipantId::new();
        let middle = ParticipantId::new();
        let mut rooms = registry.rooms.lock().unwrap();
        let peers = rooms.entry(room).or_default();
        peers.insert(publisher, handle_with(publisher, 1).0);
        peers.insert(late, handle_with(late, 4).0);
        peers.insert(early, handle_with(early, 2).0);
        peers.insert(middle, handle_with(middle, 3).0);
        drop(rooms);
        let order: Vec<ParticipantId> =
            registry.targets_for(room, publisher, true).iter().map(|handle| handle.participant).collect();
        assert_eq!(order, vec![early, middle, late], "a legacy single-mid peer must pick a stable publisher");
    }

    /// 3 peers via mpsc (T11b): one publisher frame reaches all subscribers, each
    /// Write carries the exact publisher id, and the keyframe gate is per
    /// publisher: a delta is withheld until that publisher's IDR arrives.
    #[tokio::test]
    async fn three_peers_via_mpsc_per_source_gate() {
        let registry =
            WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), test_limits(8));
        let room = RoomId::new();
        let publisher = ParticipantId::new();
        let mut subscribers: Vec<(PeerHandle, mpsc::Receiver<PeerCommand>)> =
            (0..3).map(|_| handle(ParticipantId::new())).collect();
        {
            let mut rooms = registry.rooms.lock().unwrap();
            let peers = rooms.entry(room).or_default();
            peers.insert(publisher, handle(publisher).0);
            for (handle, _) in &subscribers {
                peers.insert(handle.participant, handle.clone());
            }
        }
        let params = video_params();
        let payload = Arc::<[u8]>::from(vec![0x10u8, 0x00, 0x00, 0x9d]);
        let now = Instant::now();
        // Audio always flows; no keyframe gate applies.
        registry.enqueue_frame(room, publisher, true, false, params, now, MediaTime::from_millis(0), payload.clone());
        for (handle, rx) in subscribers.iter_mut() {
            match rx.try_recv() {
                Ok(PeerCommand::Write { from, audio, .. }) => {
                    assert_eq!(from, publisher);
                    assert!(audio);
                }
                _ => panic!("subscriber {:?} got no audio write", handle.participant),
            }
        }
        // Video delta before any IDR is dropped for every subscriber.
        registry.enqueue_frame(room, publisher, false, false, params, now, MediaTime::from_millis(0), payload.clone());
        for (handle, rx) in subscribers.iter_mut() {
            assert!(rx.try_recv().is_err(), "delta before IDR leaked to {:?}", handle.participant);
        }
        // The IDR opens the gate for everyone; the following delta flows.
        registry.enqueue_frame(room, publisher, false, true, params, now, MediaTime::from_millis(0), payload.clone());
        registry.enqueue_frame(room, publisher, false, false, params, now, MediaTime::from_millis(33), payload);
        for (handle, rx) in subscribers.iter_mut() {
            for expected_keyframe in [true, false] {
                match rx.try_recv() {
                    Ok(PeerCommand::Write { from, audio, data, .. }) => {
                        assert_eq!(from, publisher);
                        assert!(!audio);
                        assert_eq!(&*data, &[0x10u8, 0x00, 0x00, 0x9d]);
                    }
                    _ => {
                        panic!("subscriber {:?} missing video write (keyframe={expected_keyframe})", handle.participant)
                    }
                }
            }
        }
    }

    #[test]
    fn keyframe_gate_is_per_publisher() {
        let (handle, _rx) = handle(ParticipantId::new());
        let first = ParticipantId::new();
        let second = ParticipantId::new();
        assert!(!handle.admits_video(first, false), "delta before any IDR is dropped");
        assert!(!handle.admits_video(second, false), "every publisher starts gated");
        assert!(handle.admits_video(second, true), "B's IDR opens B only");
        assert!(handle.admits_video(second, false), "deltas flow for B");
        assert!(!handle.admits_video(first, false), "A is still gated after B's IDR");
        assert!(handle.admits_video(first, true));
        // Queue overflow or re-subscription re-gates.
        handle.forget_video(second);
        assert!(!handle.admits_video(second, false));
        assert!(handle.admits_video(first, false), "forgetting B must not gate A");
        handle.rearm_video();
        assert!(!handle.admits_video(first, false), "re-subscription waits for fresh IDRs from everyone");
    }

    #[tokio::test]
    async fn source_fanout_skips_a_publisher_whose_slot_was_retired() {
        // A retired egress mid must not be reused: the source goes invisible and
        // the degradation is counted instead of gluing two clock domains.
        let room = RoomId::new();
        let registry =
            WebRtcRoomRegistry::with_limits(WebRtcConfig::default(), "127.0.0.1".parse().unwrap(), test_limits(8));
        let publisher = ParticipantId::new();
        let (subscriber, mut rx) = handle(ParticipantId::new());
        registry.rooms.lock().unwrap().entry(room).or_default().insert(publisher, handle(publisher).0);
        registry.rooms.lock().unwrap().entry(room).or_default().insert(subscriber.participant, subscriber.clone());
        // Pretend the subscriber retired the publisher's mid.
        subscriber.forget_video(publisher);
        let params = video_params();
        registry.enqueue_frame(
            room,
            publisher,
            false,
            false,
            params,
            Instant::now(),
            MediaTime::from_millis(0),
            Arc::from(vec![0u8; 8]),
        );
        assert!(rx.try_recv().is_err(), "a gated source must not leak deltas");
    }

    #[tokio::test]
    async fn global_cap_rejects_first_peer_in_a_fresh_room() {
        let mut registry = default_registry();
        Arc::get_mut(&mut registry).expect("unique registry").max_peers = 0;
        let error =
            registry.accept_offer(RoomId::new(), ParticipantId::new(), "v=0\r\n").await.expect_err("global cap");
        assert!(matches!(error, WebRtcError::CapacityExceeded), "unexpected: {error:?}");
        assert_eq!(registry.peer_total.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn subscription_and_bitrate_caps_are_remembered_before_offer() {
        let registry = default_registry();
        let room = RoomId::new();
        let participant = ParticipantId::new();
        registry.set_subscription(room, participant, true, false);
        registry.set_max_bitrate(room, participant, 512_000);
        assert_eq!(registry.subscriptions.lock().unwrap().get(&(room, participant)), Some(&(true, false)));
        assert_eq!(registry.bitrate_caps.lock().unwrap().get(&(room, participant)), Some(&512_000));
        registry.remove(room, participant);
        assert!(registry.subscriptions.lock().unwrap().get(&(room, participant)).is_none());
        assert!(registry.bitrate_caps.lock().unwrap().get(&(room, participant)).is_none());
    }

    #[tokio::test]
    async fn removing_last_peer_drops_the_room_entry() {
        let registry = default_registry();
        let room = RoomId::new();
        let participant = ParticipantId::new();
        let (peer, _rx) = handle(participant);
        registry.rooms.lock().unwrap().entry(room).or_default().insert(participant, peer);
        assert_eq!(registry.peer_count(room), 1);
        registry.remove(room, participant);
        assert_eq!(registry.peer_count(room), 0);
        assert!(registry.rooms.lock().unwrap().get(&room).is_none());
    }

    /// A re-offer must be answered by the existing peer task through
    /// `PeerCommand::Reoffer` (same str0m session/DTLS), never by binding a
    /// second socket. This checks the registry plumbing end-to-end without a
    /// real UDP/DTLS handshake.
    #[tokio::test]
    async fn reoffer_is_answered_by_the_existing_peer_task() {
        let registry = default_registry();
        let room = RoomId::new();
        let participant = ParticipantId::new();
        let (peer, mut rx) = handle(participant);
        let local_addr = peer.tx.clone();
        registry.rooms.lock().unwrap().entry(room).or_default().insert(participant, peer);
        let peer_task = tokio::spawn(async move {
            let _keep = local_addr;
            match rx.recv().await {
                Some(PeerCommand::Reoffer { reply, .. }) => {
                    reply
                        .send(Ok(ReofferAnswer {
                            answer_sdp: "v=0\r\na=mid:0\r\n".to_string(),
                            rtp_maps: vec![(111, "opus".to_string(), 48_000, 2)],
                            publishes_audio: true,
                            publishes_video: true,
                        }))
                        .expect("registry must await the answer");
                }
                _ => panic!("expected a Reoffer command"),
            }
        });
        assert_eq!(registry.peer_count(room), 1, "re-offer must not add a peer");
        let accepted =
            registry.accept_offer(room, participant, "v=0\r\n").await.expect("existing peer must answer the re-offer");
        assert_eq!(accepted.answer_sdp, "v=0\r\na=mid:0\r\n");
        assert_eq!(accepted.rtp_maps.len(), 1);
        assert_eq!(registry.peer_count(room), 1);
        peer_task.await.expect("peer task");
    }

    #[tokio::test]
    async fn reoffer_on_a_full_command_queue_errors_instead_of_forking_a_new_session() {
        let registry = default_registry();
        let room = RoomId::new();
        let participant = ParticipantId::new();
        let (peer, _rx) = handle(participant);
        for _ in 0..8 {
            peer.tx.try_send(PeerCommand::SetSubscription { audio: true, video: true }).expect("fill queue");
        }
        registry.rooms.lock().unwrap().entry(room).or_default().insert(participant, peer);
        // Re-offer must not silently create a second DTLS/ICE session.
        let error = registry.accept_offer(room, participant, "v=0\r\n").await.expect_err("queue is full");
        assert!(error.to_string().contains("queue is full"), "{error}");
        assert_eq!(registry.peer_count(room), 1, "old session must survive a queue-full re-offer");
        assert!(registry.has_peer(room, participant), "registration must stay");
    }

    // ---- T11b review B fix-ups ----------------------------------------------

    /// P2-1/P3-4 regression: the task-exit path (ICE/DTLS death, not a
    /// signalling `remove`) must retire the source in every survivor, clear the
    /// PLI coalescing window and re-arm the per-publisher keyframe gate.
    #[tokio::test]
    async fn a_dead_peer_task_retires_the_source_and_rearms_survivor_gates() {
        let registry = default_registry();
        let room = RoomId::new();
        let publisher = ParticipantId::new();
        let (publisher_handle, _publisher_rx) = handle_with(publisher, 7);
        let (survivor, mut survivor_rx) = handle_with(ParticipantId::new(), 8);
        let survivor_participant = survivor.participant;
        {
            let mut rooms = registry.rooms.lock().unwrap();
            let peers = rooms.entry(room).or_default();
            peers.insert(publisher, publisher_handle);
            peers.insert(survivor_participant, survivor.clone());
        }
        registry.peer_total.store(2, Ordering::Relaxed);
        // The subscriber already forwarded this publisher's IDR and a
        // coalescing window is open for it.
        assert!(survivor.admits_video(publisher, true));
        registry.last_keyframe_request.lock().unwrap().insert((room, publisher, false), Instant::now());

        registry.remove_generation(room, publisher, 7);

        assert!(!survivor.admits_video(publisher, false), "task exit must close the subscriber's gate again");
        assert!(!registry.has_peer(room, publisher));
        assert_eq!(registry.peer_total.load(Ordering::Relaxed), 1);
        assert!(
            registry.last_keyframe_request.lock().unwrap().get(&(room, publisher, false)).is_none(),
            "a stale PLI window must not suppress the returning publisher's first keyframe"
        );
        match survivor_rx.try_recv() {
            Ok(PeerCommand::DropSource { from }) => assert_eq!(from, publisher),
            _ => panic!("survivors must be told to retire the dead publisher's source"),
        }
    }

    /// P2-1 reviewer probe, end-to-end through the real `run_peer` command
    /// loop: a `DropSource` must reset the gate shared with the `PeerHandle`.
    #[tokio::test]
    async fn drop_source_command_closes_the_subscriber_gate_in_the_peer_task() {
        let registry = default_registry();
        let room = RoomId::new();
        let subscriber = ParticipantId::new();
        let publisher = ParticipantId::new();
        let (handle, rx) = handle(subscriber);
        let video_ready = Arc::clone(&handle.video_ready);
        let std_socket = std::net::UdpSocket::bind(("127.0.0.1", 0)).expect("bind ephemeral socket");
        std_socket.set_nonblocking(true).expect("nonblocking");
        let socket = UdpSocket::from_std(std_socket).expect("tokio socket");
        let local_addr = socket.local_addr().expect("local addr");
        let peer = WebRtcPeer::accept_offer(&browser_offer(1, 1), local_addr, &WebRtcConfig::default())
            .expect("accept a real 1+1 offer");
        // The subscriber had already shown this publisher's IDR.
        assert!(handle.admits_video(publisher, true));
        let task = tokio::spawn(run_peer(
            Arc::clone(&registry),
            room,
            subscriber,
            1,
            socket,
            peer,
            rx,
            video_ready,
            Arc::new(AtomicU64::new(0)),
        ));
        handle.tx.try_send(PeerCommand::DropSource { from: publisher }).expect("queue DropSource");
        let mut closed = false;
        for _ in 0..200 {
            if !handle.admits_video(publisher, false) {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(closed, "DropSource must close the subscriber's per-publisher video gate");
        // Closing the handle closes the command channel; the task must exit.
        drop(handle);
        tokio::time::timeout(Duration::from_secs(5), task).await.expect("peer task exits").expect("peer task");
    }

    /// P3-7: parse-succeeding caps that cannot be honoured must be warned
    /// about at startup (mid ceiling, ports vs process cap, absurd values).
    #[test]
    fn feasibility_warnings_cover_mid_ceiling_ports_and_huge_caps() {
        let huge = RoomLimits { max_peers_per_room: 1_000_000, max_peers: 1_000_000 };
        let warnings = huge.feasibility_warnings(Some((43_000, 43_100)));
        assert!(warnings.iter().any(|warning| warning.contains("mid ceiling")), "{warnings:?}");
        assert!(warnings.iter().any(|warning| warning.contains("UDP range")), "{warnings:?}");
        assert!(warnings.iter().any(|warning| warning.contains("implausibly large")), "{warnings:?}");
        let inverted = RoomLimits { max_peers_per_room: 16, max_peers: 8 };
        let inverted_warnings = inverted.feasibility_warnings(None);
        assert!(
            inverted_warnings.iter().any(|warning| warning.contains("can never be reached")),
            "{inverted_warnings:?}"
        );
        let sane = RoomLimits { max_peers_per_room: 8, max_peers: 101 };
        assert!(sane.feasibility_warnings(Some((43_000, 43_100))).is_empty(), "no warning for a feasible config");
    }
}

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

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use vks_common::metrics::{MEDIA_BYTES, WEBRTC_FRAMES_DROPPED, WEBRTC_ICE_FAILURES, WEBRTC_OFFERS, WEBRTC_PEERS};
use vks_core::{ParticipantId, RoomId};

use crate::webrtc::{WebRtcAction, WebRtcConfig, WebRtcError, WebRtcPeer};
use str0m::format::PayloadParams;
use str0m::media::MediaTime;

const PEER_COMMAND_QUEUE: usize = 64;
const MAX_DATAGRAM: usize = 4096;
/// The frame fanout currently maps one audio and one video source per
/// subscriber. More publishers would interleave unrelated RTP clock domains
/// into the same egress mid; reject the third peer instead of garbling media.
pub const MAX_PEERS_PER_ROOM: usize = 2;
const CONNECT_DEADLINE: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// One browser in one room.
#[derive(Debug, Clone)]
pub struct PeerHandle {
    pub participant: ParticipantId,
    tx: mpsc::Sender<PeerCommand>,
    audio: Arc<AtomicBool>,
    video: Arc<AtomicBool>,
    /// Operator/client cap for BWE, 0 = unlimited.
    max_bitrate_bps: Arc<AtomicU64>,
    /// A new/changed video subscription cannot decode until the next IDR:
    /// delta frames are dropped and the publisher is asked for a keyframe.
    waiting_keyframe: Arc<AtomicBool>,
    publishes_video: bool,
    generation: u64,
}

impl PeerHandle {
    fn wants(&self, audio: bool) -> bool {
        (if audio { &self.audio } else { &self.video }).load(Ordering::Relaxed)
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
    publishes_video: bool,
}

enum PeerCommand {
    Reoffer {
        offer_sdp: String,
        reply: tokio::sync::oneshot::Sender<Result<ReofferAnswer, String>>,
    },
    Write {
        from: ParticipantId,
        audio: bool,
        is_keyframe: bool,
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
    /// Process-wide cap, separate from the per-room cap; prevents valid JWTs
    /// from exhausting FDs/UDP ports.
    peer_total: AtomicUsize,
    max_peers: usize,
}

impl std::fmt::Debug for WebRtcRoomRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let rooms = self.rooms.lock().map(|r| r.len()).unwrap_or(0);
        f.debug_struct("WebRtcRoomRegistry")
            .field("advertise_ip", &self.advertise_ip)
            .field("rooms", &rooms)
            .finish_non_exhaustive()
    }
}

impl WebRtcRoomRegistry {
    /// Creates a registry bound to the current Tokio runtime.
    pub fn new(config: WebRtcConfig, advertise_ip: IpAddr) -> Arc<Self> {
        let port_range = match (
            std::env::var("VKS_WEBRTC_PORT_MIN").ok().and_then(|v| v.parse::<u16>().ok()),
            std::env::var("VKS_WEBRTC_PORT_MAX").ok().and_then(|v| v.parse::<u16>().ok()),
        ) {
            (Some(min), Some(max)) if min > 0 && max >= min => Some((min, max)),
            _ => None,
        };
        Arc::new(Self {
            config,
            advertise_ip,
            rooms: Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
            next_generation: AtomicU64::new(1),
            port_range,
            subscriptions: Mutex::new(HashMap::new()),
            bitrate_caps: Mutex::new(HashMap::new()),
            peer_total: AtomicUsize::new(0),
            max_peers: std::env::var("VKS_WEBRTC_MAX_PEERS")
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|value| *value > 0)
                .unwrap_or(256),
        })
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
                    Ok(socket) => return Ok(socket),
                    Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => continue,
                    Err(error) => return Err(error.into()),
                }
            }
            WEBRTC_OFFERS.with_label_values(&["port_range_exhausted"]).inc();
            tracing::warn!(min, max, "WebRTC port range exhausted; refusing peer");
            return Err(WebRtcError::Net(format!("WebRTC port range {min}-{max} exhausted")));
        }
        Ok(std::net::UdpSocket::bind((bind_ip, 0))?)
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
                    // The participant may have started publishing video in
                    // this negotiation; keep the handle's view current.
                    if reoffer.publishes_video && !handle.publishes_video {
                        if let Some(peers) =
                            self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get_mut(&room)
                        {
                            if let Some(current) = peers.get_mut(&participant) {
                                current.publishes_video = true;
                            }
                        }
                    }
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
                if peers.len() >= MAX_PEERS_PER_ROOM && !existing {
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
        let publishes_video = offer_publishes_video(offer_sdp);
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
        tracing::info!(?room, ?participant, audio, video, publishes_video, "WebRTC peer subscription at offer");
        let (tx, rx) = mpsc::channel(PEER_COMMAND_QUEUE);
        let max_bitrate_bps = Arc::new(AtomicU64::new(max_bitrate_bps));
        let waiting_keyframe = Arc::new(AtomicBool::new(video));
        let handle = PeerHandle {
            participant,
            tx,
            audio: Arc::new(AtomicBool::new(audio)),
            video: Arc::new(AtomicBool::new(video)),
            max_bitrate_bps: Arc::clone(&max_bitrate_bps),
            waiting_keyframe: Arc::clone(&waiting_keyframe),
            publishes_video,
            generation,
        };
        {
            let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let peers = rooms.entry(room).or_default();
            let replacing = peers.contains_key(&participant);
            if peers.len() >= MAX_PEERS_PER_ROOM && !replacing {
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
        self.runtime.spawn(run_peer(
            registry,
            room,
            participant,
            generation,
            socket,
            peer,
            rx,
            max_bitrate_bps,
            waiting_keyframe,
        ));
        Ok(AcceptedPeer { answer_sdp, rtp_maps, handle, reoffered: false })
    }

    /// Removes a peer and closes its command channel; the task exits after
    /// flushing its last datagrams and dropping its `Rtc` (DTLS close).
    pub fn remove(&self, room: RoomId, participant: ParticipantId) {
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(room, participant));
        self.bitrate_caps.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(room, participant));
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(peers) = rooms.get_mut(&room) {
            if peers.remove(&participant).is_some() {
                self.peer_total.fetch_sub(1, Ordering::Relaxed);
            }
            if peers.is_empty() {
                rooms.remove(&room);
            }
            WEBRTC_PEERS.set(rooms.values().map(HashMap::len).sum::<usize>() as i64);
        }
    }

    /// Exits a peer task without deleting a newer generation of the same
    /// participant (re-offer/ICE restart race).
    fn remove_generation(&self, room: RoomId, participant: ParticipantId, generation: u64) {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut removed = false;
        if let Some(peers) = rooms.get_mut(&room) {
            if peers.get(&participant).is_some_and(|handle| handle.generation == generation) {
                peers.remove(&participant);
                self.peer_total.fetch_sub(1, Ordering::Relaxed);
                removed = true;
            }
            if peers.is_empty() {
                rooms.remove(&room);
            }
        }
        if removed {
            WEBRTC_PEERS.set(rooms.values().map(HashMap::len).sum::<usize>() as i64);
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
            // ahead of the SetSubscription command.
            handle.waiting_keyframe.store(true, Ordering::Relaxed);
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
        rooms
            .get(&room)
            .map(|peers| {
                peers
                    .iter()
                    .filter(|(pid, handle)| **pid != from && handle.wants(audio))
                    .map(|(_, h)| h.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Whether a participant has a live browser transport.
    pub fn has_peer(&self, room: RoomId, participant: ParticipantId) -> bool {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&room)
            .is_some_and(|peers| peers.contains_key(&participant))
    }

    fn request_keyframe_from(&self, room: RoomId, publisher: ParticipantId, audio: bool) {
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
        let audio = data.params.spec().codec.is_audio();
        let targets = self.targets_for(room, from, audio);
        if targets.is_empty() {
            return;
        }
        for target in targets {
            // A slow browser must never stall the publisher or other peers.
            if target
                .tx
                .try_send(PeerCommand::Write {
                    from,
                    audio,
                    is_keyframe: data.is_keyframe(),
                    params: data.params,
                    wallclock: data.network_time,
                    rtp_time: data.time,
                    data: Arc::clone(&data.data),
                })
                .is_err()
            {
                WEBRTC_FRAMES_DROPPED.with_label_values(&["queue_full"]).inc();
                tracing::debug!(participant = ?target.participant, "webrtc fanout queue full; dropping frame");
                // The next delivered frame may be a delta after a gap: wait
                // for a fresh keyframe instead of corrupting the decoder.
                if !audio {
                    target.waiting_keyframe.store(true, Ordering::Relaxed);
                }
                // Ask the publisher for a fresh keyframe so the slow
                // subscriber can resynchronise after its queue drains.
                self.request_keyframe_from(room, from, audio);
            } else {
                MEDIA_BYTES
                    .with_label_values(&[if audio { "opus" } else { "vp8" }, "downlink"])
                    .inc_by(data.data.len() as u64);
            }
        }
    }
}

/// Keyframe gate: a subscriber that is still waiting for an IDR must not
/// receive delta frames (they would corrupt its decoder). The gate opens on
/// the first keyframe and then lets everything through.
fn gate_video(waiting: &AtomicBool, is_keyframe: bool) -> bool {
    if !waiting.load(Ordering::Relaxed) {
        return true;
    }
    if is_keyframe {
        waiting.store(false, Ordering::Relaxed);
        true
    } else {
        false
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
    max_bitrate_bps: Arc<AtomicU64>,
    waiting_keyframe: Arc<AtomicBool>,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let created_at = Instant::now();
    let mut last_seen = created_at;
    let mut last_keyframe_request = Instant::now() - Duration::from_secs(1);
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
                    PeerCommand::Write { from, audio, is_keyframe, params, wallclock, rtp_time, data } => {
                        if !audio && !gate_video(&waiting_keyframe, is_keyframe) {
                            WEBRTC_FRAMES_DROPPED.with_label_values(&["awaiting_keyframe"]).inc();
                            // Ask the publisher for an IDR, but not on every
                            // dropped frame (PLI storms are worse than waiting).
                            if last_keyframe_request.elapsed() >= Duration::from_millis(500) {
                                last_keyframe_request = Instant::now();
                                registry.request_keyframe_from(room, from, false);
                            }
                            continue;
                        }
                        let mid = peer.mid_for_kind(audio);
                        match mid {
                            Some(mid) => peer.write_media_matched(mid, params, wallclock, rtp_time, data),
                            None => Ok(Vec::new()),
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
                                    publishes_video: offer_publishes_video(&offer_sdp),
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
                        let result = match peer.mid_for_kind(audio) {
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
                        if video {
                            // Re-subscribing restarts the decoder state: wait
                            // for a fresh keyframe instead of forwarding deltas.
                            waiting_keyframe.store(true, Ordering::Relaxed);
                        }
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
                if let Some(mid) = peer.mid_for_kind(false) {
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
            WebRtcAction::KeyframeRequest { audio } => {
                registry.request_keyframe(room, participant, audio);
            }
            WebRtcAction::Closed => return false,
        }
    }
    true
}

/// Whether the browser offer publishes video (a video m-line that is not
/// `a=recvonly`). Used to avoid requesting keyframes from view-only peers.
fn offer_publishes_video(offer: &str) -> bool {
    let mut video_seen = false;
    let mut video_recvonly = false;
    for line in offer.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("m=") {
            if rest.starts_with("video ") {
                video_seen = true;
                video_recvonly = false;
            } else if video_seen {
                // The previous video section ended; classify it now.
                break;
            }
            continue;
        }
        if !video_seen {
            continue;
        }
        if line.starts_with("a=recvonly") || line.starts_with("a=inactive") {
            video_recvonly = true;
        }
    }
    // No explicit direction on the video m-line defaults to sendrecv.
    video_seen && !video_recvonly
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle(participant: ParticipantId) -> (PeerHandle, mpsc::Receiver<PeerCommand>) {
        let (tx, rx) = mpsc::channel(8);
        (
            PeerHandle {
                participant,
                tx,
                audio: Arc::new(AtomicBool::new(true)),
                video: Arc::new(AtomicBool::new(true)),
                max_bitrate_bps: Arc::new(AtomicU64::new(0)),
                waiting_keyframe: Arc::new(AtomicBool::new(true)),
                publishes_video: true,
                generation: 1,
            },
            rx,
        )
    }

    #[tokio::test]
    async fn fanout_respects_participant_exclusion_and_audio_only() {
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
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
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
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
    async fn third_real_peer_is_rejected_before_binding() {
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
        let room = RoomId::new();
        {
            let mut rooms = registry.rooms.lock().unwrap();
            let peers = rooms.entry(room).or_default();
            for _ in 0..MAX_PEERS_PER_ROOM {
                let pid = ParticipantId::new();
                peers.insert(pid, handle(pid).0);
            }
        }
        let error = registry
            .accept_offer(room, ParticipantId::new(), "v=0\r\n")
            .await
            .expect_err("third peer must be rejected");
        assert!(matches!(error, WebRtcError::RoomFull));
        assert_eq!(registry.peer_count(room), MAX_PEERS_PER_ROOM);
    }

    #[tokio::test]
    async fn global_cap_rejects_first_peer_in_a_fresh_room() {
        let mut registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
        Arc::get_mut(&mut registry).expect("unique registry").max_peers = 0;
        let error =
            registry.accept_offer(RoomId::new(), ParticipantId::new(), "v=0\r\n").await.expect_err("global cap");
        assert!(matches!(error, WebRtcError::CapacityExceeded), "unexpected: {error:?}");
        assert_eq!(registry.peer_total.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn subscription_and_bitrate_caps_are_remembered_before_offer() {
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
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
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
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
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
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
        let registry = WebRtcRoomRegistry::new(WebRtcConfig::default(), "127.0.0.1".parse().unwrap());
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

    #[test]
    fn keyframe_gate_blocks_deltas_until_the_first_idr() {
        let waiting = AtomicBool::new(true);
        assert!(!gate_video(&waiting, false), "delta before IDR is dropped");
        assert!(waiting.load(Ordering::Relaxed));
        assert!(gate_video(&waiting, true), "IDR opens the gate");
        assert!(!waiting.load(Ordering::Relaxed));
        assert!(gate_video(&waiting, false), "deltas flow after the IDR");

        // Re-subscription/queue overflow closes the gate again.
        waiting.store(true, Ordering::Relaxed);
        assert!(!gate_video(&waiting, false));
        assert!(gate_video(&waiting, true));
    }
}

//! Room-scoped real WebRTC peers: per-peer UDP socket, timer loop and SFU fanout.
//!
//! Each peer gets an ephemeral UDP socket (ICE-lite host candidate) and a
//! dedicated Tokio task that is the only owner of its [`WebRtcPeer`]. Signalling
//! calls [`WebRtcRoomRegistry::accept_offer`] synchronously and sends back the
//! real str0m answer; media then flows through SRTP, is depayloaded to frames,
//! and is re-encrypted separately for every receiving browser.
//!
//! This is intentionally a small SFU fanout, not the legacy plaintext
//! [`crate::SfuEngine`] path: SRTP keys, RTP sequence numbers, NACK/RTX/FEC and
//! BWE are per-peer state and cannot be shared through the plaintext forwarder.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use vks_common::metrics::{WEBRTC_FRAMES_DROPPED, WEBRTC_ICE_FAILURES, WEBRTC_OFFERS, WEBRTC_PEERS};
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
}

#[derive(Debug)]
enum PeerCommand {
    Write { audio: bool, params: PayloadParams, wallclock: Instant, rtp_time: MediaTime, data: Arc<[u8]> },
    RequestKeyframe { audio: bool },
    SetSubscription { audio: bool, video: bool },
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
            tracing::warn!(min, max, "WebRTC port range exhausted; refusing peer");
            return Err(WebRtcError::Net(format!("WebRTC port range {min}-{max} exhausted")));
        }
        Ok(std::net::UdpSocket::bind((bind_ip, 0))?)
    }

    /// Accepts one browser offer, binds the peer's UDP socket and starts its loop.
    ///
    /// The answer advertises `advertise_ip` with the ephemeral port that was
    /// actually bound, so ICE host candidates always match the socket.
    pub fn accept_offer(
        self: &Arc<Self>,
        room: RoomId,
        participant: ParticipantId,
        offer_sdp: &str,
    ) -> Result<AcceptedPeer, WebRtcError> {
        // Reserve capacity before doing any expensive binding/crypto work.
        {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(peers) = rooms.get(&room) {
                if peers.len() >= MAX_PEERS_PER_ROOM && !peers.contains_key(&participant) {
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
        WEBRTC_OFFERS.with_label_values(&["accepted"]).inc();
        let answer_sdp = peer.answer_sdp().to_string();
        let rtp_maps = peer.rtp_maps();
        let socket = UdpSocket::from_std(std_socket)?;

        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let (audio, video) = self
            .subscriptions
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&(room, participant))
            .copied()
            .unwrap_or((false, false));
        let (tx, rx) = mpsc::channel(PEER_COMMAND_QUEUE);
        let handle = PeerHandle {
            participant,
            tx,
            audio: Arc::new(AtomicBool::new(audio)),
            video: Arc::new(AtomicBool::new(video)),
            generation,
        };
        {
            let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let peers = rooms.entry(room).or_default();
            if peers.len() >= MAX_PEERS_PER_ROOM && !peers.contains_key(&participant) {
                WEBRTC_OFFERS.with_label_values(&["room_full"]).inc();
                return Err(WebRtcError::RoomFull);
            }
            // Re-offer: replace the old generation; its task exits on channel
            // close and its stale `remove_generation` becomes a no-op.
            peers.insert(participant, handle.clone());
            WEBRTC_PEERS.set(rooms.values().map(HashMap::len).sum::<usize>() as i64);
        }

        // A new subscriber cannot decode video until the publisher sends an
        // IDR; ask every already-connected peer for a fresh keyframe.
        let existing: Vec<PeerHandle> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .get(&room)
                .map(|peers| peers.iter().filter(|(pid, _)| **pid != participant).map(|(_, h)| h.clone()).collect())
                .unwrap_or_default()
        };
        for handle in existing {
            let _ = handle.tx.try_send(PeerCommand::RequestKeyframe { audio: false });
        }

        let registry = Arc::clone(self);
        self.runtime.spawn(run_peer(registry, room, participant, generation, socket, peer, rx));
        Ok(AcceptedPeer { answer_sdp, rtp_maps, handle })
    }

    /// Removes a peer and closes its command channel; the task exits after
    /// flushing its last datagrams and dropping its `Rtc` (DTLS close).
    pub fn remove(&self, room: RoomId, participant: ParticipantId) {
        self.subscriptions.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&(room, participant));
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(peers) = rooms.get_mut(&room) {
            peers.remove(&participant);
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
        let _ = handle.tx.try_send(PeerCommand::SetSubscription { audio, video });
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
            let _ = handle.tx.try_send(PeerCommand::RequestKeyframe { audio });
        }
    }

    fn request_keyframe(&self, room: RoomId, from: ParticipantId, audio: bool) {
        let targets: Vec<PeerHandle> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .get(&room)
                .map(|peers| peers.iter().filter(|(pid, _)| **pid != from).map(|(_, h)| h.clone()).collect())
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
                    audio,
                    params: data.params,
                    wallclock: data.network_time,
                    rtp_time: data.time,
                    data: Arc::clone(&data.data),
                })
                .is_err()
            {
                WEBRTC_FRAMES_DROPPED.with_label_values(&["queue_full"]).inc();
                tracing::debug!(participant = ?target.participant, "webrtc fanout queue full; dropping frame");
                // Ask the publisher for a fresh keyframe so the slow
                // subscriber can resynchronise after its queue drains.
                self.request_keyframe_from(room, from, audio);
            }
        }
    }
}

async fn run_peer(
    registry: Arc<WebRtcRoomRegistry>,
    room: RoomId,
    participant: ParticipantId,
    generation: u64,
    socket: UdpSocket,
    mut peer: WebRtcPeer,
    mut rx: mpsc::Receiver<PeerCommand>,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    let created_at = Instant::now();
    let mut last_seen = created_at;
    loop {
        if !peer.is_connected() && created_at.elapsed() > CONNECT_DEADLINE {
            tracing::warn!(?participant, "WebRTC peer did not connect before deadline");
            WEBRTC_ICE_FAILURES.with_label_values(&["connect_deadline"]).inc();
            break;
        }
        if last_seen.elapsed() > IDLE_TIMEOUT {
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
                if !dispatch(&registry, room, participant, &socket, &mut peer, actions).await {
                    break;
                }
            }
            command = rx.recv() => {
                let Some(command) = command else { break };
                let now = Instant::now();
                let actions = match command {
                    PeerCommand::Write { audio, params, wallclock, rtp_time, data } => {
                        let mid = peer.mid_for_kind(audio);
                        match mid {
                            Some(mid) => peer.write_media_matched(mid, params, wallclock, rtp_time, data),
                            None => Ok(Vec::new()),
                        }
                    }
                    PeerCommand::RequestKeyframe { audio } => match peer.mid_for_kind(audio) {
                        Some(mid) => peer.request_keyframe(mid),
                        None => Ok(Vec::new()),
                    },
                    PeerCommand::SetSubscription { audio, video } => {
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
                if !dispatch(&registry, room, participant, &socket, &mut peer, actions).await {
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
                if !dispatch(&registry, room, participant, &socket, &mut peer, actions).await {
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
    mut actions: Vec<WebRtcAction>,
) -> bool {
    // Apply local-only actions before fanout. BWE must actually steer the
    // pacer; a connected peer must immediately ask the publisher for an IDR.
    let mut extra = Vec::new();
    for action in &actions {
        match action {
            WebRtcAction::Bitrate(bps) => peer.set_desired_bitrate(*bps),
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
        let error =
            registry.accept_offer(room, ParticipantId::new(), "v=0\r\n").expect_err("third peer must be rejected");
        assert!(matches!(error, WebRtcError::RoomFull));
        assert_eq!(registry.peer_count(room), MAX_PEERS_PER_ROOM);
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
}

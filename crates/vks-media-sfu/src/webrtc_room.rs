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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use vks_core::{ParticipantId, RoomId};

use crate::webrtc::{WebRtcAction, WebRtcConfig, WebRtcError, WebRtcPeer};
use str0m::format::PayloadParams;
use str0m::media::MediaTime;

const PEER_COMMAND_QUEUE: usize = 64;
const MAX_DATAGRAM: usize = 2048;

/// One browser in one room.
#[derive(Debug, Clone)]
pub struct PeerHandle {
    pub participant: ParticipantId,
    tx: mpsc::Sender<PeerCommand>,
}

/// A peer was accepted; answer SDP and handle for later removal.
#[derive(Debug)]
pub struct AcceptedPeer {
    pub answer_sdp: String,
    pub handle: PeerHandle,
}

#[derive(Debug)]
enum PeerCommand {
    Write { audio: bool, params: PayloadParams, wallclock: Instant, rtp_time: MediaTime, data: Arc<[u8]> },
    RequestKeyframe { audio: bool },
}

/// Registry of real WebRTC peers, keyed by room and participant.
pub struct WebRtcRoomRegistry {
    config: WebRtcConfig,
    advertise_ip: IpAddr,
    rooms: Mutex<HashMap<RoomId, HashMap<ParticipantId, PeerHandle>>>,
    runtime: tokio::runtime::Handle,
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
        Arc::new(Self {
            config,
            advertise_ip,
            rooms: Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::current(),
        })
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
        let std_socket = std::net::UdpSocket::bind(("0.0.0.0", 0))?;
        std_socket.set_nonblocking(true)?;
        let port = std_socket.local_addr()?.port();
        let local_addr = SocketAddr::new(self.advertise_ip, port);
        let peer = WebRtcPeer::accept_offer(offer_sdp, local_addr, &self.config)?;
        let answer_sdp = peer.answer_sdp().to_string();
        let socket = UdpSocket::from_std(std_socket)?;

        let (tx, rx) = mpsc::channel(PEER_COMMAND_QUEUE);
        let handle = PeerHandle { participant, tx };
        {
            let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms.entry(room).or_default().insert(participant, handle.clone());
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
        self.runtime.spawn(run_peer(registry, room, participant, socket, peer, rx));
        Ok(AcceptedPeer { answer_sdp, handle })
    }

    /// Removes a peer and closes its command channel; the task exits after
    /// flushing its last datagrams and dropping its `Rtc` (DTLS close).
    pub fn remove(&self, room: RoomId, participant: ParticipantId) {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(peers) = rooms.get_mut(&room) {
            peers.remove(&participant);
            if peers.is_empty() {
                rooms.remove(&room);
            }
        }
    }

    /// Number of registered browser peers in a room (tests/observability).
    pub fn peer_count(&self, room: RoomId) -> usize {
        self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&room).map_or(0, HashMap::len)
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
        let targets: Vec<PeerHandle> = {
            let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            rooms
                .get(&room)
                .map(|peers| peers.iter().filter(|(pid, _)| **pid != from).map(|(_, h)| h.clone()).collect())
                .unwrap_or_default()
        };
        if targets.is_empty() {
            return;
        }
        let audio = data.params.spec().codec.is_audio();
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
                tracing::debug!(participant = ?target.participant, "webrtc fanout queue full; dropping frame");
            }
        }
    }
}

async fn run_peer(
    registry: Arc<WebRtcRoomRegistry>,
    room: RoomId,
    participant: ParticipantId,
    socket: UdpSocket,
    mut peer: WebRtcPeer,
    mut rx: mpsc::Receiver<PeerCommand>,
) {
    let mut buf = vec![0u8; MAX_DATAGRAM];
    loop {
        let deadline = peer.next_timeout();
        let sleep = async move {
            match deadline {
                Some(at) => {
                    let at = tokio::time::Instant::from_std(at);
                    let now = tokio::time::Instant::now();
                    if at > now {
                        tokio::time::sleep_until(at).await;
                    } else {
                        tokio::time::sleep(Duration::from_millis(1)).await;
                    }
                }
                None => tokio::time::sleep(Duration::from_millis(10)).await,
            }
        };
        tokio::select! {
            received = socket.recv_from(&mut buf) => {
                let Ok((len, source)) = received else { break };
                let destination = socket.local_addr().unwrap_or(SocketAddr::new(registry.advertise_ip, 0));
                let actions = match peer.handle_receive(Instant::now(), source, destination, &buf[..len]) {
                    Ok(actions) => actions,
                    Err(error) => {
                        tracing::debug!(?error, "webrtc datagram rejected");
                        continue;
                    }
                };
                if !dispatch(&registry, room, participant, &socket, actions).await {
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
                };
                let actions = match actions {
                    Ok(actions) => actions,
                    Err(error) => {
                        tracing::debug!(?error, "webrtc write failed");
                        continue;
                    }
                };
                let actions = match peer.handle_timeout(now) {
                    Ok(mut tail) => {
                        let mut actions = actions;
                        actions.append(&mut tail);
                        actions
                    }
                    Err(error) => {
                        tracing::debug!(?error, "webrtc timer failed");
                        actions
                    }
                };
                if !dispatch(&registry, room, participant, &socket, actions).await {
                    break;
                }
            }
            _ = sleep => {
                let actions = match peer.handle_timeout(Instant::now()) {
                    Ok(actions) => actions,
                    Err(error) => {
                        tracing::debug!(?error, "webrtc timer failed");
                        continue;
                    }
                };
                if !dispatch(&registry, room, participant, &socket, actions).await {
                    break;
                }
            }
        }
    }

    registry.remove(room, participant);
    tracing::debug!(?participant, "webrtc peer task stopped");
}

async fn dispatch(
    registry: &Arc<WebRtcRoomRegistry>,
    room: RoomId,
    participant: ParticipantId,
    socket: &UdpSocket,
    actions: Vec<WebRtcAction>,
) -> bool {
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
            WebRtcAction::Bitrate(bps) => {
                tracing::trace!(bps, ?participant, "webrtc BWE estimate");
            }
            WebRtcAction::KeyframeRequest { audio } => {
                registry.request_keyframe(room, participant, audio);
            }
            WebRtcAction::Closed => return false,
            WebRtcAction::Connected | WebRtcAction::IceState(_) => {}
        }
    }
    true
}

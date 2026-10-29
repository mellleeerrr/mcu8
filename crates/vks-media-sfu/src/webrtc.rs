//! Real WebRTC transport on top of `str0m` (feature `str0m`).
//!
//! This is the bridge between VKS signalling/SFU and a browser peer:
//! ICE (host candidates, ICE-lite on the server), DTLS, SRTP/SRTCP,
//! NACK/RTX, jitter buffering, TWCC/REMB bandwidth estimation and
//! RTP/RTCP multiplexing are all implemented by `str0m`.
//!
//! The wrapper is deliberately sans-I/O: the caller owns the UDP socket,
//! feeds datagrams in, and sends every [`WebRtcAction::Send`] out. That keeps
//! the transport testable without a browser and lets the per-peer room task
//! own the socket lifecycle.
//!
//! Mutations follow str0m's single-mutation invariant: every public method
//! that mutates the inner [`Rtc`] drains `poll_output()` before returning.

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use str0m::change::SdpOffer;
use str0m::format::{Codec, PayloadParams};
use str0m::media::{KeyframeRequestKind, MediaData, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig, RtcError};
use vks_core::ParticipantId;

/// Upper bound on audio m-lines in one browser offer (T11b).
///
/// The SFU has no audio mixer: every remote publisher needs its own audio
/// m-line, so a room of `N` peers needs `1 + (N-1)` audio mids. Eight matches
/// the default per-room peer cap of eight.
pub const MAX_AUDIO_MIDS: usize = 8;

/// Upper bound on video m-lines in one browser offer (T11b).
///
/// Same shape as [`MAX_AUDIO_MIDS`]: one sendrecv (the browser camera) plus
/// `N-1` recvonly pre-offered egress slots. The limit is required: without it a
/// 500-m-line offer (which still fits the 64 KiB SDP cap) is cheap state churn.
pub const MAX_VIDEO_MIDS: usize = 8;

/// Reserved source id for the server-side mixed-room bridge egress (T12).
///
/// The bridge audio stream is a source like any publisher — it needs its own
/// egress mid and RTP clock domain (one mid never carries two clock domains,
/// T11b) — but it is not a room participant. A fixed UUID makes the id stable
/// across bridge restarts (the same mid/clock domain keeps being reused) while
/// staying outside the UUIDv4 space real participants are drawn from.
pub fn bridge_source() -> ParticipantId {
    static SOURCE: OnceLock<ParticipantId> = OnceLock::new();
    *SOURCE.get_or_init(|| ParticipantId::from_uuid(uuid::Uuid::from_u128(0x6272_6964_6765_4000_8000_0000_0000_0001)))
}

/// Direction of one negotiated m-line from the SFU's point of view.
///
/// Taken from the SDP answer and refreshed from str0m's `MediaAdded` /
/// `MediaChanged` events (the browser's `sendrecv` is the SFU's `sendrecv`,
/// its `recvonly` becomes our `sendonly`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MidDirection {
    /// SFU only sends (the browser offered `recvonly`: an egress slot).
    SendOnly,
    /// SFU only receives (the browser offered `sendonly`).
    RecvOnly,
    /// Both directions (a legacy 1+1 browser's own publish m-line).
    SendRecv,
    /// Rejected (`m=… 0`) or explicitly `a=inactive`: never usable.
    Inactive,
}

impl MidDirection {
    /// Whether the SFU may write media on this m-line.
    pub fn can_send(self) -> bool {
        matches!(self, Self::SendOnly | Self::SendRecv)
    }

    /// Whether the browser may publish on this m-line.
    pub fn can_recv(self) -> bool {
        matches!(self, Self::RecvOnly | Self::SendRecv)
    }

    fn from_sdp_line(line: &str) -> Option<Self> {
        match line.trim() {
            "a=sendonly" => Some(Self::SendOnly),
            "a=recvonly" => Some(Self::RecvOnly),
            "a=sendrecv" => Some(Self::SendRecv),
            "a=inactive" => Some(Self::Inactive),
            _ => None,
        }
    }
}

impl From<str0m::media::Direction> for MidDirection {
    fn from(direction: str0m::media::Direction) -> Self {
        match direction {
            str0m::media::Direction::SendOnly => Self::SendOnly,
            str0m::media::Direction::RecvOnly => Self::RecvOnly,
            str0m::media::Direction::SendRecv => Self::SendRecv,
            str0m::media::Direction::Inactive => Self::Inactive,
        }
    }
}

/// One negotiated m-line: mid, media kind and the SFU-local direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MidSection {
    pub mid: Mid,
    /// `true` for `m=audio`.
    pub audio: bool,
    pub direction: MidDirection,
}

/// Parses `a=mid`, direction and port-rejection state out of an SDP answer.
///
/// A rejected m-line (`m=video 0 …`) is `Inactive` regardless of its direction
/// attribute: str0m returns no writer for it, so it must never enter a pool.
fn parse_sections(answer_sdp: &str) -> Vec<MidSection> {
    let mut sections: Vec<MidSection> = Vec::new();
    let mut current_audio: Option<bool> = None;
    let mut current_port_zero = false;
    let mut current_direction: Option<MidDirection> = None;
    let mut current_index: Option<usize> = None;
    for line in answer_sdp.lines().map(|line| line.trim_end_matches('\r').trim()) {
        if let Some(rest) = line.strip_prefix("m=") {
            current_audio = if rest.starts_with("audio ") {
                Some(true)
            } else if rest.starts_with("video ") {
                Some(false)
            } else {
                None
            };
            current_port_zero = rest.split_whitespace().nth(1).and_then(|port| port.parse::<u16>().ok()) == Some(0);
            current_direction = None;
            current_index = None;
            continue;
        }
        if let Some(direction) = MidDirection::from_sdp_line(line) {
            if current_audio.is_some() {
                current_direction = Some(direction);
                if let Some(index) = current_index {
                    sections[index].direction = if current_port_zero { MidDirection::Inactive } else { direction };
                }
            }
            continue;
        }
        if let Some(mid) = line.strip_prefix("a=mid:") {
            if let Some(audio) = current_audio {
                let direction = if current_port_zero {
                    MidDirection::Inactive
                } else {
                    current_direction.unwrap_or(MidDirection::SendRecv)
                };
                sections.push(MidSection { mid: Mid::from(mid.trim()), audio, direction });
                current_index = Some(sections.len() - 1);
            }
        }
    }
    sections
}

/// Egress mid bookkeeping for one peer (T11b per-source fanout).
///
/// Invariants:
/// - one egress mid carries at most one publisher per kind: two RTP clock
///   domains in one mid corrupt the subscriber's decoder;
/// - an egress mid is handed out once per session and never reassigned to a
///   different publisher (`retired`), so an SSRC/clock-domain switch cannot
///   slide into a live decoder;
/// - sections the answer rejected (port 0 / `inactive`) never enter a pool;
/// - pool exhaustion returns `None` (the caller drops the frame and counts it),
///   it never panics and never reuses a mid.
#[derive(Debug, Default)]
pub struct PeerMids {
    sections: Vec<MidSection>,
    /// Dedicated `sendonly` sections first, then the peer's own `sendrecv`
    /// publish m-line: a legacy 1+1 browser only has that one, and it used to
    /// carry the remote stream before T11b, so it must keep working.
    audio_pool: VecDeque<Mid>,
    video_pool: VecDeque<Mid>,
    src_audio: HashMap<ParticipantId, Mid>,
    src_video: HashMap<ParticipantId, Mid>,
    retired: HashSet<Mid>,
    /// `(publisher, kind)` pairs whose source was retired by a `DropSource`. A
    /// late `Write` from the dead task must not allocate a fresh mid (P2-1);
    /// an explicit `EnsureSource` for that kind clears the mark, so a publisher
    /// that reconnects with the same id gets a *fresh* mid while the old one
    /// stays retired. Per kind, because a returning peer may publish audio
    /// without a camera.
    retired_sources: HashSet<(ParticipantId, bool)>,
    /// Mids whose offered section had port 0 (RFC 3264 §6.1 rejection). They
    /// stay `Inactive` even when str0m emits `MediaAdded`/`MediaChanged` with
    /// `sendrecv` for them, and never enter an egress pool.
    rejected: HashSet<Mid>,
}

impl PeerMids {
    fn new(sections: Vec<MidSection>, rejected: HashSet<Mid>) -> Self {
        let mut mids = Self { sections, rejected, ..Self::default() };
        mids.refresh_pools();
        mids
    }

    /// Negotiated m-lines in SDP order.
    pub fn sections(&self) -> &[MidSection] {
        &self.sections
    }

    /// The mid carrying the browser's own audio/video publish stream (first
    /// m-line the SFU may receive on), never an egress-only section.
    pub fn publish_mid(&self, audio: bool) -> Option<Mid> {
        self.sections
            .iter()
            .find(|section| {
                section.audio == audio && section.direction.can_recv() && !self.rejected.contains(&section.mid)
            })
            .map(|section| section.mid)
    }

    /// Egress mid for one publisher, allocated lazily on first use.
    ///
    /// A publisher retired by `DropSource` gets `None` here: a late `Write`
    /// from its dead task must not occupy a fresh egress slot (P2-1). The
    /// explicit [`Self::ensure_source`] path re-arms it when the room announces
    /// the publisher again.
    pub fn mid_for_source(&mut self, from: ParticipantId, audio: bool) -> Option<Mid> {
        if self.retired_sources.contains(&(from, audio)) {
            return None;
        }
        let (map, pool) = if audio {
            (&mut self.src_audio, &mut self.audio_pool)
        } else {
            (&mut self.src_video, &mut self.video_pool)
        };
        if let Some(mid) = map.get(&from) {
            return Some(*mid);
        }
        while let Some(mid) = pool.pop_front() {
            if self.retired.contains(&mid) {
                continue;
            }
            map.insert(from, mid);
            return Some(mid);
        }
        None
    }

    /// Explicit (re-)announcement of a publisher, used by the eager
    /// `EnsureSource` path. Clears the retired-source mark, so a publisher that
    /// returns with the same `ParticipantId` (new generation, new str0m
    /// session) gets a fresh mid; the old mid stays retired and is never
    /// reused.
    pub fn ensure_source(&mut self, from: ParticipantId, audio: bool) -> Option<Mid> {
        self.retired_sources.remove(&(from, audio));
        self.mid_for_source(from, audio)
    }

    /// Publisher currently mapped to an egress mid (used to route a browser PLI
    /// to the exact publisher instead of broadcasting it).
    pub fn source_for_mid(&self, mid: Mid) -> Option<ParticipantId> {
        self.src_video
            .iter()
            .chain(self.src_audio.iter())
            .find(|(_, candidate)| **candidate == mid)
            .map(|(from, _)| *from)
    }

    /// Retires every egress mid of a publisher: the mids stay allocated (never
    /// returned to the pool) because a new publisher on the same mid would
    /// restart an RTP clock domain under the subscriber's decoder.
    pub fn retire_source(&mut self, from: ParticipantId) {
        if let Some(mid) = self.src_audio.remove(&from) {
            self.retired.insert(mid);
        }
        if let Some(mid) = self.src_video.remove(&from) {
            self.retired.insert(mid);
        }
        self.retired_sources.insert((from, true));
        self.retired_sources.insert((from, false));
        self.refresh_pools();
    }

    /// Number of egress mids handed out to publishers.
    pub fn allocated(&self) -> usize {
        self.src_audio.len() + self.src_video.len()
    }

    /// Mids retired from reuse (observability/tests).
    pub fn retired(&self) -> &HashSet<Mid> {
        &self.retired
    }

    fn set_kind(&mut self, mid: Mid, audio: bool) {
        if let Some(section) = self.sections.iter_mut().find(|section| section.mid == mid) {
            if section.audio != audio {
                section.audio = audio;
                self.refresh_pools();
            }
        }
    }

    fn set_direction(&mut self, mid: Mid, direction: MidDirection) {
        // A section the offer rejected with port 0 stays inactive even if
        // str0m reports it as sendrecv (`MediaAdded`/`MediaChanged`).
        let direction = if self.rejected.contains(&mid) { MidDirection::Inactive } else { direction };
        if let Some(section) = self.sections.iter_mut().find(|section| section.mid == mid) {
            if section.direction != direction {
                section.direction = direction;
                self.refresh_pools();
            }
        }
    }

    /// Rebuilds the section list after a re-offer, keeping live allocations.
    fn rebuild(&mut self, sections: Vec<MidSection>, rejected: HashSet<Mid>) {
        self.rejected = rejected;
        let known: HashSet<Mid> = sections.iter().map(|section| section.mid).collect();
        let stale: Vec<Mid> = self
            .src_audio
            .values()
            .chain(self.src_video.values())
            .filter(|mid| !known.contains(mid))
            .copied()
            .collect();
        for mid in stale {
            self.retired.insert(mid);
        }
        self.src_audio.retain(|_, mid| known.contains(mid));
        self.src_video.retain(|_, mid| known.contains(mid));
        self.sections = sections;
        self.refresh_pools();
    }

    fn refresh_pools(&mut self) {
        let allocated: HashSet<Mid> = self.src_audio.values().chain(self.src_video.values()).copied().collect();
        self.audio_pool = self.egress_pool(true, &allocated);
        self.video_pool = self.egress_pool(false, &allocated);
    }

    fn egress_pool(&self, audio: bool, allocated: &HashSet<Mid>) -> VecDeque<Mid> {
        let mut dedicated = VecDeque::new();
        let mut shared = VecDeque::new();
        for section in &self.sections {
            if section.audio != audio || !section.direction.can_send() {
                continue;
            }
            if self.rejected.contains(&section.mid) {
                continue;
            }
            if self.retired.contains(&section.mid) || allocated.contains(&section.mid) {
                continue;
            }
            match section.direction {
                MidDirection::SendOnly => dedicated.push_back(section.mid),
                _ => shared.push_back(section.mid),
            }
        }
        dedicated.extend(shared);
        dedicated
    }
}

/// Transport configuration for one peer.
#[derive(Debug, Clone)]
pub struct WebRtcConfig {
    /// SFU is an ICE-lite endpoint (host candidates only).
    pub ice_lite: bool,
    /// Enable TWCC/REMB bandwidth estimation.
    pub bwe: bool,
    /// Initial BWE estimate in bit/s (default 1 Mbit/s).
    pub bwe_initial_bps: u64,
    /// Video reordering jitter before declaring loss (default 200 ms).
    pub video_jitter_ms: u64,
    /// Video reordering buffer packets (default 30).
    pub video_reorder_size: usize,
    /// Bounded audio send buffer packets.
    pub send_buffer_audio: usize,
    /// Bounded video send buffer packets.
    pub send_buffer_video: usize,
}

impl Default for WebRtcConfig {
    fn default() -> Self {
        Self {
            ice_lite: true,
            bwe: true,
            bwe_initial_bps: 1_000_000,
            video_jitter_ms: 200,
            video_reorder_size: 30,
            send_buffer_audio: 100,
            send_buffer_video: 200,
        }
    }
}

/// Errors from the WebRTC transport.
#[derive(Debug, thiserror::Error)]
pub enum WebRtcError {
    #[error("str0m: {0}")]
    Rtc(#[from] RtcError),
    #[error("SDP: {0}")]
    Sdp(String),
    #[error("network: {0}")]
    Net(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("no UDP host candidate for {0}")]
    Candidate(SocketAddr),
    #[error("unknown media mid")]
    UnknownMid,
    #[error("room already has the maximum number of real WebRTC peers")]
    RoomFull,
    #[error("server reached its global WebRTC peer limit")]
    CapacityExceeded,
    #[error("invalid offer: {0}")]
    InvalidOffer(String),
}

/// A side effect produced by the transport after an input.
#[derive(Debug)]
pub enum WebRtcAction {
    /// Send this datagram through the UDP socket owned by the caller.
    Send { source: SocketAddr, destination: SocketAddr, contents: Vec<u8> },
    /// ICE+DTLS are connected; SRTP is active.
    Connected,
    /// ICE state changed.
    IceState(IceConnectionState),
    /// A complete depayloaded frame arrived from the peer.
    Media(Box<MediaData>),
    /// New egress bandwidth estimate (bit/s).
    Bitrate(u64),
    /// Remote closed DTLS or the whole session.
    Closed,
    /// A browser asked for a video keyframe. `source` is the publisher that owns
    /// the egress mid the browser complained about; `None` when the mid is not
    /// mapped yet (or the request came for the browser's own publish stream), in
    /// which case the caller falls back to broadcasting.
    KeyframeRequest { audio: bool, source: Option<ParticipantId> },
}

/// One passive WebRTC peer (browser → SFU).
pub struct WebRtcPeer {
    rtc: Rtc,
    answer_sdp: String,
    local_addr: SocketAddr,
    connected: bool,
    /// Negotiated m-lines (kind + local direction) and the per-source egress
    /// mid map (T11b). One mid never carries two publishers.
    mids: PeerMids,
    /// Mids in the order of the first accepted offer: a re-offer must keep the
    /// same m-line order (str0m rejects structural changes; the SDP body is not
    /// applied, but its ICE credentials may already be — see `accept_reoffer`).
    remote_mids: Vec<String>,
    /// Remote DTLS fingerprint from the accepted offer. A re-offer from a
    /// recreated PeerConnection has a new fingerprint that str0m silently
    /// ignores, so it must be rejected instead of leaving 30s dead media.
    remote_fingerprint: Option<String>,
    next_timeout: Option<Instant>,
}

impl std::fmt::Debug for WebRtcPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebRtcPeer")
            .field("local_addr", &self.local_addr)
            .field("connected", &self.connected)
            .finish_non_exhaustive()
    }
}

impl WebRtcPeer {
    /// Accepts a browser SDP offer and returns a real str0m answer
    /// (with a real ICE ufrag/pwd and a real DTLS certificate fingerprint).
    pub fn accept_offer(offer_sdp: &str, local_addr: SocketAddr, config: &WebRtcConfig) -> Result<Self, WebRtcError> {
        validate_offer(offer_sdp)?;
        let mut builder = RtcConfig::new()
            .set_ice_lite(config.ice_lite)
            // Honest SFU capabilities: the codec SSOT decides which encodings
            // enter the answer (Opus + VP8 today). The feature-gated registry
            // (vp9/av1) must not leak here: str0m would advertise codecs the
            // forwarding path cannot carry.
            .clear_codecs()
            .set_reordering_timeout_video(Some(std::time::Duration::from_millis(config.video_jitter_ms)))
            .set_reordering_size_video(config.video_reorder_size)
            .set_send_buffer_audio(config.send_buffer_audio)
            .set_send_buffer_video(config.send_buffer_video);
        for spec in vks_common::codec_spec::webrtc_answer_specs() {
            builder = match spec.id {
                vks_common::codec_spec::CodecId::Opus => builder.enable_opus(true, false),
                vks_common::codec_spec::CodecId::Vp8 => builder.enable_vp8(true),
                // Fail closed instead of panicking: if the codec SSOT answer
                // slice ever grows (VP9/AV1/H264), the answer must not claim a
                // codec this forwarding path cannot carry. The error is
                // surfaced as a rejected offer, not an `unreachable!` abort.
                other => {
                    return Err(WebRtcError::Sdp(format!(
                        "codec SSOT answer slice contains {other:?}, but the str0m peer builder enables Opus+VP8 only"
                    )));
                }
            };
        }
        if config.bwe {
            builder = builder.enable_bwe(Some(str0m::bwe::Bitrate::bps(config.bwe_initial_bps)));
        }
        if let Some(cert) = shared_dtls_cert() {
            builder = builder.set_dtls_cert(cert);
        }
        let mut rtc = builder.build(Instant::now());

        let candidate = Candidate::host(local_addr, "udp").map_err(|_| WebRtcError::Candidate(local_addr))?;
        rtc.add_local_candidate(candidate);

        let offer = SdpOffer::from_sdp_string(offer_sdp).map_err(|e| WebRtcError::Sdp(e.to_string()))?;
        let answer = rtc.sdp_api().accept_offer(offer)?;
        // This transport does not apply trickled remote candidates; do not
        // advertise trickle support we cannot honour.
        let answer_sdp = answer.to_sdp_string().replace("a=ice-options:trickle\r\n", "");
        // RFC 3264 §6.1: a section the browser offered with port 0 is rejected
        // in the answer (`m=… 0 …` + `a=inactive`). str0m 0.24.1 answers
        // `sendrecv` on a live port instead, so enforce the rule explicitly.
        let rejected = offered_port_zero_mids(offer_sdp);
        let answer_sdp = reject_port_zero_sections(offer_sdp, &answer_sdp);
        // Direction-aware sections: the answer carries one m-line per offered
        // section (RFC 3264 §6), `sendonly` for the browser's `recvonly`
        // pre-offered egress slots and `sendrecv` for its own publish stream.
        let sections = parse_sections(&answer_sdp);

        let mut peer = Self {
            rtc,
            answer_sdp,
            local_addr,
            connected: false,
            mids: PeerMids::new(sections, rejected),
            remote_mids: mids_of(offer_sdp),
            remote_fingerprint: fingerprint_of(offer_sdp),
            next_timeout: None,
        };
        // accept_offer mutates the session; drain the invariant before the
        // first datagram arrives. No remote input exists yet, so any output
        // here is a local ICE/DTLS action we must not silently keep queued.
        let _ = peer.drain()?;
        Ok(peer)
    }

    /// Re-negotiates on the SAME str0m session (no new ICE/DTLS, no new UDP
    /// socket). Returns the fresh answer SDP plus the local actions produced
    /// by the renegotiation, which the caller must transmit.
    pub fn accept_reoffer(&mut self, offer_sdp: &str) -> Result<(String, Vec<WebRtcAction>), WebRtcError> {
        validate_offer(offer_sdp)?;
        let next_mids = mids_of(offer_sdp);
        // Existing sections must keep their order/identity; a browser may append
        // new m-lines (e.g. enabling the camera after an audio-only join).
        let keeps_prefix =
            next_mids.len() >= self.remote_mids.len() && next_mids[..self.remote_mids.len()] == self.remote_mids[..];
        if !next_mids.is_empty() && !keeps_prefix {
            return Err(WebRtcError::InvalidOffer(
                "m-line/mid order changed: re-offers may only append new sections".into(),
            ));
        }
        if let (Some(current), Some(next)) = (&self.remote_fingerprint, fingerprint_of(offer_sdp)) {
            if *current != next {
                return Err(WebRtcError::InvalidOffer(
                    "DTLS fingerprint changed: the browser created a new PeerConnection; open a new transport".into(),
                ));
            }
        }
        // Parse errors happen before the session is mutated, so the live call
        // is safe to keep: classify them as a rejected re-offer, not a fatal
        // transport failure.
        let offer = SdpOffer::from_sdp_string(offer_sdp)
            .map_err(|e| WebRtcError::InvalidOffer(format!("re-offer SDP is not valid: {e}")))?;
        // str0m's `accept_offer` validates the new SDP against the existing
        // remote description and rejects it with an error (verified: answer and
        // a subsequent valid re-offer still work). The DTLS session and media
        // keep running, but str0m runs `add_ice_details` before that
        // validation, so the server ufrag may already have rotated; such
        // rejections must still keep the live call alive.
        let answer = self
            .rtc
            .sdp_api()
            .accept_offer(offer)
            .map_err(|error| WebRtcError::InvalidOffer(format!("re-offer rejected by str0m: {error}")))?;
        // Do not advertise trickle: this transport never applies trickled
        // remote candidates.
        let answer_sdp = answer.to_sdp_string().replace("a=ice-options:trickle\r\n", "");
        let rejected = offered_port_zero_mids(offer_sdp);
        let answer_sdp = reject_port_zero_sections(offer_sdp, &answer_sdp);
        // Keep live per-source allocations across the re-offer; the rebuilt
        // section list only adds/removes egress slots. A mid that disappeared
        // is retired, never recycled (T11b).
        self.mids.rebuild(parse_sections(&answer_sdp), rejected);
        self.answer_sdp = answer_sdp.clone();
        let actions = self.drain()?;
        Ok((answer_sdp, actions))
    }

    /// Whether ICE+DTLS+SRTP are fully established.
    pub fn is_connected(&self) -> bool {
        self.connected && self.rtc.is_connected()
    }

    /// The SDP answer to send back to the browser.
    pub fn answer_sdp(&self) -> &str {
        &self.answer_sdp
    }

    /// Local UDP address advertised as the ICE host candidate.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Negotiated media mids in SDP order (audio, video, ...).
    pub fn mids(&self) -> Vec<Mid> {
        self.mids.sections().iter().map(|section| section.mid).collect()
    }

    /// Negotiated m-lines with kind and SFU-local direction (T11b).
    pub fn sections(&self) -> &[MidSection] {
        self.mids.sections()
    }

    /// The mid carrying this peer's own audio/video publish stream: the first
    /// m-line the SFU may receive on. PLI and connected-time keyframe requests
    /// must use it, never an egress-only (`sendonly`) section.
    pub fn publish_mid(&self, audio: bool) -> Option<Mid> {
        self.mids.publish_mid(audio)
    }

    /// Egress mid for one publisher, allocated on first use.
    pub fn mid_for_source(&mut self, from: ParticipantId, audio: bool) -> Option<Mid> {
        self.mids.mid_for_source(from, audio)
    }

    /// Reserved `ParticipantId` that owns the server-side bridge egress stream
    /// (T12: mixed room browser↔SIP/H.323 audio) on every peer.
    ///
    /// It is a fixed, non-random UUID: the bridge stream is a *source* like any
    /// publisher (it needs its own egress mid and RTP clock domain, T11b), but
    /// it never appears in the room roster. Never collide it with a real
    /// participant — real ids are UUIDv4/v5 from `ParticipantId::new`.
    pub fn bridge_egress_mid(&mut self, audio: bool) -> Option<Mid> {
        self.mids.ensure_source(bridge_source(), audio)
    }

    /// Allocates a publisher's egress mid ahead of its first frame, so mids are
    /// assigned in room registration order rather than packet arrival order.
    ///
    /// This is also the explicit re-announcement path: it clears the
    /// retired-source mark, so a publisher that reconnects with the same
    /// `ParticipantId` gets a fresh mid (the old one stays retired).
    pub fn ensure_source(&mut self, from: ParticipantId, audio: bool) -> bool {
        // A repeated `EnsureSource` for the same publisher must not inflate the
        // `allocated` counter (P3-5): the mid is only newly allocated on the
        // first mapping, while re-announcements keep the existing slot.
        let already_mapped =
            if audio { self.mids.src_audio.contains_key(&from) } else { self.mids.src_video.contains_key(&from) };
        self.mids.ensure_source(from, audio).is_some() && !already_mapped
    }

    /// Retires a publisher's egress mids (publisher left the room). They are
    /// never reassigned: a new publisher would restart an RTP clock domain on a
    /// mid the subscriber's decoder already tracks.
    pub fn retire_source(&mut self, from: ParticipantId) {
        self.mids.retire_source(from);
    }

    /// Publisher currently mapped to an egress mid (routes a browser PLI to the
    /// exact publisher instead of broadcasting it to everyone).
    pub fn source_for_mid(&self, mid: Mid) -> Option<ParticipantId> {
        self.mids.source_for_mid(mid)
    }

    /// Kind of a negotiated mid (`Some(true)` = audio), used to feed the
    /// mixed-room media tap the frame kind without parsing codecs (T12).
    pub(crate) fn is_audio_mid(&self, mid: Mid) -> Option<bool> {
        self.mids.sections().iter().find(|section| section.mid == mid).map(|section| section.audio)
    }

    /// Negotiated primary RTP maps `(pt, codec, clock, channels)` from the answer.
    ///
    /// The server rebuilds `SdpAnswer.rtp_map` from these so clients never see
    /// the legacy profile's stale payload types.
    pub fn rtp_maps(&self) -> Vec<(u8, String, u32, u32)> {
        let mut maps = Vec::new();
        for line in self.answer_sdp.lines() {
            let Some(spec) = line.strip_prefix("a=rtpmap:") else { continue };
            let Some((pt, codec)) = spec.split_once(' ') else { continue };
            let Ok(pt) = pt.trim().parse::<u8>() else { continue };
            let mut parts = codec.split('/');
            let name = parts.next().unwrap_or_default();
            if name.eq_ignore_ascii_case("rtx") {
                continue;
            }
            let clock = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            let channels = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            maps.push((pt, name.to_ascii_lowercase(), clock, channels));
        }
        maps
    }

    /// First negotiated payload type for a mid (codec chosen by str0m).
    pub fn payload_type(&mut self, mid: Mid) -> Option<Pt> {
        self.rtc.writer(mid)?.payload_params().next().map(|p| p.pt())
    }

    /// Write a depayloaded frame from another peer: match its codec parameters
    /// to this peer's own negotiated payload type (PTs differ per browser).
    pub fn write_media_matched(
        &mut self,
        mid: Mid,
        params: PayloadParams,
        wallclock: Instant,
        rtp_time: MediaTime,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Vec<WebRtcAction>, WebRtcError> {
        let pt = {
            let writer = self.rtc.writer(mid).ok_or(WebRtcError::UnknownMid)?;
            writer.match_params(params).ok_or(WebRtcError::UnknownMid)?
        };
        self.write_media(mid, pt, wallclock, rtp_time, data)
    }

    /// Ask the browser for a keyframe on this receiving media.
    pub fn request_keyframe(&mut self, mid: Mid) -> Result<Vec<WebRtcAction>, WebRtcError> {
        let mut writer = self.rtc.writer(mid).ok_or(WebRtcError::UnknownMid)?;
        writer.request_keyframe(None, KeyframeRequestKind::Pli)?;
        self.drain()
    }

    /// Negotiated payload type for a specific codec on a mid.
    pub fn payload_type_for_codec(&mut self, mid: Mid, codec: Codec) -> Option<Pt> {
        self.rtc.writer(mid)?.payload_params().find(|p| p.spec().codec == codec).map(|p| p.pt())
    }

    /// Earliest instant the caller should drive [`Self::handle_timeout`].
    pub fn next_timeout(&self) -> Option<Instant> {
        self.next_timeout
    }

    /// Feed one received UDP datagram and drain all resulting output.
    pub fn handle_receive(
        &mut self,
        now: Instant,
        source: SocketAddr,
        destination: SocketAddr,
        contents: &[u8],
    ) -> Result<Vec<WebRtcAction>, WebRtcError> {
        let receive = match Receive::new(Protocol::Udp, source, destination, contents) {
            Ok(receive) => receive,
            // Unknown/truncated datagrams are data-plane noise, never a
            // reason to tear down an established peer.
            Err(_) => return Ok(Vec::new()),
        };
        let input = Input::Receive(now, receive);
        // Caller-side demux/security gate: str0m's handle_input does not
        // validate the source address for RTP/RTCP. A datagram from a
        // different peer must never reach this session.
        if !self.rtc.accepts(&input) {
            return Ok(Vec::new());
        }
        self.rtc.handle_input(input)?;
        self.drain()
    }

    /// Advance the transport clock and drain all output.
    pub fn handle_timeout(&mut self, now: Instant) -> Result<Vec<WebRtcAction>, WebRtcError> {
        self.rtc.handle_input(Input::Timeout(now))?;
        self.drain()
    }

    /// Write one encoded frame to the peer (SRTP), then drain output.
    pub fn write_media(
        &mut self,
        mid: Mid,
        pt: Pt,
        wallclock: Instant,
        rtp_time: MediaTime,
        data: impl Into<Arc<[u8]>>,
    ) -> Result<Vec<WebRtcAction>, WebRtcError> {
        let writer = self.rtc.writer(mid).ok_or(WebRtcError::UnknownMid)?;
        writer.write(pt, wallclock, rtp_time, data)?;
        self.drain()
    }

    /// Configure the desired egress bitrate for BWE (bit/s).
    pub fn set_desired_bitrate(&mut self, bps: u64) {
        self.rtc.bwe().set_desired_bitrate(str0m::bwe::Bitrate::bps(bps));
    }

    fn drain(&mut self) -> Result<Vec<WebRtcAction>, WebRtcError> {
        let mut actions = Vec::new();
        loop {
            match self.rtc.poll_output()? {
                Output::Timeout(next) => {
                    self.next_timeout = Some(next);
                    break;
                }
                Output::Transmit(t) => actions.push(WebRtcAction::Send {
                    source: t.source,
                    destination: t.destination,
                    contents: t.contents.into(),
                }),
                Output::Event(event) => match event {
                    Event::Connected => {
                        self.connected = true;
                        actions.push(WebRtcAction::Connected);
                    }
                    Event::IceConnectionStateChange(state) => actions.push(WebRtcAction::IceState(state)),
                    Event::MediaAdded(added) => {
                        // str0m is authoritative for kind/direction; this keeps
                        // the pools in sync even if an answer line was parsed
                        // conservatively.
                        self.mids.set_kind(added.mid, matches!(added.kind, MediaKind::Audio));
                        self.mids.set_direction(added.mid, added.direction.into());
                    }
                    Event::MediaChanged(changed) => {
                        self.mids.set_direction(changed.mid, changed.direction.into());
                    }
                    Event::MediaData(data) => actions.push(WebRtcAction::Media(Box::new(data))),
                    Event::EgressBitrateEstimate(kind) => {
                        let estimate = match kind {
                            str0m::bwe::BweKind::Twcc { estimate, .. } | str0m::bwe::BweKind::Remb { estimate, .. } => {
                                estimate
                            }
                            _ => continue,
                        };
                        actions.push(WebRtcAction::Bitrate(estimate.as_u64()));
                    }
                    Event::Closed => {
                        self.connected = false;
                        actions.push(WebRtcAction::Closed);
                    }
                    Event::KeyframeRequest(request) => {
                        // A browser PLI on one egress mid names exactly one
                        // publisher: forward it there instead of broadcasting a
                        // PLI to every publisher in the room (T11b).
                        actions.push(WebRtcAction::KeyframeRequest {
                            audio: self.is_audio_mid(request.mid).unwrap_or(false),
                            source: self.mids.source_for_mid(request.mid),
                        });
                    }
                    _ => {}
                },
            }
        }
        Ok(actions)
    }
}

fn shared_dtls_cert() -> Option<str0m::crypto::dtls::DtlsCert> {
    static CERT: OnceLock<Option<str0m::crypto::dtls::DtlsCert>> = OnceLock::new();
    CERT.get_or_init(|| str0m::crypto::from_feature_flags().dtls_provider.generate_certificate()).clone()
}

/// Extracts `a=mid:` values in SDP order.
fn mids_of(offer: &str) -> Vec<String> {
    offer.lines().filter_map(|line| line.strip_prefix("a=mid:")).map(|value| value.trim().to_string()).collect()
}

/// Extracts the first `a=fingerprint:<algo> <value>` line.
fn fingerprint_of(offer: &str) -> Option<String> {
    offer.lines().find_map(|line| line.strip_prefix("a=fingerprint:")).map(|value| value.trim().to_ascii_lowercase())
}

/// Mids of offer sections rejected with port 0 (RFC 3264 §6.1).
///
/// `m=<kind> 0 …` means "do not use this stream". The answer must keep the
/// section (same m-line count/order) but reject it with port 0; str0m 0.24.1
/// ignores the offered port and answers `sendrecv` on a live port, so the
/// answer is rewritten by [`reject_port_zero_sections`] and these mids are
/// forced `Inactive` in [`PeerMids`].
fn offered_port_zero_mids(offer: &str) -> HashSet<Mid> {
    let mut rejected = HashSet::new();
    let mut port_zero = false;
    for line in offer.lines().map(|line| line.trim_end_matches('\r').trim()) {
        if let Some(rest) = line.strip_prefix("m=") {
            port_zero = rest.split_whitespace().nth(1) == Some("0");
            continue;
        }
        if let Some(mid) = line.strip_prefix("a=mid:") {
            if port_zero {
                rejected.insert(Mid::from(mid.trim()));
            }
        }
    }
    rejected
}

/// Rewrites the sections of `answer` whose offer had port 0 to `m=… 0 …` and
/// `a=inactive` (RFC 3264 §6.1), instead of the `sendrecv` str0m emits.
fn reject_port_zero_sections(offer: &str, answer: &str) -> String {
    let rejected = offered_port_zero_mids(offer);
    if rejected.is_empty() {
        return answer.to_string();
    }
    // Answers use CRLF; split on the m-line prefix so the header stays intact.
    let mut parts: Vec<String> = answer.split("\r\nm=").map(str::to_string).collect();
    for part in parts.iter_mut().skip(1) {
        let Some(mid_line) = part.split("\r\n").find(|line| line.starts_with("a=mid:")) else { continue };
        let mid = Mid::from(mid_line.trim_start_matches("a=mid:").trim());
        if !rejected.contains(&mid) {
            continue;
        }
        let mut lines: Vec<String> = part.split("\r\n").map(str::to_string).collect();
        let mut header: Vec<&str> = lines[0].split_whitespace().collect();
        if header.len() > 1 {
            header[1] = "0";
            lines[0] = header.join(" ");
        }
        for line in lines.iter_mut().skip(1) {
            if matches!(line.as_str(), "a=sendonly" | "a=recvonly" | "a=sendrecv" | "a=inactive") {
                *line = "a=inactive".to_string();
            }
        }
        *part = lines.join("\r\n");
    }
    parts.join("\r\nm=")
}

/// Rejects offers this SFU cannot safely serve before str0m allocates state.
///
/// T11b shape: one audio m-line per remote publisher (`1 + (N-1)`, there is no
/// SFU mixer) and one video m-line per remote publisher, with hard ceilings
/// ([`MAX_AUDIO_MIDS`]/[`MAX_VIDEO_MIDS`]) so a huge m-line count cannot churn
/// state inside the 64 KiB SDP cap. Browser ICE/DTLS credentials are still
/// mandatory; application/data m-lines are rejected explicitly.
pub(crate) fn validate_offer(offer: &str) -> Result<(), WebRtcError> {
    if offer.len() > 64 * 1024 {
        return Err(WebRtcError::InvalidOffer("SDP larger than 64 KiB".into()));
    }
    if !offer.contains("a=ice-ufrag:") || !offer.contains("a=ice-pwd:") {
        return Err(WebRtcError::InvalidOffer("missing ICE credentials".into()));
    }
    if !offer.contains("a=fingerprint:") {
        return Err(WebRtcError::InvalidOffer("missing DTLS fingerprint".into()));
    }
    let mut audio = 0;
    let mut video = 0;
    // A mid is the identity of an m-line; one mid cannot be an audio AND a
    // video slot (the egress pools would alias and a reverse lookup would be
    // ambiguous). Duplicates *within* one kind are left to str0m, which
    // collapses the extra section (observed 0.24.1) - that behaviour predates
    // T11b and cannot glue two clock domains.
    let mut kinds_by_mid: HashMap<&str, bool> = HashMap::new();
    let mut current_audio: Option<bool> = None;
    for line in offer.lines().map(|line| line.trim_end_matches('\r').trim()) {
        if let Some(rest) = line.strip_prefix("m=") {
            if rest.starts_with("audio ") {
                audio += 1;
                current_audio = Some(true);
            } else if rest.starts_with("video ") {
                video += 1;
                current_audio = Some(false);
            } else {
                return Err(WebRtcError::InvalidOffer(format!("unsupported media line: {line}")));
            }
            continue;
        }
        if let Some(mid) = line.strip_prefix("a=mid:") {
            let mid = mid.trim();
            if let Some(kind) = current_audio {
                if let Some(previous_audio) = kinds_by_mid.get(mid) {
                    if *previous_audio != kind {
                        return Err(WebRtcError::Sdp(format!(
                            "duplicate a=mid:{mid} across audio and video sections: one mid cannot carry both kinds"
                        )));
                    }
                } else {
                    kinds_by_mid.insert(mid, kind);
                }
            }
        }
    }
    if audio > MAX_AUDIO_MIDS || video > MAX_VIDEO_MIDS {
        return Err(WebRtcError::InvalidOffer(format!(
            "too many media lines: audio={audio} video={video} (max {MAX_AUDIO_MIDS} audio + {MAX_VIDEO_MIDS} video)"
        )));
    }
    if audio == 0 && video == 0 {
        return Err(WebRtcError::InvalidOffer("no audio or video m-line".into()));
    }
    // The answer only enables Opus + VP8; an offer carrying neither would be
    // answered with rejected m-lines while we still announce tracks.
    let lower = offer.to_ascii_lowercase();
    if !lower.contains("vp8/90000") && !lower.contains("opus/48000") {
        return Err(WebRtcError::InvalidOffer("offer has no supported codec (Opus/VP8)".into()));
    }
    Ok(())
}

/// Whether the browser offer publishes `audio`/`video`: **any** m-line of that
/// kind with a sending direction (`sendonly`/`sendrecv`, and `sendrecv` is the
/// SDP default when the direction attribute is absent).
///
/// T11b: a pre-offer is `1 sendrecv + (N-1) recvonly`, so classifying only the
/// first section of a kind gives the wrong answer as soon as the recvonly
/// sections come first.
///
/// A port-0 section is a rejected stream (RFC 3264 §6.1): it can never publish,
/// even with `a=sendonly` (P3-2).
pub fn offer_publishes(offer: &str, audio: bool) -> bool {
    fn publishes(current: Option<(bool, MidDirection, bool)>, audio: bool) -> bool {
        current.is_some_and(|(kind, direction, port_zero)| kind == audio && !port_zero && direction.can_send())
    }
    let mut current: Option<(bool, MidDirection, bool)> = None;
    for line in offer.lines().map(|line| line.trim_end_matches('\r').trim()) {
        if let Some(rest) = line.strip_prefix("m=") {
            if publishes(current.take(), audio) {
                return true;
            }
            let port_zero = rest.split_whitespace().nth(1) == Some("0");
            current = if rest.starts_with("audio ") {
                Some((true, MidDirection::SendRecv, port_zero))
            } else if rest.starts_with("video ") {
                Some((false, MidDirection::SendRecv, port_zero))
            } else {
                None
            };
            continue;
        }
        if let Some((_, direction, _)) = current.as_mut() {
            if let Some(parsed) = MidDirection::from_sdp_line(line) {
                *direction = parsed;
            }
        }
    }
    publishes(current, audio)
}
#[cfg(test)]
mod tests {
    use super::*;
    use str0m::change::{SdpAnswer, SdpPendingOffer};
    use str0m::media::{Direction, MediaKind};
    use str0m::{Input, Output, Rtc};

    const A: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 41001);
    const B: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(std::net::Ipv4Addr::new(127, 0, 0, 1)), 41002);

    fn drain(rtc: &mut Rtc) -> Vec<(SocketAddr, SocketAddr, Vec<u8>)> {
        drain_with_events(rtc).0
    }

    fn drain_with_events(rtc: &mut Rtc) -> (Vec<(SocketAddr, SocketAddr, Vec<u8>)>, usize) {
        let mut out = Vec::new();
        let mut media = 0usize;
        loop {
            match rtc.poll_output().expect("poll") {
                Output::Timeout(_) => break,
                Output::Transmit(t) => out.push((t.source, t.destination, t.contents.into())),
                Output::Event(str0m::Event::MediaData(_)) => media += 1,
                Output::Event(_) => {}
            }
        }
        (out, media)
    }

    fn deliver(
        peer: &mut WebRtcPeer,
        packets: Vec<(SocketAddr, SocketAddr, Vec<u8>)>,
        now: Instant,
    ) -> Vec<WebRtcAction> {
        let mut actions = Vec::new();
        for (source, destination, contents) in packets {
            actions.extend(peer.handle_receive(now, source, destination, &contents).expect("receive"));
        }
        actions
    }

    #[test]
    fn rtc_constructs_with_crypto_provider() {
        // Regression guard: with `default-features = false` a missing crypto
        // provider panics on the first Rtc::new; this fails fast if the
        // str0m feature wiring ever loses its provider.
        let _rtc = RtcConfig::new().set_ice_lite(true).build(Instant::now());
    }

    /// A re-offer is negotiated on the SAME str0m session: no new socket, no
    /// new DTLS handshake (no second `Connected` action), same local address.
    #[test]
    fn accept_reoffer_reuses_the_same_session_and_socket() {
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(A, "udp").expect("candidate A"));
        let mut change = client.sdp_api();
        change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        let (offer, _pending): (str0m::change::SdpOffer, SdpPendingOffer) = change.apply().expect("offer");
        let offer_sdp = offer.to_sdp_string();

        let mut peer = WebRtcPeer::accept_offer(&offer_sdp, B, &WebRtcConfig::default()).expect("accept");
        let local_addr = peer.local_addr();
        let (answer, actions) = peer.accept_reoffer(&offer_sdp).expect("re-offer");
        assert_eq!(peer.local_addr(), local_addr, "re-offer must not bind a new socket");
        assert!(
            !actions.iter().any(|action| matches!(action, WebRtcAction::Connected)),
            "re-offer must not restart DTLS"
        );
        assert!(answer.contains("VP8/90000"), "re-offer answer keeps VP8-only caps");
        assert_eq!(peer.mids().len(), 2, "same two m-lines renegotiated");
    }

    fn browser_offer() -> String {
        browser_offer_with(1, 1, true, true)
    }

    /// Browser offer with `audio_sections`/`video_sections` m-lines, T11b shape:
    /// the first section of each kind is `sendrecv` (the browser publishes), the
    /// rest are `recvonly` (pre-offered per-source egress slots).
    fn browser_offer_with(audio_sections: usize, video_sections: usize, audio_send: bool, video_send: bool) -> String {
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(A, "udp").expect("candidate A"));
        let mut change = client.sdp_api();
        for index in 0..audio_sections {
            let direction = if index == 0 && audio_send { Direction::SendRecv } else { Direction::RecvOnly };
            change.add_media(MediaKind::Audio, direction, None, None, None);
        }
        for index in 0..video_sections {
            let direction = if index == 0 && video_send { Direction::SendRecv } else { Direction::RecvOnly };
            change.add_media(MediaKind::Video, direction, None, None, None);
        }
        let (offer, _pending): (str0m::change::SdpOffer, SdpPendingOffer) = change.apply().expect("offer");
        offer.to_sdp_string()
    }

    /// Swaps two whole m-line sections (1-based, header excluded) of an SDP.
    fn swap_offer_sections(offer: &str, a: usize, b: usize) -> String {
        let mut parts: Vec<String> = offer.split("\r\nm=").map(str::to_string).collect();
        assert!(a > 0 && b > 0 && a < parts.len() && b < parts.len(), "section index out of range");
        parts.swap(a, b);
        parts.join("\r\nm=")
    }

    fn connect(client: &mut Rtc, peer: &mut WebRtcPeer, now: &mut Instant) -> bool {
        for _ in 0..300 {
            *now += std::time::Duration::from_millis(10);
            client.handle_input(Input::Timeout(*now)).expect("client timeout");
            let client_out = drain(client);
            let server_actions = deliver(peer, client_out, *now);
            let mut server_out = Vec::new();
            for action in &server_actions {
                if let WebRtcAction::Send { source, destination, contents } = action {
                    server_out.push((*source, *destination, contents.clone()));
                }
            }
            for (source, destination, contents) in server_out {
                client
                    .handle_input(Input::Receive(
                        *now,
                        Receive::new(Protocol::Udp, source, destination, &contents).expect("receive"),
                    ))
                    .expect("client receive");
            }
            let client_tail = drain(client);
            for (source, destination, contents) in client_tail {
                peer.handle_receive(*now, source, destination, &contents).expect("server receive");
            }
            if client.is_connected() && peer.is_connected() {
                return true;
            }
        }
        false
    }

    /// Datagram tuple used throughout the in-process negotiation tests.
    type Datagram = (SocketAddr, SocketAddr, Vec<u8>);

    /// Drains output of a raw client `Rtc`, keeping received media data.
    fn drain_media(rtc: &mut Rtc) -> (Vec<Datagram>, Vec<MediaData>) {
        let mut out = Vec::new();
        let mut media = Vec::new();
        loop {
            match rtc.poll_output().expect("poll") {
                Output::Timeout(_) => break,
                Output::Transmit(t) => out.push((t.source, t.destination, t.contents.into())),
                Output::Event(str0m::Event::MediaData(data)) => media.push(data),
                Output::Event(_) => {}
            }
        }
        (out, media)
    }

    /// Pumps both sides and returns every `MediaData` the browser received.
    fn pump_browser_media(
        client: &mut Rtc,
        peer: &mut WebRtcPeer,
        now: &mut Instant,
        actions: Vec<WebRtcAction>,
    ) -> Vec<MediaData> {
        let mut received = Vec::new();
        let mut pending = actions;
        for _ in 0..40 {
            *now += std::time::Duration::from_millis(10);
            for action in pending.drain(..) {
                if let WebRtcAction::Send { source, destination, contents } = action {
                    client
                        .handle_input(Input::Receive(
                            *now,
                            Receive::new(Protocol::Udp, source, destination, &contents).expect("receive"),
                        ))
                        .expect("client receive");
                }
            }
            client.handle_input(Input::Timeout(*now)).expect("client tick");
            let (client_out, client_media) = drain_media(client);
            received.extend(client_media);
            for (source, destination, contents) in client_out {
                pending.extend(peer.handle_receive(*now, source, destination, &contents).expect("server receive"));
            }
            pending.extend(peer.handle_timeout(*now).expect("server tick"));
            let mut keep = Vec::new();
            for action in pending.drain(..) {
                if let WebRtcAction::Send { source, destination, contents } = action {
                    keep.push(WebRtcAction::Send { source, destination, contents });
                }
            }
            pending = keep;
            if received.len() >= 3 {
                break;
            }
        }
        received
    }

    /// Regression: a re-offer that passes our own validation but is rejected by
    /// str0m's SDP parser must NOT close the live session (the parse happens
    /// before mutation, so the call is safe to keep).
    #[test]
    fn malformed_reoffer_is_rejected_without_touching_the_live_session() {
        let offer_sdp = browser_offer();
        let mut peer = WebRtcPeer::accept_offer(&offer_sdp, B, &WebRtcConfig::default()).expect("accept");
        let answer_before = peer.answer_sdp().to_string();
        let malformed = offer_sdp.replacen("t=0 0", "t=0 1", 1);
        assert_ne!(malformed, offer_sdp, "fixture must change the timing line");
        match peer.accept_reoffer(&malformed) {
            Err(WebRtcError::InvalidOffer(message)) => assert!(message.contains("re-offer SDP"), "{message}"),
            other => panic!("expected a rejected re-offer, got {other:?}"),
        }
        assert_eq!(peer.answer_sdp(), answer_before, "rejected re-offer must not rewrite the answer");
        assert_eq!(peer.mids().len(), 2, "session keeps both m-lines");
        // The very same session still negotiates a valid re-offer.
        peer.accept_reoffer(&offer_sdp).expect("valid re-offer after a rejected one");
    }

    /// Regression: a re-offer with swapped m-line order is rejected before
    /// str0m mutates the session (it used to close the live call).
    #[test]
    fn swapped_m_line_reoffer_is_rejected_and_keeps_the_session() {
        let offer_sdp = browser_offer();
        let mut peer = WebRtcPeer::accept_offer(&offer_sdp, B, &WebRtcConfig::default()).expect("accept");
        let answer_before = peer.answer_sdp().to_string();
        // Swap the whole audio and video sections (m-line + its attributes).
        let (head, rest) = offer_sdp.split_once("m=audio").expect("audio section");
        let (audio, video) = rest.split_once("m=video").expect("video section");
        let audio_section = format!("m=audio{audio}");
        let video_section = format!("m=video{video}");
        let swapped = format!("{head}{video_section}{audio_section}");
        match peer.accept_reoffer(&swapped) {
            Err(WebRtcError::InvalidOffer(message)) => assert!(message.contains("mid"), "{message}"),
            other => panic!("expected a rejected re-offer, got {other:?}"),
        }
        assert_eq!(peer.answer_sdp(), answer_before, "rejected re-offer must not rewrite the answer");
        assert_eq!(peer.mids().len(), 2, "session keeps both m-lines");
        peer.accept_reoffer(&offer_sdp).expect("valid re-offer after a rejected one");
    }

    /// Appending a section (audio-only join, then camera enabled) is a valid
    /// re-offer: existing mids keep their order, the new video m-line is appended.
    #[test]
    fn audio_only_peer_can_append_a_video_section_on_reoffer() {
        let full = browser_offer();
        let audio_only = full.split("m=video").next().expect("audio section").to_string();
        let mut peer = WebRtcPeer::accept_offer(&audio_only, B, &WebRtcConfig::default()).expect("audio-only accept");
        assert_eq!(peer.mids().len(), 1);
        let (answer_sdp, _actions) = peer.accept_reoffer(&full).expect("appended m-line is a valid re-offer");
        assert_eq!(peer.mids().len(), 2, "the new video mid is registered");
        assert!(!answer_sdp.is_empty());
    }

    /// Regression (re-review #2): a pre-mutation str0m rejection (missing
    /// a=group:BUNDLE) must keep the live session, not just parse errors.
    #[test]
    fn reoffer_rejected_by_str0m_keeps_the_session() {
        let offer_sdp = browser_offer();
        let mut peer = WebRtcPeer::accept_offer(&offer_sdp, B, &WebRtcConfig::default()).expect("accept");
        let answer_before = peer.answer_sdp().to_string();
        let no_bundle: String = offer_sdp
            .lines()
            .filter(|line| !line.trim_start().starts_with("a=group:BUNDLE"))
            .collect::<Vec<_>>()
            .join("\r\n");
        match peer.accept_reoffer(&no_bundle) {
            Err(WebRtcError::InvalidOffer(_)) => {}
            other => panic!("expected InvalidOffer for a str0m pre-mutation rejection, got {other:?}"),
        }
        assert_eq!(peer.answer_sdp(), answer_before, "rejected re-offer must not rewrite the answer");
        peer.accept_reoffer(&offer_sdp).expect("valid re-offer after a str0m rejection");
    }

    /// A re-created browser PeerConnection has a new DTLS fingerprint that str0m
    /// silently ignores; accepting it would leave dead media for 30s.
    #[test]
    fn reoffer_with_a_new_dtls_fingerprint_is_rejected() {
        let offer_sdp = browser_offer();
        let mut peer = WebRtcPeer::accept_offer(&offer_sdp, B, &WebRtcConfig::default()).expect("accept first client");
        // Keep the same mids, only replace the DTLS certificate fingerprint.
        let altered = offer_sdp
            .lines()
            .map(|line| {
                if line.starts_with("a=fingerprint:") {
                    "a=fingerprint:sha-256 FF:EE:DD:CC:BB:AA:99:88:77:66:55:44:33:22:11:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00"
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\r\n");
        match peer.accept_reoffer(&altered) {
            Err(WebRtcError::InvalidOffer(message)) => assert!(message.contains("fingerprint"), "{message}"),
            other => panic!("expected a fingerprint rejection, got {other:?}"),
        }
    }

    #[test]
    fn real_dtls_srtp_handshake_and_media() {
        // Client (browser side) with a real str0m stack.
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(A, "udp").expect("candidate A"));
        let mut change = client.sdp_api();
        let audio_mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        let video_mid = change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        let (offer, pending): (str0m::change::SdpOffer, SdpPendingOffer) = change.apply().expect("offer");

        // Server (our transport) accepts the offer.
        let mut peer =
            WebRtcPeer::accept_offer(&offer.to_sdp_string(), B, &WebRtcConfig::default()).expect("accept offer");
        eprintln!("ANSWER:\n{}", peer.answer_sdp());
        assert!(peer.answer_sdp().contains("a=fingerprint:sha-256 "), "real DTLS fingerprint missing");
        assert!(!peer.answer_sdp().contains("FF:00:11"), "fake fingerprint must be gone");
        assert!(peer.answer_sdp().contains("a=ice-ufrag:"), "ICE ufrag missing");
        assert!(peer.answer_sdp().contains("a=ice-pwd:"), "ICE pwd missing");
        assert!(peer.answer_sdp().contains("a=candidate:"), "host candidate missing: {}", peer.answer_sdp());
        assert!(peer.answer_sdp().contains("a=setup:passive"), "DTLS role missing");
        assert!(peer.answer_sdp().contains("a=rtcp-mux"), "rtcp-mux missing");
        for forbidden in ["H264/90000", "VP9/90000", "AV1/90000", "H265/90000"] {
            assert!(!peer.answer_sdp().contains(forbidden), "unexpected codec {forbidden} in answer");
        }
        assert!(peer.answer_sdp().contains("opus/48000/2"), "opus missing from answer");
        assert!(peer.answer_sdp().contains("VP8/90000"), "vp8 missing from answer");
        let answer = SdpAnswer::from_sdp_string(peer.answer_sdp()).expect("answer parses");
        client.sdp_api().accept_answer(pending, answer).expect("accept answer");

        let start = Instant::now();
        let mut now = start;
        let mut connected = false;
        for _ in 0..300 {
            now += std::time::Duration::from_millis(10);
            client.handle_input(Input::Timeout(now)).expect("client timeout");
            let client_out = drain(&mut client);
            let server_actions = deliver(&mut peer, client_out, now);
            let mut server_out = Vec::new();
            for action in &server_actions {
                if let WebRtcAction::Send { source, destination, contents } = action {
                    server_out.push((*source, *destination, contents.clone()));
                }
                if matches!(action, WebRtcAction::Connected) {
                    connected = true;
                }
            }
            for (source, destination, contents) in server_out {
                client
                    .handle_input(Input::Receive(
                        now,
                        Receive::new(Protocol::Udp, source, destination, &contents).expect("receive"),
                    ))
                    .expect("client receive");
            }
            let client_timeout = drain(&mut client);
            for (source, destination, contents) in client_timeout {
                let actions = peer.handle_receive(now, source, destination, &contents).expect("server receive");
                for action in actions {
                    if matches!(action, WebRtcAction::Connected) {
                        connected = true;
                    }
                }
            }
            if client.is_connected() && peer.is_connected() {
                connected = true;
                break;
            }
        }
        assert!(connected, "DTLS/SRTP handshake did not complete");
        assert!(client.is_connected(), "client ICE/DTLS did not connect");
        assert!(peer.is_connected(), "server transport did not become connected");
        assert!(peer.next_timeout().is_some(), "pacer/ICE deadline was not recorded");
        let first_mid = peer.mids()[0];
        assert!(peer.payload_type_for_codec(first_mid, Codec::Opus).is_some(), "Opus PT not negotiated");
        let video_mid_answer = peer.mids()[1];
        assert!(peer.payload_type_for_codec(video_mid_answer, Codec::Vp8).is_some(), "VP8 PT not negotiated");

        // Malformed datagrams must be dropped, not turn into a fatal transport error.
        // Use the nominated 5-tuple (client source, server destination) so
        // `accepts()` lets the datagram through and the malformed-datagram
        // path itself is exercised.
        let bogus = peer.handle_receive(now, A, B, &[0xff; 17]).expect("malformed datagram is not fatal");
        assert!(bogus.is_empty(), "malformed datagram produced actions");

        // A real Opus RTP frame from browser to SFU, through SRTP.
        let pt =
            client.writer(audio_mid).expect("audio writer").payload_params().next().expect("negotiated payload").pt();
        let mut writes = 0;
        let mut received = 0usize;
        let mut received_params: Option<PayloadParams> = None;
        for i in 0..20 {
            client
                .writer(audio_mid)
                .expect("audio writer")
                .write(pt, now, MediaTime::from_millis((i as u64) * 20), vec![0x55u8; 40])
                .expect("write opus");
            writes += 1;
            now += std::time::Duration::from_millis(20);
            client.handle_input(Input::Timeout(now)).expect("client tick");
            let queued = drain(&mut client);
            for action in deliver(&mut peer, queued, now) {
                if let WebRtcAction::Media(m) = action {
                    assert_eq!(&*m.data, &[0x55u8; 40], "depayloaded Opus data mismatch");
                    received_params = Some(m.params);
                    received += 1;
                }
            }
            for action in peer.handle_timeout(now).expect("server tick") {
                if let WebRtcAction::Media(m) = action {
                    received_params = Some(m.params);
                    received += 1;
                }
            }
        }
        assert_eq!(writes, 20);

        for _ in 0..50 {
            now += std::time::Duration::from_millis(10);
            client.handle_input(Input::Timeout(now)).expect("client tail tick");
            let client_out = drain(&mut client);
            for (source, destination, contents) in client_out {
                let actions = peer.handle_receive(now, source, destination, &contents).expect("receive");
                received += actions.iter().filter(|a| matches!(a, WebRtcAction::Media(_))).count();
            }
            let _ = peer.handle_timeout(now).expect("timeout");
            let _ = drain(&mut client);
        }
        assert!(received > 0, "no media arrived through SRTP");

        // SFU → browser direction: real SRTP back to the client.
        let audio_id = peer.mids()[0];
        let mut back = 0usize;
        let mut sent_total = 0usize;
        let audio_params = received_params.expect("no Opus frame params captured");
        assert_eq!(peer.publish_mid(true), Some(audio_id));
        for i in 0..10 {
            now += std::time::Duration::from_millis(20);
            let mut out = peer
                .write_media_matched(
                    audio_id,
                    audio_params,
                    now,
                    MediaTime::from_millis((i as u64) * 20),
                    vec![0x66u8; 40],
                )
                .expect("write to client");
            out.extend(peer.handle_timeout(now).expect("server tick"));
            sent_total += out.iter().filter(|a| matches!(a, WebRtcAction::Send { .. })).count();
            for action in out {
                if let WebRtcAction::Send { source, destination, contents } = action {
                    client
                        .handle_input(Input::Receive(
                            now,
                            Receive::new(Protocol::Udp, source, destination, &contents).expect("receive"),
                        ))
                        .expect("client receive");
                }
            }
            client.handle_input(Input::Timeout(now)).expect("client tick");
            back += drain_with_events(&mut client).1;
        }
        assert!(sent_total > 0, "SFU did not emit SRTP to browser");
        assert!(back > 0, "no media arrived back at the browser peer");
        let _ = video_mid;
    }
    #[test]
    fn webrtc_answer_slice_matches_peer_builder() {
        let offer_sdp = browser_offer();
        let peer = WebRtcPeer::accept_offer(&offer_sdp, B, &WebRtcConfig::default()).expect("accept");
        let answer = peer.answer_sdp().to_ascii_lowercase();
        let names = vks_common::codec_spec::webrtc_answer_names();
        assert_eq!(names, ["OPUS", "VP8"], "SSOT answer slice changed");
        for name in names {
            assert!(answer.contains(&format!("{}/", name.to_ascii_lowercase())), "missing {name}: {answer}");
        }
        for forbidden in ["vp9/90000", "av1/90000", "h264/90000", "h265/90000"] {
            assert!(!answer.contains(forbidden), "unexpected {forbidden} in answer");
        }
    }

    // ---- T11b: multi-peer / per-source mids ---------------------------------

    #[test]
    fn offer_validation_accepts_eight_plus_eight_and_rejects_nine() {
        validate_offer(&browser_offer_with(8, 8, true, true)).expect("1+7 audio and 1+7 video is the T11b ceiling");
        validate_offer(&browser_offer_with(8, 1, true, true)).expect("eight audio m-lines");
        validate_offer(&browser_offer_with(1, 8, true, true)).expect("eight video m-lines");
        let too_many_audio = validate_offer(&browser_offer_with(9, 1, true, true));
        assert!(matches!(too_many_audio, Err(WebRtcError::InvalidOffer(_))), "{too_many_audio:?}");
        let too_many_video = validate_offer(&browser_offer_with(1, 9, true, true));
        assert!(matches!(too_many_video, Err(WebRtcError::InvalidOffer(_))), "{too_many_video:?}");
        // The pre-T11b cap of one m-line per kind must be gone.
        validate_offer(&browser_offer_with(2, 2, true, true)).expect("two publishers' mids are legal");
    }

    #[test]
    fn answer_sections_are_direction_aware_and_reject_port_zero() {
        let answer = concat!(
            "v=0\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:0\r\na=sendrecv\r\n",
            "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:1\r\na=sendonly\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:2\r\na=inactive\r\n",
            "m=video 0 UDP/TLS/RTP/SAVPF 96\r\na=mid:3\r\na=sendonly\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:4\r\n"
        );
        let sections = parse_sections(answer);
        assert_eq!(sections.len(), 5);
        assert_eq!(sections[0], MidSection { mid: Mid::from("0"), audio: true, direction: MidDirection::SendRecv });
        assert_eq!(sections[1].direction, MidDirection::SendOnly);
        assert_eq!(sections[2].direction, MidDirection::Inactive, "a=inactive is unusable");
        assert_eq!(sections[3].direction, MidDirection::Inactive, "port 0 is rejected regardless of a=sendonly");
        assert_eq!(sections[4].direction, MidDirection::SendRecv, "missing direction defaults to sendrecv");
    }

    #[test]
    fn offer_publishes_reads_every_section_not_only_the_first() {
        let offer = concat!(
            "v=0\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\na=recvonly\r\n",
            "m=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:1\r\na=sendrecv\r\n"
        );
        assert!(offer_publishes(offer, false), "sendrecv after a recvonly section still publishes");
        assert!(!offer_publishes(offer, true), "no audio section at all");
        let all_recv = offer.replace("a=sendrecv", "a=recvonly");
        assert!(!offer_publishes(&all_recv, false), "view-only peer does not publish");
        let default_direction = "v=0\r\nm=video 9 UDP/TLS/RTP/SAVPF 96\r\na=mid:0\r\n";
        assert!(offer_publishes(default_direction, false), "missing direction defaults to sendrecv");
    }

    #[test]
    fn multi_mid_offer_is_answered_with_one_section_per_m_line() {
        let offer = browser_offer_with(8, 8, true, true);
        let peer = WebRtcPeer::accept_offer(&offer, B, &WebRtcConfig::default()).expect("accept 16 m-lines");
        let sections = peer.sections();
        // RFC 3264 §6: the answer has exactly one m-line per offered m-line.
        assert_eq!(sections.len(), 16, "answer must not collapse the 8+8 offer");
        assert_eq!(sections.iter().filter(|section| section.audio).count(), 8);
        assert_eq!(sections.iter().filter(|section| !section.audio).count(), 8);
        assert_eq!(peer.mids().len(), 16);
        assert_eq!(peer.publish_mid(true), Some(sections[0].mid), "first audio sendrecv is the mic");
        assert_eq!(peer.publish_mid(false), Some(sections[8].mid), "first video sendrecv is the camera");
        for section in &sections[1..8] {
            assert_eq!(section.direction, MidDirection::SendOnly, "browser recvonly becomes SFU sendonly");
        }
        for section in &sections[9..] {
            assert_eq!(section.direction, MidDirection::SendOnly, "pre-offered video egress slot");
        }
    }

    #[test]
    fn per_source_audio_mids_are_unique_and_never_reused_after_retirement() {
        let mut peer = WebRtcPeer::accept_offer(&browser_offer_with(8, 8, true, true), B, &WebRtcConfig::default())
            .expect("accept");
        let first = ParticipantId::new();
        let first_mid = peer.mid_for_source(first, true).expect("audio slot");
        assert_eq!(peer.mid_for_source(first, true), Some(first_mid), "a publisher keeps its mid");
        assert_eq!(peer.source_for_mid(first_mid), Some(first));
        assert_ne!(first_mid, peer.publish_mid(true).expect("mic mid"), "egress never uses the mic mid");

        // Eight audio slots exist for 1 + 7 peers.
        let mut mids = vec![first_mid];
        for _ in 0..7 {
            mids.push(peer.mid_for_source(ParticipantId::new(), true).expect("audio slot"));
        }
        let unique: std::collections::HashSet<Mid> = mids.iter().copied().collect();
        assert_eq!(unique.len(), 8, "every publisher gets its own audio mid (no mixing)");
        assert!(peer.mid_for_source(ParticipantId::new(), true).is_none(), "ninth publisher has no slot");

        // Retirement: the mid is not returned to the pool.
        peer.retire_source(first);
        assert_eq!(peer.source_for_mid(first_mid), None);
        assert!(peer.mids.retired().contains(&first_mid), "retired mid must never be reassigned");
        assert!(peer.mid_for_source(ParticipantId::new(), true).is_none(), "no slot left after retirement");
        let next = ParticipantId::new();
        assert_eq!(peer.mid_for_source(next, true), None, "a retired mid cannot be handed to another publisher");
    }

    #[test]
    fn legacy_1_plus_1_peer_gets_deterministic_single_video_slot() {
        let mut peer = WebRtcPeer::accept_offer(&browser_offer(), B, &WebRtcConfig::default()).expect("accept");
        let first = ParticipantId::new();
        let second = ParticipantId::new();
        let mid = peer.mid_for_source(first, false).expect("legacy peer still receives video");
        assert_eq!(peer.mid_for_source(first, false), Some(mid), "same publisher keeps the slot");
        assert_eq!(peer.mid_for_source(second, false), None, "second publisher is explicitly dropped, not glued");
        let audio_mid = peer.mid_for_source(first, true).expect("one audio slot for the first publisher");
        assert_ne!(audio_mid, mid);
        assert_eq!(peer.mid_for_source(second, true), None, "a second audio source has no mid (no mixing)");
    }

    #[test]
    fn reoffer_with_sixteen_mids_keeps_the_order_and_survives_a_rejection() {
        let offer = browser_offer_with(8, 8, true, true);
        let mut peer = WebRtcPeer::accept_offer(&offer, B, &WebRtcConfig::default()).expect("accept");
        assert_eq!(peer.sections().len(), 16);
        peer.accept_reoffer(&offer).expect("identical 16 m-lines re-offer");
        assert_eq!(peer.sections().len(), 16, "re-offer must not drop sections");
        match peer.accept_reoffer(&swap_offer_sections(&offer, 2, 4)) {
            Err(WebRtcError::InvalidOffer(message)) => assert!(message.contains("order"), "{message}"),
            other => panic!("reordered m-lines must be rejected, got {other:?}"),
        }
        peer.accept_reoffer(&offer).expect("live session survives a rejected re-offer");
        assert_eq!(peer.sections().len(), 16);
    }

    /// Full str0m negotiation without a browser: 1 mic + 7 pre-offered audio
    /// egress slots + camera + 7 video slots, then real SRTP media delivered to
    /// three different publishers' mids.
    #[test]
    fn real_dtls_handshake_with_eight_audio_mids_delivers_per_source_streams() {
        let mut client = RtcConfig::new().set_ice_lite(false).build(Instant::now());
        client.add_local_candidate(Candidate::host(A, "udp").expect("candidate A"));
        let mut change = client.sdp_api();
        let audio_mid = change.add_media(MediaKind::Audio, Direction::SendRecv, None, None, None);
        for _ in 0..7 {
            change.add_media(MediaKind::Audio, Direction::RecvOnly, None, None, None);
        }
        let video_mid = change.add_media(MediaKind::Video, Direction::SendRecv, None, None, None);
        let (offer, pending): (str0m::change::SdpOffer, SdpPendingOffer) = change.apply().expect("offer");

        let mut peer = WebRtcPeer::accept_offer(&offer.to_sdp_string(), B, &WebRtcConfig::default())
            .expect("accept 1 audio + 7 audio + 1 video");
        let answer = SdpAnswer::from_sdp_string(peer.answer_sdp()).expect("answer parses");
        client.sdp_api().accept_answer(pending, answer).expect("accept answer");

        let mut now = Instant::now();
        assert!(connect(&mut client, &mut peer, &mut now), "DTLS handshake failed");

        // The published mic frame: the only sendrecv audio section.
        let opus_pt = client
            .writer(audio_mid)
            .expect("audio writer")
            .payload_params()
            .find(|params| params.spec().codec == Codec::Opus)
            .expect("opus payload")
            .pt();
        client
            .writer(audio_mid)
            .expect("audio writer")
            .write(opus_pt, now, MediaTime::from_millis(0), vec![0x55u8; 40])
            .expect("write opus");
        client.handle_input(Input::Timeout(now)).expect("client tick");

        let (client_out, _) = drain_media(&mut client);
        let mut server_actions = Vec::new();
        for (source, destination, contents) in client_out {
            server_actions.extend(peer.handle_receive(now, source, destination, &contents).expect("receive"));
        }
        server_actions.extend(peer.handle_timeout(now).expect("server tick"));
        let audio_params = server_actions
            .iter()
            .find_map(|action| match action {
                WebRtcAction::Media(data) => Some(data.params),
                _ => None,
            })
            .expect("no Opus frame from the browser");

        // Three publishers, three distinct egress audio mids (N-1 without a mixer).
        let publishers: Vec<ParticipantId> = (0..3).map(|_| ParticipantId::new()).collect();
        let mids: Vec<Mid> = publishers
            .iter()
            .map(|publisher| peer.mid_for_source(*publisher, true).expect("audio egress slot"))
            .collect();
        assert_eq!(mids.iter().collect::<std::collections::HashSet<_>>().len(), 3, "mids must differ");

        let mut actions = Vec::new();
        for (index, mid) in mids.iter().enumerate() {
            actions.extend(
                peer.write_media_matched(
                    *mid,
                    audio_params,
                    now,
                    MediaTime::from_millis(20 * index as u64),
                    vec![0x60u8 + index as u8; 40],
                )
                .expect("write to a per-source mid"),
            );
        }
        let received = pump_browser_media(&mut client, &mut peer, &mut now, actions);
        assert_eq!(received.len(), 3, "browser must receive three separate audio streams");
        for (index, mid) in mids.iter().enumerate() {
            let frame = received
                .iter()
                .find(|data| data.mid == *mid)
                .unwrap_or_else(|| panic!("no media on publisher mid {mid}"));
            assert_eq!(&*frame.data, &vec![0x60u8 + index as u8; 40], "wrong payload on mid {mid}");
        }
        // The answer's video sections are usable egress slots too.
        let video_egress = peer.mid_for_source(ParticipantId::new(), false).expect("video egress slot");
        assert!(peer.payload_type_for_codec(video_egress, Codec::Vp8).is_some(), "no VP8 writer on the egress mid");
        let _ = video_mid;
    }

    // ---- T11b review B fix-ups ----------------------------------------------

    /// P2-1 reviewer probe: after `DropSource` a late `Write` must not be handed
    /// a fresh mid. Only an explicit re-announcement (`EnsureSource`) may
    /// re-arm the publisher, and it gets a *fresh* mid while the old one stays
    /// retired.
    #[test]
    fn retired_source_gets_no_fresh_mid_until_explicitly_reannounced() {
        let mut peer = WebRtcPeer::accept_offer(&browser_offer_with(3, 3, true, true), B, &WebRtcConfig::default())
            .expect("accept");
        let publisher = ParticipantId::new();
        let old_audio = peer.mid_for_source(publisher, true).expect("audio slot");
        let old_video = peer.mid_for_source(publisher, false).expect("video slot");
        peer.retire_source(publisher);
        assert_eq!(peer.mid_for_source(publisher, true), None, "a late Write must not reallocate an audio mid");
        assert_eq!(peer.mid_for_source(publisher, false), None, "a late Write must not reallocate a video mid");
        assert_eq!(peer.source_for_mid(old_audio), None, "the retired source must be forgotten");
        assert_eq!(peer.source_for_mid(old_video), None);
        assert!(peer.mids.retired().contains(&old_audio), "old mid stays retired, never recycled");
        assert!(peer.mids.retired().contains(&old_video));

        // Explicit re-announcement (new generation): clears the mark and takes
        // a fresh mid from the pool.
        assert!(peer.ensure_source(publisher, true), "the returning publisher has a free audio slot");
        let fresh_audio = peer.mid_for_source(publisher, true).expect("fresh audio slot");
        assert_ne!(fresh_audio, old_audio, "the returning publisher must not reuse the retired mid");
        assert!(peer.mids.retired().contains(&old_audio));
        assert_eq!(peer.mid_for_source(publisher, false), None, "video was not re-announced yet");
        let other = ParticipantId::new();
        let other_mid = peer.mid_for_source(other, true).expect("another audio slot is still free");
        assert_ne!(other_mid, old_audio, "a different publisher never inherits a retired mid");
    }

    /// P3-1: one mid cannot be both an audio and a video m-line; the offer must
    /// be rejected with an explicit `Sdp` error before str0m allocates state.
    #[test]
    fn duplicate_mid_across_kinds_is_rejected_with_an_sdp_error() {
        let offer = browser_offer();
        let mids = mids_of(&offer);
        assert_eq!(mids.len(), 2, "fixture must have one audio and one video section");
        let duplicated = offer.replacen(&format!("a=mid:{}", mids[1]), &format!("a=mid:{}", mids[0]), 1);
        assert_ne!(duplicated, offer, "fixture must actually duplicate the audio mid on the video section");
        match WebRtcPeer::accept_offer(&duplicated, B, &WebRtcConfig::default()) {
            Err(WebRtcError::Sdp(message)) => {
                assert!(message.contains("duplicate a=mid"), "{message}");
                assert!(message.contains(&mids[0]), "{message}");
                assert!(message.contains("audio") && message.contains("video"), "{message}");
            }
            other => panic!("a cross-kind duplicate mid must be an explicit Sdp error, got {other:?}"),
        }
    }

    /// P3-1 scope: a duplicate mid *within* one kind predates T11b and is
    /// collapsed by str0m; only the cross-kind alias is ambiguous, so it stays
    /// accepted here by design.
    #[test]
    fn duplicate_mid_within_one_kind_is_left_to_str0m() {
        let offer = browser_offer_with(2, 0, true, true);
        let mids = mids_of(&offer);
        assert_eq!(mids.len(), 2);
        let duplicated = offer.replacen(&format!("a=mid:{}", mids[1]), &format!("a=mid:{}", mids[0]), 1);
        validate_offer(&duplicated).expect("single-kind duplicate is collapsed by str0m, not a cross-kind alias");
    }

    /// P3-2: a port-0 section is a rejected stream (RFC 3264 §6.1) and can
    /// never make the peer a publisher of that kind, even with `a=sendonly`.
    #[test]
    fn offer_publishes_ignores_port_zero_sections() {
        let offer = browser_offer();
        let muted_video = offer.replacen("m=video 9 ", "m=video 0 ", 1);
        assert_ne!(muted_video, offer);
        assert!(!offer_publishes(&muted_video, false), "a rejected video section cannot publish");
        assert!(offer_publishes(&muted_video, true), "the audio section still publishes");
        let muted_audio = offer.replacen("m=audio 9 ", "m=audio 0 ", 1);
        assert_ne!(muted_audio, offer);
        assert!(!offer_publishes(&muted_audio, true), "a rejected audio section cannot publish");
        assert!(offer_publishes(&muted_audio, false), "the video section still publishes");
    }

    /// P3-3: str0m answers a port-0 offer as `sendrecv` on a live port; the
    /// answer must reject that section per RFC 3264 §6.1 and it must never
    /// become an egress slot or a publish mid.
    #[test]
    fn port_zero_section_is_answered_inactive_and_never_becomes_an_egress_slot() {
        let offer = browser_offer();
        let muted = offer.replacen("m=video 9 ", "m=video 0 ", 1);
        assert_ne!(muted, offer);
        let mut peer = WebRtcPeer::accept_offer(&muted, B, &WebRtcConfig::default())
            .expect("a port-0 section is answered (rejected), not a fatal error");
        let video_header = peer.answer_sdp().lines().find(|line| line.starts_with("m=video")).expect("video m-line");
        assert_eq!(
            video_header.split_whitespace().nth(1),
            Some("0"),
            "answer must set the rejected section's port to 0: {video_header}"
        );
        assert!(peer.answer_sdp().lines().any(|line| line.trim() == "a=inactive"), "answer must mark it inactive");
        let video = peer.sections().iter().find(|section| !section.audio).expect("video section");
        assert_eq!(video.direction, MidDirection::Inactive, "a port-0 offer section is not usable");
        assert_eq!(peer.publish_mid(false), None, "a rejected camera section cannot be a publish mid");
        assert_eq!(peer.mid_for_source(ParticipantId::new(), false), None, "a rejected section is not an egress slot");
        assert!(peer.publish_mid(true).is_some(), "the accepted audio section keeps working");
    }
}

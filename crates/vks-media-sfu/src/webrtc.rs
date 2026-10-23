//! Real WebRTC transport on top of `str0m` (feature `str0m`).
//!
//! This is the bridge between VKS signalling/SFU and a browser peer:
//! ICE (host candidates, ICE-lite on the server), DTLS, SRTP/SRTCP,
//! NACK/RTX, jitter buffering, TWCC/REMB bandwidth estimation and
//! RTP/RTCP multiplexing are all implemented by `str0m`.
//!
//! The wrapper is deliberately sans-I/O: the caller owns the UDP socket,
//! feeds datagrams in, and sends every [`WebRtcAction::Send`] out. That keeps
//! the transport testable without a browser and lets the existing
//! `UdpMediaLoop` own socket lifecycle.
//!
//! Mutations follow str0m's single-mutation invariant: every public method
//! that mutates the inner [`Rtc`] drains `poll_output()` before returning.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use str0m::change::SdpOffer;
use str0m::format::{Codec, PayloadParams};
use str0m::media::{KeyframeRequestKind, MediaData, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig, RtcError};

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
    /// A browser asked for a video keyframe; forward it to the publisher.
    KeyframeRequest { audio: bool },
}

/// One passive WebRTC peer (browser → SFU).
pub struct WebRtcPeer {
    rtc: Rtc,
    answer_sdp: String,
    local_addr: SocketAddr,
    connected: bool,
    mids: Vec<Mid>,
    /// Parallel to `mids`; true for `m=audio`.
    mid_audio: Vec<bool>,
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
            // VKS SFU pipeline is Opus + VP8 only. Never let str0m's default
            // H264/VP9/AV1/H265 set leak into an answer the rest of the
            // server cannot forward or record.
            .clear_codecs()
            .enable_opus(true, false)
            .enable_vp8(true)
            .set_reordering_timeout_video(Some(std::time::Duration::from_millis(config.video_jitter_ms)))
            .set_reordering_size_video(config.video_reorder_size)
            .set_send_buffer_audio(config.send_buffer_audio)
            .set_send_buffer_video(config.send_buffer_video);
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
        let mut mids = Vec::new();
        let mut mid_audio = Vec::new();
        let mut current_audio = None;
        for line in answer_sdp.lines() {
            if line.starts_with("m=audio") {
                current_audio = Some(true);
            } else if line.starts_with("m=video") {
                current_audio = Some(false);
            } else if let Some(mid) = line.strip_prefix("a=mid:") {
                mids.push(Mid::from(mid));
                mid_audio.push(current_audio.unwrap_or(false));
            }
        }

        let mut peer = Self { rtc, answer_sdp, local_addr, connected: false, mids, mid_audio, next_timeout: None };
        // accept_offer mutates the session; drain the invariant before the
        // first datagram arrives. No remote input exists yet, so any output
        // here is a local ICE/DTLS action we must not silently keep queued.
        let _ = peer.drain()?;
        Ok(peer)
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
    pub fn mids(&self) -> &[Mid] {
        &self.mids
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

    /// The negotiated mid for an audio/video frame from the browser.
    pub fn mid_for_kind(&self, audio: bool) -> Option<Mid> {
        self.mids.iter().zip(&self.mid_audio).find(|(_, is_audio)| **is_audio == audio).map(|(mid, _)| *mid)
    }

    fn is_audio_mid(&self, mid: Mid) -> bool {
        self.mids.iter().zip(&self.mid_audio).find(|(candidate, _)| **candidate == mid).is_some_and(|(_, audio)| *audio)
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
                        if let Some(index) = self.mids.iter().position(|mid| *mid == added.mid) {
                            self.mid_audio[index] = matches!(added.kind, MediaKind::Audio);
                        }
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
                        actions.push(WebRtcAction::KeyframeRequest { audio: self.is_audio_mid(request.mid) });
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

/// Rejects offers this SFU cannot safely serve before str0m allocates state.
///
/// The real path supports exactly one audio + one video m-line per peer and
/// requires browser ICE/DTLS credentials. Application/data or extra m-lines
/// are rejected explicitly instead of being silently mis-mapped.
fn validate_offer(offer: &str) -> Result<(), WebRtcError> {
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
    for line in offer.lines() {
        if let Some(rest) = line.strip_prefix("m=") {
            if rest.starts_with("audio ") {
                audio += 1;
            } else if rest.starts_with("video ") {
                video += 1;
            } else {
                return Err(WebRtcError::InvalidOffer(format!("unsupported media line: {line}")));
            }
        }
    }
    if audio > 1 || video > 1 {
        return Err(WebRtcError::InvalidOffer(format!("too many media lines: audio={audio} video={video}")));
    }
    if audio == 0 && video == 0 {
        return Err(WebRtcError::InvalidOffer("no audio or video m-line".into()));
    }
    Ok(())
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
        assert_eq!(peer.mid_for_kind(true), Some(audio_id));
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
}

//! Dispatcher of signaling messages (SIGNALING_PROTOCOL §2–§5).
//!
//! `SignalingHandler` takes a decoded `Envelope` and returns the list of
//! reply `Envelope`s. Every accepted request drives the **Session FSM**
//! (`session.rs`): a request illegal for the current state gets an
//! `Error` reply (state unchanged).
//!
//! Transport-agnostic: works with any `WsSession`/`WebTransportSession`
//! (see `ws.rs`/`wt.rs`); replies are returned for the glue to send.

use vks_proto::media;
use vks_proto::signaling as sig;
use vks_proto::signaling::SignalingMessage;
use vks_proto::signaling::{signaling_message::Payload, IceComplete as ProtoIceComplete, SdpAnswer};

use crate::envelope::Envelope;
use crate::sdp::{RtpMap, Sdp, SfuAnswerProfile};
use crate::session::{Session, SessionEvent, SessionState};

/// (config/defaults)
const MIN_V2: u32 = 1;
const MAX_V2: u32 = 1;

/// (config/defaults)
#[derive(Debug, Clone)]
pub struct HandlerConfig {
    pub server_id: String,
    pub server_name: String,
    pub ufrag: String,
    pub pwd: String,
    pub fingerprint: String,
    pub candidate_addr: String,
    pub candidate_port: u16,
    pub rtp_maps: Vec<RtpMap>,
    pub ice_servers: Vec<sig::IceServer>,
    pub room_mode: i32,
    pub caps: Option<vks_proto::media::RoomCaps>,
    /// `b=AS` для SFU video m-line, kbit/s.
    pub video_bitrate_kbps: u32,
}

/// Parse/clamp `VKS_SFU_VIDEO_BITRATE_KBPS`: invalid/empty → 2048, range 64..20000.
fn video_bitrate_from(raw: Option<&str>) -> u32 {
    raw.and_then(|value| value.trim().parse::<u32>().ok()).unwrap_or(2048).clamp(64, 20_000)
}

impl HandlerConfig {
    /// Создаёт конфигурацию обработчика с явными параметрами.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        server_id: impl Into<String>,
        server_name: impl Into<String>,
        ufrag: impl Into<String>,
        pwd: impl Into<String>,
        fingerprint: impl Into<String>,
        candidate_addr: impl Into<String>,
        candidate_port: u16,
        rtp_maps: Vec<RtpMap>,
    ) -> Self {
        Self {
            server_id: server_id.into(),
            server_name: server_name.into(),
            ufrag: ufrag.into(),
            pwd: pwd.into(),
            fingerprint: fingerprint.into(),
            candidate_addr: candidate_addr.into(),
            candidate_port,
            rtp_maps,
            ice_servers: Vec::new(),
            room_mode: 1, // ROOM_MODE_SFU
            caps: Some(vks_proto::media::RoomCaps {
                room_mode: 1,
                video_codecs: vec![1], // VP8; H.264 browser-leg не мостится в SFU до T06b
                audio_codecs: vec![6], // OPUS
                simulcast: true,
                svc: false,
                dtls_srtp: true,
            }),
            video_bitrate_kbps: video_bitrate_from(std::env::var("VKS_SFU_VIDEO_BITRATE_KBPS").ok().as_deref()),
        }
    }

    /// Переопределяет `b=AS` видео для SFU-ответа (kbit/s).
    pub fn with_video_bitrate(mut self, kbps: u32) -> Self {
        self.video_bitrate_kbps = kbps.clamp(64, 20_000);
        self
    }

    /// Задает список ICE-серверов (STUN/TURN).
    pub fn with_ice_servers(mut self, servers: Vec<sig::IceServer>) -> Self {
        self.ice_servers = servers;
        self
    }

    /// Задает режим комнаты (M0.5, 1 = SFU, 2 = MCU).
    pub fn with_room_mode(mut self, mode: i32) -> Self {
        self.room_mode = mode;
        if let Some(c) = &mut self.caps {
            c.room_mode = mode;
        }
        self
    }

    /// Задает возможности комнаты (M0.5).
    pub fn with_caps(mut self, caps: vks_proto::media::RoomCaps) -> Self {
        self.caps = Some(caps);
        self
    }

    /// (config/defaults)
    pub fn default_test() -> Self {
        Self {
            server_id: "srv-0001".into(),
            server_name: "vks-signaling-test".into(),
            ufrag: "sfuufrag".into(),
            pwd: "sfupwd".into(),
            fingerprint: "sha-256 FF:00:11".into(),
            candidate_addr: "203.0.113.7".into(),
            candidate_port: 49152,
            rtp_maps: vec![
                RtpMap { payload: 111, codec: "opus".into(), clock: 48000, channels: 2 },
                RtpMap { payload: 96, codec: "vp8".into(), clock: 90000, channels: 0 },
            ],
            ice_servers: Vec::new(),
            room_mode: 1,
            caps: Some(vks_proto::media::RoomCaps {
                room_mode: 1,
                video_codecs: vec![1],
                audio_codecs: vec![6],
                simulcast: true,
                svc: false,
                dtls_srtp: true,
            }),
            video_bitrate_kbps: 2048,
        }
    }

    /// (config/defaults)
    pub fn answer_profile(&self) -> SfuAnswerProfile {
        SfuAnswerProfile {
            rtp_maps: self.rtp_maps.clone(),
            ufrag: self.ufrag.clone(),
            pwd: self.pwd.clone(),
            host_candidate: crate::sdp::IceCandidate {
                ufrag: self.ufrag.clone(),
                pwd: self.pwd.clone(),
                addr: self.candidate_addr.clone(),
                port: self.candidate_port,
                typ: "host".into(),
            },
            fingerprint: self.fingerprint.clone(),
            b_as: Some(self.video_bitrate_kbps.clamp(64, 20_000)),
            fmtp: Vec::new(),
            rtcp_fb: vec![
                "96 nack".into(),
                "96 nack pli".into(),
                "96 ccm fir".into(),
                "96 goog-remb".into(),
                "96 transport-cc".into(),
            ],
            extmap: vec![
                "1 urn:ietf:params:rtp-hdrext:sdes:mid".into(),
                "3 http://www.ietf.org/id/draft-holmer-rmcat-transport-wide-cc-extensions-01".into(),
            ],
        }
    }
}

/// (config/defaults)
pub struct SignalingHandler {
    session: Session,
    config: HandlerConfig,
    room_id: Option<String>,
    participant_id: Option<String>,
    /// State before the latest SdpOffer, so a transport rejection can roll back.
    pre_offer_state: Option<SessionState>,
}

impl SignalingHandler {
    pub fn new(config: HandlerConfig) -> Self {
        Self { session: Session::new(), config, room_id: None, participant_id: None, pre_offer_state: None }
    }

    pub fn state(&self) -> SessionState {
        self.session.state()
    }

    pub fn participant_id(&self) -> Option<&str> {
        self.participant_id.as_deref()
    }

    pub fn room_id(&self) -> Option<&str> {
        self.room_id.as_deref()
    }

    /// Rolls back an already-applied SdpOffer when the media transport rejected
    /// it, so the client can retry on the same WebSocket.
    pub fn rollback_offer(&mut self) {
        if let Some(state) = self.pre_offer_state.take() {
            self.session.restore(state);
        }
    }

    /// Текущая конфигурация ответа (для тестов и диагностики).
    pub fn config(&self) -> &HandlerConfig {
        &self.config
    }

    /// Переопределяет `b=AS` видео из живого профиля комнаты.
    pub fn set_video_bitrate(&mut self, kbps: u32) {
        self.config.video_bitrate_kbps = kbps.clamp(64, 20_000);
    }

    /// (config/defaults)
    pub fn handle(&mut self, env: Envelope) -> Vec<Envelope> {
        let reply_to = env.msg_id();
        match env.payload().as_ref() {
            Some(Payload::ClientHello(h)) => self.on_client_hello(h, reply_to),
            Some(Payload::JoinRoom(j)) => self.on_join_room(j, reply_to),
            Some(Payload::SdpOffer(o)) => self.on_sdp_offer(o, reply_to),
            Some(Payload::IceCandidate(_)) => self.on_ice_candidate(reply_to),
            Some(Payload::IceComplete(c)) => self.on_ice_complete(c, reply_to),
            Some(Payload::LeaveRoom(_)) => self.on_leave_room(reply_to),
            Some(Payload::SubscribeRequest(_)) => self.on_subscribe_room(reply_to),
            Some(Payload::Unsubscribe(_)) => self.on_unsubscribe(reply_to),
            Some(Payload::QualityReport(_)) => self.on_ack(reply_to),
            Some(Payload::Announcement(_)) => self.on_ack(reply_to),
            Some(Payload::ServerHello(_))
            | Some(Payload::JoinRoomResult(_))
            | Some(Payload::SdpAnswer(_))
            | Some(Payload::SubscribeResponse(_))
            | Some(Payload::TrackAdded(_))
            | Some(Payload::TrackRemoved(_))
            | Some(Payload::LayoutChanged(_))
            | Some(Payload::RoomModeChanged(_))
            | Some(Payload::ParticipantJoined(_))
            | Some(Payload::ParticipantLeft(_))
            | Some(Payload::SpeakingStateChanged(_))
            | Some(Payload::Error(_))
            | Some(Payload::Ack(_))
            | Some(Payload::ServerMaintaining(_)) => Vec::new(),
            None => vec![Envelope::error_reply(reply_to, "no_payload", "envelope has no payload", true)],
        }
    }

    fn on_ack(&self, reply_to: u64) -> Vec<Envelope> {
        vec![Envelope::ack_reply(reply_to)]
    }

    fn make_participant(pid: &str) -> sig::ParticipantInfo {
        sig::ParticipantInfo {
            participant_id: pid.into(),
            display_name: "participant".into(),
            is_recorder: false,
            is_speaking: false,
            track_ids: Vec::new(),
        }
    }

    fn on_client_hello(&mut self, hello: &sig::ClientHello, reply_to: u64) -> Vec<Envelope> {
        if !(MIN_V2..=MAX_V2).contains(&hello.proto_version) {
            let _ = self.session.apply(SessionEvent::Disconnect);
            return vec![Envelope::error_reply(
                reply_to,
                "version_mismatch",
                &format!("proto_version {} not in [{}..={}]", hello.proto_version, MIN_V2, MAX_V2),
                false,
            )];
        }
        if let Err(e) = self.session.apply(SessionEvent::Hello) {
            return vec![Envelope::error_reply(reply_to, "bad_state", &format!("ClientHello rejected: {e}"), false)];
        }
        if let Err(e) = self.session.apply(SessionEvent::AuthOk) {
            return vec![Envelope::error_reply(reply_to, "internal_error", &format!("authOk: {e}"), false)];
        }
        self.room_id = Some(hello.room_id.clone());
        let ice = if self.config.ice_servers.is_empty() {
            None
        } else {
            Some(sig::IceConfig { servers: self.config.ice_servers.clone() })
        };
        let server_hello = sig::ServerHello {
            proto_version: MAX_V2,
            min_supported: MIN_V2,
            max_supported: MAX_V2,
            server_id: self.config.server_id.clone(),
            server_name: self.config.server_name.clone(),
            room_mode: self.config.room_mode,
            caps: self.config.caps.clone(),
            ice,
        };
        vec![Envelope::new(SignalingMessage { msg_id: 0, reply_to, payload: Some(Payload::ServerHello(server_hello)) })]
    }

    fn on_join_room(&mut self, join: &sig::JoinRoom, reply_to: u64) -> Vec<Envelope> {
        if self.session.state() != SessionState::Connected {
            let state = self.session.state();
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("JoinRoom requires Connected (current: {state:?})"),
                false,
            )];
        }
        if join.room_id.trim().is_empty() {
            return vec![Envelope::error_reply(reply_to, "invalid_room", "room_id cannot be empty", false)];
        }
        if let Some(ref expected_room) = self.room_id {
            if !expected_room.is_empty() && expected_room != &join.room_id {
                return vec![Envelope::error_reply(
                    reply_to,
                    "room_mismatch",
                    &format!(
                        "JoinRoom room_id '{}' does not match ClientHello room_id '{}'",
                        join.room_id, expected_room
                    ),
                    false,
                )];
            }
        }
        if let Err(e) = self.session.apply(SessionEvent::JoinRoom) {
            return vec![Envelope::error_reply(reply_to, "bad_state", &format!("join: {e}"), false)];
        }
        let room_id = join.room_id.clone();
        self.room_id = Some(room_id.clone());
        if let Err(e) = self.session.apply(SessionEvent::Joined) {
            return vec![Envelope::error_reply(reply_to, "internal_error", &format!("joined: {e}"), false)];
        }
        let participant_id = format!("part-{}-{}", room_id, uuid::Uuid::now_v7());
        self.participant_id = Some(participant_id.clone());
        let join_nonce = (uuid::Uuid::now_v7().as_u128() as u64) | 1;
        let result = sig::JoinRoomResult {
            participant_id: participant_id.clone(),
            join_nonce,
            participants: vec![Self::make_participant(&participant_id)],
            publisher_tracks: Vec::new(),
            layout: None,
            caps: None,
        };
        vec![Envelope::new(SignalingMessage { msg_id: 0, reply_to, payload: Some(Payload::JoinRoomResult(result)) })]
    }

    fn on_sdp_offer(&mut self, offer: &sig::SdpOffer, reply_to: u64) -> Vec<Envelope> {
        let state = self.session.state();
        if !matches!(state, SessionState::InRoom | SessionState::MediaActive) {
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("SdpOffer requires InRoom or MediaActive (current: {state:?})"),
                false,
            )];
        }
        let offer_doc = match Sdp::parse(&offer.sdp) {
            Ok(d) => d,
            Err(e) => {
                return vec![Envelope::error_reply(reply_to, "sdp_parse_error", &e.to_string(), true)];
            }
        };
        self.pre_offer_state = Some(state);
        if let Err(e) = self.session.apply(SessionEvent::SdpOffer) {
            return vec![Envelope::error_reply(reply_to, "bad_state", &format!("offer: {e}"), false)];
        }
        let profile = self.config.answer_profile();
        let (answer_doc, rtp_maps) = offer_doc.sfu_answer(&profile);
        let answer = answer_doc.with_ice_candidates(&[profile.host_candidate]);
        let proto_maps = rtp_maps
            .iter()
            .map(|r| sig::RtpMap { payload: r.payload, codec: r.codec.clone(), clock: r.clock, channels: r.channels })
            .collect();
        let answer_msg = SdpAnswer { sdp: answer.to_sdp_text(), rtp_map: proto_maps };
        vec![Envelope::new(SignalingMessage { msg_id: 0, reply_to, payload: Some(Payload::SdpAnswer(answer_msg)) })]
    }

    fn on_ice_candidate(&mut self, reply_to: u64) -> Vec<Envelope> {
        let state = self.session.state();
        if !matches!(state, SessionState::Negotiating | SessionState::MediaActive | SessionState::InRoom) {
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("IceCandidate requires active session (current: {state:?})"),
                false,
            )];
        }
        if reply_to != 0 {
            self.on_ack(reply_to)
        } else {
            Vec::new()
        }
    }

    fn on_ice_complete(&mut self, c: &ProtoIceComplete, reply_to: u64) -> Vec<Envelope> {
        if self.session.state() != SessionState::Negotiating {
            let state = self.session.state();
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("IceComplete requires Negotiating (current: {state:?})"),
                false,
            )];
        }
        if !c.success {
            // The dead peer must not leave the session stuck in Negotiating;
            // roll back so the client can retry on the same socket.
            self.rollback_offer();
            return vec![Envelope::error_reply(reply_to, "ice_failed", "ICE negotiation failed", false)];
        }
        self.pre_offer_state = None;
        if let Err(e) = self.session.apply(SessionEvent::IceComplete) {
            return vec![Envelope::error_reply(reply_to, "bad_state", &format!("ice: {e}"), false)];
        }
        self.on_ack(reply_to)
    }

    fn on_leave_room(&mut self, reply_to: u64) -> Vec<Envelope> {
        let state = self.session.state();
        if !matches!(
            state,
            SessionState::Joining
                | SessionState::InRoom
                | SessionState::Negotiating
                | SessionState::MediaActive
                | SessionState::Reconnecting
        ) {
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("LeaveRoom requires active room state (current: {state:?})"),
                false,
            )];
        }
        if let Err(e) = self.session.apply(SessionEvent::LeaveRoom) {
            return vec![Envelope::error_reply(reply_to, "bad_state", &format!("leave: {e}"), false)];
        }
        if let Err(e) = self.session.apply(SessionEvent::Left) {
            return vec![Envelope::error_reply(reply_to, "internal_error", &format!("left: {e}"), false)];
        }
        self.room_id = None;
        self.on_ack(reply_to)
    }

    fn on_subscribe_room(&mut self, reply_to: u64) -> Vec<Envelope> {
        let state = self.session.state();
        if !matches!(state, SessionState::InRoom | SessionState::MediaActive) {
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("SubscribeRequest requires InRoom/MediaActive (current: {state:?})"),
                false,
            )];
        }
        let resp = media::SubscribeResponse { ok: true, streams: Vec::new(), error_code: String::new() };
        vec![Envelope::new(SignalingMessage { msg_id: 0, reply_to, payload: Some(Payload::SubscribeResponse(resp)) })]
    }

    fn on_unsubscribe(&mut self, reply_to: u64) -> Vec<Envelope> {
        let state = self.session.state();
        if !matches!(state, SessionState::InRoom | SessionState::MediaActive) {
            return vec![Envelope::error_reply(
                reply_to,
                "bad_state",
                &format!("Unsubscribe requires InRoom/MediaActive (current: {state:?})"),
                false,
            )];
        }
        self.on_ack(reply_to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ws::MockWs;
    use crate::wt::{MockWt, WebTransportSession};

    fn hello(msg_id: u64, proto_version: u32) -> Envelope {
        let msg = sig::ClientHello {
            proto_version,
            client_id: "c1".into(),
            sdk_version: "1.0".into(),
            user_agent: "test".into(),
            auth_token: "tok".into(),
            room_id: "r1".into(),
            capabilities: Vec::new(),
            resume_nonce: None,
        };
        Envelope::new(SignalingMessage { msg_id, reply_to: 0, payload: Some(Payload::ClientHello(msg)) })
    }

    fn join(msg_id: u64, room_id: &str) -> Envelope {
        let msg = sig::JoinRoom { room_id: room_id.into(), display_name: "alice".into(), is_recorder: false };
        Envelope::new(SignalingMessage { msg_id, reply_to: 0, payload: Some(Payload::JoinRoom(msg)) })
    }

    fn offer(msg_id: u64, sdp: &str) -> Envelope {
        let msg = sig::SdpOffer { sdp: sdp.into() };
        Envelope::new(SignalingMessage { msg_id, reply_to: 0, payload: Some(Payload::SdpOffer(msg)) })
    }

    const OFFER_2: &str = "v=0\no=c1 1 1 0 IN IP4 0.0.0.0\ns=test\nm=audio 9 UDP/TLS/RTP/SAVPF 111\na=mid:0\na=rtpmap:111 opus/48000/2\nm=video 9 UDP/TLS/RTP/SAVPF 96\na=mid:1\na=rtpmap:96 VP8/90000";

    #[test]
    fn rejected_offer_can_be_retried_after_rollback() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.handle(hello(1, 1));
        handler.handle(join(2, "r1"));
        handler.handle(offer(3, OFFER_2));
        assert_eq!(handler.state(), SessionState::Negotiating);
        // The server rejected the real transport and rolled the FSM back.
        handler.rollback_offer();
        assert_eq!(handler.state(), SessionState::InRoom);
        let retry = handler.handle(offer(4, OFFER_2));
        assert!(matches!(retry[0].payload(), Some(Payload::SdpAnswer(_))));
        assert_eq!(handler.state(), SessionState::Negotiating);
    }

    #[test]
    fn ice_failure_rolls_back_to_in_room() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.handle(hello(1, 1));
        handler.handle(join(2, "r1"));
        handler.handle(offer(3, OFFER_2));
        assert_eq!(handler.state(), SessionState::Negotiating);
        let reply = handler.handle(Envelope::new(SignalingMessage {
            msg_id: 4,
            reply_to: 0,
            payload: Some(Payload::IceComplete(sig::IceComplete { success: false, rtt_ms: 0 })),
        }));
        assert!(matches!(reply[0].payload(), Some(Payload::Error(_))));
        assert_eq!(handler.state(), SessionState::InRoom);
        let retry = handler.handle(offer(5, OFFER_2));
        assert!(matches!(retry[0].payload(), Some(Payload::SdpAnswer(_))));
    }

    #[test]
    fn full_happy_path_over_mock_ws() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        let _mock = MockWs::new();

        // ClientHello → ServerHello.
        let resp = handler.handle(hello(1, 1));
        assert_eq!(handler.state(), SessionState::Connected);
        match &resp[0].payload() {
            Some(Payload::ServerHello(sh)) => {
                assert_eq!(sh.server_id, "srv-0001");
                assert_eq!(sh.max_supported, 1);
            }
            other => panic!("expected ServerHello, got {other:?}"),
        }

        // JoinRoom → JoinRoomResult (join_nonce).
        let resp = handler.handle(join(2, "r1"));
        assert_eq!(handler.state(), SessionState::InRoom);
        match &resp[0].payload() {
            Some(Payload::JoinRoomResult(r)) => {
                assert!(!r.participant_id.is_empty());
                assert_ne!(r.join_nonce, 0);
            }
            other => panic!("expected JoinRoomResult, got {other:?}"),
        }
        assert!(handler.participant_id().unwrap().starts_with("part-r1-"));
        assert_eq!(handler.room_id(), Some("r1"));

        // SdpOffer → SdpAnswer (m-lines preserved, ICE-candidates added).
        let resp = handler.handle(offer(3, OFFER_2));
        assert_eq!(handler.state(), SessionState::Negotiating);
        match &resp[0].payload() {
            Some(Payload::SdpAnswer(a)) => {
                let sdp = &a.sdp;
                assert!(sdp.lines().any(|l| l.starts_with("m=audio")));
                assert!(sdp.lines().any(|l| l.starts_with("m=video")));
                assert!(sdp.contains("ice-ufrag"));
                assert!(!a.rtp_map.is_empty());
            }
            other => panic!("expected SdpAnswer, got {other:?}"),
        }

        // IceComplete → Ack, MediaActive.
        let ice = Envelope::new(SignalingMessage {
            msg_id: 4,
            reply_to: 0,
            payload: Some(Payload::IceComplete(sig::IceComplete { success: true, rtt_ms: 0 })),
        });
        let resp = handler.handle(ice);
        assert_eq!(handler.state(), SessionState::MediaActive);
        assert!(matches!(resp[0].payload(), Some(Payload::Ack(_))));

        // LeaveRoom → Ack, Disconnected.
        let leave = Envelope::new(SignalingMessage {
            msg_id: 5,
            reply_to: 0,
            payload: Some(Payload::LeaveRoom(sig::LeaveRoom { room_id: "r1".into() })),
        });
        let resp = handler.handle(leave);
        assert_eq!(handler.state(), SessionState::Disconnected);
        assert!(matches!(resp[0].payload(), Some(Payload::Ack(_))));
    }

    #[test]
    fn sdp_offer_before_join_is_bad_state() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.handle(hello(1, 1));
        let resp = handler.handle(offer(2, "v=0"));
        assert_eq!(handler.state(), SessionState::Connected);
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "bad_state"));
    }

    #[test]
    fn version_mismatch_replaced() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        let resp = handler.handle(hello(1, 99));
        assert_eq!(handler.state(), SessionState::Disconnected);
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "version_mismatch"));
    }

    #[test]
    fn transport_agnostic_with_mock_wt() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        let mut wt = MockWt::new();
        for env in [hello(1, 1), join(2, "r1"), offer(3, OFFER_2)] {
            let r = handler.handle(env);
            wt.send(r[0].encode());
        }
        assert_eq!(wt.take_sent().len(), 3);
        assert_eq!(handler.state(), SessionState::Negotiating);
    }

    #[test]
    fn no_payload_is_error() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        let env = Envelope::new(SignalingMessage { msg_id: 1, reply_to: 0, payload: None });
        let resp = handler.handle(env);
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "no_payload"));
    }

    #[test]
    fn join_before_hello_is_bad_state() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        let resp = handler.handle(join(1, "r1"));
        assert_eq!(handler.state(), SessionState::Disconnected);
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "bad_state"));
    }

    #[test]
    fn server_hello_includes_ice_servers_when_configured() {
        let config = HandlerConfig::default_test().with_ice_servers(vec![sig::IceServer {
            url: "stun:stun.l.google.com:19302".into(),
            username: "".into(),
            credential: "".into(),
        }]);
        let mut handler = SignalingHandler::new(config);
        let resp = handler.handle(hello(1, 1));
        match resp[0].payload() {
            Some(Payload::ServerHello(h)) => {
                assert!(h.ice.is_some());
                let servers = &h.ice.as_ref().unwrap().servers;
                assert_eq!(servers.len(), 1);
                assert_eq!(servers[0].url, "stun:stun.l.google.com:19302");
            }
            other => panic!("expected ServerHello, got {other:?}"),
        }
    }

    #[test]
    fn join_room_mismatch_rejected() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.handle(hello(1, 1)); // sets room_id: "r1"
        let resp = handler.handle(join(2, "different_room"));
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "room_mismatch"));
    }

    #[test]
    fn join_empty_room_rejected() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.handle(hello(1, 1));
        let resp = handler.handle(join(2, "   "));
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "invalid_room"));
    }

    #[test]
    fn ice_complete_failure_rejected() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.handle(hello(1, 1));
        handler.handle(join(2, "r1"));
        handler.handle(offer(3, OFFER_2));
        assert_eq!(handler.state(), SessionState::Negotiating);

        let ice_failed = Envelope::new(SignalingMessage {
            msg_id: 4,
            reply_to: 0,
            payload: Some(Payload::IceComplete(sig::IceComplete { success: false, rtt_ms: 0 })),
        });
        let resp = handler.handle(ice_failed);
        assert_ne!(handler.state(), SessionState::MediaActive);
        assert!(matches!(resp[0].payload(), Some(Payload::Error(e)) if e.code == "ice_failed"));
    }

    #[test]
    fn room_video_bitrate_setter_reaches_answer_with_clamp() {
        let mut handler = SignalingHandler::new(HandlerConfig::default_test());
        handler.set_video_bitrate(4096);
        assert_eq!(handler.config.answer_profile().b_as, Some(4096));
        handler.set_video_bitrate(0);
        assert_eq!(handler.config.answer_profile().b_as, Some(64));
        handler.set_video_bitrate(100_000);
        assert_eq!(handler.config.answer_profile().b_as, Some(20_000));
    }

    #[test]
    fn sfu_video_bitrate_is_clamped_in_constructor_and_answer() {
        assert_eq!(video_bitrate_from(Some("0")), 64);
        assert_eq!(video_bitrate_from(Some("100000")), 20_000);
        assert_eq!(video_bitrate_from(Some("4096")), 4096);
        assert_eq!(video_bitrate_from(Some("junk")), 2048);
        assert_eq!(video_bitrate_from(None), 2048);
        let mut low = HandlerConfig::default_test();
        low.video_bitrate_kbps = 0;
        assert_eq!(low.answer_profile().b_as, Some(64));
        let mut high = HandlerConfig::default_test();
        high.video_bitrate_kbps = 100_000;
        assert_eq!(high.answer_profile().b_as, Some(20_000));
        let configured = HandlerConfig::default_test().with_video_bitrate(4096);
        assert_eq!(configured.answer_profile().b_as, Some(4096));
    }
}

//! Room/publisher/subscriber registry behind the real WebRTC plane.
//!
//! The plaintext MCU (forwarder, BWE, simulcast, UDP loop) was removed in
//! T11c. What remains of the former `SfuEngine` is the *participant roster*
//! half that the signalling glue still needs: which rooms exist, which
//! participants publish which tracks, who subscribes to what, and the room
//! capabilities. Media itself never passes through here — every byte is
//! carried by a per-peer str0m session ([`crate::webrtc_room`]).
//!
//! Keeping this a separate, small registry (instead of a `SfuEngine` with a
//! forwarding path) makes the honesty explicit: there is no server-side RTP
//! routing to configure, only bookkeeping for `TrackAdded`, layout seating and
//! subscription fanout.

use std::collections::BTreeMap;
use std::sync::Mutex;

use async_trait::async_trait;
use vks_core::ports::media_engine::{
    LayoutDescriptor, MediaEngine, MediaEngineError, MediaTrack, PublisherHandle, RoomCaps, RoomMediaCfg,
    RoomMediaHandle, SubscribeRequest, SubscriberHandle,
};
use vks_core::{ParticipantId, RoomId, RoomMode, TrackId};

/// Per-room bookkeeping. No forwarder, no sockets, no bandwidth state.
#[derive(Debug)]
struct RoomState {
    mode: RoomMode,
    caps: RoomCaps,
    /// Publisher → its tracks.
    publisher_tracks: BTreeMap<ParticipantId, Vec<MediaTrack>>,
    /// Active tracks in the room: track_id → MediaTrack.
    tracks: BTreeMap<TrackId, MediaTrack>,
    /// Subscriber → requested subscription.
    subscribers: BTreeMap<ParticipantId, SubscribeRequest>,
}

impl RoomState {
    fn new(cfg: &RoomMediaCfg) -> Self {
        Self {
            mode: cfg.mode,
            caps: cfg.caps.clone(),
            publisher_tracks: BTreeMap::new(),
            tracks: BTreeMap::new(),
            subscribers: BTreeMap::new(),
        }
    }

    /// Both publishers and subscribers count towards `max_participants`;
    /// a participant present in either role is not admitted twice.
    fn admits(&self, participant: ParticipantId) -> bool {
        let Some(max) = self.caps.max_participants else { return true };
        let existing = self.publisher_tracks.contains_key(&participant) || self.subscribers.contains_key(&participant);
        if existing {
            return true;
        }
        let unique: std::collections::BTreeSet<&ParticipantId> =
            self.publisher_tracks.keys().chain(self.subscribers.keys()).collect();
        (unique.len() as u32) < max
    }
}

/// Process-local registry of rooms and their participant roster.
#[derive(Debug)]
pub struct MediaRegistry {
    rooms: Mutex<BTreeMap<RoomId, RoomState>>,
}

impl Default for MediaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl MediaRegistry {
    pub fn new() -> Self {
        Self { rooms: Mutex::new(BTreeMap::new()) }
    }

    /// Track kinds already registered for a publisher.
    ///
    /// Signalling uses this to announce only genuinely new tracks on a
    /// re-offer instead of duplicating `TrackAdded` for every m-line.
    pub fn publisher_track_kinds(&self, room: RoomId, publisher: ParticipantId) -> Vec<vks_core::MediaKind> {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&room)
            .and_then(|state| state.publisher_tracks.get(&publisher))
            .map(|tracks| tracks.iter().map(|track| track.media_kind).collect())
            .unwrap_or_default()
    }
}

#[async_trait]
impl MediaEngine for MediaRegistry {
    fn mode(&self) -> RoomMode {
        RoomMode::Sfu
    }

    async fn create_room(&self, cfg: RoomMediaCfg) -> Result<RoomMediaHandle, MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if rooms.contains_key(&cfg.room_id) {
            return Err(MediaEngineError::RoomAlreadyExists(cfg.room_id));
        }
        rooms.insert(cfg.room_id, RoomState::new(&cfg));
        Ok(RoomMediaHandle { room: cfg.room_id, mode: cfg.mode })
    }

    async fn destroy_room(&self, room: RoomId) -> Result<(), MediaEngineError> {
        let removed = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&room);
        removed.map(|_| ()).ok_or(MediaEngineError::RoomNotFound(room))
    }

    async fn switch_room_mode(&self, room: RoomId, new_mode: RoomMode) -> Result<RoomMode, MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.mode = new_mode;
        Ok(new_mode)
    }

    async fn add_publisher(
        &self,
        room: RoomId,
        publisher: ParticipantId,
        tracks: Vec<MediaTrack>,
    ) -> Result<PublisherHandle, MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        if !state.admits(publisher) {
            return Err(MediaEngineError::Other(format!(
                "room capacity exceeded: max {} participants",
                state.caps.max_participants.unwrap_or(0)
            )));
        }
        if let Some(old_tracks) = state.publisher_tracks.get(&publisher) {
            for old in old_tracks {
                if !tracks.iter().any(|track| track.id == old.id) {
                    state.tracks.remove(&old.id);
                }
            }
        }
        for track in &tracks {
            state.tracks.insert(track.id, track.clone());
        }
        state.publisher_tracks.insert(publisher, tracks);
        Ok(PublisherHandle { room, publisher })
    }

    async fn remove_publisher(&self, room: RoomId, publisher: ParticipantId) -> Result<(), MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        if let Some(tracks) = state.publisher_tracks.remove(&publisher) {
            for track in tracks {
                state.tracks.remove(&track.id);
            }
        }
        Ok(())
    }

    async fn add_subscriber(
        &self,
        room: RoomId,
        subscriber: ParticipantId,
        req: SubscribeRequest,
    ) -> Result<SubscriberHandle, MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        if !state.admits(subscriber) {
            return Err(MediaEngineError::Other(format!(
                "room capacity exceeded: max {} participants",
                state.caps.max_participants.unwrap_or(0)
            )));
        }
        state.subscribers.insert(subscriber, req);
        Ok(SubscriberHandle { room, subscriber })
    }

    async fn remove_subscriber(&self, room: RoomId, subscriber: ParticipantId) -> Result<(), MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.subscribers.remove(&subscriber);
        Ok(())
    }

    async fn apply_layout(&self, room: RoomId, _layout: LayoutDescriptor) -> Result<(), MediaEngineError> {
        // SFU layout is client-side; the registry only validates the room.
        let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        rooms.contains_key(&room).then_some(()).ok_or(MediaEngineError::RoomNotFound(room))
    }

    async fn set_room_capabilities(&self, room: RoomId, caps: RoomCaps) -> Result<(), MediaEngineError> {
        let mut rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.caps = caps;
        Ok(())
    }

    async fn list_publishers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(state.publisher_tracks.keys().copied().collect())
    }

    async fn list_subscribers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        let rooms = self.rooms.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let state = rooms.get(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(state.subscribers.keys().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vks_core::track::Track;

    fn room_id() -> RoomId {
        RoomId::from_uuid("30000000-0000-0000-0000-000000000001".parse().unwrap())
    }
    fn p(n: u64) -> ParticipantId {
        ParticipantId::from_uuid(format!("10000000-0000-0000-0000-{n:012}").parse().unwrap())
    }
    fn track(pid: ParticipantId, kind: vks_core::TrackKind) -> Track {
        Track::new(pid, kind)
    }
    fn cfg(rid: RoomId) -> RoomMediaCfg {
        RoomMediaCfg { room_id: rid, mode: RoomMode::Sfu, caps: RoomCaps::default() }
    }
    fn sub() -> SubscribeRequest {
        SubscribeRequest { track_ids: Vec::new(), audio_only: false }
    }

    #[tokio::test]
    async fn lifecycle_room_publishers_and_subscribers() {
        let registry = MediaRegistry::new();
        let rid = room_id();
        registry.create_room(cfg(rid)).await.expect("create");
        let alice = p(1);
        let bob = p(2);
        let sub_a = p(10);
        let sub_b = p(11);

        registry
            .add_publisher(rid, alice, vec![track(alice, vks_core::TrackKind::Audio)])
            .await
            .expect("alice publishes audio");
        registry
            .add_publisher(rid, bob, vec![track(bob, vks_core::TrackKind::Video)])
            .await
            .expect("bob publishes video");
        registry.add_subscriber(rid, sub_a, sub()).await.expect("subscriber a");
        registry.add_subscriber(rid, sub_b, sub()).await.expect("subscriber b");

        assert_eq!(registry.list_publishers(rid).await.expect("publishers"), vec![alice, bob]);
        assert_eq!(registry.list_subscribers(rid).await.expect("subscribers"), vec![sub_a, sub_b]);
        assert_eq!(registry.publisher_track_kinds(rid, alice), vec![vks_core::MediaKind::Audio]);
        assert_eq!(registry.publisher_track_kinds(rid, bob), vec![vks_core::MediaKind::Video]);
        assert!(registry.publisher_track_kinds(rid, sub_a).is_empty());

        registry.remove_publisher(rid, alice).await.expect("remove publisher");
        registry.remove_subscriber(rid, sub_b).await.expect("remove subscriber");
        assert_eq!(registry.list_publishers(rid).await.expect("publishers"), vec![bob]);
        assert_eq!(registry.list_subscribers(rid).await.expect("subscribers"), vec![sub_a]);
        assert!(registry.publisher_track_kinds(rid, alice).is_empty());
    }

    #[tokio::test]
    async fn reoffer_withdraws_only_removed_tracks() {
        let registry = MediaRegistry::new();
        let rid = room_id();
        registry.create_room(cfg(rid)).await.expect("create");
        let alice = p(1);
        let audio = track(alice, vks_core::TrackKind::Audio);
        let video = track(alice, vks_core::TrackKind::Video);
        registry.add_publisher(rid, alice, vec![audio.clone(), video.clone()]).await.expect("publish both");
        registry.add_publisher(rid, alice, vec![audio.clone()]).await.expect("withdraw video");
        assert_eq!(registry.publisher_track_kinds(rid, alice), vec![vks_core::MediaKind::Audio]);
    }

    #[tokio::test]
    async fn missing_room_errors_for_every_operation() {
        let registry = MediaRegistry::new();
        let rid = room_id();
        let pid = p(1);
        assert!(matches!(registry.destroy_room(rid).await, Err(MediaEngineError::RoomNotFound(_))));
        assert!(matches!(registry.add_publisher(rid, pid, Vec::new()).await, Err(MediaEngineError::RoomNotFound(_))));
        assert!(matches!(registry.add_subscriber(rid, pid, sub()).await, Err(MediaEngineError::RoomNotFound(_))));
        assert!(matches!(registry.list_publishers(rid).await, Err(MediaEngineError::RoomNotFound(_))));
        assert!(matches!(registry.list_subscribers(rid).await, Err(MediaEngineError::RoomNotFound(_))));
        assert!(matches!(
            registry.set_room_capabilities(rid, RoomCaps::default()).await,
            Err(MediaEngineError::RoomNotFound(_))
        ));
        assert!(matches!(registry.switch_room_mode(rid, RoomMode::Mcu).await, Err(MediaEngineError::RoomNotFound(_))));
    }

    #[tokio::test]
    async fn duplicate_create_and_mode_switch_are_honest() {
        let registry = MediaRegistry::new();
        let rid = room_id();
        registry.create_room(cfg(rid)).await.expect("create");
        assert!(matches!(registry.create_room(cfg(rid)).await, Err(MediaEngineError::RoomAlreadyExists(_))));
        assert_eq!(registry.switch_room_mode(rid, RoomMode::Mcu).await.expect("switch"), RoomMode::Mcu);
        assert_eq!(registry.mode(), RoomMode::Sfu, "registry remains an SFU roster; room mode is per-room state");
    }

    #[tokio::test]
    async fn capacity_counts_unique_participants_across_roles() {
        let registry = MediaRegistry::new();
        let rid = room_id();
        let caps = RoomCaps { max_participants: Some(2), ..RoomCaps::default() };
        registry.create_room(RoomMediaCfg { room_id: rid, mode: RoomMode::Sfu, caps }).await.expect("create");
        registry.add_publisher(rid, p(1), vec![]).await.expect("first seat");
        registry.add_subscriber(rid, p(2), sub()).await.expect("second seat");
        // The same participant re-registering in the other role is not a new seat.
        registry.add_subscriber(rid, p(1), sub()).await.expect("existing participant");
        assert!(registry.add_publisher(rid, p(3), vec![]).await.is_err(), "third unique participant must be refused");
    }

    /// T11c: an H264-only browser offer must be refused with a typed error and
    /// no panic/unreachable, never answered with dead VP8 m-lines.
    #[test]
    fn h264_only_offer_is_rejected_without_panicking() {
        let offer = "v=0\r\n\
                     o=- 1 2 IN IP4 127.0.0.1\r\n\
                     s=-\r\n\
                     t=0 0\r\n\
                     m=video 9 UDP/TLS/RTP/SAVPF 102\r\n\
                     a=mid:0\r\n\
                     a=ice-ufrag:client\r\n\
                     a=ice-pwd:client-password-0123456789\r\n\
                     a=fingerprint:sha-256 AA:BB:CC:DD\r\n\
                     a=rtpmap:102 H264/90000\r\n\
                     a=sendrecv\r\n";
        let result = crate::webrtc::WebRtcPeer::accept_offer(
            offer,
            "127.0.0.1:0".parse().expect("local addr"),
            &crate::webrtc::WebRtcConfig::default(),
        );
        match result {
            Err(crate::webrtc::WebRtcError::InvalidOffer(message)) => {
                assert!(message.contains("no supported codec"), "unexpected reason: {message}");
            }
            Err(other) => panic!("H264-only offer must fail as InvalidOffer, got {other}"),
            Ok(_) => panic!("H264-only offer must not be accepted"),
        }
    }

    #[tokio::test]
    async fn apply_layout_never_creates_state() {
        let registry = MediaRegistry::new();
        let rid = room_id();
        registry.create_room(cfg(rid)).await.expect("create");
        let layout = LayoutDescriptor { layout_id: "grid".into(), version: 1, ..LayoutDescriptor::default() };
        registry.apply_layout(rid, layout.clone()).await.expect("known room");
        registry.destroy_room(rid).await.expect("destroy");
        assert!(matches!(registry.apply_layout(rid, layout).await, Err(MediaEngineError::RoomNotFound(_))));
    }
}

//! SFU engine — impl [`MediaEngine`](vks_core::MediaEngine) (SFU).
//!
//! In-memory SFU, который связывает:
//! - [`Forwarder】 (full-mesh N-to-N routing),
//! - [`SimulcastSelector】 (spatial layer по BWE),
//! - [`BweEstimator】 (TWCC feedback → bps),
//! - [`CongestionLadder】 (degrade/recovery).
//!
//! MediaEngine trait (vks-core) — async. Здесь — sync in-memory SFU;
//! async methods делегируют (без I/O). Loopback (mock) для тестов:
//! [`SfuEngine::route】 возвращает (target, packet), которые тест
//! использует как «sent» + feedback.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use vks_core::ports::media_engine::{
    LayoutDescriptor, MediaEngine, MediaEngineError, MediaTrack, PublisherHandle, RoomCaps, RoomMediaCfg,
    RoomMediaHandle, SubscribeRequest, SubscriberHandle,
};
use vks_core::{ParticipantId, RoomId, RoomMode, TrackId};

use crate::bwe::{BweEstimator, TwccReportedPacket};
use crate::codec::CodecRegistry;
use crate::congestion::{CongestionLadder, CongestionLevel};
use crate::forwarder::{ForwardTarget, Forwarder, ForwarderConfig, RtpPacket};
use crate::simulcast::{SimulcastLayer, SimulcastSelector};

/// Per-room SFU state.
#[derive(Debug)]
struct RoomState {
    mode: RoomMode,
    caps: RoomCaps,
    forwarder: Forwarder,
    /// Publisher → его track'и.
    publisher_tracks: BTreeMap<ParticipantId, Vec<MediaTrack>>,
    /// Все активные треки в комнате: track_id → MediaTrack.
    tracks: BTreeMap<TrackId, MediaTrack>,
    /// Subscriber → BWE/selector/congestion (per-subscriber downlink).
    subscribers: BTreeMap<ParticipantId, SubscriberState>,
}

/// Per-subscriber media state (downlink BWE).
#[derive(Debug)]
struct SubscriberState {
    /// BWE (TWCC).
    bwe: BweEstimator,
    /// Simulcast (spatial layer).
    simulcast: SimulcastSelector,
    /// Congestion (ladder).
    congestion: CongestionLadder,
    /// Желаемая подписка (треки + audio_only фильтр).
    subscription: SubscribeRequest,
}

#[inline]
fn track_matches_subscription(req: &SubscribeRequest, track: &MediaTrack) -> bool {
    (!req.audio_only || track.media_kind == vks_core::MediaKind::Audio)
        && (req.track_ids.is_empty() || req.track_ids.contains(&track.id))
}

/// SFU engine (impl [`MediaEngine`]).
#[derive(Debug)]
pub struct SfuEngine {
    /// Rooms (room_id → state).
    rooms: Arc<Mutex<BTreeMap<RoomId, RoomState>>>,
    /// Codec registry (feature-gated, ADR-0017).
    codecs: CodecRegistry,
}

impl Default for SfuEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl SfuEngine {
    /// Создаёт engine (full-mesh).
    pub fn new() -> Self {
        Self { rooms: Arc::new(Mutex::new(BTreeMap::new())), codecs: CodecRegistry::new() }
    }

    /// Codec registry (immutable).
    pub fn codecs(&self) -> &CodecRegistry {
        &self.codecs
    }

    /// Текущий spatial layer для подписчика (None если нет).
    pub fn subscriber_layer(&self, room: RoomId, subscriber: ParticipantId) -> Option<SimulcastLayer> {
        let g = self.rooms.lock().unwrap();
        g.get(&room)?.subscribers.get(&subscriber).map(|s| s.simulcast.current())
    }

    /// Текущий congestion level для подписчика.
    pub fn subscriber_congestion(&self, room: RoomId, subscriber: ParticipantId) -> Option<CongestionLevel> {
        let g = self.rooms.lock().unwrap();
        g.get(&room)?.subscribers.get(&subscriber).map(|s| s.congestion.current())
    }

    /// Обработка inbound TWCC feedback (subscriber → SFU).
    ///
    /// Пересчитывает BWE, обновляет simulcast + congestion, и
    /// возвращает новый spatial layer (для PLI/FIR при смене).
    pub fn on_feedback(
        &self,
        room: RoomId,
        subscriber: ParticipantId,
        reported: &[TwccReportedPacket],
    ) -> Option<SimulcastLayer> {
        let mut g = self.rooms.lock().unwrap();
        let room_state = g.get_mut(&room)?;
        let sub = room_state.subscribers.get_mut(&subscriber)?;
        let bwe_result = sub.bwe.report(reported);
        let bps = bwe_result.bps;
        // Simulcast (spatial layer).
        let sim = sub.simulcast.select(bps);
        // Congestion (ladder).
        let ladder = sub.congestion.update(bps);
        // Ladder влияет на simulcast: принудительный spatial down.
        let mut layer = sim.layer;
        if let CongestionLevel::DegradeSpatial(target) = ladder.level {
            if target < layer {
                layer = target;
            }
        }
        if ladder.level == CongestionLevel::AudioOnly {
            layer = SimulcastLayer::Low;
        }
        Some(layer)
    }

    /// Routing: пакет → все подписчики track'а. Возвращает список
    /// (target, packet) — для loopback-transport (mock) в тестах.
    pub fn route(&self, room: RoomId, packet: RtpPacket) -> Option<Vec<(ForwardTarget, RtpPacket)>> {
        let g = self.rooms.lock().unwrap();
        let room_state = g.get(&room)?;
        let track = packet.track;
        let subscribers = room_state.forwarder.subscribers_of(&track).iter().copied().collect::<Vec<_>>();
        let out = subscribers
            .into_iter()
            .map(|subscriber| (ForwardTarget { subscriber, track }, packet.clone()))
            .collect::<Vec<_>>();
        Some(out)
    }
}

#[async_trait]
impl MediaEngine for SfuEngine {
    fn mode(&self) -> RoomMode {
        RoomMode::Sfu
    }

    async fn create_room(&self, cfg: RoomMediaCfg) -> Result<RoomMediaHandle, MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        if g.contains_key(&cfg.room_id) {
            return Err(MediaEngineError::RoomAlreadyExists(cfg.room_id));
        }
        let state = RoomState {
            mode: cfg.mode,
            caps: cfg.caps,
            forwarder: Forwarder::new(ForwarderConfig { full_mesh_default: true }),
            publisher_tracks: BTreeMap::new(),
            tracks: BTreeMap::new(),
            subscribers: BTreeMap::new(),
        };
        g.insert(cfg.room_id, state);
        Ok(RoomMediaHandle { room: cfg.room_id, mode: cfg.mode })
    }

    async fn destroy_room(&self, room: RoomId) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        if g.remove(&room).is_none() {
            return Err(MediaEngineError::RoomNotFound(room));
        }
        Ok(())
    }

    async fn switch_room_mode(&self, room: RoomId, new_mode: RoomMode) -> Result<RoomMode, MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.mode = new_mode;
        Ok(new_mode)
    }

    async fn add_publisher(
        &self,
        room: RoomId,
        publisher: ParticipantId,
        tracks: Vec<MediaTrack>,
    ) -> Result<PublisherHandle, MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;

        // Admission quota check: max_participants
        if let Some(max_p) = state.caps.max_participants {
            let is_existing =
                state.publisher_tracks.contains_key(&publisher) || state.subscribers.contains_key(&publisher);
            let total_unique = state
                .publisher_tracks
                .keys()
                .chain(state.subscribers.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .len() as u32;
            if total_unique >= max_p && !is_existing {
                return Err(MediaEngineError::Other(format!("room capacity exceeded: max {} participants", max_p)));
            }
        }

        // 1. Withdraw tracks that are no longer published
        if let Some(old_tracks) = state.publisher_tracks.get(&publisher) {
            for old_t in old_tracks {
                if !tracks.iter().any(|t| t.id == old_t.id) {
                    state.tracks.remove(&old_t.id);
                    state.forwarder.withdraw_track(old_t.id);
                }
            }
        }

        // 2. Announce new tracks and apply selective subscription filters
        for t in &tracks {
            state.tracks.insert(t.id, t.clone());
            state.forwarder.announce_track(t.id);

            // Selective subscriptions: check all subscribers
            for (sub_id, sub_state) in &state.subscribers {
                if track_matches_subscription(&sub_state.subscription, t) {
                    state.forwarder.subscribe(*sub_id, t.id);
                } else {
                    state.forwarder.unsubscribe(*sub_id, t.id);
                }
            }
        }

        state.publisher_tracks.insert(publisher, tracks);
        Ok(PublisherHandle { room, publisher })
    }

    async fn remove_publisher(&self, room: RoomId, publisher: ParticipantId) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        if let Some(tracks) = state.publisher_tracks.remove(&publisher) {
            for t in tracks {
                state.tracks.remove(&t.id);
                state.forwarder.withdraw_track(t.id);
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
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;

        // Admission quota check: max_participants
        if let Some(max_p) = state.caps.max_participants {
            let is_existing =
                state.subscribers.contains_key(&subscriber) || state.publisher_tracks.contains_key(&subscriber);
            let total_unique = state
                .subscribers
                .keys()
                .chain(state.publisher_tracks.keys())
                .collect::<std::collections::BTreeSet<_>>()
                .len() as u32;
            if total_unique >= max_p && !is_existing {
                return Err(MediaEngineError::Other(format!("room capacity exceeded: max {} participants", max_p)));
            }
        }

        state.forwarder.add_subscriber(subscriber);

        // Apply selective subscription filters to already active tracks
        if req.audio_only || !req.track_ids.is_empty() {
            for (track_id, track) in &state.tracks {
                if track_matches_subscription(&req, track) {
                    state.forwarder.subscribe(subscriber, *track_id);
                } else {
                    state.forwarder.unsubscribe(subscriber, *track_id);
                }
            }
        }

        if let Some(existing) = state.subscribers.get_mut(&subscriber) {
            existing.subscription = req;
        } else {
            let sub = SubscriberState {
                bwe: BweEstimator::new(),
                simulcast: SimulcastSelector::new(),
                congestion: CongestionLadder::new(),
                subscription: req,
            };
            state.subscribers.insert(subscriber, sub);
        }
        Ok(SubscriberHandle { room, subscriber })
    }

    async fn remove_subscriber(&self, room: RoomId, subscriber: ParticipantId) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.forwarder.remove_subscriber(subscriber);
        state.subscribers.remove(&subscriber);
        Ok(())
    }

    async fn apply_layout(&self, room: RoomId, _layout: LayoutDescriptor) -> Result<(), MediaEngineError> {
        // SFU: layout client-side — no server-side effect (прозрачность).
        let g = self.rooms.lock().unwrap();
        if g.contains_key(&room) {
            Ok(())
        } else {
            Err(MediaEngineError::RoomNotFound(room))
        }
    }

    async fn set_room_capabilities(&self, room: RoomId, caps: RoomCaps) -> Result<(), MediaEngineError> {
        let mut g = self.rooms.lock().unwrap();
        let state = g.get_mut(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        state.caps = caps;
        Ok(())
    }

    async fn list_publishers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        let g = self.rooms.lock().unwrap();
        let state = g.get(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(state.publisher_tracks.keys().copied().collect())
    }

    async fn list_subscribers(&self, room: RoomId) -> Result<Vec<ParticipantId>, MediaEngineError> {
        let g = self.rooms.lock().unwrap();
        let state = g.get(&room).ok_or(MediaEngineError::RoomNotFound(room))?;
        Ok(state.subscribers.keys().copied().collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use vks_core::track::{Track, TrackKind};

    fn room_id() -> RoomId {
        RoomId::from_uuid("30000000-0000-0000-0000-000000000001".parse().unwrap())
    }
    fn p(n: u64) -> ParticipantId {
        ParticipantId::from_uuid(format!("10000000-0000-0000-0000-{n:012}").parse().unwrap())
    }
    fn video_track(pid: ParticipantId) -> Track {
        Track::new(pid, TrackKind::Video)
    }
    fn audio_track(pid: ParticipantId) -> Track {
        Track::new(pid, TrackKind::Audio)
    }
    fn cfg(rid: RoomId) -> RoomMediaCfg {
        RoomMediaCfg { room_id: rid, mode: RoomMode::Sfu, caps: RoomCaps::default() }
    }
    fn sub_req() -> SubscribeRequest {
        SubscribeRequest { track_ids: Vec::new(), audio_only: false }
    }

    #[tokio::test]
    async fn create_and_destroy_room() {
        let e = SfuEngine::new();
        let rid = room_id();
        let handle = e.create_room(cfg(rid)).await.unwrap();
        assert_eq!(handle.mode, RoomMode::Sfu);
        assert_eq!(handle.room, rid);
        e.destroy_room(rid).await.unwrap();
        let err = e.destroy_room(rid).await.unwrap_err();
        assert!(matches!(err, MediaEngineError::RoomNotFound(_)));
    }

    #[tokio::test]
    async fn duplicate_room_errors() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let err = e.create_room(cfg(rid)).await.unwrap_err();
        assert!(matches!(err, MediaEngineError::RoomAlreadyExists(_)));
    }

    #[tokio::test]
    async fn publisher_listing() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let pub1 = p(1);
        let pub2 = p(2);
        e.add_publisher(rid, pub1, vec![video_track(pub1)]).await.unwrap();
        e.add_publisher(rid, pub2, vec![video_track(pub2)]).await.unwrap();
        let pubs = e.list_publishers(rid).await.unwrap();
        assert!(pubs.contains(&pub1));
        assert!(pubs.contains(&pub2));
    }

    #[tokio::test]
    async fn subscriber_listing() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let sub1 = p(10);
        e.add_subscriber(rid, sub1, sub_req()).await.unwrap();
        let subs = e.list_subscribers(rid).await.unwrap();
        assert!(subs.contains(&sub1));
    }

    #[tokio::test]
    async fn full_mesh_loopback_route() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let pub1 = p(1);
        let track1 = video_track(pub1);
        e.add_publisher(rid, pub1, vec![track1.clone()]).await.unwrap();
        let sub1 = p(10);
        let sub2 = p(11);
        e.add_subscriber(rid, sub1, sub_req()).await.unwrap();
        e.add_subscriber(rid, sub2, sub_req()).await.unwrap();
        // Route: track1 → 2 subs.
        let routed = e.route(rid, RtpPacket { track: track1.id, payload: vec![1].into() }).unwrap();
        assert_eq!(routed.len(), 2);
        assert!(routed.iter().any(|(t, _)| t.subscriber == sub1));
        assert!(routed.iter().any(|(t, _)| t.subscriber == sub2));
    }

    #[tokio::test]
    async fn feedback_moves_bwe_and_layer() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let sub = p(20);
        e.add_subscriber(rid, sub, sub_req()).await.unwrap();
        // Fast feedback (10ms @ 1200B ≈ 960kbps) → High layer.
        let now = Instant::now();
        let fast: Vec<TwccReportedPacket> = (0..30)
            .map(|i| TwccReportedPacket {
                sent_seq: i as u16,
                arrival: now - Duration::from_secs(1) + Duration::from_millis(i as u64 * 10),
            })
            .collect();
        let layer = e.on_feedback(rid, sub, &fast).unwrap();
        assert_eq!(layer, SimulcastLayer::High);
        // Slow feedback (100ms @ 1200B ≈ 96kbps) → Low layer.
        let slow: Vec<TwccReportedPacket> = (0..30)
            .map(|i| TwccReportedPacket {
                sent_seq: (1000 + i) as u16,
                arrival: now + Duration::from_millis(i as u64 * 100),
            })
            .collect();
        let layer = e.on_feedback(rid, sub, &slow).unwrap();
        assert_eq!(layer, SimulcastLayer::Low);
    }

    #[tokio::test]
    async fn remove_publisher_stops_tracks() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let pub1 = p(1);
        let track1 = video_track(pub1);
        e.add_publisher(rid, pub1, vec![track1.clone()]).await.unwrap();
        let sub1 = p(10);
        e.add_subscriber(rid, sub1, sub_req()).await.unwrap();
        let before = e.route(rid, RtpPacket { track: track1.id, payload: vec![].into() }).unwrap();
        assert_eq!(before.len(), 1);
        e.remove_publisher(rid, pub1).await.unwrap();
        let after = e.route(rid, RtpPacket { track: track1.id, payload: vec![].into() }).unwrap();
        assert!(after.is_empty());
    }

    #[tokio::test]
    async fn renegotiate_publisher_withdraws_removed_tracks() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let pub1 = p(1);
        let track1 = video_track(pub1);
        let track2 = video_track(pub1);
        e.add_publisher(rid, pub1, vec![track1.clone()]).await.unwrap();

        let sub1 = p(10);
        e.add_subscriber(rid, sub1, sub_req()).await.unwrap();

        // Track 1 routes to subscriber
        let r1 = e.route(rid, RtpPacket { track: track1.id, payload: vec![].into() }).unwrap();
        assert_eq!(r1.len(), 1);

        // Publisher renegotiates: replaces track1 with track2 (L04)
        e.add_publisher(rid, pub1, vec![track2.clone()]).await.unwrap();

        // Track 1 is now withdrawn: routes to 0 subscribers
        let r1_after = e.route(rid, RtpPacket { track: track1.id, payload: vec![].into() }).unwrap();
        assert!(r1_after.is_empty(), "withdrawn track 1 must no longer route");

        // Track 2 routes to subscriber
        let r2 = e.route(rid, RtpPacket { track: track2.id, payload: vec![].into() }).unwrap();
        assert_eq!(r2.len(), 1, "new track 2 must route to subscriber");
    }

    #[tokio::test]
    async fn unknown_room_errors() {
        let e = SfuEngine::new();
        let missing = RoomId::from_uuid("40000000-0000-0000-0000-000000000001".parse().unwrap());
        let err = e.destroy_room(missing).await.unwrap_err();
        assert!(matches!(err, MediaEngineError::RoomNotFound(_)));
        let err = e.list_publishers(missing).await.unwrap_err();
        assert!(matches!(err, MediaEngineError::RoomNotFound(_)));
    }

    #[tokio::test]
    async fn switch_mode_updates_room() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let new_mode = e.switch_room_mode(rid, RoomMode::Mcu).await.unwrap();
        assert_eq!(new_mode, RoomMode::Mcu);
    }

    #[tokio::test]
    async fn audio_only_subscriber_does_not_receive_video_tracks() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();

        let pub1 = p(1);
        let a_track = audio_track(pub1);
        let v_track = video_track(pub1);
        e.add_publisher(rid, pub1, vec![a_track.clone(), v_track.clone()]).await.unwrap();

        let sub_audio = p(10);
        e.add_subscriber(rid, sub_audio, SubscribeRequest { track_ids: Vec::new(), audio_only: true }).await.unwrap();

        let sub_all = p(11);
        e.add_subscriber(rid, sub_all, sub_req()).await.unwrap();

        // Audio packet routes to both
        let a_routed = e.route(rid, RtpPacket { track: a_track.id, payload: vec![1].into() }).unwrap();
        assert_eq!(a_routed.len(), 2);
        assert!(a_routed.iter().any(|(t, _)| t.subscriber == sub_audio));
        assert!(a_routed.iter().any(|(t, _)| t.subscriber == sub_all));

        // Video packet routes only to sub_all, NOT sub_audio
        let v_routed = e.route(rid, RtpPacket { track: v_track.id, payload: vec![2].into() }).unwrap();
        assert_eq!(v_routed.len(), 1);
        assert_eq!(v_routed[0].0.subscriber, sub_all);
    }

    #[tokio::test]
    async fn selective_track_ids_subscription() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();

        let pub1 = p(1);
        let t1 = video_track(pub1);
        let t2 = video_track(pub1);
        e.add_publisher(rid, pub1, vec![t1.clone(), t2.clone()]).await.unwrap();

        let sub = p(10);
        // Only subscribe to t1
        e.add_subscriber(rid, sub, SubscribeRequest { track_ids: vec![t1.id], audio_only: false }).await.unwrap();

        let r1 = e.route(rid, RtpPacket { track: t1.id, payload: vec![1].into() }).unwrap();
        assert_eq!(r1.len(), 1);
        assert_eq!(r1[0].0.subscriber, sub);

        let r2 = e.route(rid, RtpPacket { track: t2.id, payload: vec![2].into() }).unwrap();
        assert!(r2.is_empty(), "track 2 must not be routed to subscriber selective for track 1");
    }

    #[tokio::test]
    async fn late_publisher_respects_subscriber_filters() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();

        let sub_audio = p(10);
        e.add_subscriber(rid, sub_audio, SubscribeRequest { track_ids: Vec::new(), audio_only: true }).await.unwrap();

        let pub1 = p(1);
        let a_track = audio_track(pub1);
        let v_track = video_track(pub1);
        // Publisher joins AFTER subscriber
        e.add_publisher(rid, pub1, vec![a_track.clone(), v_track.clone()]).await.unwrap();

        let a_routed = e.route(rid, RtpPacket { track: a_track.id, payload: vec![1].into() }).unwrap();
        assert_eq!(a_routed.len(), 1);
        assert_eq!(a_routed[0].0.subscriber, sub_audio);

        let v_routed = e.route(rid, RtpPacket { track: v_track.id, payload: vec![2].into() }).unwrap();
        assert!(v_routed.is_empty(), "video track must not be routed to audio-only subscriber");
    }

    #[tokio::test]
    async fn room_max_participants_quota_enforced() {
        let e = SfuEngine::new();
        let rid = room_id();
        let mut c = cfg(rid);
        c.caps.max_participants = Some(2);
        e.create_room(c).await.unwrap();

        let s1 = p(1);
        let s2 = p(2);
        let s3 = p(3);

        e.add_subscriber(rid, s1, sub_req()).await.unwrap();
        e.add_subscriber(rid, s2, sub_req()).await.unwrap();
        let err = e.add_subscriber(rid, s3, sub_req()).await.unwrap_err();
        assert!(matches!(err, MediaEngineError::Other(msg) if msg.contains("capacity exceeded")));

        // Re-adding existing subscriber succeeds (idempotent / update)
        e.add_subscriber(rid, s1, sub_req()).await.unwrap();
    }

    #[tokio::test]
    async fn renegotiation_preserves_subscriber_bwe_state() {
        let e = SfuEngine::new();
        let rid = room_id();
        e.create_room(cfg(rid)).await.unwrap();
        let sub = p(20);
        e.add_subscriber(rid, sub, sub_req()).await.unwrap();

        // Feed fast TWCC packets to elevate BWE to High layer
        let now = Instant::now();
        let fast: Vec<TwccReportedPacket> = (0..30)
            .map(|i| TwccReportedPacket {
                sent_seq: i as u16,
                arrival: now - Duration::from_secs(1) + Duration::from_millis(i as u64 * 10),
            })
            .collect();
        let layer = e.on_feedback(rid, sub, &fast).unwrap();
        assert_eq!(layer, SimulcastLayer::High);

        // Update subscription (renegotiation)
        e.add_subscriber(rid, sub, SubscribeRequest { track_ids: Vec::new(), audio_only: true }).await.unwrap();

        // Check congestion/BWE state is preserved (not reset to default)
        let c = e.subscriber_congestion(rid, sub).unwrap();
        assert_eq!(c, CongestionLevel::Normal);
    }

    #[tokio::test]
    async fn room_max_participants_counts_publishers_and_subscribers() {
        let e = SfuEngine::new();
        let rid = room_id();
        let mut c = cfg(rid);
        c.caps.max_participants = Some(2);
        e.create_room(c).await.unwrap();

        let pub1 = p(1);
        let pub2 = p(2);
        let pub3 = p(3);

        e.add_publisher(rid, pub1, vec![video_track(pub1)]).await.unwrap();
        e.add_publisher(rid, pub2, vec![video_track(pub2)]).await.unwrap();
        let err = e.add_publisher(rid, pub3, vec![video_track(pub3)]).await.unwrap_err();
        assert!(matches!(err, MediaEngineError::Other(msg) if msg.contains("capacity exceeded")));
    }
}

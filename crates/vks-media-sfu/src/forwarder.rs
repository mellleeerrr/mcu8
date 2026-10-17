//! Forwarder (MEDIA_PIPELINE §2.2).
//!
//! Subscription model: **full-mesh по умолчанию** — новый подписчик
//! автоматически получает все активные track'и; каждый publisher
//! транслируется всем подписчикам комнаты (N-to-N). 1-to-1 — частный
//! случай.
//!
//! Без реальных сокетов: in-memory fan-out через pluggable [`Sink`]
//! (mock в тестах). RTP-payload — opaque bytes (SFU не декодирует).

use std::collections::BTreeSet;
use std::collections::HashMap;

use bytes::Bytes;
use vks_core::ParticipantId;
use vks_core::TrackId;

/// Opaque RTP-пакет (payload). SFU не инспектирует — только форвардит.
/// Внутренний буфер `Bytes` обеспечивает zero-copy fan-out между N подписчиками.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    /// Track, которому принадлежит пакет.
    pub track: TrackId,
    /// RTP payload (headers + data).
    pub payload: Bytes,
}

impl RtpPacket {
    pub fn new(track: TrackId, payload: impl Into<Bytes>) -> Self {
        Self { track, payload: payload.into() }
    }
}

/// Куда форвардим: подписчик + track.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForwardTarget {
    /// Подписчик.
    pub subscriber: ParticipantId,
    /// Track.
    pub track: TrackId,
}

/// Sink — куда Forwarder кладёт пакеты. Pluggable (mock в тестах,
/// реальный str0m-транспорт в фазе3).
pub trait Sink: Send {
    /// Отправить пакет подписчику.
    fn send(&mut self, target: &ForwardTarget, packet: &RtpPacket);
    /// Подписчик подключился (full-mesh: сюда приходят текущие track'и).
    fn subscriber_joined(&mut self, subscriber: &ParticipantId, tracks: &[TrackId]);
    /// Подписчик отключился.
    fn subscriber_left(&mut self, subscriber: &ParticipantId);
    /// Трек появился.
    fn track_announced(&mut self, track: &TrackId);
    /// Трек исчез.
    fn track_withdrawn(&mut self, track: &TrackId);
}

/// Конфигурация [`Forwarder】::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ForwarderConfig {
    /// Full-mesh по умолчанию: новый подписчик подписывается на все
    /// активные track'и (true) или не (false — explicit subscribe
    /// через [`Forwarder::subscribe`]).
    pub full_mesh_default: bool,
}

impl Default for ForwarderConfig {
    fn default() -> Self {
        Self { full_mesh_default: true }
    }
}

/// Forwarder: publisher→subscribers fan-out.
#[derive(Debug, Clone)]
pub struct Forwarder {
    config: ForwarderConfig,
    /// Track → подписчики.
    subscriptions: HashMap<TrackId, BTreeSet<ParticipantId>>,
    /// Подписчик → его track'и.
    subscriber_tracks: HashMap<ParticipantId, BTreeSet<TrackId>>,
    /// Активные track'и (publisher-ы).
    active_tracks: BTreeSet<TrackId>,
}

impl Default for Forwarder {
    fn default() -> Self {
        Self::new(ForwarderConfig::default())
    }
}

impl Forwarder {
    /// Создаёт Forwarder с конфигурацией.
    pub fn new(config: ForwarderConfig) -> Self {
        Self {
            config,
            subscriptions: HashMap::new(),
            subscriber_tracks: HashMap::new(),
            active_tracks: BTreeSet::new(),
        }
    }

    /// Создаёт Forwarder в режиме **full-mesh**.
    pub fn full_mesh() -> Self {
        Self::new(ForwarderConfig::default())
    }

    /// Конфигурация (immutable).
    pub fn config(&self) -> ForwarderConfig {
        self.config
    }

    /// Аннуцирует track (publisher). Если full-mesh — новые подписчики
    /// получат его автоматически.
    pub fn announce_track(&mut self, track: TrackId) {
        if self.active_tracks.insert(track) {
            self.subscriptions.entry(track).or_default();
        }
    }

    /// Убирает track (publisher ушёл).
    pub fn withdraw_track(&mut self, track: TrackId) {
        if self.active_tracks.remove(&track) {
            self.subscriptions.remove(&track);
            for tracks in self.subscriber_tracks.values_mut() {
                tracks.remove(&track);
            }
        }
    }

    /// Добавляет подписчика. Если full-mesh — auto-subscribe на все
    /// активные track'и.
    pub fn add_subscriber(&mut self, subscriber: ParticipantId) {
        let tracks = if self.config.full_mesh_default { self.active_tracks.clone() } else { BTreeSet::new() };
        self.subscriber_tracks.insert(subscriber, tracks.clone());
        for track in &tracks {
            self.subscriptions.entry(*track).or_default().insert(subscriber);
        }
    }

    /// Удаляет подписчика (unsubscribe со всех track'ов).
    pub fn remove_subscriber(&mut self, subscriber: ParticipantId) {
        if self.subscriber_tracks.remove(&subscriber).is_some() {
            for set in self.subscriptions.values_mut() {
                set.remove(&subscriber);
            }
        }
    }

    /// Explicit subscribe (для non-full-mesh конфигурации).
    pub fn subscribe(&mut self, subscriber: ParticipantId, track: TrackId) {
        if !self.active_tracks.contains(&track) {
            return;
        }
        self.subscriptions.entry(track).or_default().insert(subscriber);
        self.subscriber_tracks.entry(subscriber).or_default().insert(track);
    }

    /// Explicit unsubscribe.
    pub fn unsubscribe(&mut self, subscriber: ParticipantId, track: TrackId) {
        if let Some(set) = self.subscriptions.get_mut(&track) {
            set.remove(&subscriber);
        }
        if let Some(tracks) = self.subscriber_tracks.get_mut(&subscriber) {
            tracks.remove(&track);
        }
    }

    /// Подписчики track'а (immutable view; пусто если track неизвестен).
    pub fn subscribers_of(&self, track: &TrackId) -> &BTreeSet<ParticipantId> {
        static EMPTY: std::sync::OnceLock<BTreeSet<ParticipantId>> = std::sync::OnceLock::new();
        self.subscriptions.get(track).unwrap_or_else(|| EMPTY.get_or_init(BTreeSet::new))
    }

    /// Track'и подписчика (None если подписчик неизвестен).
    pub fn tracks_of(&self, subscriber: &ParticipantId) -> Option<&BTreeSet<TrackId>> {
        self.subscriber_tracks.get(subscriber)
    }

    /// Активные track'и (копия).
    pub fn active_tracks(&self) -> Vec<TrackId> {
        self.active_tracks.iter().copied().collect()
    }

    /// Fan-out: пакет → все подписчики track'а.
    pub fn route(&mut self, sink: &mut dyn Sink, packet: RtpPacket) {
        let track = packet.track;
        let subscribers =
            self.subscriptions.get(&track).map(|s| s.iter().copied().collect::<Vec<_>>()).unwrap_or_default();
        for subscriber in subscribers {
            let target = ForwardTarget { subscriber, track };
            sink.send(&target, &packet);
        }
    }
}

/// Mock Sink — собирает события в Vec (для тестов).
#[derive(Debug, Default)]
pub struct RecordingSink {
    /// (target, packet) sent.
    pub sent: Vec<(ForwardTarget, RtpPacket)>,
    /// subscriber_joined (subscriber, tracks).
    pub joined: Vec<(ParticipantId, Vec<TrackId>)>,
    /// subscriber_left.
    pub left: Vec<ParticipantId>,
    /// track_announced.
    pub announced: Vec<TrackId>,
    /// track_withdrawn.
    pub withdrawn: Vec<TrackId>,
}

impl Sink for RecordingSink {
    fn send(&mut self, target: &ForwardTarget, packet: &RtpPacket) {
        self.sent.push((target.clone(), packet.clone()));
    }
    fn subscriber_joined(&mut self, subscriber: &ParticipantId, tracks: &[TrackId]) {
        self.joined.push((*subscriber, tracks.to_vec()));
    }
    fn subscriber_left(&mut self, subscriber: &ParticipantId) {
        self.left.push(*subscriber);
    }
    fn track_announced(&mut self, track: &TrackId) {
        self.announced.push(*track);
    }
    fn track_withdrawn(&mut self, track: &TrackId) {
        self.withdrawn.push(*track);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(n: u64) -> ParticipantId {
        ParticipantId::from_uuid(format!("10000000-0000-0000-0000-{n:012}").parse().unwrap())
    }
    fn t(n: u64) -> TrackId {
        TrackId::from_uuid(format!("20000000-0000-0000-0000-{n:012}").parse().unwrap())
    }

    #[test]
    fn default_is_full_mesh() {
        let f = Forwarder::default();
        assert!(f.config().full_mesh_default);
    }

    #[test]
    fn full_mesh_new_subscriber_gets_all_active() {
        let mut f = Forwarder::full_mesh();
        f.announce_track(t(1));
        f.announce_track(t(2));
        let sub = p(10);
        f.add_subscriber(sub);
        let tracks = f.tracks_of(&sub).unwrap();
        assert!(tracks.contains(&t(1)));
        assert!(tracks.contains(&t(2)));
    }

    #[test]
    fn n_to_n_routing() {
        let mut f = Forwarder::full_mesh();
        let mut sink = RecordingSink::default();
        f.announce_track(t(1));
        f.announce_track(t(2));
        let s1 = p(1);
        let s2 = p(2);
        let s3 = p(3);
        f.add_subscriber(s1);
        f.add_subscriber(s2);
        f.add_subscriber(s3);
        // Packet track1 → все 3 подписчика.
        f.route(&mut sink, RtpPacket { track: t(1), payload: vec![1].into() });
        assert_eq!(sink.sent.len(), 3);
        assert!(sink.sent.iter().all(|(tg, _)| tg.track == t(1)));
        // Packet track2 → тоже все 3.
        f.route(&mut sink, RtpPacket { track: t(2), payload: vec![2].into() });
        assert_eq!(sink.sent.len(), 6);
    }

    #[test]
    fn remove_subscriber_stops_routing() {
        let mut f = Forwarder::full_mesh();
        let mut sink = RecordingSink::default();
        f.announce_track(t(1));
        let s1 = p(1);
        let s2 = p(2);
        f.add_subscriber(s1);
        f.add_subscriber(s2);
        f.remove_subscriber(s2);
        f.route(&mut sink, RtpPacket { track: t(1), payload: vec![].into() });
        assert_eq!(sink.sent.len(), 1);
        assert_eq!(sink.sent[0].0.subscriber, s1);
    }

    #[test]
    fn withdraw_track_stops_routing() {
        let mut f = Forwarder::full_mesh();
        let mut sink = RecordingSink::default();
        f.announce_track(t(1));
        let s1 = p(1);
        f.add_subscriber(s1);
        f.withdraw_track(t(1));
        f.route(&mut sink, RtpPacket { track: t(1), payload: vec![].into() });
        assert!(sink.sent.is_empty());
    }

    #[test]
    fn explicit_subscribe_non_full_mesh() {
        let mut f = Forwarder::new(ForwarderConfig { full_mesh_default: false });
        let mut sink = RecordingSink::default();
        f.announce_track(t(1));
        let s1 = p(1);
        f.add_subscriber(s1);
        // Не подписан: route ничего не даст.
        f.route(&mut sink, RtpPacket { track: t(1), payload: vec![9].into() });
        assert!(sink.sent.is_empty());
        f.subscribe(s1, t(1));
        f.route(&mut sink, RtpPacket { track: t(1), payload: vec![9].into() });
        assert_eq!(sink.sent.len(), 1);
    }

    #[test]
    fn unsubscribe_removes_target() {
        let mut f = Forwarder::full_mesh();
        let mut sink = RecordingSink::default();
        f.announce_track(t(1));
        let s1 = p(1);
        f.add_subscriber(s1);
        f.unsubscribe(s1, t(1));
        f.route(&mut sink, RtpPacket { track: t(1), payload: vec![].into() });
        assert!(sink.sent.is_empty());
    }

    #[test]
    fn unknown_track_routes_to_no_one() {
        let mut f = Forwarder::full_mesh();
        let mut sink = RecordingSink::default();
        f.route(&mut sink, RtpPacket { track: t(99), payload: vec![].into() });
        assert!(sink.sent.is_empty());
    }
}

//! `Bus` — in-memory pub/sub, заменяющий NATS JetStream (ADR-0014).
//!
//! Темы `vks.cluster.*`, DLQ `vks.cluster.dlq.*`.
//! Идемпотентность: дубликаты по `Msg::id` игнорируются (ADR-0019).
//!
//! Что реально: модель сообщений, dedup, delivery, DLQ, backpressure-политика.
//! Что мокнуто: персистентность JetStream, реальный кластер NATS, per-subject
//! ordering (в памяти ordering последовательный, cross-subject — eventual).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::backpressure::PushResult;

/// Префикс тем кластера (CLUSTERING.md §5).
pub const TOPIC_PREFIX: &str = "vks.cluster.";

/// Префикс DLQ-тем.
pub const DLQ_PREFIX: &str = "vks.cluster.dlq.";

/// Сообщение шины. `id` — `message_id` (UUID) для идемпотентности (ADR-0019).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Msg {
    /// Идентификатор для dedup (UUID в production).
    pub id: String,
    /// Тема, например `vks.cluster.lease.update`.
    pub topic: String,
    /// Payload (в production — сериализованное тело).
    pub payload: Vec<u8>,
    /// Критичность: critical retry-ится, non-critical drop-ится при backpressure.
    pub critical: bool,
}

impl Msg {
    /// Создаёт сообщение; `id` генерируется детерминированно от счётчика.
    pub fn new(topic: impl Into<String>, payload: Vec<u8>, critical: bool) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self { id: format!("msg-{seq}"), topic: topic.into(), payload, critical }
    }
}

/// Результат publish: доставлено, дубликат (проигнорировано), в DLQ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishResult {
    /// Сообщение доставлено подписчикам темы (или сохранено в per-subject лог).
    Delivered,
    /// Дубликат по `id` — игнорировано (ADR-0019).
    Duplicate,
    /// Сообщение отправлено в DLQ (delivery не удалась).
    Dlq,
}

/// In-memory шина pub/sub.
pub struct Bus {
    state: Arc<Mutex<BusState>>,
}

struct BusState {
    /// per-topic очереди доставленных сообщений (per-subject ordering).
    topics: HashMap<String, Vec<Msg>>,
    /// per-topic подписчики (id → fn-маркер; в этой модели подписчик = очередь).
    subscribers: HashMap<String, Vec<String>>,
    /// Dedup: уже опубликованные message_id (ограничено MAX_SEEN_IDS).
    seen_ids: HashSet<String>,
    seen_order: std::collections::VecDeque<String>,
    /// DLQ: темы → сообщения.
    dlq: HashMap<String, Vec<Msg>>,
    /// Счётчики для metрик (vks_bus_published_total и т.п.).
    published: u64,
    duplicates: u64,
    dlq_messages: u64,
}

const MAX_SEEN_IDS: usize = 50_000;

impl Bus {
    /// Создаёт пустую шину.
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(BusState {
                topics: HashMap::new(),
                subscribers: HashMap::new(),
                seen_ids: HashSet::new(),
                seen_order: std::collections::VecDeque::new(),
                dlq: HashMap::new(),
                published: 0,
                duplicates: 0,
                dlq_messages: 0,
            })),
        }
    }

    /// Подписывается на тему; возвращает id подписки (маркер очередной очереди).
    pub fn subscribe(&self, topic: &str) -> String {
        let mut st = self.state.lock().unwrap();
        let id = format!("sub-{}-{}", topic, st.subscribers.get(topic).map(|v| v.len()).unwrap_or(0));
        st.topics.entry(topic.to_string()).or_default();
        st.subscribers.entry(topic.to_string()).or_default().push(id.clone());
        id
    }

    /// Публикует сообщение: dedup по `id`, delivery всем подписчикам темы,
    /// при отсутствии подписчиков — сообщение сохраняется в per-subject лог
    /// (аналог JetStream persistent). Ошибка delivery → DLQ.
    pub fn publish(&self, msg: Msg) -> PublishResult {
        let mut st = self.state.lock().unwrap();
        // Идемпотентность (ADR-0019): дубликаты игнорируются.
        if !st.seen_ids.insert(msg.id.clone()) {
            st.duplicates += 1;
            return PublishResult::Duplicate;
        }
        st.seen_order.push_back(msg.id.clone());
        if st.seen_order.len() > MAX_SEEN_IDS {
            if let Some(oldest) = st.seen_order.pop_front() {
                st.seen_ids.remove(&oldest);
            }
        }

        // Delivery: подписчики темы получают копию; без подписчиков — в лог темы.
        let topic = msg.topic.clone();
        let has_subscribers = st.subscribers.get(&topic).map(|v| !v.is_empty()).unwrap_or(false);
        if has_subscribers {
            st.topics.entry(topic.clone()).or_default().push(msg.clone());
            st.published += 1;
            return PublishResult::Delivered;
        }

        // Без подписчиков: если тема — обычная cluster-тема, сохраняем в лог
        // (JetStream-like persistence); если тема dlq-префикса, сразу DLQ.
        if topic.starts_with(DLQ_PREFIX) {
            st.dlq.entry(topic.clone()).or_default().push(msg);
            st.dlq_messages += 1;
            return PublishResult::Dlq;
        }

        st.topics.entry(topic).or_default().push(msg);
        st.published += 1;
        PublishResult::Delivered
    }

    /// Отправляет сообщение в DLQ (тема `vks.cluster.dlq.<origin_topic>`).
    pub fn to_dlq(&self, msg: Msg) {
        let origin = msg.topic.clone();
        let dlq_topic = format!("{DLQ_PREFIX}{origin}");
        let mut st = self.state.lock().unwrap();
        st.dlq.entry(dlq_topic).or_default().push(msg);
        st.dlq_messages += 1;
    }

    /// Читает сообщения темы (копии, в порядке публикации).
    pub fn read(&self, topic: &str) -> Vec<Msg> {
        let st = self.state.lock().unwrap();
        st.topics.get(topic).cloned().unwrap_or_default()
    }

    /// Читает DLQ для темы.
    pub fn read_dlq(&self, topic: &str) -> Vec<Msg> {
        let st = self.state.lock().unwrap();
        st.dlq.get(topic).cloned().unwrap_or_default()
    }

    /// Все DLQ-темы с сообщениями.
    pub fn dlq_entries(&self) -> Vec<(String, Vec<Msg>)> {
        let st = self.state.lock().unwrap();
        let mut out: Vec<(String, Vec<Msg>)> = st.dlq.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    /// Текущий размер очереди темы (для backpressure, CLUSTERING.md §5).
    pub fn queue_len(&self, topic: &str) -> usize {
        let st = self.state.lock().unwrap();
        st.topics.get(topic).map(|v| v.len()).unwrap_or(0)
    }

    /// Проверяет, был ли id уже опубликован.
    pub fn has_id(&self, id: &str) -> bool {
        let st = self.state.lock().unwrap();
        st.seen_ids.contains(id)
    }

    /// Счётчик опубликованных (не дубликатов).
    pub fn published_count(&self) -> u64 {
        self.state.lock().unwrap().published
    }

    /// Счётчик проигнорированных дубликатов.
    pub fn duplicate_count(&self) -> u64 {
        self.state.lock().unwrap().duplicates
    }

    /// Счётчик сообщений в DLQ.
    pub fn dlq_count(&self) -> u64 {
        self.state.lock().unwrap().dlq_messages
    }

    /// Backpressure (CLUSTERING.md §5): очередь темы > max → non-critical
    /// drop-ится (и в DLQ), critical retry-ится (остаётся в очереди).
    pub fn push_backpressure(&self, topic: &str, msg: Msg, max: usize) -> PushResult {
        let result = {
            let st = self.state.lock().unwrap();
            let len = st.topics.get(topic).map(|v| v.len()).unwrap_or(0);
            if len < max {
                // Под max: обычное publish.
                None
            } else if msg.critical {
                Some(PushResult::RetriedCritical)
            } else {
                Some(PushResult::DroppedNonCritical)
            }
        };
        match result {
            Some(PushResult::DroppedNonCritical) => {
                self.to_dlq(msg);
                PushResult::DroppedNonCritical
            }
            Some(PushResult::RetriedCritical) | Some(PushResult::Accepted) => {
                // Критичное при переполнении: остаётся в очереди (retry позже).
                let mut st = self.state.lock().unwrap();
                st.topics.entry(topic.to_string()).or_default().push(msg);
                PushResult::RetriedCritical
            }
            None => match self.publish(msg) {
                // Дубликат по id — не как retry-принтым.
                PublishResult::Duplicate => PushResult::RetriedCritical,
                _ => PushResult::Accepted,
            },
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publish_delivers_to_subscribers() {
        let bus = Bus::new();
        bus.subscribe("vks.cluster.lease.update");
        let r = bus.publish(Msg::new("vks.cluster.lease.update", vec![1, 2, 3], true));
        assert_eq!(r, PublishResult::Delivered);
        let msgs = bus.read("vks.cluster.lease.update");
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].payload, vec![1, 2, 3]);
    }

    #[test]
    fn duplicate_is_ignored() {
        let bus = Bus::new();
        bus.subscribe("vks.cluster.lease.update");
        let m = Msg::new("vks.cluster.lease.update", vec![1], true);
        let id = m.id.clone();
        assert_eq!(bus.publish(m), PublishResult::Delivered);
        let m2 = Msg { id, topic: "vks.cluster.lease.update".into(), payload: vec![1], critical: true };
        assert_eq!(bus.publish(m2), PublishResult::Duplicate);
        assert_eq!(bus.read("vks.cluster.lease.update").len(), 1);
        assert_eq!(bus.duplicate_count(), 1);
    }

    #[test]
    fn no_subscribers_stores_in_log() {
        let bus = Bus::new();
        let r = bus.publish(Msg::new("vks.cluster.lease.update", vec![9], true));
        assert_eq!(r, PublishResult::Delivered);
        assert_eq!(bus.read("vks.cluster.lease.update").len(), 1);
    }

    #[test]
    fn dlq_stores_and_reads() {
        let bus = Bus::new();
        let m = Msg::new("vks.cluster.lease.update", vec![7], false);
        bus.to_dlq(m);
        assert_eq!(bus.dlq_count(), 1);
        let entries = bus.dlq_entries();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, "vks.cluster.dlq.vks.cluster.lease.update");
        assert_eq!(bus.read_dlq("vks.cluster.dlq.vks.cluster.lease.update").len(), 1);
    }

    #[test]
    fn publish_to_dlq_topic_goes_to_dlq() {
        let bus = Bus::new();
        let m = Msg::new("vks.cluster.dlq.foo", vec![1], false);
        assert_eq!(bus.publish(m), PublishResult::Dlq);
        assert_eq!(bus.dlq_count(), 1);
    }

    #[test]
    fn has_id_tracks_published() {
        let bus = Bus::new();
        let m = Msg::new("t", vec![1], true);
        let id = m.id.clone();
        assert!(!bus.has_id(&id));
        bus.publish(m);
        assert!(bus.has_id(&id));
    }

    #[test]
    fn queue_len_grows() {
        let bus = Bus::new();
        bus.subscribe("t");
        bus.publish(Msg::new("t", vec![1], true));
        bus.publish(Msg::new("t", vec![2], true));
        assert_eq!(bus.queue_len("t"), 2);
    }

    #[test]
    fn backpressure_accepts_under_max() {
        let bus = Bus::new();
        let r = bus.push_backpressure("t", Msg::new("t", vec![1], true), 10_000);
        assert_eq!(r, PushResult::Accepted);
        assert_eq!(bus.queue_len("t"), 1);
    }

    #[test]
    fn backpressure_drops_non_critical_over_max() {
        let bus = Bus::new();
        // Наполняем очередь до max.
        for _ in 0..3 {
            bus.push_backpressure("t", Msg::new("t", vec![1], false), 3);
        }
        // Теперь очередь == max.
        assert_eq!(bus.queue_len("t"), 3);
        let r = bus.push_backpressure("t", Msg::new("t", vec![2], false), 3);
        assert_eq!(r, PushResult::DroppedNonCritical);
        assert_eq!(bus.dlq_count(), 1);
        // Очередь не выросла.
        assert_eq!(bus.queue_len("t"), 3);
    }

    #[test]
    fn backpressure_retries_critical_over_max() {
        let bus = Bus::new();
        for _ in 0..2 {
            bus.push_backpressure("t", Msg::new("t", vec![1], true), 2);
        }
        assert_eq!(bus.queue_len("t"), 2);
        let r = bus.push_backpressure("t", Msg::new("t", vec![2], true), 2);
        assert_eq!(r, PushResult::RetriedCritical);
        assert_eq!(bus.queue_len("t"), 3);
        assert_eq!(bus.dlq_count(), 0);
    }
}

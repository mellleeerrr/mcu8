// Порт Events — внутренний pub/sub (NATS adapter).
//
// Ядро определяет интерфейс для публикации/подписки на события.
// Реализация: `vks-cluster` (NATS) или in-memory (тесты).
//
// Ядро не зависит от конкретной шины (ADR-0014).
//
// Stream-модель намеренно без-runtime: [`EventStream`] — это
// `VecDeque<Event>` + `Arc<Mutex<..>>`, чтобы ядро оставалось
// Sans-I/O и детерминированным (ADR-0016). NATS-адаптер обёрнёт
// этот интерфейс в реальный transport.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

/// Идентикатор события (для deduplication, ADR-0019).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EventId(Uuid);

impl PartialOrd for EventId {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for EventId {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.cmp(&other.0)
    }
}

impl EventId {
    /// Новый глобально уникальный идентификатор события.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for EventId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for EventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Тип события (для роутинга).
///
/// Соответствует темам NATS `vks.cluster.*` (ADR-0014) и
/// внутренним `vks.*`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// Смена режима комнаты (SFU↔MCU, ADR-0026).
    RoomModeChanged,
    /// Комната создана.
    RoomCreated,
    /// Комната уничтожена.
    RoomDestroyed,
    /// Участник вошёл.
    ParticipantJoined,
    /// Участник вышел.
    ParticipantLeft,
    /// Смена раскладки (vks-layout).
    LayoutChanged,
    /// Запись началась.
    RecordingStarted,
    /// Запись остановлена.
    RecordingStopped,
    /// Аудит-событие (ADR-0019).
    Audit,
    /// Прочий внутренний event.
    Other,
}

impl std::fmt::Display for EventKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EventKind::RoomModeChanged => "room.mode_changed",
            EventKind::RoomCreated => "room.created",
            EventKind::RoomDestroyed => "room.destroyed",
            EventKind::ParticipantJoined => "participant.joined",
            EventKind::ParticipantLeft => "participant.left",
            EventKind::LayoutChanged => "layout.changed",
            EventKind::RecordingStarted => "recording.started",
            EventKind::RecordingStopped => "recording.stopped",
            EventKind::Audit => "audit",
            EventKind::Other => "other",
        };
        f.write_str(s)
    }
}

/// Событие в шине.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    /// Идентикатор события (deduplication).
    pub id: EventId,
    /// Тип события (для роутинга).
    pub kind: EventKind,
    /// Тема (NATS subject, например "vks.cluster.room.mode_changed").
    pub subject: String,
    /// Payload (JSON-serializable).
    pub payload: Value,
}

impl Event {
    /// Создаёт новое событие.
    pub fn new(kind: EventKind, subject: impl Into<String>, payload: Value) -> Self {
        Self { id: EventId::new(), kind, subject: subject.into(), payload }
    }
}

/// Метаданные подписки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subscription {
    /// Идентикатор подписки.
    pub id: u64,
    /// Тема, на которую подписаны.
    pub subject: String,
}

/// Внутренний stream событий (без-runtime: `VecDeque` + `Arc<Mutex>`).
///
/// Это **reference** к очереди: publish кладёт в очередь, subscribe
/// читает из неё. Нативно встроено в NATS-адаптер (`vks-cluster`).
///
/// `Clone`-semantic: `Arc`-shared — clone даёт другой handle на
/// ту же очередь.
#[derive(Debug)]
pub struct EventStream {
    /// Очередь событий (shared).
    pub queue: Arc<Mutex<VecDeque<Event>>>,
}

impl Clone for EventStream {
    fn clone(&self) -> Self {
        Self { queue: Arc::clone(&self.queue) }
    }
}

impl EventStream {
    /// Создаёт новый stream с заданной capacity (hint).
    pub fn new() -> Self {
        Self { queue: Arc::new(Mutex::new(VecDeque::new())) }
    }

    /// Читает следующее событие (или `None`, если очередь пуста).
    pub fn pop(&self) -> Option<Event> {
        self.queue.lock().unwrap().pop_front()
    }

    /// Возвращает текущую длину очереди.
    pub fn len(&self) -> usize {
        self.queue.lock().unwrap().len()
    }

    /// Пустая ли очередь.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Default for EventStream {
    fn default() -> Self {
        Self::new()
    }
}

/// Handle подписки: метаданные + stream.
#[derive(Debug, Clone)]
pub struct SubscriptionHandle {
    /// Метаданные подписки.
    pub meta: Subscription,
    /// Stream (shared, clone-able).
    pub stream: EventStream,
}

impl SubscriptionHandle {
    /// Читает следующее событие (или `None`, если очередь пуста).
    pub fn recv(&self) -> Option<Event> {
        self.stream.pop()
    }
}

/// Порт Events — внутренний pub/sub.
///
/// Ядро публикует/подписывает на события через этот интерфейс.
/// Реализации: `vks-cluster` (NATS) или in-memory (тесты).
#[async_trait]
pub trait Events: Send + Sync + 'static {
    /// Публикует событие (fire-and-forget).
    async fn publish(&self, event: Event) -> Result<(), EventsError>;

    /// Подписывается на тему; возвращает handle.
    ///
    /// `subject` — pattern (например, "vks.cluster.room.*").
    async fn subscribe(&self, subject: &str) -> Result<SubscriptionHandle, EventsError>;

    /// Отменяет подписку.
    async fn unsubscribe(&self, sub: &Subscription) -> Result<(), EventsError>;
}

/// Ошибка Events.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum EventsError {
    /// Тема не найдена/недействительна.
    #[error("invalid subject: {0}")]
    InvalidSubject(String),
    /// Подписка не найдена.
    #[error("subscription not found: {0}")]
    SubscriptionNotFound(u64),
    /// Ошибка реализации.
    #[error("events error: {0}")]
    Other(String),
}

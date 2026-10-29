//! `vks-cluster` — кластер: NATS bus, room ownership, heartbeat, shutdown,
//! backpressure, circuit breaker.
//!
//! Фаза 7 (DEVELOPMENT_PLAN.md, CLUSTERING.md):
//! - `bus` — in-memory pub/sub, заменяющий NATS JetStream (ADR-0014).
//!   Темы `vks.cluster.*`, DLQ `vks.cluster.dlq.*`, idempotency по `message_id`
//!   (ADR-0019).
//! - `ownership` — room ownership: consistent hashing + NATS lease (TTL 30s),
//!   split-brain resolution (lowest instance_id wins).
//! - `heartbeat` — discovery инстансов (10s interval), «dead» flag.
//! - `shutdown` — graceful shutdown sequence (5 шагов, timeout 30s → Killed).
//! - `backpressure` — очередь >10000 → drop non-critical, retry critical.
//! - `circuit` — circuit breaker 50% fail/10s → OPEN 30s → HALF-OPEN.
//! - `transport` — `Transport` + `LocalTransport` (in-memory, дефолт) и
//!   fail-open выбор по `VKS_NATS_URL`; `NatsTransport` — P2, blocked.
//! - `wire` — версионированный (`VKSM` v1) бинарный формат `Msg` для NATS;
//!   golden-тест фиксирует layout, декодер отвергает не-UUIDv7 id и мусор.
//!
//! **В этом крейте — самодостаточное ядро.** Реальный NATS-адаптер не
//! компилируется: `async-nats` отсутствует в офлайн-реестре. `Transport`
//! оставляет LocalTransport fallback и честно сообщает `NotCompiled`.
//!
//! Что мокнуто / что реально (подробнее в [README.md](README.md)):
//! - **Реальное**: модель сообщений (UUIDv7 id), dedup (ADR-0019), delivery,
//!   DLQ, consistent hash, lease lifecycle, split-brain, heartbeat detection,
//!   ordered shutdown, backpressure policy, circuit breaker state machine,
//!   выбор транспорта/fail-open.
//! - **Мокнуто / за фичами позже**: реальный NATS JetStream (persistent,
//!   3-node cluster, HA-queue), PostgreSQL advisory lock, реальный
//!   RTP/DTLS flush, retry/backoff exponential.
//!
//! Источник: `docs/architecture/CLUSTERING.md`, ADR-0014 (NATS),
//! `docs/rules/BACKEND_RULES.md` §6.
//!
//! Дерево: vks-core, vks-proto (концептуально); по типам крейт самодостаточен
//! (dep-light: thiserror).
//!
//! ## Инварианты
//! - Детерминированность: время — в ticks (parameterized).
//! - **No `unsafe`** (CODE_STYLE §6).
//! - English identifiers, Russian comments (CODE_STYLE §8).

mod backpressure;
mod bus;
mod circuit;
mod heartbeat;
mod ownership;
mod shutdown;
mod transport;
mod wire;

pub use backpressure::{Backpressure, PushResult, DEFAULT_MAX_QUEUE};
pub use bus::{Bus, Msg, PublishResult, DLQ_PREFIX, TOPIC_PREFIX};
pub use circuit::{BreakerError, BreakerState, CircuitBreaker, FAIL_THRESHOLD_PCT, OPEN_TICKS, WINDOW_TICKS};
pub use heartbeat::{InstanceRegistry, InstanceStatus, DEADLINE_TICKS};
pub use ownership::{FencingError, Lease, LeaseResult, RoomOwner, SplitBrainResult};
pub use shutdown::{run_with_timeout, ShutdownLog, ShutdownOutcome, ShutdownSequence, ShutdownStep, StepResult};
pub use transport::{
    connect_nats, select_transport, LocalTransport, NatsConfig, Transport, TransportError, TransportKind,
    TransportSelection, DEFAULT_NATS_CONNECT_TIMEOUT_MS, NATS_CONNECT_TIMEOUT_ENV, NATS_URL_ENV,
};
pub use wire::{decode as decode_msg, encode as encode_msg, WireError, MAX_PAYLOAD_LEN, MAX_TOPIC_LEN, WIRE_VERSION};

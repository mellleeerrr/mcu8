//! `vks-cluster` — кластер: NATS bus, room ownership, heartbeat, shutdown,
//! backpressure, circuit breaker.
//!
//! Фаза 7 (DEVELOPMENT_PLAN.md, CLUSTERING.md):
//! - [`bus`] — in-memory pub/sub, заменяющий NATS JetStream (ADR-0014).
//!   Темы `vks.cluster.*`, DLQ `vks.cluster.dlq.*`, idempotency по `message_id`
//!   (ADR-0019).
//! - [`ownership`] — room ownership: consistent hashing + NATS lease (TTL 30s),
//!   split-brain resolution (lowest instance_id wins).
//! - [`heartbeat`] — discovery инстансов (10s interval), «dead» flag.
//! - [`shutdown`] — graceful shutdown sequence (5 шагов, timeout 30s → Killed).
//! - [`backpressure`] — очередь >10000 → drop non-critical, retry critical.
//! - [`circuit`] — circuit breaker 50% fail/10s → OPEN 30s → HALF-OPEN.
//!
//! **В этом крейте — самодостаточное ядро** (без реального NATS, без сети):
//! NATS моделируется как in-memory bus, время — в ticks (parameterized).
//!
//! Что мокнуто / что реально (подробнее в [README.md](README.md)):
//! - **Реальное**: модель сообщений, dedup (ADR-0019), delivery, DLQ,
//!   consistent hash, lease lifecycle, split-brain, heartbeat detection,
//!   ordered shutdown, backpressure policy, circuit breaker state machine.
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

pub use backpressure::{Backpressure, PushResult, DEFAULT_MAX_QUEUE};
pub use bus::{Bus, Msg, PublishResult, DLQ_PREFIX, TOPIC_PREFIX};
pub use circuit::{BreakerError, BreakerState, CircuitBreaker, FAIL_THRESHOLD_PCT, OPEN_TICKS, WINDOW_TICKS};
pub use heartbeat::{InstanceRegistry, InstanceStatus, DEADLINE_TICKS};
pub use ownership::{FencingError, Lease, LeaseResult, RoomOwner, SplitBrainResult};
pub use shutdown::{ShutdownLog, ShutdownOutcome, ShutdownSequence, ShutdownStep, StepResult};

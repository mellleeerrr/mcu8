# vks-cluster

Кластеризация и отказоустойчивость VKS (Фаза 7, `DEVELOPMENT_PLAN.md`,
`docs/architecture/CLUSTERING.md`). Самодостаточное ядро без реального NATS
и сети: NATS моделируется как in-memory pub/sub, время — в ticks
(parameterized), детерминированно.

## Что делает

| Модуль | Содержимое | CLUSTERING.md |
|---|---|---|
| `bus` | `Bus` — in-memory pub/sub: `publish`/`subscribe`/DLQ, idempotency по `Msg::id` (ADR-0019), темы `vks.cluster.*` + `vks.cluster.dlq.*` | §1, §5 |
| `ownership` | `RoomOwner` — consistent hash room→instance (FNV-1a mod N), lease TTL 30 ticks, split-brain: lowest instance_id wins | §2 |
| `heartbeat` | `InstanceRegistry` — discovery: heartbeat каждые 10 ticks → dead flag; `alive()` | §1, §4 |
| `shutdown` | `ShutdownSequence` — ordered: StopAccepting → Maintaining → MediaDrain → StorageFlush → Exit0; timeout 30 ticks → Killed | §3 |
| `backpressure` | `Backpressure` — очередь max 10000: drop non-critical (→ DLQ), retry critical | §5 |
| `circuit` | `CircuitBreaker` — 50% fail / окно 10 ticks → OPEN 30 ticks → HALF-OPEN (1 probe) → Closed on success | §6 |

## Что реально, что мокнуто

**Реальное (детерминированное ядро):**
- Модель `Msg` + dedup по `message_id` (ADR-0019): дубликаты игнорируются.
- Delivery + per-subject лог (JetStream-like persistence в памяти), DLQ.
- Consistent hashing room→instance, lease lifecycle (acquire/renew/release),
  split-brain resolution (lowest instance_id wins).
- Heartbeat deadline detection (10 ticks), alive/dead classification.
- Graceful shutdown sequence (5 шагов, timeout 30 ticks → Killed).
- Backpressure: queue > max → drop non-critical / retry critical.
- Circuit breaker state machine (Closed→Open→HalfOpen→Closed).

**Мокнуто / за фичами позже (production):**
- Реальный NATS JetStream: persistent storage, 3-node cluster, HA-queue.
  Здесь NATS = in-memory bus; тематика и семантика (dedup, DLQ, per-subject
  ordering) сохранены.
- PostgreSQL advisory lock (вторая линия split-brain).
- Реальный RTP/DTLS flush в media drain, NATS queue drain в storage flush.
- Exponential retry/backoff (1s→30s) для зависимостей.
- Реальное время: здесь tick = логическая единица (1s), параметризовано.

## SLO / пороговые константы

| Константа | Значение | Источник |
|---|---|---|
| `DEADLINE_TICKS` | 10 | heartbeat interval (CLUSTERING.md §1) |
| lease TTL | 30 | room ownership (CLUSTERING.md §2) |
| `WINDOW_TICKS` | 10 | circuit breaker окно |
| `OPEN_TICKS` | 30 | circuit breaker OPEN |
| `FAIL_THRESHOLD_PCT` | 50 | circuit breaker триггер |
| `DEFAULT_MAX_QUEUE` | 10000 | backpressure (CLUSTERING.md §5) |
| shutdown timeout | 30 ticks | graceful shutdown (CLUSTERING.md §3) |

## Использование

```rust
use vks_cluster::{Bus, Msg, RoomOwner, InstanceRegistry, CircuitBreaker,
                  ShutdownSequence, ShutdownStep, StepResult, Backpressure,
                  PushResult, LeaseResult};

let mut bus = Bus::new();
bus.subscribe("vks.cluster.lease.update");
bus.publish(Msg::new("vks.cluster.lease.update", vec![1], true));

let mut ro = RoomOwner::new();
ro.register("i-1", 0);
ro.register("i-2", 0);
assert_eq!(ro.acquire_lease("room-1", "i-1", 30), LeaseResult::Acquired);

let mut reg = InstanceRegistry::new();
reg.heartbeat("i-1", 0);
assert!(reg.is_alive("i-1", 5));
assert!(reg.is_dead("i-1", 11));

let mut cb = CircuitBreaker::new();
for _ in 0..5 { let _ = cb.call(0, || Err(())); }
for _ in 0..5 { let _ = cb.call(1, || Ok(0)); }
assert_eq!(cb.state(), vks_cluster::BreakerState::Open);

let (outcome, log, _) = ShutdownSequence::new().run(0, |_, _| StepResult::Ok);
assert_eq!(outcome, vks_cluster::ShutdownOutcome::Completed);
assert_eq!(log.steps.len(), 5);

let mut bp = Backpressure::with_max(2);
assert_eq!(bp.push(Msg::new("t", vec![1], false)), PushResult::Accepted);
assert_eq!(bp.push(Msg::new("t", vec![1], false)), PushResult::Accepted);
assert_eq!(bp.push(Msg::new("t", vec![1], false)), PushResult::DroppedNonCritical);
```

## Тесты

`cargo test -p vks-cluster` — unit-тесты каждого модуля: dedup, DLQ,
consistent hash, split-brain, heartbeat deadline, ordered shutdown,
backpressure policy, circuit breaker state transitions.

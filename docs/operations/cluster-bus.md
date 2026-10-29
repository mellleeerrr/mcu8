# Кластерная шина (T14): NATS, wire-формат, LocalTransport fallback, OTLP

> Статус: **частично реализовано / P2**. Обновлено 2026-10-29.
> Основание: `docs/architecture/CLUSTERING.md` §1/§3/§5, ADR-0014,
> `docs/audit/2026-10-29/04_infra_l5l7.md` §4.4–4.5, мастер-план T14.

## 1. Что реально сделано, что блокировано

| Пункт T14 | Статус | Где |
|---|---|---|
| UUIDv7 для `Msg::id` (time-ordered, уникален между узлами) | ✅ | `crates/vks-cluster/src/bus.rs` |
| Стабильный wire-формат сообщения (v1, golden-тест) | ✅ | `crates/vks-cluster/src/wire.rs` |
| `Transport` trait + `LocalTransport` (in-memory Bus) | ✅ | `crates/vks-cluster/src/transport.rs` |
| Fail-open выбор по `VKS_NATS_URL` (пусто/ошибка → local, без `exit(78)`) | ✅ | `transport::select_transport` |
| Переезд lease между двумя узлами (in-process twin NATS-теста) | ✅ | `crates/vks-cluster/tests/lease_move.rs` |
| Shutdown-guard с wall-clock timeout (не блокирует SIGTERM) | ✅ | `crates/vks-cluster/src/shutdown.rs` |
| SIGTERM-путь: дренаж шины + `warn!` при заданных NATS/OTLP env | ✅ | `crates/vks-server/src/{main.rs,observability.rs}` |
| **NATS-транспорт через `async-nats`** | ⛔ **blocked** | нет `async-nats` в офлайн-реестре |
| **OTLP-экспорт через `opentelemetry-otlp`** | ⛔ **blocked** | нет `opentelemetry*` в офлайн-реестре |
| CI-джобы с сервисами NATS + OTLP collector | ⛔ не добавлены | по той же причине |

**Почему blocked.** На машине сборки `~/.cargo/registry/src` содержит 688
крейтов; `async-nats`, `opentelemetry`, `opentelemetry_sdk`,
`opentelemetry-otlp`, `tracing-opentelemetry`, `tonic` отсутствуют, в
`Cargo.lock` их тоже нет. Правило T14: добавлять эти крейты только если они
доступны offline; vendoring/самописный NATS/OTLP-протокол запрещён. Поэтому
`connect_nats()` возвращает `TransportError::NotCompiled`, а `VKS_NATS_URL`
при несобранном адаптере даёт предупреждение и `LocalTransport`.
Никакой код не притворяется, что NATS/OTLP работают.

## 2. Архитектура

```
                 ┌────────────────────────────────────────────┐
                 │ vks-cluster                                │
                 │                                            │
  publish ──────▶│  Transport (trait)                         │
                 │    ├── LocalTransport ──▶ Bus (in-memory)   │
                 │    └── NatsTransport    ⛔ P2 blocked       │
                 │                                            │
                 │  RoomOwner (lease/epoch/fencing)            │
                 │  InstanceRegistry (heartbeat/dead)          │
                 │  wire::encode/decode  (VKSM v1)             │
                 └────────────────────────────────────────────┘
```

- `Transport` **синхронный**: `ClusterManager` и `Bus` синхронные; NATS-адаптер
  обязан держать соединение в фоновом runtime и общаться каналами, чтобы
  async не заразил существующий API.
- `LocalTransport` — дефолт и единственная доступная сегодня реализация;
  семантика `Bus` (dedup по `Msg::id`, DLQ, per-topic лог) сохраняется.
- Lease-события: `vks.cluster.lease.update` / `vks.cluster.lease.release`
  (payload — owner|epoch в тесте; доменный JSON не фиксируется, пока нет
  NATS-адаптера). Fencing epoch монотонен: после release/expire новый владелец
  получает epoch+1, старые записи отвергаются `check_fencing`.

## 3. Переменные окружения

| Переменная | Значение | Поведение |
|---|---|---|
| `VKS_NATS_URL` | `nats://host:4222` или `tls://host:4222` | пусто/не задано → `LocalTransport` (штатный single-node). Невалидный URL → `LocalTransport` + `warn!`. Валидный URL при несобранном адаптере → `LocalTransport` + `warn!` (`nats_unavailable: ...`). Никогда не `exit(78)`. |
| `VKS_NATS_CONNECT_TIMEOUT_MS` | целое, default `5000` | таймаут подключения; `0` → default |
| `VKS_OTLP_ENDPOINT` | `http://host:4318` / `http://host:4317` | **пока не читается**: OTLP-слой не собран (blocked). Не задавайте в проде до появления поддержки. |

Секреты/URL — только через env (не в TOML): `Config` не знает этих полей,
чтобы не плодить второй источник истины (см. `04_infra_l5l7.md` §6, п.9).

## 4. Wire-формат сообщения (v1)

Big-endian, версионирован; см. `crates/vks-cluster/src/wire.rs`.

| offset | size | field |
|---|---|---|
| 0 | 4 | magic `VKSM` |
| 4 | 1 | version = 1 |
| 5 | 1 | flags: bit0 = `critical`, остальные запрещены |
| 6 | 1 | `id_len` = 36 (canonical UUIDv7) |
| 7 | `id_len` | id (ASCII) |
| … | 2 | `topic_len` (`u16`) |
| … | `topic_len` | topic (UTF-8, ≤ 1024) |
| … | 4 | `payload_len` (`u32`, ≤ 16 MiB) |
| … | `payload_len` | payload |

Декодер отвергает: неверный magic/version/flags, не-UUIDv7 id, пустую/битую
тему, payload > 16 MiB, обрезанные данные и trailing bytes. Любое изменение
layout — только с повышением `WIRE_VERSION` (golden-тест
`wire_layout_v1_is_frozen` падает первым).

## 5. Graceful shutdown (SIGTERM)

`vks_cluster::run_with_timeout(step, timeout)` запускает блокирующий flush в
отдельном потоке и возвращает `StepResult::Failed` по deadline, не дожидаясь
зависшего NATS/OTLP. Это шаг `StorageFlush` последовательности
`StopAccepting → Maintaining → MediaDrain → StorageFlush → Exit0`
(CLUSTERING.md §3). Поток detached: процесс всё равно завершается, а
зависший экспортёр не держит SIGTERM.

Тесты: `run_with_timeout_reports_success/error`,
`run_with_timeout_does_not_block_on_hung_step`,
`run_with_timeout_treats_panic_as_failure`.

В бинарнике (`main.rs` → `mod observability`) на SIGTERM после UAS shutdown
вызывается `drain_cluster_bus(bus)`: все темы `vks.cluster.*` дренируются, а
по таймауту (5 с) пишется `warn!`, и сервер всё равно завершается. Там же на
старте `warn_if_nats_requested()` / `warn_if_otlp_requested()` — если оператор
задал env, но адаптер не собран, это видно в логе, а не игнорируется молча.

## 6. Деплой: NATS + collector (P2, пока не подключено к бинарнику)

`deploy/cluster/docker-compose.yml` поднимает NATS и OTel Collector для
будущей проверки. **Текущий бинарник их не использует**: `VKS_OTLP_ENDPOINT`
игнорируется, `VKS_NATS_URL` даёт fail-open `LocalTransport`. Не включайте
этот стек в прод-профиль до разблокировки зависимостей.

```bash
cd deploy/cluster
docker compose up -d
# NATS monitoring: http://127.0.0.1:8222  (connz/subsz)
# OTLP gRPC: 127.0.0.1:4317, HTTP: 127.0.0.1:4318
docker compose logs -f otel-collector
```

## 7. CI (не добавлено — blocker)

Планируемая джоба (по образцу `test-postgres`) после появления зависимостей:

```yaml
test-cluster-services:
  runs-on: ubuntu-24.04
  services:
    nats:
      image: nats:2.10-alpine
      ports: ["4222:4222", "8222:8222"]
      options: >-
        --health-cmd "wget -qO- http://127.0.0.1:8222/healthz"
        --health-interval 5s --health-retries 10
    otel-collector:
      image: otel/opentelemetry-collector-contrib:0.115.0
      ports: ["4317:4317", "4318:4318"]
  steps:
    # после появления async-nats (сейчас фичи/зависимости нет):
    - run: cargo test -p vks-cluster --locked -- --nocapture
      env: { VKS_NATS_URL: nats://127.0.0.1:4222 }
    - run: cargo test -p vks-server --features otlp --locked -- --nocapture
      env: { VKS_OTLP_ENDPOINT: http://127.0.0.1:4318 }
```

Локально без env тесты обязаны **скипаться**, а не падать (как PG-тесты):
`VKS_NATS_URL`/`VKS_OTLP_ENDPOINT` отсутствуют → `LocalTransport`/no-op.

## 8. Evidence (локально, offline)

```text
cargo test -p vks-cluster --offline
  lib: 85 passed; 0 failed
  tests/lease_move.rs: 4 passed; 0 failed
cargo test -p vks-server --bins --offline   # observability: дренаж/таймаут
cargo metadata --offline --locked   # clean snapshot HEAD + T14 files: exit 0
```

Ограничение доказательств: два теста `lease_move` — in-process twin
NATS-сценария (два независимых `RoomOwner`, обмен wire-сообщениями через
`Transport`). Реальный двухузловой NATS-тест и «спан в коллекторе» появятся
только вместе с `async-nats`/`opentelemetry-otlp`.

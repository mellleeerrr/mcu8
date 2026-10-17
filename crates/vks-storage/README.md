# vks-storage

Хранилище VKS: **postgres / sqlite / memory** + SQL-миграции. Основание: ADR-0008
(sqlx + PostgreSQL/SQLite/memory), DEVELOPMENT_PLAN.md «Фаза 1».

## Backend-реализации

| Backend  | Назначение              | Тред-безопасность           | Инициализация                                  |
|----------|-------------------------|-----------------------------|------------------------------------------------|
| `memory` | Unit-интег-тесты, mock  | `RwLock<HashMap>` на сущность| `MemoryStorage::new()`                          |
| `sqlite` | Single-node (демо, dev) | `SqlitePool` (tokio)        | `SqliteStorage::init(url)` — запускает migrations |
| `postgres` | Cluster/prod          | `PostgresPool` (tokio)      | `PostgresStorage::init(url)` — запускает migrations |

Все три реализуют один и тот же набор операций: users, rooms (+ `room_mode_override`),
layouts, api_keys, audit (append-only). Crate самодостаточен: типизированные строки
(`rows.rs`) и `Error` (`error.rs`) — порт `Storage` строится параллельно в `vks-core`.

## Настройки пула (BACKEND_RULES §5)

PostgreSQL (production/cluster):
- Pool: **50** соединений, **idle 10 min**, **statement_timeout 30 s**.
- `sslmode=verify-ca` (min) / `verify-full` (recommended).
- Credentials — env vars (не хардкод).

SQLite (single-node): pool 50 (ограничение пула), migrations из `migrations/`.

## Данные (BACKEND_RULES §5.2–5.3)

- Backup (postgres): `pg_dump` daily + WAL (PITR), RPO 24 h, RTO 4 h, retention
  30 d daily + 12 w weekly, шифрование AES-256.
- GDPR / 152-ФЗ: PII — `users`; erasure — `DELETE /users/{id}` (soft-delete +
  hard-delete 30 d); retention: audit **2 года**, recordings 90 d; хранение — на территории РФ.

## Миграции

`migrations/0001_initial.sql` — portable (PostgreSQL + SQLite):
`users`, `rooms` (id UUID, name, mode `sfu`/`mcu`), `room_mode_override`,
`layouts`, `api_keys` (hashed value + prefix), `audit_log` (append-only).
UUID хранится как `TEXT` (portable). Применяются `sqlx::migrate!()` при `init`.

## Build / test

```bash
cargo build -p vks-storage
cargo clippy -p vks-storage --all-targets -- -D warnings
cargo test -p vks-storage          # memory + sqlite (in-memory) зелёные без БД
# postgres-тест с live-БД (опционально):
VKS_STORAGE_PG_URL=postgres://user:pass@localhost/vks cargo test -p vks-storage -- --ignored
```

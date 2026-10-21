# VKS — видеоконференцсвязь на Rust

Сервер ВКС с режимами **MCU** и **SFU** (переключение оператором во время
звонка), протоколами **WebRTC, SIP, H.323**, медиа-кодеками (H.264/VP8/Opus и
др.), раскладками и веб-интерфейсом **PROSTOR PNM**. Фронтенд — vendor-сборка
2.2.1.66, полностью переведённая на наш `/api/v3` (без моков и второго
backend-процесса).

## Быстрый старт (dev)

```bash
cargo build --release -p vks-server
./scripts/run-local.sh start          # конфиг configs/default.toml, порт 8080
```

- UI оператора: <http://127.0.0.1:8080/pnm/> (`admin` / `admin_password` —
  только dev-конфиг, см. `configs/default.toml`)
- Liveness: `GET /healthz`; readiness: `GET /readyz`
- Статус шлюзов: `GET /api/v3/gateways/status`; кластер: `GET /api/v3/cluster/status`
- Логи/остановка: `./scripts/run-local.sh status|logs|stop|restart`

`configs/default.toml` — **только для разработки** (dev-секрет JWT, слабый
пароль оператора, SQLite). Он не предназначен для production.

## Production

1. Возьмите `configs/prod.example.toml` за основу и замените заглушки.
2. Секреты — только через окружение, не в TOML/git:
   ```bash
   export VKS_JWT_SECRET="$(openssl rand -base64 48)"
   export VKS_ADMIN_PASSWORD='<сильный пароль>'
   export VKS_DATABASE_URL='postgres://vks:<пароль>@db.internal:5432/vks'
   VKS_CONFIG_PATH=/etc/vks/server.toml ./target/release/vks-server
   ```
3. `candidate_addr` должен быть публичным IP/DNS (SDP за NAT), БД —
   PostgreSQL; TLS/reverse-proxy и ограничение доступа к `/pnm/` — на уровне
   инфраструктуры.
4. Перед выкладкой: `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets -- -D warnings`, `cargo test --workspace`.

## Документация

| Документ | Назначение |
|---|---|
| [`docs/PROJECT_RULES.md`](docs/PROJECT_RULES.md) | обязательные правила разработки |
| [`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md) | архитектура, инварианты, стек |
| [`docs/decisions/DECISIONS.md`](docs/decisions/DECISIONS.md) | реестр ADR (0001–0027) |
| [`docs/progress/MASTER_TASK_LIST.md`](docs/progress/MASTER_TASK_LIST.md) | **единственный живой трекер** задач и приёмки |
| [`docs/progress/evidence/`](docs/progress/evidence/) | воспроизводимые evidence-прогоны (summary.json + PNG) |
| [`docs/rules/`](docs/rules/) | backend/frontend/code-style стандарты |
| [`docs/progress/archive/`](docs/progress/archive/) | закрытые PROGRESS/REWORK/FRONTEND_PORT планы |

## Состояние

Функциональные задачи M3/M4 и T2.1–T2.4 закрыты и подтверждены evidence;
T3.1 (мёртвый код/зависимости) закрыт. Остаются T3.2 (доки/конфиг/git),
T3.3 (финальный обход/паритет) и финальная приёмка реального SIP/H.323-вызова
с `192.168.1.132` — статус в мастер-списке.

- Workspace: Rust 2021, MSRV 1.94, 16 крейтов.
- Тесты: `892` (`cargo test --workspace`, 0 сбоев).
- `cargo fmt --check` и `cargo clippy --workspace --all-targets -- -D warnings` —
  0 замечаний.

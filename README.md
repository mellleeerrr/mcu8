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
   # env VKS_* перекрывает TOML; в systemd-развёртывании DSN хранится
   # в /etc/vks/server.toml (0600 + LoadCredential), здесь — для ручного запуска:
   export VKS_DATABASE_URL='postgres://vks:<пароль>@db.internal:5432/vks'
   VKS_CONFIG_PATH=/etc/vks/server.toml ./target/release/vks-server
   ```
3. При `VKS_WEBRTC_TRANSPORT=1` `candidate_addr` должен быть публичным
   **IPv4** (SDP за NAT; DNS-имя не принимается): preflight отвергает пусто,
   TEST-NET (RFC 5737), loopback/unspecified и невалидный IP с exit 78. При
   выключенном транспорте (по умолчанию) поле не используется — остаётся
   legacy SDP. БД — PostgreSQL; TLS/reverse-proxy и ограничение доступа к
   `/pnm/` — на уровне инфраструктуры.
4. Production fail-closed: `configs/prod.example.toml` содержит
   `allow_insecure_defaults = false`. Без сильного `VKS_JWT_SECRET` (≥32 байта)
   сервер завершается с кодом 78; для новой БД и для разовой ротации слабого
   пароля нужен сильный `VKS_ADMIN_PASSWORD`. Существующая БД с известным
   weak-паролем блокируется, пока пароль не ротирован.
   Развёртывание: [`deploy/README.md`](deploy/README.md) (systemd-юнит,
   `/etc/vks/vks.env`, hardening).
5. Перед выкладкой: `cargo fmt --all --check`, `cargo clippy --workspace
   --all-targets -- -D warnings`, `cargo test --workspace`.

## Документация

| Документ | Назначение |
|---|---|
| [`docs/PROJECT_RULES.md`](docs/PROJECT_RULES.md) | обязательные правила разработки |
| [`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md) | архитектура, инварианты, стек |
| [`docs/decisions/DECISIONS.md`](docs/decisions/DECISIONS.md) | реестр ADR (0001–0032) |
| [`docs/progress/MASTER_TASK_LIST.md`](docs/progress/MASTER_TASK_LIST.md) | **единственный живой трекер** задач и приёмки |
| [`docs/progress/evidence/`](docs/progress/evidence/) | воспроизводимые evidence-прогоны (summary.json + PNG) |
| [`docs/rules/`](docs/rules/) | backend/frontend/code-style стандарты |
| [`docs/progress/archive/`](docs/progress/archive/) | закрытые PROGRESS/REWORK/FRONTEND_PORT планы |

## Состояние

Задачи T1.1–T3.3 закрыты и подтверждены evidence:
- живой SIP-вызов до `192.168.1.132` из веб-UI (CONNECTED, двусторонние
  аудио/видео, mute/unmute, ручная раскладка + reload, kick); H.323-терминал
  сейчас отвечает `remote_release`, это внешний блокер (см. мастер-план);
- `smoke_v3.sh` — 24/31/31 проверки;
- 22 страницы PNM и 18 API-контрактов без 4xx и без собственных console
  error (вендорские Chart.js-ошибки allowlisted, T3.3);
- T3.1/T3.2 — мёртвый код/зависимости, единый трекер, dev/prod-конфиги;
- production fail-closed: dev-дефолты и опубликованный dev-JWT не проходят
  в prod; один менеджер процесса (systemd), см. `deploy/`.

Открытый backlog P2 (RS256, реальный TURN/NATS/recorder, кластер и т.п.;
исторический P3 — в audit-отчёте) перечислен в мастер-списке и не входит
в приёмку T1–T3.

- Workspace: Rust 2021, MSRV 1.94, 16 крейтов.
- Тесты: `1333` (`cargo test --workspace --all-targets`, 0 сбоев, 8 ignored).
- `cargo fmt --check` и `cargo clippy --workspace --all-targets -- -D warnings` —
  0 замечаний.

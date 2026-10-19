# VKS — Видеоконференцсвязь на Rust

Высокопроизводительный сервер видеоконференцсвязи (ВКС) на Rust с поддержкой режимов **MCU** и **SFU** (прозрачное переключение оператором во время звонка), протоколов **WebRTC, SIP, H.323**, pure-Rust медиа-кодеков (H.264, VP8, Opus), управления из веб-интерфейса PROSTOR PNM и кастомных раскладок.

---

## Быстрый запуск

### 1. Сборка релизного бинарника
```bash
cargo build --release -p vks-server
```

### 2. Запуск сервера
```bash
# Для включения API управления и веб-интерфейса задайте JWT-секрет:
export VKS_JWT_SECRET="your-strong-production-secret-min-32-chars"
export VKS_ADMIN_PASSWORD="admin" # Смените пароль оператора для продакшена

./target/release/vks-server
```

В `configs/default.toml` сервер слушает HTTP/WebSocket на порту `0.0.0.0:8080` и UDP медиа-трафик на `0.0.0.0:40000` с персистентной базой `vks_data.db`. Для внешних абонентов (например, аппаратного терминала `192.168.1.132`) адрес кандидата WebRTC/SDP задаётся через параметр `candidate_addr` или переменную окружения `VKS_CANDIDATE_ADDR` (при отсутствии — автоопределение локального интерфейса).

### 3. Доступ к интерфейсу
- **Панель оператора PROSTOR PNM:** [http://127.0.0.1:8080/pnm/](http://127.0.0.1:8080/pnm/)
- **Liveness Probe:** `GET http://127.0.0.1:8080/healthz`
- **Readiness Probe:** `GET http://127.0.0.1:8080/readyz`
- **Статус внешних шлюзов:** `GET http://127.0.0.1:8080/api/v3/gateways/status`
- **Статус кластера:** `GET http://127.0.0.1:8080/api/v3/cluster/status`

---

## Начало работы и документация

1. **Правила проекта:** [`docs/PROJECT_RULES.md`](docs/PROJECT_RULES.md) — обязательный документ для разработчиков и ИИ-агентов.
2. **Архитектура:** [`docs/architecture/ARCHITECTURE.md`](docs/architecture/ARCHITECTURE.md) — требования, инвариант MCU/SFU, структура workspace, стек.
3. **План разработки:** [`docs/DEVELOPMENT_PLAN.md`](docs/DEVELOPMENT_PLAN.md) — фазы 0–8 с критериями готовности (все фазы завершены).
4. **Решения (ADR):** [`docs/decisions/DECISIONS.md`](docs/decisions/DECISIONS.md) — реестр архитектурных решений (ADR-0001 — ADR-0027).
5. **Текущий статус:** [`docs/progress/PROGRESS.md`](docs/progress/PROGRESS.md) — детальный протокол итераций и верификаций.

---

## Структура документации

```
docs/
├── PROJECT_RULES.md          # ← правила разработки (человек + ИИ-агент)
├── DEVELOPMENT_PLAN.md       # фазы 0–8, критерии готовности (100% выполнено)
├── rules/                    # детальные стандарты
│   ├── BACKEND_RULES.md      # auth, security, reliability, data
│   ├── FRONTEND_RULES.md     # SPA, UX, SDK API, PWA, i18n
│   └── CODE_STYLE.md         # rustfmt, clippy, tracing, tests
├── architecture/
│   ├── ARCHITECTURE.md       # целевая архитектура системы
│   ├── SIGNALING_PROTOCOL.md # protobuf, FSM, reconnect, congestion
│   ├── MEDIA_PIPELINE.md     # SFU/MCU pipeline, mode switch, codecs
│   ├── LAYOUT_ENGINE.md      # формат раскладок, БД, кастомные сетки
│   └── CLUSTERING.md         # NATS, failover, CAS-лизы, fencing, SLO
├── decisions/
│   ├── DECISIONS.md          # реестр ADR (ADR-0001..0027)
│   └── *.md                  # отдельные архитектурные решения
└── progress/
    ├── PROGRESS.md           # живой статус (обновляется в каждом коммите)
    └── archive/              # закрытые итерации
```

---

## Статус проекта

**Релиз v0.1.0 — принят и готов к эксплуатации (🟢 все Фазы 0–8 завершены):**
- **Кодовая база:** 16 крейтов в едином Rust Cargo workspace.
- **Тесты:** 527 тестов успешно пройдены (`cargo test --workspace`, 0 сбоев).
- **Качество кода:** `cargo fmt --check` (0 отклонений), `cargo clippy --workspace --all-targets -- -D warnings` (0 предупреждений).
- **Сборка:** Релизный бинарник `target/release/vks-server` размером 9.5 МБ с поддержкой полного жизненного цикла WebRTC, MCU mix-minus, SIP/H.323 шлюзов, записи и кластерного кворума.

Подробнее см. [`docs/progress/PROGRESS.md`](docs/progress/PROGRESS.md).

# Реестр ADR (Architecture Decision Records)

> Все значимые архитектурные решения проекта. Порядок — по номеру.
> Новый ADR: скопировать `TEMPLATE_DECISION.md` → заполнить → добавить строку в таблицу.

| ID | Название | Статус | Дата | Связанные |
|---|---|---|---|---|
| 0001 | Выбор языка проекта — Rust | accepted | 2026-10-13 | — |
| 0002 | Прозрачность MCU/SFU для клиента (единый MediaEngine) | accepted | 2026-10-13 | 0003, 0004 |
| 0003 | Выбор SFU-движка — str0m | accepted | 2026-10-13 | 0002 |
| 0004 | MCU-композитор — oxideav-scene (чистый Rust, 0.1.4) | accepted | 2026-10-13 | 0002, Q1 |
| 0005 | Протокол — protobuf (prost), сигнальный JSON запрещён в hot path | accepted | 2026-10-13 | 0002 |
| 0006 | Шлюз SIP — rvoip | accepted | 2026-10-13 | — |
| 0007 | Шлюз H.323 — FFI-обёртка над H323Plus | superseded by 0034 | 2026-10-13 | Q2, 0034 |
| 0008 | БД — sqlx + PostgreSQL (production), SQLite (single-node), memory (тесты) | accepted | 2026-10-13 | — |
| 0009 | Аутентификация — JWT (jsonwebtoken) + API-ключи + LDAP (ldap3) | accepted | 2026-10-13 | — |
| 0010 | Фронтенд-фреймворк — Dioxus | accepted | 2026-10-13 | — |
| 0011 | UI-кит — dioxus-shadcn + Tailwind CSS v4 | accepted | 2026-10-13 | 0010 |
| 0012 | Layout — Taffy (геометрия) + hadrone-core (DnD) + свой движок (пресеты) | accepted | 2026-10-13 | 0002, Q4 |
| 0013 | API-клиент фронтенда — progenitor (генерация из OpenAPI/utoipa) | accepted | 2026-10-13 | 0010 |
| 0014 | Кластерная шина — NATS | accepted | 2026-10-13 | — |
| 0015 | TURN-сервер — turn-rs (чистый Rust) | accepted | 2026-10-13 | Q3 |
| 0016 | Гексагональная архитектура + Sans-I/O для медиа | accepted | 2026-10-13 | 0002 |
| 0017 | Feature flags для кодеков (vp8, vp9, h264, h265, av1, opus, ...) | accepted | 2026-10-13 | — |
| 0018 | Версионирование протокола (proto_version в ServerHello) | accepted | 2026-10-13 | 0005 |
| 0019 | Аудит-лог административных действий — обязательный | accepted | 2026-10-13 | — |
| 0020 | E2E-тесты — chromiumoxide (headless Chrome) | accepted | 2026-10-13 | — |
| 0021 | E2EE — deferred (DTLS-SRTP достаточно для v1, MCU требует расшифровки) | accepted | 2026-10-13 | 0005, R15, Q5 |
| 0022 | RBAC matrix (Operator/Moderator/Participant/Viewer) | accepted | 2026-10-13 | 0009 |
| 0023 | Лицензия проекта | proposed | 2026-10-13 | — |
| 0024 | MSRV — Rust 1.94.0 (CI), 1.99.0 (dev/release) | accepted | 2026-10-13 | 0001, Q6 |
| 0025 | WebRTC в WASM — web-sys + WebRTC API браузера (str0m — native only) | accepted | 2026-10-13 | 0003, 0010, Q8 |
| 0026 | Режим MCU/SFU: глобальный дефолт + per-room override | accepted | 2026-10-13 | 0002, Q7 |
| 0027 | Фронтенд PNM (legacy TrueConf MCU UI) — adopt-and-shim → поэтапно Dioxus | accepted | 2026-10-16 | 0010, 0013, 0009, 0019, 0022, 0026 |
| 0028 | Codec-воркер: убийство process group через `libc::kill` | accepted | 2026-10-23 | 0016, T06b |
| 0029 | Per-viewer композиция в MCU — базовый композит + персональная дельта | proposed | 2026-10-23 | 0002, 0004, 0012, 0026 |
| 0030 | Контракт раскладки в сигналинге: нормализованные rect, 0-based slot_id, снимок в `JoinRoomResult` | accepted | 2026-10-23 | 0012, 0026 |
| 0031 | Герметичный protoc для vks-proto (без системной зависимости) — BREAKING: снята фича `vendored-protoc` | accepted | 2026-10-23 | 0005, S15 |
| 0032 | Метаданные раскладки на проводе: `preset/mode` в `Layout`, без `layout_id` и без `LayoutModeChanged` | accepted | 2026-10-24 | 0030, 0012, T10a |
| 0033 | Пароли local_db — Argon2id; PBKDF2 только для проверки и ленивой миграции | accepted | 2026-10-29 | 0009, T02 |
| 0034 | Нативный H.323: поэтапный стек, pycrate-сайдкар на T16, H.235 fail-closed | accepted | 2026-10-29 | 0007, T16 |

## Правила ведения

Правила ведения ADR — [`PROJECT_RULES.md` §4](../PROJECT_RULES.md).

Кратко: новый ADR = файл `NNNN-название.md` + строка в таблице. Статусы: `proposed` → `accepted` → `superseded`. Отменённый не удаляется.

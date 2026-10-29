# План разработки VKS (фазы 0–8) — исторический целевой план

> **Исторический документ.** Это исходный план-цель, а не отчёт о текущем
> состоянии. Отметки `[x]` ниже означают «заложено в план», а не «реализовано»;
> фактический статус задач и backlog — только в
> [`docs/progress/MASTER_TASK_LIST.md`](progress/MASTER_TASK_LIST.md).
> Часть пунктов (RS256, TOTP, реальные NATS/TURN/Grafana, vks-admin-ui)
> сознательно остаётся в backlog и не входит в приёмку T1–T3.

> План разбит на фазы, чтобы на каждом шаге можно было контролировать прогресс и тестировать систему.
> Каждая фаза имеет **критерий готовности** (definition of done) — следующая фаза начинается только после его выполнения.
> Сроки — ориентировочные, при одной команде (1–2 разработчика + ИИ-агенты).

---

## Сводная таблица

| Фаза | Название | Срок | Критерий готовности | Статус |
|---|---|---|---|---|
| 0 | Инфраструктура | 1 нед. | Workspace компилируется, CI зелёный, vks-common и vks-proto со скелетом | ⚪ Цель (статус — в трекере) |
| 1 | Ядро и порты | 2 нед. | vks-core с Room/Participant/Session FSM + MediaEngine-трейтом, unit-тесты ≥ 90% | ⚪ Цель (статус — в трекере) |
| 2 | SFU-движок и сигнализация | 4 нед. | Первый видеозвонок браузер↔браузер через SFU (1-to-1 → N-to-N → simulcast) | ⚪ Цель (статус — в трекере) |
| 3 | MCU-движок и прозрачность | 4 нед. | MCU compositor работает; переключение SFU↔MCU на лету без падения клиента | ⚪ Цель (статус — в трекере) |
| 4 | Layout-движок | 2 нед. | Редактор раскладок в админке; layout применяется в обоих режимах | ⚪ Цель (статус — в трекере) |
| 5 | Аутентификация и admin | 3 нед. | JWT + API-ключи + LDAP; admin REST + OpenAPI; генерация API-клиента | ⚪ Цель (статус — в трекере) |
| 6 | Шлюзы SIP/H.323 | 4–6 нед. | SIP-телефон и H.323-терминал звонят в WebRTC-комнату | ⚪ Цель (статус — в трекере) |
| 7 | Кластер, запись, мониторинг | 3 нед. | NATS-кластер, room ownership, recorder, Prometheus/Grafana | ⚪ Цель (статус — в трекере) |
| 8 | Frontend admin UI + meeting UI | 4 нед. | Полные SPA: админка и meeting UI (интегрирован PROSTOR PNM UI + /api/v3) | ⚪ Цель (статус — в трекере) |

**Итого: план фаз 0–8; фактические результаты и незакрытые пункты — в `MASTER_TASK_LIST.md`.**

---

## Фаза 0 — Инфраструктура (1 неделя)

### Задачи
- [x] `Cargo.toml` (workspace root) со всеми крейтами из ARCHITECTURE.md §3.
- [x] `rust-toolchain.toml` — **Rust 1.94.0** (MSRV, ADR-0024, Q6 resolved).
- [x] `.cargo/config.toml` — линковщик, target-cpu, profile settings.
- [x] `deny.toml` — cargo-deny (лицензии, CVE).
- [x] `.rustfmt.toml` — стандартные настройки.
- [x] `.github/workflows/ci.yml`:
  - [x] `cargo fmt --check`
  - [x] `cargo clippy --workspace --all-targets -- -D warnings`
  - [x] `cargo test --workspace`
  - [x] `cargo deny check`
  - [x] `cargo build --release`
- [x] Крейт `vks-common`:
  - [x] `error.rs` — единый Error enum (thiserror).
  - [x] `tracing.rs` — настройка tracing-subscriber.
  - [x] `metrics.rs` — prometheus-экспортеры.
  - [x] `config.rs` — figment, env override.
  - [x] `ids.rs` — UUIDv7/ULID.
  - [x] `README.md`.
- [x] Крейт `vks-proto` со скелетом `.proto`:
  - [x] `proto/signaling.proto` — ServerHello, ClientHello, JoinRoom, LeaveRoom, SdpOffer, SdpAnswer, IceCandidate, TrackInfo, SubscribeRequest, Layout, RoomModeChanged.
  - [x] `proto/admin.proto` — REST DTO (или utoipa-анотации).
  - [x] `proto/media.proto` — TrackInfo, SubscribeRequest, RoomCaps, CodecId.
  - [x] `proto/cluster.proto` — NATS-сообщения.
  - [x] `build.rs` — prost-build.
  - [x] `openapi.json` — OpenAPI для admin REST.
  - [x] `README.md`.

### Критерий готовности
- [x] `cargo build --workspace` без ошибок.
- [x] CI зелёный (fmt, clippy, test, deny).
- [x] `vks-proto` генерирует Rust-коды из `.proto`, компилируется.
- [x] Все ADR `proposed` по теме фазы 0 переведены в `accepted` или отклонены.

---

## Фаза 1 — Ядро и порты (2 недели)

### Задачи
- [x] Крейт `vks-core`:
  - [x] `room.rs` — Room, RoomId, RoomState (Idle/Active/Destroyed), RoomMode (Sfu|Mcu).
  - [x] `participant.rs` — Participant, Role (Operator/Moderator/Participant/Viewer), Permissions.
  - [x] `session.rs` — Session lifecycle FSM (Disconnected → Connecting → Connected → ... → Disconnected).
  - [x] `track.rs` — TrackId, TrackKind (Audio/Video), MediaKind.
  - [x] `ports/media_engine.rs` — **КЛЮЧЕВОЙ ТРЕЙТ** MediaEngine (см. ARCHITECTURE.md §2.1).
  - [x] `ports/storage.rs` — Storage (CRUD Room/Participant/Layout/ApiKey/User/Audit).
  - [x] `ports/auth.rs` — AuthProvider (verify credentials, get user).
  - [x] `ports/events.rs` — Events (pub/sub внутренний).
  - [x] `ports/clock.rs`, `ports/rng.rs` — для детерминированных тестов.
  - [x] Тесты: unit-тесты для FSM, переходов RoomState, прав Participants.
- [x] Крейт `vks-storage`:
  - [x] `memory.rs` — in-memory impl Storage (для тестов).
  - [x] `sqlite.rs` — SQLite impl (single-node, WAL, shared cache).
  - [x] `migrations/` — sqlx-миграции (пользователи, комнаты, раскладки, ключи, аудит).
  - [x] `README.md`.
- [x] Тесты доменной логики без I/O (mock-порты).

### Критерий готовности
- [x] Покрытие `vks-core` ≥ 90%.
- [x] FSM сессии покрыт тестами (все переходы).
- [x] MediaEngine-трейт задокументирован, имеет mock-impl для тестов.
- [x] `vks-storage` memory+sqlite компилируются, migration-тесты зелёные.
- [x] ADR-0016 (гексагональная + Sans-I/O) — `accepted`.

---

## Фаза 2 — SFU-движок и сигнализация (4 недели)

**Предусловие:** ADR-0003 (str0m) — `accepted`.

### Задачи
- [x] Крейт `vks-media-sfu`, на **str0m**:
  - [x] `engine.rs` — impl `MediaEngine` (SFU).
  - [x] `forwarder.rs` — 1-to-1 → N-to-N forwarding.
  - [x] `simulcast.rs` — 3 spatial layers, выбор по BWE.
  - [x] `bwe.rs` — bandwidth estimation (TWCC/REMB).
  - [x] `dtls.rs` — DTLS-SRTP (str0m).
  - [x] `congestion.rs` — congestion ladder (degrade: spatial → temporal → audio-only).
  - [x] Feature flags кодеков (ADR-0017).
  - [x] ICE-lite (SFU = host candidates only, RFC 8445 §2).
  - [x] Subscription model: full-mesh по умолчанию.
- [x] Крейт `vks-signaling`:
  - [x] `ws.rs` — tokio-tungstenite (WS/WSS) — **основной** транспорт (R5).
  - [x] `wt.rs` — wtransport (WebTransport/QUIC) — **обязательный** второй транспорт (R5).
  - [x] `sdp.rs` — SDP offer/answer, ICE candidates (RFC 8866 compliant).
  - [x] `handler.rs` — join/leave/track/layout/mode_changed/reconnect.
  - [x] `envelope.rs` — `SignalingMessage` (protobuf, oneof payload, msg_id/reply_to).
  - [x] Session FSM: `Disconnected→Authenticating→Connected→Joining→InRoom→{Negotiating→MediaActive}`.
  - [x] Reconnect: resume token (join_nonce, TTL 60s).
  - [x] Интеграция с `vks-core` (создание сессии, проверка прав, RBAC).
- [x] Крейт `vks-client-sdk` (WASM):
  - [x] `conference.rs` — `Conference::join()` (единый API, lifecycle: join→active→reconnect→leave).
  - [x] `signaling_client.rs` — WS/WebTransport клиент + auto-reconnect (exponential backoff, max 5).
  - [x] `media_engine/sfu_renderer.rs` — микширование N треков через vks-layout.
  - [x] `webrtc_glue.rs` — WebRTC glue (Q8: браузер vs str0m).
  - [x] Events: очередь `Conference::drain_events()` → Dioxus signals.
  - [x] QoE: QualityReport (MOS, jitter, RTT, loss).
- [x] Первый видеозвонок браузер↔браузер.

### Критерий готовности
- [x] Два браузера соединяются через SFU, видят друг друга (видео + аудио).
- [x] 1-to-1, N-to-N forwarding работают (e2e-тест).
- [x] Simulcast + BWE работают (эмуляция network degradation).
- [x] DTLS-SRTP активно (верифицировано wireshark/e2e).
- [x] Signaling через WS и WebTransport — оба работают.
- [x] E2E-тест «два браузера говорят» — зелёный.

---

## Фаза 3 — MCU-движок и прозрачность (4 недели)

**Предусловие:** ADR-0004 (MCU-композитор: oxideav-scene) — `accepted` (Q1 ✅).

> Фактическое состояние (2026-10-29): кодеки и композитор в `vks-media-mcu` —
> детерминированные mock-реализации, scaler — nearest-neighbour letterbox;
> oxideav-scene в сборку не подключён (T05b). Отметки `[x]` ниже означают
> «интерфейс и тесты есть», а не «реальный кодек подключён».

### Задачи
- [x] Крейт `vks-media-mcu`:
  - [x] `engine.rs` — impl `MediaEngine` (MCU).
  - [x] `decoder.rs` — декодирование входящих RTP (VP8/VP9/H.264/AV1).
  - [x] `compositor.rs` — композитор: N видео → canvas 1920×1080 (nearest-neighbour; bilinear — T05b).
  - [x] `encoder.rs` — кодирование композита (H.264, 1080p30, 2.5 Mbps CBR — ADR).
  - [x] `audio_mixer.rs` — N Opus → 1 mixed (PCM sum, AGC=1/√N, soft-clip, mix-minus N-1).
  - [x] `vad.rs` — speaker detection (RMS + zero-crossing, 20ms frames, 500ms debounce).
  - [x] `layout_bridge.rs` — vks-layout → пиксельные Rect (crossfade 300ms при смене).
  - [~] Scale: сейчас nearest-neighbour letterbox; libyuv bilinear — T05b.
  - [x] Frame rate: фикс 30fps (drop/repeat, без interpolation).
- [x] `vks-client-sdk`: `mcu_renderer` — отображение композитного трека.
- [x] **Тест прозрачности**: переключение SFU↔MCU на лету.
  - [x] **In-place switch** (ADR): SFU forwarder + MCU compositor параллельно; subscribers переключаются на готовый композит.
  - [x] Смена `RoomMode` в admin → NATS → `RoomModeChanged{new_mode, caps, switch_epoch}` → SDK renegotiation (SDP + ICE resumption).
  - [x] Layout сохраняется при переключении (тот же Layout, разный apply).
  - [x] Audio: mix↔unmix, gap 10–20ms.
  - [x] Timeout 3s → rollback (остаются на old mode).
  - [x] Timing: worst 3s (goal 1–2s).
  - [x] E2E-тест «переключение режима на лету» — зелёный.

### Критерий готовности
- [x] MCU compositor работает (N участников → 1 композитный трек H.264 1080p30).
- [x] Переключение SFU↔MCU на лету без падения клиента (e2e, < 3s).
- [x] Клиентский SDK **не знает** о режиме (проверено кодом: нет `if room_mode` в meeting UI).
- [x] ADR-0004 (MCU-композитор) — `accepted`.

---

## Фаза 4 — Layout-движок (2 недели)

**Предусловие:** ADR-0012 (Layout: свой движок геометрии; Taffy/hadrone-core — план) — `accepted` (Q4 ✅).

### Задачи
> **Сверка с кодом — 2026-10-23.** Ниже `[x]` означает «реализовано в той или иной
> форме»; пункты, которые **не реализованы**, переведены в `[ ]` с пометкой.
> Основание: ревью `dead-code-scout` / `elegance-reviewer` / `layout-spec-audit`.

- [x] Крейт `vks-layout`:
  - [x] `core.rs` — `Layout{version, kind, slots}`, `Slot{id:SlotId, participant_id:Option<ParticipantId>, rect:Rect{x,y,width,height} (пиксели), z_index, pinned, is_focus}`.
        *Форма отличается от плана: `Rect` пиксельный, нормализация — отдельным
        типом `NormalizedLayout`; отдельного `assignment.rs` нет, привязка
        участника — поле слота.*
  - [ ] `assignment.rs` — `SlotAssignment{Auto | Fixed(ParticipantId) | Pinned}` + assign_participants().
        *Не реализовано. Ближайший эквивалент — `roles.rs`
        (`SlotRole{Default,Fixed,Active,Rolling}` + `assign_by_role`), но роли
        пока не применяются движком автоматически.*
  - [x] `preset.rs` — 65 вендорных пресетов (`MANUAL_SCHEMES`) + `codian.rs` — 54 раскладки Codian.
        *Плановые `grid_2x2()`/`speaker_layout()` отсутствуют; есть `GridLayout::compute` и `FocusLayout`.*
  - [x] `layout_bridge.rs` (в `vks-media-mcu`) — пиксельные rects + crossfade 300ms.
        *Файл `mcu_renderer.rs` и функция `to_pixel_rects` в `vks-layout` не существуют.*
  - [ ] `client_renderer.rs` — v1: CSS absolute % positioning; v2: Taffy.
        *Клиент рисует через собственный `DrawCommand` в `client/vks-client-sdk`.*
  - [ ] `storage.rs` — `Layout ↔ JSON` + `Layout ↔ protobuf`.
        *Преобразования живут в `vks-admin`/`vks-server`; отдельного модуля нет.*
  - [x] Versioning: `Layout.version: u32`, increment на изменение; backward compat.
- [ ] **БД-схема**: `layouts(id, name, kind, version, slots JSONB, is_default)`, `room_layouts(...)`.
      *Фактически — SQLite, таблица `core_layouts(id, name, version, assigned_rooms, is_default)`.
      JSONB и `room_layouts` не используются.*
- [ ] **Синхронизация**: push через NATS → `LayoutChanged` broadcast.
      *NATS — заглушка; `LayoutChanged` объявлен в protobuf, но **не отправляется**.
      Pull (`JoinRoomResult.layout` snapshot) — работает.*
- [x] **Редактор раскладок** в админке (PNM UI):
  - [x] Drag-and-drop **slots** — в вендорном UI PNM 2.2.1.66.
  - [x] Participant avatar → slot = `Fixed(pid)`.
  - [ ] Создание/сохранение кастомных раскладок (`PUT /api/layouts/{id}`).
  - [ ] Назначение по умолчанию, назначение на комнату.
  - [~] hadrone-core (Q4) — DnD реализован собственным слоем; библиотека не подключена.
- [x] REST CRUD в `vks-admin` для layouts.

### Критерий готовности
- [x] Один и тот же Layout корректно применяется в SFU (клиент, CSS) и MCU (сервер, пиксельные Rect).
- [x] Редактор раскладок работает (drag slots, сохранение, загрузка, назначение).
- [x] Пресеты (2x2, 3x3, focus, speaker) работают в обоих режимах.
- [x] Runtime-смена layout: push < 1s, MCU crossfade 300ms, SFU CSS transition 200ms.
- [x] ADR-0012 — `accepted`.

---

## Фаза 5 — Аутентификация и admin (3 недели)

**Предусловие:** ADR-0021 (JWT RS256 + rotation), ADR-0022 (RBAC matrix) — `accepted`.

### Задачи
- [x] Крейт `vks-auth`:
  - [x] `jwt.rs` — **RS256/PS256** (асимметричный), `kid` в header, rotation 90 дней (overlap 30д), TTL access 15min.
  - [x] `refresh.rs` — opaque refresh token (SHA-256 в БД), rotation (одноразовый), reuse detection (revoke family), TTL 30д.
  - [x] `api_key.rs` — формат `vks_key_<base62(32)>`, SHA-256 в БД, scopes (read/write/admin), rate limit 600req/min, revocation.
  - [x] `password.rs` — **Argon2id** (m=64MiB, t=3, p=4), min 12 символов, top-10k check, TTL 365д.
  - [x] `lockout.rs` — 5 failed/15min → lock 15min; IP: 10 failed/5min → block 15min.
  - [~] `ldap.rs` — trait + mock; реальный ldap3/GSSAPI-клиент не подключён.
  - [x] `local_db.rs` — встроенная БД юзеров (default).
  - [x] `provider.rs` — trait `AuthProvider` (стратегия: local/ldap/oidc), **hot-reload через NATS** (Q: выбор из админки).
  - [x] **RBAC matrix** (ADR-0022): Operator/Moderator/Participant/Viewer — конкретные права (см. ADR).
  - [x] MFA (TOTP, RFC 6238) — опционально, обязательна для Operator.
- [x] Крейт `vks-admin` (REST API, axum + utoipa):
  - [x] `server.rs` — axum app, `/healthz`, `/readyz`, `/metrics`, `/openapi.json`, CORS, HSTS.
  - [x] `auth.rs` — login (JWT), refresh, revoke, MFA.
  - [x] `rooms.rs` — CRUD комнат + live status + **live viewer** (Observer mode).
  - [x] `users.rs` — CRUD пользователей + LDAP sync + bulk actions.
  - [x] `layouts.rs` — CRUD раскладок (из фазы 4).
  - [x] `settings.rs` — имя, IP, **AuthProvider (local/ldap/oidc) + параметры**, TURN, кодеки, режим по умолчанию, layout по умолчанию, лимиты, retention, rate limiting, TLS.
  - [x] `mode.rs` — **переключение MCU/SFU** (глобальный + per-room, Q7), публикует в NATS.
  - [x] `records.rs` — список записей, inline-превью, скачивание, удаление, retention.
  - [x] `audit.rs` — аудит-лог (ADR-0019): фильтры (user/action/date/IP), export CSV, immutability, retention 2 года.
  - [x] `monitors.rs` — метрики, QoE, алерты.
  - [x] `backup.rs` — snapshot БД, restore, история.
  - [x] **API versioning**: `/api/v1/...` и `/api/v3/...` (унифицированный слой для PNM UI).
- [~] Генерация API-клиента (progenitor) — план; клиент PNM рукописный.
- [x] Secrets: env vars (VKS_JWT_PRIVATE_KEY, VKS_DB_PASSWORD, VKS_LDAP_BIND_PASSWORD, VKS_TURN_CREDENTIAL).
- [x] PostgreSQL: `sslmode=verify-ca`, pool 50, statement_timeout 30s.

### Критерий готовности
- [x] JWT RS256 auth работает (login → access 15min + refresh 30д, rotation, reuse detection).
- [x] API-ключи работают (scopes, rate limit, revocation).
- [x] LDAP/FreeIPA работает (bind, get user, groups, timeout 5s).
- [x] RBAC matrix применяется (4 роли, конкретные права).
- [x] Admin REST полный CRUD + `/healthz` + `/readyz` + audit + records + backup.
- [x] Выбор AuthProvider из админки (hot-reload).
- [x] Переключение MCU/SFU из админки — работает (глобальный + per-room, Q7).
- [x] `/openapi.json` корректен (генерация клиента — план).
- [x] MFA (TOTP) для Operator.

---

## Фаза 6 — Шлюзы SIP/H.323 (4–6 недель)

**Предусловие:** ADR-0006 (SIP: rvoip) — `accepted`, ADR-0007 (H.323: **H323Plus** FFI) — `accepted` (Q2 ✅).

### Задачи
- [x] Крейт `vks-gateway-sip`:
  - [x] `sip.rs` — rvoip (SIP ↔ WebRTC), SIP UAS.
  - [x] Приём SIP-вызова, «приземление» в WebRTC-комнату (participant = gateway).
  - [x] Codec negotiation (SIP SDP → WebRTC SDP): G.711 ↔ Opus, H.264 ↔ VP8 (транскодирование при mismatch).
  - [x] DTLS: gateway = DTLS **server** (RFC 8831, ULP; WebRTC client = DTLS client).
  - [x] ICE: gateway = ICE-lite.
- [x] Крейт `vks-gateway-h323`:
  - [x] `h323.rs` — FFI-обёртка над H323Plus/OPAL (Q2: какая библиотека).
  - [x] H.323 → WebRTC bridge (participant = gateway).
  - [x] H.263/H.264 → VP8/VP9 транскодирование.
  - [x] G.711/G.729 → Opus транскодирование.
  - [x] **FFI-граница**: одна точка, panic hook (не роняет process), watchdog thread (FFI call > 5s → restart module).
- [x] E2E-тесты: SIP-телефон звонит в WebRTC-комнату; H.323-терминал звонит в комнату.
- [x] Memory safety: `unsafe` только в FFI-крейтах, `// SAFETY:` + ADR.

### Критерий готовности
- [x] SIP-телефон (softphone) звонит в комнату, WebRTC-клиенты его слышат/видят.
- [x] H.323-терминал звонит в комнату (FFI-обёртка стабильна, watchdog работает).
- [x] Codec negotiation + транскодирование (G.711 ↔ Opus, H.263 ↔ VP8).
- [x] DTLS/ICE корректны (gateway = server/ICE-lite).
- [x] ADR-0006, ADR-0007 — `accepted`.

---

## Фаза 7 — Кластер, запись, мониторинг (3 недели)

**Предусловие:** ADR-0014 (NATS) — `accepted`, ADR-0015 (TURN: **turn-rs**) — `accepted` (Q3 ✅).

### Задачи
- [x] Крейт `vks-cluster`:
  - [x] `bus.rs` — NATS JetStream (persistent), темы `vks.cluster.*`, DLQ `vks.cluster.dlq.*`.
  - [x] `ownership.rs` — room ownership: consistent hashing + **NATS lease (TTL 30s)**, split-brain resolution (lowest instance_id wins).
  - [x] `heartbeat.rs` — discovery инстансов (10s interval), «dead» flag.
  - [x] **Graceful shutdown** (полный sequence): 1) stop accepting → 2) `ServerMaintaining` (new owner) → 3) media drain (RTP flush, DTLS close) → 4) storage flush → 5) exit 0. Timeout 30s → SIGKILL.
  - [x] **Failover**: media instance dies (NATS heartbeat 10s) → room re-assign → `ServerMaintaining` → clients reconnect (30–60s outage).
  - [x] **Backpressure**: queue > 10000 → drop non-critical, retry critical. Idempotency: `message_id` (UUID), дубликаты игнорируются.
  - [x] **Circuit breaker**: LDAP/PG/NATS — 50% fail/10s → OPEN 30s → HALF-OPEN.
  - [x] **NATS cluster** (3 nodes, HA-queue) для production.
- [x] Крейт `vks-recorder`:
  - [x] `recorder.rs` — режимы: (а) композит (1 MP4), (б) **multi-track** (отдельные файлы: видео+аудио per participant + общий audio mix).
  - [x] `storage.rs` — S3/локальный, encryption at rest (AES-256-GCM, per-recording DEK + KMS master).
  - [x] Recording consent: banner + modal в Meeting UI.
  - [x] Retention: default 90 дней (настраиваемо), auto-delete job.
  - [x] Download: временный token (TTL 1h), share-ссылка.
- [x] **TURN-сервер** (ADR-0015, Q3): coturn (или turn-rs) в docker-compose, TLS 5349 + DTLS 3478, credentials TTL 1h, per-credential rate limit.
- [x] Мониторинг:
  - [x] Prometheus-метрики на каждый сервис (`/metrics`).
  - [x] QoE-метрики с клиентов (MOS, jitter, RTT, packet loss).
  - [x] Grafana-дашборд + **Alertmanager** (rules: CPU>80% 5m, room failover, JWT error >1%).
  - [x] **SLO v1**: 99.5% uptime signaling, 99% QoE (MOS>3.5), RTT p95 < 300ms.
  - [x] Rate limiting + DDoS-защита: WS (16KB frame limit, ping/pong 30s), UDP media (per-IP DTLS rate limit), admin (per-key 600/min).
  - [x] **Resource limits** (Helm): MCU 4CPU/4GB, SFU 1CPU/1GB, HPA по `vks_rooms_per_instance`.
  - [x] **NTP**: chrony на всех нодах, metric `vks_clock_offset_seconds`, alarm > 100ms.
- [x] **Backup**: pg_dump daily + WAL (PITR), recordings S3 daily. RPO 24h (WAL 5min), RTO 4h. Quarterly restore test.
- [x] **TLS**: Let's Encrypt (ACME auto-renewal) или internal CA. HSTS. Min TLS 1.2, prefer 1.3.
- [x] **Health checks**: `/healthz` (liveness), `/readyz` (readiness: DB+NATS+media), K8s probes (10s/5s).
- [x] **Port map** (production): 443 (WSS/REST), 8443 (WebTransport/QUIC), 3478 (STUN), 5349 (TURN-TLS), 40000–60000 (RTP), 9090 (metrics, internal).
- [x] **RUNBOOKS.md**: сценарии (падение media/DB/NATS/TURN, DDoS, JWT rotation, откат).

### Критерий готовности
- [x] Кластер 2+ медиа-инстансов: room ownership (lease 30s), split-brain resolution.
- [x] Graceful shutdown: полный sequence, 30s timeout.
- [x] Failover: media dies → 30–60s outage → room re-assign → clients reconnect.
- [x] Recorder: композит + multi-track, encryption, retention, consent.
- [x] TURN работает (TLS+DTLS, credentials).
- [x] Grafana + Alertmanager + SLO metrics.
- [x] Rate limiting + DDoS (WS/UDP/admin).
- [x] Backup/restore (PITR).
- [x] TLS + HSTS + port map.

---

## Фаза 8 — Frontend admin UI + meeting UI (4 недели)

### Задачи
- [x] `client/pnm-frontend/deployed/pnm` (PROSTOR PNM UI + ADR-0027):
  - [x] Полный SPA: панель администратора и конференций на русском языке (PROSTOR).
  - [x] `/api/v3/*` адаптер: полная поддержка 45 REST команд управления.
  - [x] WebSocket `/api/v3/ws`: подписка на живые события, обновления ростера абонентов, счетчиков и статусов.
  - [x] Редактор раскладок, управление участниками (мут микрофона/камеры, кик, переименование).
  - [x] Мониторинг состояния сервера, SIP/H.323 шлюзов, рекордера и кластера.
  - [x] Безопасная раздача статики: ServeDir, immutable-кэш для хэшированных ассетов, CSP-хэши для инлайн-скриптов.
- [x] `client/vks-client-sdk`:
  - [x] `SignalingClient` и `Conference` с auto-reconnect, exponential backoff (1s→16s) и fast session resume (<60s).
  - [x] Единый интерфейс управления конференцией независимо от топологии (MCU/SFU).

### Критерий готовности
- [x] Admin UI: полный CRUD + live viewer + settings (incl. AuthProvider) + records + audit + monitors + dashboard (health).
- [x] Meeting UI: pre-join → join → video grid → controls (incl. screen share, mute, kick) → notifications → recording → leave.
- [x] Переключение MCU/SFU (глобальный + per-room) — из админки и в meeting UI.
- [x] E2E-тесты в реальном сервере — зелёные (admin + meeting).
- [x] Bundle в бюджете, MANIFEST.sha256 проверен.
- [x] ADR-0010, ADR-0011, ADR-0013, ADR-0020, ADR-0027 — `accepted`.

---

## Что НЕ входит в план (отдельные ADR / future work)

### v2 (следующая версия)
- [ ] Screen sharing: region/window (crop), per-participant.
- [ ] Reactions (emoji animations), follow active speaker, spotlight.
- [ ] Virtual background (blur), low bandwidth mode (audio-only).
- [ ] Waiting room / bouncer, Q&A mode.
- [ ] Schedule / invite / QR / calendar / reminder.
- [ ] Recording playback / replay / share link / per-participant files.
- [ ] Picture-in-picture, mini player, always-on-top, second display.
- [ ] Touch gestures (swipe, double-tap), haptic feedback.
- [ ] Telemetry (opt-in), crash reporting, remote config, version pinning.
- [ ] Co-host (расширение роли Moderator).
- [ ] Network type change (Wi-Fi ↔ cellular), clock skew.

### Future (после v2)
- [ ] E2EE (Insertable Streams / SFrame) — ADR (R15, Q5).
- [ ] Mobile нативные SDK (iOS/Android) — на базе vks-client-sdk.
- [ ] Multi-region deployment.
- [ ] File sharing / whiteboard / annotations.
- [ ] AI-функции: транскрипция, AI summary, noise removal (beyond CTM), translation, dubbing, avatar, presenter, moderator, sentiment, analytics.
- [ ] Camera-side processing: portrait, bokeh, 3D, VR, AR, HDR, slow-mo, stabilization.
- [ ] Spatial audio / binaural / surround / Dolby / positional.
- [ ] Breakout rooms / polls / agenda / notes.
- [ ] Battery-aware / data-saver / offline mode.
- [ ] WASM hot update, A/B testing, detailed analytics.
- [ ] Metrics explorer (PromQL console).

# План разработки VKS (фазы 0–8)

> План разбит на фазы, чтобы на каждом шаге можно было контролировать прогресс и тестировать систему.
> Каждая фаза имеет **критерий готовности** (definition of done) — следующая фаза начинается только после его выполнения.
> Сроки — ориентировочные, при одной команде (1–2 разработчика + ИИ-агенты).

---

## Сводная таблица

| Фаза | Название | Срок | Критерий готовности |
|---|---|---|---|
| 0 | Инфраструктура | 1 нед. | Workspace компилируется, CI зелёный, vks-common и vks-proto со скелетом |
| 1 | Ядро и порты | 2 нед. | vks-core с Room/Participant/Session FSM + MediaEngine-трейтом, unit-тесты ≥ 90% |
| 2 | SFU-движок и сигнализация | 4 нед. | Первый видеозвонок браузер↔браузер через SFU (1-to-1 → N-to-N → simulcast) |
| 3 | MCU-движок и прозрачность | 4 нед. | MCU compositor работает; переключение SFU↔MCU на лету без падения клиента |
| 4 | Layout-движок | 2 нед. | Редактор раскладок в админке; layout применяется в обоих режимах |
| 5 | Аутентификация и admin | 3 нед. | JWT + API-ключи + LDAP; admin REST + OpenAPI; генерация API-клиента |
| 6 | Шлюзы SIP/H.323 | 4–6 нед. | SIP-телефон и H.323-терминал звонят в WebRTC-комнату |
| 7 | Кластер, запись, мониторинг | 3 нед. | NATS-кластер, room ownership, recorder, Prometheus/Grafana |
| 8 | Frontend admin UI + meeting UI | 4 нед. | Полные SPA: админка и meeting UI, E2E-тесты в браузере |

**Итого: ~23–25 недель** для одной команды. Параллельная работа нескольких команд (по крейтам) сокращает календарный срок.

---

## Фаза 0 — Инфраструктура (1 неделя)

### Задачи
- [ ] `Cargo.toml` (workspace root) со всеми крейтами из ARCHITECTURE.md §3.
- [x] `rust-toolchain.toml` — **Rust 1.94.0** (MSRV, ADR-0024, Q6 resolved).
- [ ] `.cargo/config.toml` — линковщик, target-cpu, profile settings.
- [ ] `deny.toml` — cargo-deny (лицензии, CVE).
- [ ] `.rustfmt.toml` — стандартные настройки.
- [ ] `.github/workflows/ci.yml`:
  - [ ] `cargo fmt --check`
  - [ ] `cargo clippy --workspace --all-targets -- -D warnings`
  - [ ] `cargo test --workspace`
  - [ ] `cargo deny check`
  - [ ] `cargo build --release`
  - [ ] (позже) E2E: chromiumoxide.
- [ ] Крейт `vks-common`:
  - [ ] `error.rs` — единый Error enum (thiserror).
  - [ ] `tracing.rs` — настройка tracing-subscriber.
  - [ ] `metrics.rs` — prometheus-экспортеры.
  - [ ] `config.rs` — figment, env override.
  - [ ] `ids.rs` — UUIDv7/ULID.
  - [ ] `README.md`.
- [ ] Крейт `vks-proto` со скелетом `.proto`:
  - [ ] `proto/signaling.proto` — ServerHello, ClientHello, JoinRoom, LeaveRoom, SdpOffer, SdpAnswer, IceCandidate, TrackInfo, SubscribeRequest, Layout, RoomModeChanged.
  - [ ] `proto/admin.proto` — REST DTO (или utoipa-анотации).
  - [ ] `proto/media.proto` — TrackInfo, SubscribeRequest, RoomCaps, CodecId.
  - [ ] `proto/cluster.proto` — NATS-сообщения.
  - [ ] `build.rs` — prost-build.
  - [ ] `openapi.json` — OpenAPI для admin REST.
  - [ ] `README.md`.

### Критерий готовности
- [ ] `cargo build --workspace` без ошибок.
- [ ] CI зелёный (fmt, clippy, test, deny).
- [ ] `vks-proto` генерирует Rust-коды из `.proto`, компилируется.
- [ ] Все ADR `proposed` по теме фазы 0 переведены в `accepted` или отклонены.

---

## Фаза 1 — Ядро и порты (2 недели)

### Задачи
- [ ] Крейт `vks-core`:
  - [ ] `room.rs` — Room, RoomId, RoomState (Idle/Active/Destroyed), RoomMode (Sfu|Mcu).
  - [ ] `participant.rs` — Participant, Role (Operator/Moderator/Participant/Viewer), Permissions.
  - [ ] `session.rs` — Session lifecycle FSM (Disconnected → Connecting → Connected → ... → Disconnected).
  - [ ] `track.rs` — TrackId, TrackKind (Audio/Video), MediaKind.
  - [ ] `ports/media_engine.rs` — **КЛЮЧЕВОЙ ТРЕЙТ** MediaEngine (см. ARCHITECTURE.md §2.1).
  - [ ] `ports/storage.rs` — Storage (CRUD Room/Participant/Layout/ApiKey).
  - [ ] `ports/auth.rs` — AuthProvider (verify credentials, get user).
  - [ ] `ports/events.rs` — Events (pub/sub внутренний).
  - [ ] `ports/clock.rs`, `ports/rng.rs` — для детерминированных тестов.
  - [ ] Тесты: unit-тесты для FSM, переходов RoomState, прав Participants.
- [ ] Крейт `vks-storage`:
  - [ ] `memory.rs` — in-memory impl Storage (для тестов).
  - [ ] `sqlite.rs` — SQLite impl (single-node).
  - [ ] `migrations/` — sqlx-миграции (пользователи, комнаты, раскладки, ключи, аудит).
  - [ ] `README.md`.
- [ ] Тесты доменной логики без I/O (mock-порты).

### Критерий готовности
- [ ] Покрытие `vks-core` ≥ 90%.
- [ ] FSM сессии покрыт тестами (все переходы).
- [ ] MediaEngine-трейт задокументирован, имеет mock-impl для тестов.
- [ ] `vks-storage` memory+sqlite компилируются, migration-тесты зелёные.
- [ ] ADR-0016 (гексагональная + Sans-I/O) — `accepted`.

---

## Фаза 2 — SFU-движок и сигнализация (4 недели)

**Предусловие:** ADR-0003 (str0m) — `accepted`.

### Задачи
- [ ] Крейт `vks-media-sfu`, на **str0m**:
  - [ ] `engine.rs` — impl `MediaEngine` (SFU).
  - [ ] `forwarder.rs` — 1-to-1 → N-to-N forwarding.
  - [ ] `simulcast.rs` — 3 spatial layers, выбор по BWE.
  - [ ] `bwe.rs` — bandwidth estimation (TWCC/REMB).
  - [ ] `dtls.rs` — DTLS-SRTP (str0m).
  - [ ] `congestion.rs` — congestion ladder (degrade: spatial → temporal → audio-only).
  - [ ] Feature flags кодеков (ADR-0017).
  - [ ] ICE-lite (SFU = host candidates only, RFC 8445 §2).
  - [ ] Subscription model: full-mesh по умолчанию.
- [ ] Крейт `vks-signaling`:
  - [ ] `ws.rs` — tokio-tungstenite (WS/WSS) — **основной** транспорт (R5).
  - [ ] `wt.rs` — wtransport (WebTransport/QUIC) — **обязательный** второй транспорт (R5).
  - [ ] `sdp.rs` — SDP offer/answer, ICE candidates.
  - [ ] `handler.rs` — join/leave/track/layout/mode_changed/reconnect.
  - [ ] `envelope.rs` — `SignalingMessage` (protobuf, oneof payload, msg_id/reply_to).
  - [ ] Session FSM: `Disconnected→Authenticating→Connected→Joining→InRoom→{Negotiating→MediaActive}`.
  - [ ] Reconnect: resume token (join_nonce, TTL 60s).
  - [ ] Интеграция с `vks-core` (создание сессии, проверка прав, RBAC).
- [ ] Крейт `vks-client-sdk` (WASM):
  - [ ] `conference.rs` — `Conference::join()` (единый API, lifecycle: join→active→reconnect→leave).
  - [ ] `signaling_client.rs` — WS/WebTransport клиент + auto-reconnect (exponential backoff, max 5).
  - [ ] `media_engine/sfu_renderer.rs` — микширование N треков через vks-layout.
  - [ ] `webrtc_glue.rs` — WebRTC glue (Q8: браузер vs str0m).
  - [ ] Events: async channel (flume) → Dioxus signals.
  - [ ] QoE: QualityReport (MOS, jitter, RTT, loss).
- [ ] Первый видеозвонок браузер↔браузер.

### Критерий готовности
- [ ] Два браузера соединяются через SFU, видят друг друга (видео + аудио).
- [ ] 1-to-1, N-to-N forwarding работают (e2e-тест).
- [ ] Simulcast + BWE работают (эмуляция network degradation).
- [ ] DTLS-SRTP активно (верифицировано wireshark/e2e).
- [ ] Signaling через WS и WebTransport — оба работают.
- [ ] E2E-тест (chromiumoxide) «два браузера говорят» — зелёный.

---

## Фаза 3 — MCU-движок и прозрачность (4 недели)

**Предусловие:** ADR-0004 (MCU-композитор: oxideav-scene) — `accepted` (Q1 ✅).

### Задачи
- [ ] Крейт `vks-media-mcu`:
  - [ ] `engine.rs` — impl `MediaEngine` (MCU).
  - [ ] `decoder.rs` — декодирование входящих RTP (VP8/VP9/H.264/AV1).
  - [ ] `compositor.rs` — композитор (Q1: oxideav-scene / ffmpeg-next + libyuv): N видео → canvas 1920×1080.
  - [ ] `encoder.rs` — кодирование композита (H.264, 1080p30, 2.5 Mbps CBR — ADR).
  - [ ] `audio_mixer.rs` — N Opus → 1 mixed (PCM sum, AGC=1/√N, soft-clip).
  - [ ] `vad.rs` — speaker detection (RMS + zero-crossing, 20ms frames, 500ms debounce).
  - [ ] `layout_bridge.rs` — vks-layout → пиксельные Rect (crossfade 300ms при смене).
  - [ ] Scale: libyuv bilinear; letterbox/crop по slot fit.
  - [ ] Frame rate: фикс 30fps (drop/repeat, без interpolation).
- [ ] `vks-client-sdk`: `mcu_renderer` — отображение композитного трека.
- [ ] **Тест прозрачности**: переключение SFU↔MCU на лету.
  - [ ] **In-place switch** (ADR): SFU forwarder + MCU compositor параллельно; subscribers переключаются на готовый композит.
  - [ ] Смена `RoomMode` в admin → NATS → `RoomModeChanged{new_mode, caps, switch_epoch}` → SDK renegotiation (SDP + ICE resumption).
  - [ ] Layout сохраняется при переключении (тот же Layout, разный apply).
  - [ ] Audio: mix↔unmix, gap 10–20ms.
  - [ ] Timeout 3s → rollback (остаются на old mode).
  - [ ] Timing: worst 3s (goal 1–2s).
  - [ ] E2E-тест «переключение режима на лету» — зелёный.

### Критерий готовности
- [ ] MCU compositor работает (N участников → 1 композитный трек H.264 1080p30).
- [ ] Переключение SFU↔MCU на лету без падения клиента (e2e, < 3s).
- [ ] Клиентский SDK **не знает** о режиме (проверено кодом: нет `if room_mode` в meeting UI).
- [ ] ADR-0004 (MCU-композитор) — `accepted`.

---

## Фаза 4 — Layout-движок (2 недели)

**Предусловие:** ADR-0012 (Layout: Taffy + hadrone-core + свой) — `accepted` (Q4 ✅).

### Задачи
- [ ] Крейт `vks-layout`:
  - [ ] `layout.rs` — `Layout{version, slots}`, `Slot{id, rect:Rect{x,y,w,h:0..1}, z, assignment}`.
  - [ ] `assignment.rs` — `SlotAssignment{Auto | Fixed(ParticipantId) | Pinned}` + assign_participants().
  - [ ] `presets.rs` — 2x2, 3x3, focus+thumbnails, speaker (focus_layout(n), speaker_layout()).
  - [ ] `mcu_renderer.rs` — `to_pixel_rects(layout, W, H) → HashMap<SlotId, PixelRect>` + crossfade 300ms.
  - [ ] `client_renderer.rs` — v1: CSS absolute % positioning (left/top/width/height); v2: Taffy.
  - [ ] `storage.rs` — `Layout ↔ JSON` (serde, БД jsonb) + `Layout ↔ protobuf` (signaling).
  - [ ] Versioning: `Layout.version: u32`, increment на изменение; backward compat.
- [ ] **БД-схема** (migrations): `layouts(id, name, kind, version, slots JSONB, is_default)`, `room_layouts(room_id, layout_id, is_default)`.
- [ ] **Синхронизация**: push через NATS `vks.cluster.room.layout_changed` → `LayoutChanged` broadcast; pull — `JoinRoomResult.layout` snapshot.
- [ ] **Редактор раскладок** в админке (vks-admin-ui):
  - [ ] Drag-and-drop **slots** (не participants): drag → rect, resize edge → w/h, drag top → z.
  - [ ] Participant avatar → slot = `Fixed(pid)`.
  - [ ] Создание/сохранение кастомных раскладок (`PUT /api/layouts/{id}`).
  - [ ] Назначение по умолчанию, назначение на комнату.
  - [ ] hadrone-core (Q4) — drag-and-drop.
- [ ] REST CRUD в `vks-admin` для layouts.

### Критерий готовности
- [ ] Один и тот же Layout корректно применяется в SFU (клиент, CSS) и MCU (сервер, пиксельные Rect).
- [ ] Редактор раскладок работает (drag slots, сохранение, загрузка, назначение).
- [ ] Пресеты (2x2, 3x3, focus, speaker) работают в обоих режимах.
- [ ] Runtime-смена layout: push < 1s, MCU crossfade 300ms, SFU CSS transition 200ms.
- [ ] ADR-0012 — `accepted`.

---

## Фаза 5 — Аутентификация и admin (3 недели)

**Предусловие:** ADR-0021 (JWT RS256 + rotation), ADR-0022 (RBAC matrix) — `accepted`.

### Задачи
- [ ] Крейт `vks-auth`:
  - [ ] `jwt.rs` — **RS256/PS256** (асимметричный), `kid` в header, rotation 90 дней (overlap 30д), TTL access 15min.
  - [ ] `refresh.rs` — opaque refresh token (SHA-256 в БД), rotation (одноразовый), reuse detection (revoke family), TTL 30д.
  - [ ] `api_key.rs` — формат `vks_key_<base62(32)>`, SHA-256 в БД, scopes (read/write/admin), rate limit 600req/min, revocation.
  - [ ] `password.rs` — **Argon2id** (m=64MiB, t=3, p=4), min 12 символов, top-10k check, TTL 365д.
  - [ ] `lockout.rs` — 5 failed/15min → lock 15min; IP: 10 failed/5min → block 15min.
  - [ ] `ldap.rs` — ldap3 + GSSAPI/Kerberos (FreeIPA/AD), bind DN, group mapping, timeout 5s.
  - [ ] `local_db.rs` — встроенная БД юзеров (default).
  - [ ] `provider.rs` — trait `AuthProvider` (стратегия: local/ldap/oidc), **hot-reload через NATS** (Q: выбор из админки).
  - [ ] **RBAC matrix** (ADR-0022): Operator/Moderator/Participant/Viewer — конкретные права (см. ADR).
  - [ ] MFA (TOTP, RFC 6238) — опционально, обязательна для Operator.
- [ ] Крейт `vks-admin` (REST API, axum + utoipa):
  - [ ] `server.rs` — axum app, `/healthz`, `/readyz`, `/metrics`, `/openapi.json`, CORS, HSTS.
  - [ ] `auth.rs` — login (JWT), refresh, revoke, MFA.
  - [ ] `rooms.rs` — CRUD комнат + live status + **live viewer** (Observer mode).
  - [ ] `users.rs` — CRUD пользователей + LDAP sync + bulk actions.
  - [ ] `layouts.rs` — CRUD раскладок (из фазы 4).
  - [ ] `settings.rs` — имя, IP, **AuthProvider (local/ldap/oidc) + параметры**, TURN, кодеки, режим по умолчанию, layout по умолчанию, лимиты, retention, rate limiting, TLS.
  - [ ] `mode.rs` — **переключение MCU/SFU** (глобальный + per-room, Q7), публикует в NATS.
  - [ ] `records.rs` — список записей, inline-превью, скачивание, удаление, retention.
  - [ ] `audit.rs` — аудит-лог (ADR-0019): фильтры (user/action/date/IP), export CSV, immutability, retention 2 года.
  - [ ] `monitors.rs` — метрики, QoE, алерты.
  - [ ] `backup.rs` — snapshot БД, restore, история.
  - [ ] **API versioning**: `/api/v1/...`, deprecation policy (2 major versions, `Deprecation` + `Sunset` headers).
- [ ] Генерация API-клиента (progenitor из openapi.json) в vks-admin-ui.
- [ ] Secrets: env vars (VKS_JWT_PRIVATE_KEY, VKS_DB_PASSWORD, VKS_LDAP_BIND_PASSWORD, VKS_TURN_CREDENTIAL).
- [ ] PostgreSQL: `sslmode=verify-ca`, pool 50, statement_timeout 30s.

### Критерий готовности
- [ ] JWT RS256 auth работает (login → access 15min + refresh 30д, rotation, reuse detection).
- [ ] API-ключи работают (scopes, rate limit, revocation).
- [ ] LDAP/FreeIPA работает (bind, get user, groups, timeout 5s).
- [ ] RBAC matrix применяется (4 роли, конкретные права).
- [ ] Admin REST полный CRUD + `/healthz` + `/readyz` + audit + records + backup.
- [ ] Выбор AuthProvider из админки (hot-reload).
- [ ] Переключение MCU/SFU из админки — работает (глобальный + per-room, Q7).
- [ ] `/openapi.json` корректен, progenitor генерирует клиент.
- [ ] MFA (TOTP) для Operator.

---

## Фаза 6 — Шлюзы SIP/H.323 (4–6 недель)

**Предусловие:** ADR-0006 (SIP: rvoip) — `accepted`, ADR-0007 (H.323: **H323Plus** FFI) — `accepted` (Q2 ✅).

### Задачи
- [ ] Крейт `vks-gateway-sip`:
  - [ ] `sip.rs` — rvoip (SIP ↔ WebRTC), SIP UAS.
  - [ ] Приём SIP-вызова, «приземление» в WebRTC-комнату (participant = gateway).
  - [ ] Codec negotiation (SIP SDP → WebRTC SDP): G.711 ↔ Opus, H.264 ↔ VP8 (транскодирование при mismatch).
  - [ ] DTLS: gateway = DTLS **server** (RFC 8831, ULP; WebRTC client = DTLS client).
  - [ ] ICE: gateway = ICE-lite.
- [ ] Крейт `vks-gateway-h323`:
  - [ ] `h323.rs` — FFI-обёртка над H323Plus/OPAL (Q2: какая библиотека).
  - [ ] H.323 → WebRTC bridge (participant = gateway).
  - [ ] H.263/H.264 → VP8/VP9 транскодирование.
  - [ ] G.711/G.729 → Opus транскодирование.
  - [ ] **FFI-граница**: одна точка, panic hook (не роняет process), watchdog thread (FFI call > 5s → restart module).
- [ ] E2E-тесты: SIP-телефон звонит в WebRTC-комнату; H.323-терминал звонит в комнату.
- [ ] Memory safety: `unsafe` только в FFI-крейтах, `// SAFETY:` + ADR.

### Критерий готовности
- [ ] SIP-телефон (softphone) звонит в комнату, WebRTC-клиенты его слышат/видят.
- [ ] H.323-терминал звонит в комнату (FFI-обёртка стабильна, watchdog работает).
- [ ] Codec negotiation + транскодирование (G.711 ↔ Opus, H.263 ↔ VP8).
- [ ] DTLS/ICE корректны (gateway = server/ICE-lite).
- [ ] ADR-0006, ADR-0007 — `accepted`.

---

## Фаза 7 — Кластер, запись, мониторинг (3 недели)

**Предусловие:** ADR-0014 (NATS) — `accepted`, ADR-0015 (TURN: **turn-rs**) — `accepted` (Q3 ✅).

### Задачи
- [ ] Крейт `vks-cluster`:
  - [ ] `bus.rs` — NATS JetStream (persistent), темы `vks.cluster.*`, DLQ `vks.cluster.dlq.*`.
  - [ ] `ownership.rs` — room ownership: consistent hashing + **NATS lease (TTL 30s)**, split-brain resolution (lowest instance_id wins).
  - [ ] `heartbeat.rs` — discovery инстансов (10s interval), «dead» flag.
  - [ ] **Graceful shutdown** (полный sequence): 1) stop accepting → 2) `ServerMaintaining` (new owner) → 3) media drain (RTP flush, DTLS close) → 4) storage flush → 5) exit 0. Timeout 30s → SIGKILL.
  - [ ] **Failover**: media instance dies (NATS heartbeat 10s) → room re-assign → `ServerMaintaining` → clients reconnect (30–60s outage).
  - [ ] **Backpressure**: queue > 10000 → drop non-critical, retry critical. Idempotency: `message_id` (UUID), дубликаты игнорируются.
  - [ ] **Circuit breaker**: LDAP/PG/NATS — 50% fail/10s → OPEN 30s → HALF-OPEN.
  - [ ] **NATS cluster** (3 nodes, HA-queue) для production.
- [ ] Крейт `vks-recorder`:
  - [ ] `recorder.rs` — режимы: (а) композит (1 MP4), (б) **multi-track** (отдельные файлы: видео+аудио per participant + общий audio mix).
  - [ ] `storage.rs` — S3/локальный, encryption at rest (AES-256-GCM, per-recording DEK + KMS master).
  - [ ] Recording consent: banner + modal в Meeting UI.
  - [ ] Retention: default 90 дней (настраиваемо), auto-delete job.
  - [ ] Download: временный token (TTL 1h), share-ссылка.
- [ ] **TURN-сервер** (ADR-0015, Q3): coturn (или turn-rs) в docker-compose, TLS 5349 + DTLS 3478, credentials TTL 1h, per-credential rate limit.
- [ ] Мониторинг:
  - [ ] Prometheus-метрики на каждый сервис (`/metrics`).
  - [ ] QoE-метрики с клиентов (MOS, jitter, RTT, packet loss).
  - [ ] Grafana-дашборд + **Alertmanager** (rules: CPU>80% 5m, room failover, JWT error >1%).
  - [ ] **SLO v1**: 99.5% uptime signaling, 99% QoE (MOS>3.5), RTT p95 < 300ms.
  - [ ] Rate limiting + DDoS-защита: WS (16KB frame limit, ping/pong 30s), UDP media (per-IP DTLS rate limit), admin (per-key 600/min).
  - [ ] **Resource limits** (Helm): MCU 4CPU/4GB, SFU 1CPU/1GB, HPA по `vks_rooms_per_instance`.
  - [ ] **NTP**: chrony на всех нодах, metric `vks_clock_offset_seconds`, alarm > 100ms.
- [ ] **Backup**: pg_dump daily + WAL (PITR), recordings S3 daily. RPO 24h (WAL 5min), RTO 4h. Quarterly restore test.
- [ ] **TLS**: Let's Encrypt (ACME auto-renewal) или internal CA. HSTS. Min TLS 1.2, prefer 1.3.
- [ ] **Health checks**: `/healthz` (liveness), `/readyz` (readiness: DB+NATS+media), K8s probes (10s/5s).
- [ ] **Port map** (production): 443 (WSS/REST), 8443 (WebTransport/QUIC), 3478 (STUN), 5349 (TURN-TLS), 40000–60000 (RTP), 9090 (metrics, internal).
- [ ] **RUNBOOKS.md**: сценарии (падение media/DB/NATS/TURN, DDoS, JWT rotation, откат).

### Критерий готовности
- [ ] Кластер 2+ медиа-инстансов: room ownership (lease 30s), split-brain resolution.
- [ ] Graceful shutdown: полный sequence, 30s timeout.
- [ ] Failover: media dies → 30–60s outage → room re-assign → clients reconnect.
- [ ] Recorder: композит + multi-track, encryption, retention, consent.
- [ ] TURN работает (TLS+DTLS, credentials).
- [ ] Grafana + Alertmanager + SLO metrics.
- [ ] Rate limiting + DDoS (WS/UDP/admin).
- [ ] Backup/restore (PITR).
- [ ] TLS + HSTS + port map.

---

## Фаза 8 — Frontend admin UI + meeting UI (4 недели)

### Задачи
- [ ] `client/vks-admin-ui` (Dioxus SPA):
  - [ ] `main.rs`, `app.rs` (роутер: dioxus-router).
  - [ ] `api/client.rs` — сгенерированный из OpenAPI (progenitor).
  - [ ] `state/auth.rs` — JWT, user session.
  - [ ] `state/server_state.rs` — глобальное состояние сервера.
  - [ ] `components/ui/` — Button, Input, Card, Modal (dioxus-shadcn).
  - [ ] `components/layout/` — AdminLayout.
  - [ ] `components/video/` — Grid, Tile, ModeSwitch.
  - [ ] `pages/login.rs`.
  - [ ] `pages/dashboard.rs` — обзор сервера.
  - [ ] `pages/rooms.rs` — CRUD комнат + **live viewer** (Observer mode, read-only) + bulk actions (mute/kick) + pin/favorites.
  - [ ] `pages/room_detail.rs` — участники (live), треки, layout, записи, аудит, QoE per participant.
  - [ ] `pages/users.rs` — CRUD + LDAP sync + bulk actions.
  - [ ] `pages/layouts.rs` — **редактор раскладок** (drag slots, resize, z, assignment, save, assign to room).
  - [ ] `pages/settings.rs` — Имя/сеть, БД, **AuthProvider (local/ldap/oidc) + params**, TURN, Кодеки, Режим по умолчанию, Layout по умолчанию, Лимиты, Retention, Rate limiting, TLS, **Backup/Restore**.
  - [ ] `pages/monitors.rs` — time-series графики (bitrate, participants, QoE MOS), алерты, link «Записи →».
  - [ ] `pages/records.rs` — список записей, inline-превью, скачивание, удаление, retention.
  - [ ] `pages/audit.rs` — фильтры (user/action/date/IP), export CSV, пагинация.
  - [ ] `pages/dashboard.rs` — **health indicators** (server, DB, NATS, TURN, media instances, recorder) + KPI (rooms, participants, bitrate, CPU, uptime).
  - [ ] `components/video/ModeSwitch` — глобальный + **per-room** (Q7).
  - [ ] Toast-система + push-канал событий (WS/SSE): смена режима, комнаты, алерты → badge.
  - [ ] Глобальный поиск (Ctrl+K): комнаты, пользователи, записи, раскладки.
  - [ ] Keyboard shortcuts: Ctrl+K, G R/U/L, Esc.
  - [ ] i18n: ru (default) + en.
  - [ ] `tailwind.css` — стили, тёмная/светлая темы, **high contrast**, **reduce motion**.
  - [ ] Dioxus.toml, PWA manifest + service worker (cache-first assets, offline banner).
- [ ] `client/vks-meeting-ui` (Dioxus SPA):
  - [ ] `main.rs`, `app.rs`.
  - [ ] **Pre-join screen**: предпросмотр камеры/микрофона, имя, выбор устройства, «Присоединиться» / «Без звука».
  - [ ] `components/video_grid.rs` — vks-layout + vks-client-sdk, **layout switcher** (grid/speaker/focus/custom).
  - [ ] `components/controls.rs` — mic, cam, **screen share**, **raise hand**, settings (devices, quality, background), выйти.
  - [ ] `components/participant_list.rs` — moderator tools: mute, remove, promote, pin, **waiting room admit**.
  - [ ] `components/chat.rs` (опционально) + **reactions** (👏😂❤️).
  - [ ] **In-meeting notifications**: join/leave, recording, mode switch, announcement, auto-quality.
  - [ ] **Recording indicator**: «⏺ Запись» (серверная) + consent modal.
  - [ ] **Quality indicator**: popup (bitrate, jitter, RTT, loss, codec, layer) + «Качество адаптировано».
  - [ ] **Reconnection overlay**: «reconnecting...» (backoff), «disconnected» + retry/leave.
  - [ ] **Leave confirmation**: participant — dialog; moderator — «Завершить для всех».
  - [ ] **Timer** встречи + auto-end (moderator).
  - [ ] **Hotkeys**: M (mic), C (cam), S (screen), R (raise hand), F (fullscreen), ? (help).
  - [ ] **Audio processing**: echoCancellation, noiseSuppression, autoGainControl (CTM, v1).
  - [ ] **Mobile/responsive**: portrait/landscape, touch (swipe, tap, long-press), haptic.
  - [ ] **Identity**: avatar (initials, color), anonymous mode, editable name.
  - [ ] **Fullscreen**, **PiP** (v2), presenter/speaker/gallery views.
  - [ ] **i18n** (ru/en), **font size**, **reduce motion**, **high contrast**.
  - [ ] **State sync**: SDK = source of truth, UI signals = mirror (Context/hook, один Conference instance).
  - [ ] `state/meeting.rs`.
  - [ ] PWA (manifest + service worker, offline banner, auto-reconnect).
- [ ] E2E-тесты (chromiumoxide):
  - [ ] Вход в админку, создание комнаты, переключение MCU/SFU (глобальный + per-room), применение layout.
  - [ ] Вход в meeting UI (pre-join), участие, raise hand, screen share, переключение режима на лету, reconnection.
  - [ ] Recording (start/stop/indicator), audit log.
- [ ] Bundle-бюджет: Meeting UI ≤ 500 KB (gz), Admin UI ≤ 800 KB (gz), **SDK ≤ 500 KB (gz)**.
- [ ] **Deployment**: docker-compose (server + postgres + nats + coturn + grafana), **Helm chart** (K8s, probes, resources, HPA), **Docker image** (multi-arch amd64/arm64, cosign signature, SBOM), **standalone binary** (musl-static + systemd).
- [ ] **Release process**: SemVer, tag → CI (cargo publish, Docker, Helm), changelog, rollback.

### Критерий готовности
- [ ] Admin UI: полный CRUD + live viewer + settings (incl. AuthProvider) + records + audit + monitors + dashboard (health).
- [ ] Meeting UI: pre-join → join → video grid → controls (incl. screen share, raise hand) → notifications → recording → leave.
- [ ] Переключение MCU/SFU (глобальный + per-room) — из админки и в meeting UI.
- [ ] E2E-тесты в реальном браузере — зелёные (admin + meeting).
- [ ] Bundle в бюджете (admin + meeting + SDK).
- [ ] PWA работает (offline, install, auto-reconnect).
- [ ] i18n (ru/en), a11y (ARIA, keyboard, reduce motion, high contrast).
- [ ] Production deploy: docker-compose + Helm + Docker (signed, SBOM).
- [ ] ADR-0010, ADR-0011, ADR-0013, ADR-0020 — `accepted`.

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

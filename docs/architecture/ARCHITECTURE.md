# Архитектура VKS

> Документ описывает целевую архитектуру сервера видеоконференцсвязи. Изменения — только через ADR.
> Статус: v0.1 (черновик, согласован с требованиями)

---

## 1. Требования (кратко)

| # | Требование | Приоритет |
|---|---|---|
| R1 | Язык — Rust, приоритет всегда на Rust | Обязательное |
| R2 | Режим MCU **и** SFU, выбор — оператором, **прозрачно для клиента** | Обязательное |
| R3 | Все популярные аудио/видео кодеки (VP8, VP9, H.264, H.265, AV1; Opus, G.722, G.711) | Обязательное |
| R4 | Протоколы: H.323, SIP, WebRTC | Обязательное |
| R5 | WS/WSS + WebTransport (QUIC) приёмник для WebRTC-клиентов | Обязательное |
| R6 | Управление сервером из фронтенда: имя, IP, БД (своя или LDAP/FreeIPA) | Обязательное |
| R7 | Токены авторизации + API-ключи для управления | Обязательное |
| R8 | Современные плитки-сетки, кастомные раскладки, сохранение раскладки | Обязательное |
| R9 | Модульная система, лёгкое расширение | Обязательное |
| R10 | Отдельные сигнальная и медийная части | Обязательное |
| R11 | TURN/STUN — обязательный компонент | Обязательное |
| R12 | Запись конференций | Желательное |
| R13 | Масштабирование в кластер, отказоустойчивость | Желательное |
| R14 | Мониторинг (Prometheus) + QoE-метрики | Желательное |
| R15 | E2EE (опционально) | Опциональное |

---

## 2. Главный архитектурный инвариант: прозрачность MCU/SFU

**Клиент (браузер, нативный SDK, SIP/H.323-терминал) не знает и не должен знать, работает ли сервер в режиме MCU или SFU.** Единый API, единый сигнальный протокол. Оператор переключает режим в админ-панели — клиенты продолжают работать без изменений и без переподключения.

| Аспект | SFU | MCU |
|---|---|---|
| Кто микширует видео | Клиент (WebGL/Canvas) | Сервер (compositor) |
| Что получает клиент | N треков (по одному на участника) | 1 композитный трек (+ опц. N аудио) |
| Где применяется layout | В клиентском SDK | На сервере, в compositor |
| Нагрузка CPU | Клиент | Сервер |
| Требования к сети клиента | Высокие (N потоков) | Низкие (1 поток) |

### 2.1. Как достигается прозрачность

1. **Единый трейт `MediaEngine`** (в `vks-core`). Две независимые реализации: `vks-media-sfu` (на str0m) и `vks-media-mcu` (compositor). Серверный код работает с трейтом, не с конкретной реализацией.

```rust
#[async_trait]
pub trait MediaEngine: Send + Sync + 'static {
    fn mode(&self) -> RoomMode; // Sfu | Mcu
    async fn create_room(&self, cfg: RoomMediaCfg) -> Result<RoomMediaHandle>;
    async fn destroy_room(&self, room: RoomId) -> Result<()>;
    async fn add_publisher(&self, room: RoomId, p: ParticipantId, tracks: Vec<TrackInfo>) -> Result<PublisherHandle>;
    async fn add_subscriber(&self, room: RoomId, p: ParticipantId, req: SubscribeRequest) -> Result<SubscriberHandle>;
    async fn apply_layout(&self, room: RoomId, layout: Layout) -> Result<()>;
    async fn set_room_capabilities(&self, room: RoomId, caps: RoomCaps) -> Result<()>;
}
```

2. **Единый сигнальный протокол.** Сообщение `ServerHello` (после аутентификации) содержит `room_mode` — единственный источник истины о режиме:

```protobuf
message ServerHello {
    string server_name = 1;
    string server_id   = 2;
    RoomMode room_mode = 3;   // ← SFU | MCU
    RoomCaps caps       = 4;  // кодеки, simulcast, svc, layout вкл/выкл
    repeated IceServer ice  = 5;
    uint32 proto_version    = 6;
}
```

3. **Клиентский SDK выбирает рендерер сам.** Приложение не пишет `if sfu {} else {}` — эта ветка только внутри SDK:

```rust
// ЕДИНЫЙ API, который видит приложение:
let conference = Conference::join(url, token).await?;
let surface = conference.create_render_surface(canvas);
// Внутри SDK:
match server_hello.room_mode {
    RoomMode::Sfu => { /* N треков → sfu_renderer + локальный layout */ }
    RoomMode::Mcu => { /* 1 композитный трек → mcu_renderer, layout уже на сервере */ }
}
```

4. **Layout-движок — общий.** Крейт `vks-layout` используется и сервером (MCU compositor → пиксельные Rect), и клиентом (SFU → CSS-Grid/Taffy-дескриптор). Оператор редактирует **один** Layout — он корректно применяется в любом режиме.

5. **Переключение режима на лету:**
   - `vks-admin` публикует событие в NATS (`vks.cluster.room.mode_changed`);
   - все `vks-signaling`-инстансы рассылают `RoomModeChanged` активным клиентам;
   - клиентский SDK делает graceful renegotiation (отписка от старых треков, подписка на новые/композит, сохранение раскладки);
   - composer/forwarder в `vks-media` переключается внутри.
   - Клиент видит ~1–2 сек перемигивания, без переподключения.

---

## 3. Структура Cargo-workspace

```
vks/
├── Cargo.toml               # workspace root
├── Cargo.lock
├── rust-toolchain.toml      # фиксация версии компилятора (MSRV)
├── .cargo/config.toml       # линковщик, target-cpu, features
├── deny.toml                # cargo-deny (лицензии, CVE)
├── .rustfmt.toml
├── .github/workflows/       # CI: clippy, test, audit, build, e2e
│
├── crates/
│   ├── vks-core/            # 🧠 ЧИСТАЯ ДОМЕННАЯ ЛОГИКА (no I/O)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── room.rs          # Room, RoomId, RoomState, RoomMode(SFU|MCU)
│   │       ├── participant.rs   # Participant, Role, Permissions
│   │       ├── session.rs       # Session lifecycle FSM
│   │       ├── track.rs         # TrackId, TrackKind, MediaKind
│   │       └── ports/           # 🚪 Traits (гексагональные порты)
│   │           ├── media_engine.rs   # ← КЛЮЧЕВОЙ ТРЕЙТ MCU/SFU
│   │           ├── signaling_transport.rs # WS/WSS/WebTransport абстракция
│   │           ├── storage.rs
│   │           ├── auth.rs
│   │           ├── events.rs
│   │           ├── clock.rs
│   │           └── rng.rs
│   │   └── tests/             # unit-тесты логики без I/O
│   │
│   ├── vks-proto/           # 📡 ПРОВОДНОЙ ПРОТОКОЛ (protobuf + prost)
│   │   ├── build.rs           # prost-build для .proto
│   │   ├── proto/
│   │   │   ├── signaling.proto  # SDP/ICE/join/leave/layout/mode_changed
│   │   │   ├── admin.proto      # REST DTO
│   │   │   ├── media.proto      # TrackInfo, SubscribeRequest, RoomCaps
│   │   │   └── cluster.proto    # NATS темы
│   │   ├── openapi.json       # OpenAPI для admin REST (utoipa-совместимый)
│   │   └── src/lib.rs
│   │
│   ├── vks-common/          # 🧰 УТИЛИТЫ
│   │   └── src/
│   │       ├── error.rs       # единый Error enum (thiserror)
│   │       ├── tracing.rs     # настройка tracing-subscriber
│   │       ├── metrics.rs     # prometheus-экспортеры
│   │       ├── config.rs      # figment, env override
│   │       └── ids.rs         # UUIDv7/ULID
│   │
│   ├── vks-auth/            # 🔐 АУТЕНТИФИКАЦИЯ
│   │   └── src/
│   │       ├── jwt.rs         # jsonwebtoken, access/refresh
│   │       ├── api_key.rs     # API-ключи для управления
│   │       ├── ldap.rs        # ldap3 + GSSAPI/Kerberos (FreeIPA)
│   │       ├── local_db.rs    # встроенная БД юзеров
│   │       └── provider.rs    # trait AuthProvider (стратегия)
│   │
│   ├── vks-storage/         # 💾 ХРАНИЛИЩЕ
│   │   ├── src/
│   │   │   ├── migrations/    # sqlx-миграции
│   │   │   ├── postgres.rs
│   │   │   ├── sqlite.rs      # single-node
│   │   │   └── memory.rs      # тесты
│   │   └── schema.sql
│   │
│   ├── vks-layout/          # 🎨 ДВИЖОК РАСКЛАДОК (shared client/server)
│   │   └── src/
│   │       ├── layout.rs      # Layout, Slot, LayoutKind, Rect (normalized)
│   │       ├── presets.rs     # 2x2, 3x3, focus+thumbnails, speaker
│   │       ├── mcu_renderer.rs   # → пиксельные Rect для compositor
│   │       ├── client_renderer.rs# → CSS-Grid/Taffy-дескриптор
│   │       └── storage.rs     # сериализация/сохранение
│   │
│   ├── vks-signaling/       # 📡 СИГНАЛИЗАЦИЯ (WS/WSS + WebTransport)
│   │   └── src/
│   │       ├── ws.rs          # tokio-tungstenite
│   │       ├── wt.rs          # wtransport (QUIC/quinn)
│   │       ├── sdp.rs         # SDP offer/answer, ICE
│   │       └── handler.rs     # join/leave/track/layout/mode_changed
│   │
│   ├── vks-media-sfu/       # ⚡ SFU-ДВИЖОК (str0m)
│   │   ├── src/
│   │   │   ├── engine.rs      # impl MediaEngine (SFU)
│   │   │   ├── forwarder.rs   # N-to-N forwarding
│   │   │   ├── simulcast.rs   # 3 spatial layers, выбор по BWE
│   │   │   ├── bwe.rs         # TWCC/REMB
│   │   │   ├── dtls.rs        # DTLS-SRTP
│   │   │   └── congestion.rs  # congestion ladder (degrade)
│   │   └── README.md
│   │
│   ├── vks-media-mcu/       # 🎛️ MCU-ДВИЖОК (compositor)
│   │   ├── src/
│   │   │   ├── engine.rs      # impl MediaEngine (MCU)
│   │   │   ├── compositor.rs  # oxideav-scene / ffmpeg-next + libyuv
│   │   │   ├── decoder.rs
│   │   │   ├── encoder.rs
│   │   │   ├── audio_mixer.rs # N Opus → 1 mixed
│   │   │   ├── vad.rs         # speaker detection
│   │   │   └── layout_bridge.rs # vks-layout → пиксельные Rect
│   │   └── README.md
│   │
│   ├── vks-gateway/         # 🌐 ШЛЮЗЫ протоколов
│   │   ├── sip/               # rvoip (SIP ↔ WebRTC)
│   │   └── h323/              # FFI-обёртка над H323Plus/OPAL
│   │
│   ├── vks-admin/           # 🖥️ ADMIN REST API (axum + utoipa)
│   │   └── src/
│   │       ├── lib.rs          # axum app, /metrics, /openapi.json
│   │       ├── handlers.rs     # shell/legacy-роуты
│   │       ├── api_v3/         # /api/v3 (auth, conferences, settings, history,
│   │       │                   #          observability, call_strings, video_layouts)
│   │       ├── rooms.rs / users.rs / settings.rs / mode.rs / audit.rs
│   │       ├── auth_middleware.rs / state.rs / hub.rs
│   │
│   ├── vks-recorder/        # 🎞️ ЗАПИСЬ конференций
│   │   └── src/
│   │       ├── recorder.rs    # подписка на потоки, запись в MKV/MP4
│   │       └── storage.rs
│   │
│   ├── vks-cluster/         # 🕸️ КЛАСТЕР (NATS)
│   │   └── src/
│   │       ├── bus.rs         # NATS pub/sub, темы vks.cluster.*
│   │       ├── ownership.rs   # room ownership (consistent hashing)
│   │       └── heartbeat.rs   # discovery инстансов
│   │
│   └── vks-server/          # 🚀 ГЛАВНЫЙ БИНАРНИК (композиционный корень)
│       ├── src/
│       │   ├── main.rs
│       │   ├── config.rs      # загрузка конфигурации
│       │   └── bootstrap.rs   # wiring всех крейтов
│       └── configs/
│           ├── default.toml
│           ├── dev.toml
│           └── production.toml
│
├── client/
│   ├── vks-client-sdk/      # 🧠 SDK для видеоконференций (Rust + WASM)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── conference.rs  # ← ЕДИНЫЙ API: Conference::join()
│   │       ├── render_surface.rs  # абстракция рендер-сурфейса
│   │       ├── signaling_client.rs # WS/WebTransport клиент
│   │       ├── media_engine/
│   │       │   ├── mod.rs
│   │       │   ├── sfu_renderer.rs # микширование N треков через vks-layout
│   │       │   └── mcu_renderer.rs # отображение композитного трека
│   │       └── webrtc_glue.rs # обёртки над web-sys/WebRTC API
│   │
│   ├── vks-admin-ui/        # 🖥️ Админ-панель (Dioxus SPA)
│   │   ├── Dioxus.toml
│   │   ├── tailwind.css
│   │   └── src/
│   │       ├── main.rs
│   │       ├── app.rs         # корневой компонент, роутер
│   │       ├── api/client.rs  # сгенерированный из OpenAPI (progenitor)
│   │       ├── state/
│   │       │   ├── auth.rs
│   │       │   └── server_state.rs
│   │       ├── components/
│   │       │   ├── ui/        # Button, Input, Card, Modal
│   │       │   ├── layout/    # AdminLayout
│   │       │   └── video/     # Grid, Tile, ModeSwitch
│   │       └── pages/
│   │           ├── login.rs
│   │           ├── dashboard.rs
│   │           ├── rooms.rs
│   │           ├── users.rs
│   │           ├── layouts.rs # редактор раскладок
│   │           ├── settings.rs
│   │           └── monitors.rs
│   │
│   └── vks-meeting-ui/      # 🎥 UI участника конференции (Dioxus SPA)
│       ├── Dioxus.toml
│       ├── tailwind.css
│       └── src/
│           ├── main.rs
│           ├── app.rs
│           ├── components/
│           │   ├── video_grid.rs
│           │   ├── controls.rs
│           │   ├── participant_list.rs
│           │   └── chat.rs
│           └── state/meeting.rs
│
├── deployments/
│   ├── docker/
│   │   ├── Dockerfile
│   │   └── docker-compose.yml  # server + postgres + nats + coturn + grafana
│   ├── helm/
│   └── configs/
│
└── docs/
    ├── PROJECT_RULES.md
    ├── rules/
    │   ├── BACKEND_RULES.md
    │   ├── FRONTEND_RULES.md
    │   └── CODE_STYLE.md
    ├── architecture/
    │   ├── ARCHITECTURE.md     # ← этот файл
    │   ├── SIGNALING_PROTOCOL.md
    │   ├── MEDIA_PIPELINE.md
    │   ├── LAYOUT_ENGINE.md
    │   └── CLUSTERING.md
    ├── decisions/
    │   ├── DECISIONS.md
    │   ├── TEMPLATE_DECISION.md
    │   └── NNNN-*.md
    └── progress/
        ├── MASTER_TASK_LIST.md   # живой трекер
        └── archive/              # закрытые PROGRESS/REWORK/планы
        └── archive/
```

### Дерево зависимостей крейтов (строго)

```
vks-core           ← (std + ич, без внешних runtime-библиотек)
vks-proto          ← (std, prost)
vks-common         ← vks-core
vks-layout         ← vks-core
vks-auth           ← vks-core, vks-proto
vks-storage        ← vks-core
vks-media-sfu      ← vks-core
vks-media-mcu      ← vks-core, vks-proto, vks-common, vks-layout
vks-signaling      ← vks-core, vks-proto, vks-common, vks-auth
vks-gateway-sip    ← vks-common
vks-gateway-h323   ← vks-common
vks-recorder       ← vks-core, vks-proto, vks-common
vks-cluster        ← vks-core, vks-proto
vks-admin          ← vks-core, vks-proto, vks-auth, vks-common
vks-server         ← ВСЕ выше (композиционный корень)
```

Зависимости строго по дереву, циклы запрещены, обратные зависимости запрещены.

---

## 4. Архитектурные принципы

### 4.1. Гексагональная архитектура (Ports & Adapters)

Ядро (`vks-core`) определяет **порты** (traits) для взаимодействия с внешним миром:

- `MediaEngine` — media plane (SFU/MCU — две реализации);
- `SignalingTransport` — signaling plane (WS/WSS/WebTransport);
- `Storage` — persistence (memory/sqlite/postgres);
- `AuthProvider` — identity (local/LDAP/OIDC);
- `Events` — internal event bus (NATS adapter);
- `Clock`, `Rng` — для детерминированных тестов.

Это позволяет заменять WebSocket на WebTransport, PostgreSQL на SQLite, str0m на mediasoup, **не меняя логику ядра**.

### 4.2. Sans-I/O для медиа

Mediа-ядро (SFU/MCU) не владеет сокетами. Транспорт — адаптер, который вызывает методы ядра. Ядро детерминировано и тестируется без сети. Это даёт:
- максимальный контроль над сетевым вводом-выводом (tokio, io_uring, или что угодно);
- тестируемость (mock UDP-пакеты в памяти);
- возможность горизонтального масштабирования.

### 4.3. Модульность

Каждый крейт — независимая единица. Новый функционал (новый протокол, новый кодек, новый шлюз) = новый крейт или фича, не меняющий публичные API.

---

## 5. Технологический стек (сводка)

| Слой | Технология / Крейт | Обоснование |
|---|---|---|
| Язык | Rust **1.94.0** (MSRV, ADR-0024) | Производительность, безопасность памяти, асинхронность |
| Асинхронный рантайм | tokio | Стандарт де-факто, огромная экосистема |
| Веб-фреймворк | axum | Современный, минималистичный, хорошо с tokio |
| Медиа-ядро (SFU) | str0m | Sans-I/O WebRTC, полный контроль, VP8/VP9/H.264/AV1/Opus/G.722 |
| MCU-композитор | **oxideav-scene** (ADR-0004, Q1) | Чистый Rust, без FFI, 0.1.4 |
| Сигнализация | tokio-tungstenite (WS), wtransport (WebTransport/QUIC) | Совместимость + низкая задержка |
| Шлюз SIP | rvoip | Универсальный фасад SIP/WebRTC |
| Шлюз H.323 | FFI над **H323Plus** (ADR-0007, Q2) | Специализированная H.323, предсказуемая FFI |
| Layout-движок | **Taffy** (геометрия) + **hadrone-core** (DnD) + свой (пресеты) (ADR-0012, Q4) | Flexbox/Grid + DnD + speaker/focus |
| БД | sqlx / sea-orm + PostgreSQL | sqlx — контроль, sea-orm — скорость |
| Аутентификация | jsonwebtoken + ldap3 | JWT + LDAP/FreeIPA/AD |
| TURN-сервер | **turn-rs** (ADR-0015, Q3) | Чистый Rust, 40M msg/s, gRPC API |
| Мониторинг | tracing + prometheus (metrics) | Логи + метрики |
| Оркестрация | Docker + Docker Compose / Kubernetes (Helm) | Стандарт |
| Шина сообщений | NATS | Легковесная, высокопроизводительная pub/sub |
| Фронтенд (админка) | Dioxus + dioxus-shadcn + Tailwind v4 | Rust, типобезопасность |
| Фронтенд (meeting) | Dioxus + vks-client-sdk | Rust, PWA |
| API-клиент | progenitor (из OpenAPI utoipa) | Типобезопасная генерация |
| E2E-тесты | chromiumoxide | Реальные браузеры |
| Протокол | protobuf (prost) | Компактный, версионированный |
| IDs | UUIDv7 / ULID | Времяупорядоченные, децентрализованные |

---

## 6. Поток данных (high-level)

```
                         ┌──────────────────────────────────────────────────────┐
                         │                  vks-server (бинарник)                │
                         │  bootstrap: загрузка конфига, wiring всех крейтов     │
                         └──────────────────────────────────────────────────────┘
                                        │
        ┌───────────────────────────────┼───────────────────────────────┐
        │                               │                               │
   ┌────▼─────┐                  ┌──────▼──────┐                 ┌──────▼──────┐
   │ signaling │                  │    media    │                 │    admin    │
   │ WS/WSS/WT │                  │ SFU | MCU   │                 │  REST API   │
   └────┬─────┘                  └──────┬──────┘                 └──────┬──────┘
        │                               │                               │
        │   join/leave/SDP/ICE/         │   RTP/RTCP/TURN               │  CRUD rooms,
        │   layout/mode_changed         │                               │  users, layouts,
        │                               │                               │  mode, audit
   ┌────▼───────────────────────────────▼───────────────────────────────▼──────┐
   │                          vks-core (домен, no I/O)                           │
   │  Room / Participant / Session(FSM) / Track / MediaEngine(port)             │
   └────┬──────────────────────────┬──────────────────────────┬─────────────────┘
        │                          │                          │
   ┌────▼─────┐              ┌─────▼─────┐             ┌──────▼──────┐
   │ storage  │              │   auth    │             │   cluster   │
   │ pg/sql/  │              │ JWT/keys/ │             │ NATS, room  │
   │ memory   │              │ LDAP      │             │ ownership   │
   └──────────┘              └───────────┘             └─────────────┘

   Клиенты:
   WebRTC-браузер ──► WS/WSS/WT ──► signaling ──► media (SFU/MCU)
   SIP-терминал   ──► vks-gateway-sip (rvoip) ──► media
   H.323-терминал ──► vks-gateway-h323 (FFI) ──► media
   Admin UI (Dioxus) ──► REST ──► vks-admin ──► core
   Meeting UI (Dioxus) ──► vks-client-sdk ──► signaling + media
```

---

## 7. Ссылки на детальные документы

| Документ | Содержание | Статус |
|---|---|---|
| `docs/architecture/SIGNALING_PROTOCOL.md` | Детальный спецификация сигнального протокола, сообщения, состояния | TODO |
| `docs/architecture/MEDIA_PIPELINE.md` | SFU forwarding, simulcast, MCU compositor pipeline | TODO |
| `docs/architecture/LAYOUT_ENGINE.md` | Формат Layout, рендереры, пресеты | TODO |
| `docs/architecture/CLUSTERING.md` | NATS, room ownership, graceful shutdown | TODO |
| `docs/rules/BACKEND_RULES.md` | Правила бэкенда (детали) | TODO |
| `docs/rules/FRONTEND_RULES.md` | Правила фронтенда (детали) | TODO |
| `docs/rules/CODE_STYLE.md` | Стиль кода (детали) | TODO |

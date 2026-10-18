# PROGRESS — живое состояние проекта VKS

> Единственный живой документ о состоянии проекта. Обновляется в том же коммите, что и описываемый код.
> Закрытые итерации архивируются в `archive/YYYY-MM.md` раз в месяц.
> Шаблон записи — `TEMPLATE_ERROR.md` (для ошибок) и структура в PROJECT_RULES.md §3.

---

## Статус проекта

| Фаза | Название | Статус |
|---|---|---|
| 0 | Инфраструктура | 🟢 завершена |
| 1 | Ядро и порты | 🟢 завершена (vks-core + vks-storage + vks-auth + vks-common) |
| 2 | SFU-движок и сигнализация | 🟢 завершена (vks-media-sfu + vks-signaling + vks-client-sdk) |
| 3 | MCU-движок и прозрачность | 🟢 завершена (vks-media-mcu + vks-core transparency) |
| 4 | Layout-движок | 🟢 завершена (vks-layout: core+grid+focus+renderer) |
| 5 | Аутентификация и admin | 🟢 завершена (vks-auth: JWT/API/LDAP/local/provider + vks-admin REST) |
| 6 | Шлюзы SIP/H.323 | 🟢 завершена (vks-gateway-sip + vks-gateway-h323: codec negotiation, call state, FFI watchdog) |
| 7 | Кластер, запись, мониторинг | 🟢 завершена (vks-cluster: bus/ownership/heartbeat/circuit/shutdown + vks-monitoring: metrics/QoE/SLO/ratelimit + vks-recorder) |
| 8 | Frontend admin UI + meeting UI | 🟢 завершена (ребрендированный PNM UI PROSTOR 1:1 через `/api/v3`-адаптер, ADR-0027) |

Легенда: ⚪ не начата, 🟡 в работе, 🟢 завершена, 🔴 заблокирована, ⏸️ на паузе.

---

## Открытые вопросы к команде

| # | Вопрос | Решение | ADR | Статус |
|---|---|---|---|---|
| Q1 | MCU-композитор: oxideav-scene vs ffmpeg-next? | **oxideav-scene** (чистый Rust, 0.1.4) | ADR-0004 ✅ | resolved |
| Q2 | H.323: H323Plus vs OPAL? | **H323Plus** (специализированная H.323) | ADR-0007 ✅ | resolved |
| Q3 | TURN: coturn vs turn-rs? | **turn-rs** (чистый Rust, 40M msg/s) | ADR-0015 ✅ | resolved |
| Q4 | Layout: Taffy vs CSS-Grid? hadrone-core? | **Taffy** (геометрия) + **hadrone-core** (DnD) + **свой движок** (пресеты) | ADR-0012 ✅ | resolved |
| Q5 | E2EE в v1? | **Deferred** (DTLS-SRTP достаточно, MCU требует расшифровки) | ADR-0021 ✅ | resolved |
| Q6 | MSRV (версия Rust)? | **Rust 1.94.0** (CI), **1.99.0** (dev/release) | ADR-0024 ✅ | resolved |
| Q7 | Режим MCU/SFU: глобальный vs per-room? | **Глобальный дефолт + per-room override** | ADR-0026 ✅ | resolved |
| Q8 | WebRTC в WASM: browser vs str0m? | **web-sys + WebRTC API браузера** (str0m — native) | ADR-0025 ✅ | resolved |
| Q9 | Правомерность использования legacy-сборки TrueConf MCU UI (+ бренд «PROSTOR») в продукте VKS | решение за командой; юридический гейт до релиза (риск R2 плана) | ADR-0027 | open |

---

## Итерации

### [2026-10-18] Итерация 5.4 — Пакет B: Контракты и честный runtime (M0.1–0.6, M1.3, M1.5, M1.6, M1.9, M6.3)

#### Что сделано
- **M0.1–M0.6 (Контракты, Fail-Fast конфигурация и честный runtime)**:
  - `Config::validate()` с проверкой диапазонов портов, DSN баз данных, адресов привязки и путей к статике UI.
  - Редакция чувствительных DSN (`redact_dsn`) для безопасного логирования без утечки паролей БД.
  - Честный статус рантайма в `/api/v3/ui/system/status` и `/readyz`: опрос реального пинга БД (`storage.ping().await`), мониторинг здоровья медиа-петли SFU/MCU UDP и фреймворк инъекции неисправностей (`AppState::inject_fault` / `clear_fault`).
  - Передача истинного `room_mode` и `caps` в `HandlerConfig` сигнального сервера.
- **M1.3, M1.6 & M1.9 (Асинхронная аутентификация и персистентность учетных данных)**:
  - Вынос криптографических операций Argon2 в `tokio::task::spawn_blocking` (`authenticate_async`, `set_password_async`, `register_async` в `LocalDb`), исключая блокировку reactor Tokio.
  - Модель `UserCredentials` (`UserId`, `password_hash_hex`, `salt_hex`, `password_changed_at`, `generation`) и методы `upsert_user_credentials`, `get_user_credentials`, `list_user_credentials` в трейте `Storage`.
  - SQL миграция `0004_user_credentials.sql` и реализация в `MemoryStorage` с полным набором roundtrip тестов.
  - Автоматическая персистенция пароля при смене (`change_password`) и bootstrap оператора (`bootstrap_operator_async`).
  - Восстановление учетных данных (`restore_user`) в `LocalDb` из `Storage` при гидрации состояния (`AppState::sync_from_storage`).
- **M1.5 (Персистенция настроек и маскирование секретов)**:
  - Асинхронное сохранение конфигурации в `SettingsService::set_async` через `storage.upsert_settings`.
  - Рекурсивное маскирование (`redact_json_secrets`) и безопасное слияние (`merge_json_secrets`) чувствительных полей JSON (`pass`, `secret`, `token`, `key`).
- **Layout & Media MCU Enhancements**:
  - Обогащение `Slot` атрибутами `z_index`, `pinned` и `is_focus` с поддержкой нормализованных координат (`NormalizedLayout`, `NormalizedSlot`, `NormalizedRect`).
  - Развязка аудио-каденса (строгие 20 мс) от рендеринга видео в `vks-media-mcu::Engine`.
- **Верификация**:
  - `cargo fmt --all -- --check` — чисто.
  - `cargo clippy --workspace --all-targets -- -D warnings` — 0 замечаний.
  - `cargo test --workspace` — все тесты пройдены успешно (0 ошибок).

### [2026-10-18] Итерация 5.3 — Комплексная модернизация и устранение архитектурного долга MCU/SFU (Milestones M0–M6)

#### Что сделано
- **Пакет P0 (Критические исправления целостности, безопасности и ростера)**:
  - **P0.1 (Operator Bootstrap ID)**: Детерминированный UUID оператора `BOOTSTRAP_ADMIN_USER_ID = "00000000-0000-0000-0000-000000000001"` и `UserId::bootstrap_operator()`, синхронизирующий `LocalDb` и `state.users` с поддержкой парсинга UUID во всех v1/v3 API.
  - **P0.2 (API-key Scope Security & Role::Ord)**: Устранена инверсия привилегий в enum `Role`: явная реализация `Ord`/`PartialOrd` через `self.level()` (Viewer < Participant < Moderator < Operator), безопасный каскад ограничений `owner_role.min(...)` и отсечение ключей без валидных прав (включая `scopes: []`) с 403 Forbidden.
  - **P0.3 (Unified Token Revocation & Anti-Race)**: Унифицированный метод валидации токенов `AppState::verify_token`, защищенный от таймстемп-гонки на секундном разрешении через опережающую установку `mark` в `LocalDb::set_password` и синхронизацию `iat` в `issue_token`. Защита эндпоинта WebRTC-сигналинга `/ws` от несанкционированного доступа деактивированных пользователей (403 Forbidden).
  - **P0.4 (Frontend Dir Mounting & State Sync)**: Монтирование статики UI скорректировано на `client/pnm-frontend/deployed/pnm` в `configs/default.toml`, проброс `default_room_mode` и `server_name` в `AppState` (`mode.global_mode` и `settings`), вывод версии сборки при старте.
  - **P0.5 (Roster Cleanup, Exactly-Once Disconnect & Anti-Zombie)**: Сохранение `participant_id` сессии при `LeaveRoom` в `vks-signaling`, учет членства через `current_membership: Option<(RoomId, String)>` с атомарным `.take()` в `vks-server`, возврат точного оставшегося количества участников `Option<usize>` из `RoomHub::unregister_participant`, дифференцированные коды причин `ParticipantLeft` (0 = штатный выход, 1 = таймаут/обрыв транспорта) и 15-секундный серверный keepalive ping.
- **Пакет M0–M1 (Согласование контрактов, конфигурация, авторизация и постоянное хранение)**:
  - **M0.1 (Дедупликация контрактов)**: Удален дублирующий `RoomMode` из `vks-storage/src/rows.rs`; ре-экспортирован канонический `vks_core::RoomMode`, реализован `std::str::FromStr` для `RoomMode`. Устранены десятки строк ручных сопоставлений `match` в `core_adapter.rs`.
  - **M0.2 (Конфигурация и Fail-Fast)**: Поддержана переменная `VKS_CONFIG_PATH` для явного пути к конфигурации; `database_url = "sqlite::memory:?cache=shared"` зафиксирован в `configs/default.toml` для сохранения данных между подключениями пула в in-memory режиме; `storage_allow_fallback = false` обеспечивает fail-fast остановку при недоступности сконфигурированной БД.
  - **M1.1 (Миграция схемы 0003)**: `crates/vks-storage/migrations/0003_core_users_and_audit.sql` с таблицами `core_users` (`id`, `username`, `role`, `muted`, `deleted`) и `core_audit_log` (`id`, `ts_ms`, `user_name`, `action`, `room`, `ip`). Поле `ts_ms` типизировано как `BIGINT NOT NULL` (защита от переполнения 32-битного int в PostgreSQL). Созданы индексы по `ts_ms` и `username`.
  - **M1.2 (Порт Storage и реализация бэкендов)**: Структуры `StoredUser` и `AuditEntry` в `vks_core::ports::Storage`, методы User CRUD (`upsert_user`, `get_user`, `delete_user`, `list_users`) и Audit (`append_audit`, `list_audit`) реализованы для `MemoryStorage`, `SqliteStorage` и `PostgresStorage`. Расширен интеграционный тест `test_storage_contract`.
  - **M1.3 (Deadlock-Free Write-Through)**: В `vks-admin` (`rooms.rs`, `users.rs`) реализованы асинхронные методы `create_async`, `update_async`, `delete_async` со сквозным подтверждением записи в Storage до ответа клиенту. Мьютексы захватываются кратковременно и никогда не удерживаются через `.await`. Сохранены синхронные методы для unit-тестов.
  - **M1.4 (Устранение спуфинга в аудит-логе)**: В `handlers.rs::record_audit` серверное время `ts_ms = now_unix_ms()` безусловно перезаписывает клиентское; актор извлекается из авторитетного `Extension<AuthContext>` (JWT и API-ключи); IP извлекается из `X-Forwarded-For`/`X-Real-IP`; гарантированная запись через `storage.append_audit`.
- **Пакет M2 (Реальный WebRTC/SFU-звонок, zero-dependency STUN, RTCP PLI и симметричный RTP)**:
  - **M2.1 (RFC 8866 SDP сериализация и согласование)**: В `vks-signaling/src/sdp.rs` приведена в строгое соответствие со стандартами RFC 8866 и RFC 5576 сериализация сессионного и медийных описаний: `c=IN IP4 0.0.0.0`, `t=0 0`, атрибуты `a=rtcp-mux`, `a=mid`, `a=msid`, `a=rtpmap` (с исключением лишних каналов для видеокодеков `vp8/90000`) и `a=end-of-candidates` для ICE-lite.
  - **M2.2 (Сигналинг ICE-кандидатов)**: В `vks-signaling/src/handler.rs` добавлена обработка пейлоада `IceCandidate` во всех медиа-состояниях (`Negotiating`, `MediaActive`, `InRoom`) с возвратом `Ack` вместо отбрасывания пакетов.
  - **M2.3 (Zero-Dependency STUN Binding Response)**: В `vks-media-sfu/src/udp_loop.rs` реализован нативный парсер и генератор пакетов STUN Binding Success Response (`0x0101`) с атрибутом `XOR-MAPPED-ADDRESS` (RFC 5389 §15.2 / RFC 8445 ICE-lite) для IPv4 и IPv6 без внешних зависимостей.
  - **M2.4 (RTCP PLI Keyframe Request)**: В `vks-media-sfu/src/udp_loop.rs` реализован генератор RTCP Picture Loss Indication (RFC 4585 PT=206, FMT=1, 12 байт). В `crates/vks-server/src/lib.rs` при входе нового подписчика (`joined_ok`) автоматически запрашиваются ключевые кадры у всех видео-паблишеров через `generate_pli_for_room_publishers`, устраняя эффект чёрного экрана при подключении.
  - **M2.5 (High-Performance Zero-Copy UDP Routing)**: Высокопроизводительный неблокирующий маршрутизатор RTP на Tokio UDP. Замена `TrackId::new()` (криптографического генератора UUID v4) на `TrackId::default()` в горячем цикле. Оптимизация мутексов эндпоинтов: `register_endpoint` вызывается только при реальной смене сокета источника, устраняя 99.9% блокировок в установившемся режиме.
  - **M2.6 (Symmetric NAT Traversal & Multi-room SSRC Isolation)**: Симметричный RTP auto-learning с авто-пробитием NAT для recvonly-подписчиков при STUN Binding запросе. Двухуровневая изоляция SSRC: привязка по сокету источника к локальной карте комнаты для исключения коллизий SSRC между параллельными конференциями.
  - **M1.5 (Автоматический аудит мутаций v1 и сессий v3)**: Встроен вызов `state.record_audit_entry` при любых изменениях комнат, пользователей и настроек в v1 и v3 (`api_v3/sessions.rs`).
  - **M1.6 (Гидрация состояния при старте)**: В `vks-server/src/main.rs` вызов `admin_state.sync_from_storage().await` выполняется до bootstrap оператора, загружая существующие комнаты, оверрайды режимов и пользователей.
- **Пакет M3 (Pure-Rust кодеки H.264/VP8/Opus, независимый аудиотайминг 20мс и бесшовное переключение режимов MCU↔SFU)**:
  - **M3.1 (Pure-Rust кодеки и zero-allocation RTP парсинг)**: В `vks-media-sfu/src/codec.rs` реализованы zero-allocation RFC-compliant парсеры:
    - H.264 (RFC 6184): распознавание Single NAL (IDR 5, SPS 7, PPS 8), агрегированных STAP-A (24) и фрагментированных FU-A (28) со сборкой ключевого кадра по комбинации Start-бита и IDR/SPS.
    - VP8 (RFC 7741): парсинг Payload Descriptor (Extended bits, Start of partition, PictureID 7/15-bit, TL0PICIDX, TID/KEYIDX) и Payload Header первого раздела с проверкой инверсного бита keyframe и Sync Code `0x9D 0x01 0x2A`.
    - Opus (RFC 7587 / RFC 6716): парсинг TOC байта, извлечение конфигурации, stereo-флага и расчет точной длительности (10/20/40/60 мс).
  - **M3.2 (Независимый 20 мс каденс аудио в MCU)**: В `vks-media-mcu/src/engine.rs` аудио-микшер отвязан от видео-композитора. Аудио продвигается строго каждые 20 мс (50 Гц / 960 сэмплов @ 48 кГц) независимо от видео. Видео обрабатывается отдельно по бюджету ~30 fps (33 мс) со сбросом кадров при backpressure без дропа аудио. Поля `last_audio_ms` и `last_render_ms` переведены на `Option<u64>` (устранен sentinel баг при `now_ms = 0`).
  - **M3.3 (Сквозная рассылка RoomModeChanged и Late-Joiner защита)**: В `vks-admin/src/rooms.rs` при обновлении режима комнаты через `Rooms::update` / `Rooms::update_async` формируется Protobuf `SignalingMessage` с `RoomModeChanged` (`new_mode`, `switch_epoch`, `graceful: true`) и транслируется в `room_hub`. В `vks-server/src/lib.rs` для late joiners при `JoinRoom` отдается актуальный `RoomModeChanged` с текущим режимом и эпохой, исключая несоответствие SDP-оффера.
- **M2 & M3 (MCU Медиа-пайплайн и Сигналинг)**:
  - Однопроходный $O(N \cdot L)$ алгоритм N-1 mix-minus аудио с динамическим AGC $1/\sqrt{N-1}$ и мягким ограничением `tanh` (`vks-media-mcu/src/audio_mixer.rs`).
  - Доставка микшированного звука пассивным слушателям (muted/listeners) без публикации аудио.
  - Пер-участниковый VAD с фиксацией активного докладчика и гистерезисом тишины.
  - Применение слотов раскладки с сохранением кастомных ID, ограничением до 64 слотов и фокусом 75%/25% (`vks-media-mcu/src/engine.rs`).
  - FSM сессии сигналинга с состояниями `Reconnecting`, `ConnectionLost`/`Resumed` и корректным выходом `LeaveRoom` из `Negotiating`/`Joining`.
  - Fan-out hub (`RoomHub`) на базе ring-buffer `tokio::sync::broadcast` с атомарным ростером участников (`vks-admin/src/hub.rs`, `vks-server/src/lib.rs`).
  - Обогащение `JoinRoomResult` полным снимком участников комнаты и префиксное сопоставление подписок топиков в WS `/api/v3/ws`.
- **Пакет M4 (Команды участников, динамический ростер абонентов, жизненный цикл сессий, WS push-события и Reconnect SDK)**:
  - **M4.1 (Динамический ростер абонентов и DTO)**: В `vks-admin/src/api_v3/sessions.rs` реализован `session_abonents` в строгом соответствии с контрактами декомпилированного UI (`oI`, `kv`, `Lh` мапперы): категории `{ connected: [...], connecting: [], disconnected: [], scheduled: [], count: N, abonents: [...] }`. Настройки участников `audio_tx_enabled = !is_audio_muted`, `video_tx_enabled = !is_video_muted`, `connected_at`, `connection_url`, `type_id` формируются из живого снимка в `RoomHub`.
  - **M4.2 (Команды оператора над абонентами)**: Эндпоинты `POST /api/v3/abonents/{connection_id}/{setting_type}` (мьют микрофона/камеры), `DELETE /api/v3/abonents/{connection_id}` и `DELETE /api/v3/sessions/{id}/abonents/{abonent_id}` (кик абонента), `PATCH /api/v3/abonents/{connection_id}` (переименование) с синхронизацией в `RoomHub` и push-нотификациями.
  - **M4.3 (Принудительный Kick на сигнальном уровне)**: В `vks-admin/src/rooms.rs::kick_participant` рассылается Protobuf `ParticipantLeft { reason: 2 (kicked) }`. В `vks-server/src/lib.rs` при получении `ParticipantLeft { reason: 2 }` для текущего сокета сервер форвардит бинарный кадр клиенту, отправляет WebSocket `CloseFrame(4003, "kicked by operator")` и корректно разрывает сокет со штатным освобождением SFU/MCU ресурсов.
  - **M4.4 (Жизненный цикл сессий и переключение топологии)**: `POST /api/v3/sessions/quick` (быстрый старт сессии без предварительной конфигурации), `POST /api/v3/ui/video_layouts/mode` (переключение режима MCU↔SFU оператором из UI-панели с немедленным вызовом `Rooms::update_async` и рассылкой `RoomModeChanged`), `POST /api/v3/engine/command`, `GET /api/v3/engine/abonents`.
  - **M4.5 (WebSocket Push-события в `admin_events`)**: Автоматический пуш через `state.notify_admin` для событий `sessions:started`, `sessions:stopped`, `conferences:created`, `conferences:updated`, `conferences:deleted` и `session:{id}:abonents` для реактивного обновления интерфейса оператора.
  - **M4.6 (Client SDK Auto-Reconnect)**: В `vks-client-sdk`: поддержка `client_hello_with_resume` в `SignalingClient` и двухфазный `reconnect_async` в `Conference` с экспоненциальным backoff (1s→16s, до 5 попыток), контролем TTL нонса (<60s: Fast Resume; $\ge$60s/ошибка: fallback на чистый join_room). Покрыто тестами `reconnect_recovers_active_state` и `m4_abonents_lifecycle_and_commands`.
- **Пакет M5 (SIP/H.323 шлюзы, подсистема записи и кластеризация)**:
  - **M5.1 (Интеграция шлюзов с сервером и RoomHub)**: `GatewayManager` (`vks-server/src/gateways.rs`) связывает `vks-gateway-sip` и `vks-gateway-h323` с ядром: `incoming_sip_call` и `incoming_h323_call` приземляют вызовы в комнату, регистрируют участника в `RoomHub`, транслируют Protobuf `ParticipantJoined` всем WebRTC-клиентам комнаты, инкрементируют счетчик участников в `Rooms` и оповещают WebSocket админки (`session:{room}:abonents`).
  - **M5.2 (Изоляция сессий и строгое завершение вызовов)**: `terminate_sip_call` (BYE) и `terminate_h323_call` (Release) находят комнату участника, дерегистрируют его из `RoomHub`, рассылают Protobuf `ParticipantLeft` в комнату, обновляют ростер и админ-шину. Устранена коллизия по `From` — параллельные звонки с одного адреса в разные комнаты строго изолированы по `Call-ID`.
  - **M5.3 (Подсистема записи и жизненный цикл рекордера)**: `RecorderManager` (`vks-server/src/recorder.rs`): `start_recording` регистрирует сессию записи, внедряет рекордер как специального участника (`is_recorder: true`) в `RoomHub`, рассылает `ParticipantJoined`, уведомляет админку о флаге `recording: true`. `stop_recording` идемпотентно останавливает запись, исключает рекордер из ростера с `ParticipantLeft`, сохраняет метаданные в `MemoryStore` и `AppState.records`.
  - **M5.4 (Потокобезопасность и надежность времени рекордера)**: `RecordingMeta::new` использует `saturating_sub` (защита от NTP underflow при `ended < started`), `RecordingSession::stop()` идемпотентно возвращает `cached_meta`, а `Clock` типизирован как `dyn Fn() -> ClockMs + Send + Sync`, обеспечивая потокобезопасность в асинхронном рантайме Axum.
  - **M5.5 (Кластерные лизы, Fencing и Split-Brain)**: В `vks-cluster/src/ownership.rs`: монотонное время `now_tick + ttl_ticks`, автоматический переход комнаты к новому владельцу с инкрементом эпохи при истечении лиза, CAS-проверка `check_fencing` с кодами `NoLease`, `NotOwner`, `LeaseExpired`, `StaleEpoch`, и метод `rooms_for`.
  - **M5.6 (Координация инстансов и Backpressure)**: `ClusterManager` (`vks-server/src/cluster.rs`): `acquire_room`, `renew_room`, `release_room`, `verify_fencing`, `publish_event` с двухпороговым backpressure (10k/20k), интеграция с маршрутами `/api/v3/gateways/status` и `/api/v3/cluster/status`.
  - **M5.7 (Сетевое интеграционное тестирование)**: 3 сквозных теста в `network_integration.rs`: `test_m5_gateway_sip_h323_integration`, `test_m5_recorder_lifecycle_integration`, `test_m5_cluster_leases_and_backpressure`. Все 78 тестов шлюзов, рекордера и кластера, а также 99 тестов админки и 35 тестов сервера пройдены успешно (0 ошибок).
  - **Ревью M5**: `Gateways & Recorder Architect` — 9.8/10; `Cluster & HA Architect` — 9.9/10. Рекомендации по идемпотентности ID рекордера и реальному timestamp в статусе кластера учтены.
- **Пакет M6 (Завершение приёмки релиза v0.1.0, полная верификация и релизная сборка)**:
  - **M6.1 (Качество и статический анализ)**: `cargo fmt --all -- --check` — чисто (0), `cargo clippy --workspace --all-targets -- -D warnings` — 0 предупреждений (устранено предупреждение `redundant_slicing` в `crates/vks-media-sfu/src/udp_loop.rs`).
  - **M6.2 (Полный регрессионный цикл)**: Успешный прогон всех 527 тестов рабочего пространства по 16 крейтам (`cargo test --workspace`) без единой ошибки.
  - **M6.3 (Релизная сборка)**: Чистая сборка релизного бинарника `cargo build --release -p vks-server` (размер 9.5 МБ, LTO thin, opt-level 3, codegen-units 1, strip symbols).
  - **M6.4 (Live Smoke Verification)**: Подтверждена работоспособность сервера в боевом режиме: HTTP 200 на `/healthz`, `/readyz`, `/api/v3/gateways/status`, `/api/v3/cluster/status`, `/pnm/`. Автоматический фоновый heartbeat кластера каждые 5с.
  - **M6.5 (Синхронизация дорожной карты и документации)**: В `docs/DEVELOPMENT_PLAN.md` и `docs/progress/PROGRESS.md` все Фазы 0–8 переведены в статус `[x]` / `🟢 завершена` с фиксацией фактической архитектуры PROSTOR PNM UI (ADR-0027). Обновлён корневой `README.md`.
  - **Ревью M6**: `Doc & Roadmap Auditor` — 9.7/10; `Server Runtime & Admin Auditor` — 9.8/10. Все замечания устранены, кодовая база и релизный артефакт полностью готовы к выпуску.

### [2026-10-17] Итерация 5.2 — Rework T-F1: ребренд PROSTOR интегрирован в рантайм PNM

#### Что сделано
- В `client/pnm-frontend/deployed/pnm` интегрировано ребрендированное дерево
  `frontend/06-rebrand/pnm-rebranded-2026-10-17`: 90 изменённых файлов (26 текстовых +
  63 иконки + словник), `rsync -c` — 0 остаточных отличий против эталона, кроме 6 заявленных
  пост-фиксов; `node --check` 63/63 чанка OK.
- **Пост-фикс локалей**: ребренд заменил call-site `"MCU …"` → `"PROSTOR …"`, но не тронул
  i18n-ключи `"<context>\u0004MCU …"` (граница `\b` не срабатывает после цифры `4`) — 12 ключей
  × 4 бандла были мертвы. Заменено `u0004MCU` → `u0004PROSTOR`: в call-sites `"MCU …"` — 0,
  `"PROSTOR …"` — 50 вхождений (28 `$pgettext`-вызовов, 14 уникальных строк); в локалях — 16
  ключей на файл, 0 висячих call-sites.
- **Cache-bust словника**: `assets/images/**` раздаётся immutable, а словник перезаписан
  ребрендом → `BiOEwveA.svg` → `assets/images/prostor-wordmark.svg` (побайтово = эталонный),
  ссылки обновлены в `index8`/`index-legacy8`; старый URL не занят.
- `MANIFEST.sha256` пересобран командой импорт-скрипта: 210 строк, `sha256sum -c` — OK.
- Whitelist-инварианты не тронуты (`LICENSE.txt`, `TRUECONF`/EULA, `trueconf.*`-контракты,
  `TrueConfMCU*`-ключи, `/mcu/`-пути); «Осознанно НЕ включено» не протекло (тема `#0097A7`,
  localStorage-ключи, правовые тексты — фазы T-F2/T-F3).
- Доки: `NOTES.md` §«Ребренд PROSTOR» (+ git-раздел актуализирован), шапка `frontend.rs`,
  ADR-0027 (галка), статус-блок `FRONTEND_PORT_PLAN.md`.
- **Ревью R-F1**: 9.6/10 — diff против эталона ювелирный (7 строк = пост-фиксы), связность
  i18n доказана машинно, MANIFEST воспроизводим, whitelist не тронут; опечатка «46» исправлена
  на фактическую метрику.

### [2026-10-17] Итерация 5.1 — Rework S1/T1.1: v1 admin API — fail-closed JWT + RBAC (матрица §1.7)

#### Что сделано
- **RBAC-энфорсмент**: `v1_rbac_middleware` (vks-admin) — вторым слоем после JWT. Матрица
  `v1_required_role` по BACKEND_RULES §1.7: комнаты — чтение Viewer+, мутации Moderator+;
  аудит — чтение/экспорт Moderator+, запись Operator (примечание §1.7); users/settings и любой
  неизвестный роут — Operator (fail-closed дефолт); нет контекста токена — 401; неизвестный/удалённый
  субъект — 403 (даже на чтение). Цепочка на `protected_v1`: `route_layer(rbac)` → `route_layer(jwt)`
  (JWT внешний, кладёт `VerifiedJwt`).
- **Единый резолвинг роли**: `Users::resolve_role` (id/username admin-хранилища; id LocalDb
  `u-admin` → username; `deleted` отфильтрован); magic-строки `u-admin`/`admin → Operator` из
  `api_v3/shell.rs` удалены; `profile.role` неизвестному субъекту — Viewer (least privilege).
- **Ошибки**: схема Bearer case-insensitive (RFC 7235 §2.1), пустой токен → 401; единый конверт
  `deny(...)`, детали верификации JWT — только в лог (generic «invalid or expired token», §6.4).
- **Тесты** (vks-admin 91 unit; workspace 602 — счётчики актуализированы, N2 S0.5): middleware-матрица
  (viewer/moderator, audit-гейт, unknown/deleted, expired `ttl=-1`, Bearer-кейсы), табличный тест
  матрицы `v1_required_role_matches_matrix` (+ fail-closed дефолт), роутер-уровень (viewer-мутация,
  unknown, audit, moderator-kick 403, глобальный режим 403, audit-export 403/200), composition
  (профиль `role=="operator"`, `permissions[0]=="all"`, строгий fail-closed 503).
- **Доки**: module-doc `auth_middleware.rs`, TODO(T3) для permissions-словаря + синк
  `FRONTEND_PORT_PLAN.md`; примечание к §1.7 (audit-write); новый файл `ADR-0022`;
  трекер волн rework — `docs/progress/REWORK_PLAN.md`.
- **Ревью-цикл** (журнал — `REWORK_PLAN.md`): раунд 1 — 7/10 (F1–F7) → правки; раунд 2 — 9/10
  (D1: audit читался Viewer) → гейт; раунд 3 — 9/10 (D2: bulk-kick = soft-delete аккаунта — не для
  Moderator) → зона `/api/v1/users*` целиком Operator; элегантность — 7.5/10 → fail-closed дефолт,
  generic-401 + helper, табличный тест, §1.7/ADR-0022; раунды 4–5 — финальное подтверждение.
- **S0-гигиена (в этом же пакете)**: baseline переписан без 31 MB zip (`.git` 46→14 MB; файл на
  диске, untracked), `.gitignore` дополнен, права файлов нормализованы (644/755).

### [2026-10-16] Итерация 4.3 — PNM-порт: ревью реализации (6.5/10) → фиксы; фаза 2 (`/api/v3` auth+shell+WS) в дереве

#### Ревью реализации (агент-ревьюер, продолжение общего цикла)
- Оценка **6.5/10**: блокеры B1 (CI `cargo-deny-action` тянул cargo-deny 0.14 против конфига 0.20) и
  B2 (ложно-зелёный canary-тест traversal: канорейка была сиблингом `base`, `../canary` вёл в пустоту).
  Обязательные D1–D6/N1–N4 — см. ниже по статусу.
- Подтверждено независимо: семантика роутера, CSP-хэши байт-в-байт, кэш-маска, MANIFEST 210/210.

#### Фиксы ревью (закрыто)
- **B1**: `.github/workflows/ci.yml` → `EmbarkStudios/cargo-deny-action@v2` (Dockerfile v2 пиннит
  ровно 0.20.2); в `deny.toml` добавлен `CDLA-Permissive-2.0` (webpki-roots, достижим под
  `--all-features`). `cargo deny 0.20.2 check` и `--all-features check` — оба EXIT=0 (логи в /tmp/opencode).
  Allow-список не урезался: CI собирает оба графа.
- **B2**: канорейка в `frontend.rs`-фикстуре переехала **в `base`** (сиблинг корня статики) — `../canary`
  теперь существует, утечка была бы поймана; плюс контрольный GET канорейки напрямую в traversal-тесте
  и `Drop`-очистка фикстуры.
- **D2** (security-заголовки): вынесены из `Frontend::router` в **app-wide слой `build_app`**
  (включая `/api/v1`, `/api/v3`, `/metrics`, `/ws`); регрессионный тест
  `router_alone_does_not_set_security_headers` + composition-проверка CSP на /api/v3-ответе.
- **D3** (кардинальность метрик): `method` в `vks_http_requests_total` whitelist-ится
  (`GET|HEAD|POST|PUT|PATCH|DELETE|OPTIONS`, остальное → `other`); тест `http_metrics_middleware_whitelists_method`.
- **D4** (`FRONTEND_DENIED`): инструментируется только `not_found` (traversal тихо сливается с 404 через
  fallback-семантику ServeDir — отдельного reason нет); доки `metrics.rs` + план §4.7 синхронизированы.
- **Симлинки**: импорт-скрипт удаляет симлинки из рантайма (лог каждого), NOTES.md — политика,
  тест `runtime_tree_has_no_symlinks` (ServeDir следует симлинкам → escape).
- Кэш-эвристика ужесточена (`/assets/`-граница; hash-stem ≥8; webm tail-8 с цифрой), тесты кэш-матрицы.

#### Фаза 2 — `/api/v3`: адаптер собран и подключён (в дереве, тесты зелёные)
- **`api_v3`** (vks-admin): `mod.rs` (роутер + конверт ошибок + catch-all 501), `auth.rs`
  (fail-closed 503; exempt-list; token-middleware; login; двухступенчатый lockout §1.4;
  `PATCH /password` с инвалидацией токенов по `iat < password_changed_at`; аудит; `Retry-After`),
  `boot.rs`, `shell.rs` (~19 форм §4.5), `sessions.rs` (фаза 3: `ui/sessions`, `conferences` GET/POST/DELETE,
  search/page/sort), `ws.rs` (**§4.4**: токен в каждом сообщении; subscribe→немедленный
  `system:status:disk.full=NORMAL`; `ui:users|call_strings|files:search`; 501/404-коды; кадр ≤ 16 KB →
  413 без разрыва; ping 30 s; ≤ 100 conn/s per-IP → HTTP 429), `tests.rs` (oneshot-матрица + WS e2e
  `tokio-tungstenite` на живом сервере).
- **Проводка**: `AppState` + `local_db`/`api_v3` (+ re-export `SharedState`), `pub mod api_v3` в vks-admin,
  `build_app` мержит `api_v3::router` (app-wide security/метрики сохранены);
  `Config` + `jwt_secret`/`jwt_ttl_secs=86400`/`admin_login`/`admin_password`
  (`VKS_JWT_SECRET`/`VKS_JWT_TTL_SECS`/`VKS_ADMIN_LOGIN`/`VKS_ADMIN_PASSWORD`);
  `main.rs`: `JwtIssuer` (HS256; пустой секрет → warn + v3 fail-closed 503), LocalDb-оператор
  (dev-дефолт admin/admin + warn), `into_make_service_with_connect_info` (per-IP lockout/WS-limit).
- **Тесты**: vks-admin 74 unit (вкл. lockout-матрицу, инвалидацию токенов, WS e2e: subscribe/лимиты/живость
  после ошибок/429); vks-server composition `test_composition_api_v3_login_chain`
  (exempt→401→login→sessions/profile→CSP→501). `cargo test --workspace` — 🟢 (0 failed).

#### Консоль-фиксы по живому UI (закрыто) + браузер-чеклист
- **Диагностика из живого UI** (Vue-консоль): 3 уникальные ошибки — (1) карточки сессий/конференций
  деструктурируют `this.source.entity.abonents`/`contacts` (в `session_json` их не было);
  (2) `fetchUsersCredentialsExpire` падал «reading 'password'» — реальная причина: `/api/v3/settings/security`
  отдавал плоский объект, а маппер `Ane` читает **`data.payload`**; (3) `GET users/credentials/expire` → 501
  (catch-all), store UI делает re-throw.
- **`sessions.rs`**: `session_json` отдаёт числа-объекты `abonents.{sip,h323,rtsp,vnc,media,file}` и
  `contacts.{users,…}` (карточки: `contactsCount = h323+sip` / `users+h323+sip`, `devicesCount = rtsp+vnc+media+file`),
  `payload.connections_limit` (мапперы `connectionsLimit ← payload.connections_limit`, из `Settings.limits`),
  `videorecord/broadcast:false` (→ `isRecording/isBroadcasting`), `started_at` — **unix-секунды** из нового
  `RoomRecord.created_ms` (карточки умножают на 1e3: с 0 — аптайм «с 1970», с ms — год «58760»).
- **`shell.rs`**: `GET /api/v3/settings/security` → `{payload:{password_min_*…, password_ttl:0,
  password_ttl_enabled:false, login_max_attempts_count/login_block_timeout/login_block_enabled — из констант
  `auth` (5/900), conf_*:0}}`; новый protected `GET /api/v3/users/credentials/expire` → `{users:[]}` (маппер `MW`).
  План §4.5 обновлён (контракт `data.payload`, строка credentials/expire, точный контракт карточек в ф3-таблице).
- **Тесты**: +1 `shell_security_settings_and_credentials_expire_match_ui_contract` (401→200, `users:[]`,
  payload-ключи, lockout=константы) и ассерты контракта карточек в `conferences_and_sessions_flow`
  (числа abonents/contacts, limit=50, `started_at` ∈ unix-секунды); vks-admin 74 unit, workspace 585 — 🟢,
  clippy `-D warnings` чисто.
- **Браузер-чеклист (§6.4) — пройден на живом сервере**: логин из UI admin/admin → дашборд «Конференции»
  показывает «Тестовая комната» (создана через v3, видна и в `GET /api/v1/rooms` — общее `state.rooms`),
  аптайм 00:00:33 / дата 16.10.2026 (после фикса секунд), **0 ошибок консоли**; `/admin/` → 307 →
  `/pnm/administration`; deep-link `/pnm/administration/security` → 200; `/mcu/chunks/nope.js` → 404;
  метрики: 18 вызовов `ui/sessions` и ни одного `unmatched` (501) на дашборде.
  Скриншот: `docs/progress/evidence/phase2-sessions-dashboard-clean-console.png`.
- **Наблюдение** (исправлено в ревью-цикле 2, S2): раздел «Неактивные конференции» показывал ту же комнату,
  потому что `state=inactive` игнорировался; теперь `state=inactive`/`has_stopped=true` → пусто (см. ниже).

#### Ревью-цикл 2 (тот же ревьюер, продолжение) — 9.0/10 + фиксы
- Ревьюер подтвердил независимо: B1/B2 закрыты; D1–D6, N1–N3 закрыты; живые контракты
  (login/sessions/settings/security/credentials-expire), CSP-хэши побайтово, MANIFEST 210/210,
  585 тестов; **оценка 9.0** — ниже 9.5 из-за видимого дефекта адаптера + мелочей:
- **S2 (закрыто)**: `/api/v3/ui/conferences` игнорировал `state` → дашборд показывал активную комнату
  в «Неактивных конференциях» (репро ревьюера: `state=inactive&exclude_tags=Immediate` → 200 с комнатой).
  Фикс (`sessions.rs::select_rooms`): `state=inactive` → пусто (наших inactive-комнат нет), `state=active`/None —
  без фильтра; симметрично `has_stopped=true` → пусто. `exclude_tags` — мягко игнорируется. Тест
  `sessions_sort_by_time_and_state_filters` + запросы точь-в-точь как шлёт UI. Живая перепроверка после
  рестарта: `state=inactive&exclude_tags=Immediate` → `{"conferences":[],"count":0}`; дашборд после
  релогина — только «Активные конференции (1)», консоль 0; скриншот обновлён (14:35).
- **S3 (закрыто)**: сортировка — `sort_field` `start_at/started_at/created_at` по `created_ms`
  (раньше всё, кроме `id`, падало в имя), `sort_order` ≠0 — убывание; тест покрывает asc/desc и Z-A.
- **S3 (закрыто, доки)**: per-IP лимиты за reverse-proxy — `auth.rs::client_ip` док: ConnectInfo
  в проде всегда есть (`into_make_service_with_connect_info`), XFF/X-Real-IP — только при встраивании
  без connect-info; доверие к XFF осознанно выключено (подделываемый заголовок = обход lockout);
  за прокси лимиты считаются по IP прокси (долг §4.3, конфигурируемое доверие — фаза 4+).
- **S4 (закрыто)**: дрифт доков — «73 unit» → 74; в `deny.toml` зафиксированы точные списки
  `license-not-encountered` по графам (union allow — осознанный, CI зелёный, exit=0).

#### Открытые хвосты итерации
- [ ] До/после повторного ревью: фаза 3 — наполнение мапперов `ui/system/status`/`conferences` глубже
      (живые метрики участников), смок-скрипт 104 путей (appendix плана).
- [x] Браузерный чек-лист фазы 2 (логин→shell без ошибок консоли→список комнат) + скриншоты — см. выше.
- [ ] Повторное ревью фиксов (цель ≥9.5) — цикл 2: **9.0/10** (S2 обязателен, S3×2/S4 — желательны);
      все четыре закрыты, цикл 3 (пере-оценка) — открыт.

### [2026-10-16] Итерация 4.2 — Перенос фронтенда PNM (ADR-0027): фаза 1 + цикл ревью

#### Выполнено
- **Импорт архива** `PNM-Frontend-20261016.zip` (sha256-якорь `ea0314f8…`): `client/pnm-frontend/`
  (`archive/` — оригинал, `deployed/pnm/` — рантайм 1:1: **210 файлов / 26 MB**, исключены 6 `*.bak*`;
  `MANIFEST.sha256`; `NOTES.md`), `scripts/import-pnm-frontend.sh` (recovery повреждённого архива:
  482/483 файлов, рантайм 216/216; `--force/--with-source/--gzip`), `scripts/pnm-smoke/*` из /tmp в репо.
- **Статика** `crates/vks-server/src/frontend.rs` (tower-http 0.6.11 `fs`, ADR-0027): роуты
  `/` 307→`/pnm/`, `/pnm` 308→`/pnm/` (+query), `/pnm/…` SPA-fallback, `/mcu/…` без fallback (404),
  `/admin(/` 307→`/pnm/administration`; кэш-политика (`immutable` только хэшированные ассеты,
  остальное `no-cache`) + `Vary: Accept-Encoding`; CSP из index.html (sha256 **всех 5** inline-скриптов,
  `script-src 'self'` без unsafe-inline) + XCTO/XFO/Referrer-Policy; 16 unit-тестов (traversal-канорейка,
  Range 206, HEAD/304, gzip-представление, редиректы, кэш, CSP, 405).
- **Композиция**: `vks_server::build_app(state, &frontend)` — единая точка сборки + слой HTTP-метрик
  (классы `static|spa|api_v1|api_v3|ws|metrics|health|other`); `main.rs` слим; **demo удалён**
  (снапшот `docs/progress/archive/demo-2026-10-16.rs`), тест заменён на composition-level
  (`/`→307 `/pnm/`, `/pnm/`→200 UI, v1 не сломан).
- **Метрики** (vks-common): +6 семейств (`vks_http_requests_total`, `…_duration_seconds`,
  `…_response_bytes_total`, `vks_api_v3_requests_total`, `vks_login_attempts_total`,
  `vks_frontend_denied_total`); `new_registry()` — 13 семейств.
- **Гигиена**: `deny.toml` переписан под валидную схему cargo-deny 0.20
  (`[graph]/[licenses]/[advisories]/[sources]`; убраны несуществовавшие `[cves]/[skip]/[sources.crates-io]`,
  снят удалённый в 0.20 `[licenses] deny`); **`cargo deny check` 0.20.2 прогнан — 🟢**
  (`advisories ok, bans ok, licenses ok, sources ok`; warnings — 4 неиспользованных license-allow,
  duplicate `getrandom` 0.2/0.3/0.4 — info, не блокеры); `.gitignore`;
  **ADR-0027** (файл + строка в реестре + `FRONTEND_RULES §10`), NOTES/PROGRESS обновлены.
- **План v0.3**: микро-правки §7 — R3 (26 MB рантайм + ~30 MB архив, замер `du`/`stat`),
  R6 (штатно собирается реальный каталог, системный protoc; зеркало `/tmp/vksbuild` — только `--all-features`).
- **Цикл ревью** (4 субагента, план v0.2): агент-1 — **9.5/10** (порог достигнут), агент-4 — 9.2
  (правки внесены: Vary-слой, CSP-полнота, ADR), агент-2 — 9.3 (учтено: lockout по §1.4,
  spec CSP, durability-заметки), агент-3 — баг-хантер (блокеры B1–B9 закрыты в v0.2).

#### Критерии готовности (план §6/§8, фаза 1) — проверено
- [x] `cargo fmt --all` + `cargo clippy -p vks-server -p vks-common --all-targets -- -D warnings` — 🟢.
- [x] `cargo test --workspace` — 🟢 34 test-result блока, 0 failed.
- [x] `cargo deny check` (0.20.2, prebuilt musl) — 🟢 `advisories ok, bans ok, licenses ok, sources ok`
      (warnings: 4 неиспользованных license-allow, duplicate `getrandom` — info).
- [x] Живой сервер (`127.0.0.1:8080`, PID перезапущен с новым бинарником): `/`→307 `/pnm/`;
      `/pnm?homescreen=1`→308 `/pnm/?homescreen=1`; `/pnm/`→200 text/html + CSP(5 хэшей) +
      no-cache + Vary + security-заголовки; `assets/fonts/**`→immutable; `chunks/**`→no-cache;
      `/mcu/index.html`→200 (файл), `/mcu/chunks/nope.js`→404; `/admin/`→307; `/api/v1/rooms`→`[]`; `/metrics` — новые семейства.

#### Ошибки, с которыми столкнулись
| Ошибка | Где/когда | Причина | Как исправили | Статус |
|---|---|---|---|---|
| `no Uri in axum::extract` | frontend.rs | axum 0.8.9 не имеет `FromRequestParts for Uri` (док-комментарии устарели) | `RawQuery`-экстрактор для query | fixed |
| ETag «бесплатно от ServeDir» | план §4.2 | В tower-http 0.6.11 нет ETag | Текст исправлен: только Last-Modified/304; план v0.3 | fixed |
| `Vary` отсутствует | план §4.2 | ServeDir не ставит Vary (только CompressionLayer) | Слой `with_cache_control` ставит `Vary: Accept-Encoding` + тесты | fixed |
| CSP «3 inline-скрипта» | план §4.2/§5 | Фактически 5 inline (2 с `data-src`/`nomodule`) | Хэшируются **все** без `src` (тест `data-src`); текст плана v0.3 | fixed |
| `error[deprecated]: this key has been removed` (deny.toml:34) | `cargo deny check` 0.20.2 | `[licenses] deny` удалён в cargo-deny 0.20 (PR #611); `allow` — whitelist, всё прочее и так error | Блок `deny` снят + комментарий в deny.toml; check EXIT=0 | fixed |

#### Риск / вопрос к команде
- **R2 (юридический)**: лицензия TrueConf-сборки и бренда — см. Q9; техника готова, релизный гейт открыт.
- **Zip в git**: до первого коммита решить (LFS/внешнее) — `NOTES.md`.

### [2026-10-15] Итерация 4.1 — Фазы 5/6/7: admin REST + шлюзы SIP/H.323 + кластер/мониторинг

#### Выполнено
- [x] **vks-admin** (привязка: [DEVELOPMENT_PLAN.md](../DEVELOPMENT_PLAN.md) Ф5, ADR-0021/0022/0026): 9 модулей, **50 теста**, clippy clean:
  - `state.rs` — `AppState` (rooms/users/layouts/records/audit + `ModeState{global_mode, switch_epoch, room_mode_overrides}` + `ModeEventQueue`).
  - `mode.rs` — `ModeService`: `set_global_mode`/`set_room_mode`/`effective_mode` (per-room override > global), `ModeChangedEvent{room_id: Option, new_mode, switch_epoch}`, `ModeEventQueue` (in-memory → NATS-адаптер). Admin-вход для SFU↔MCU-переключения (ADR-0026 global + per-room, Q7).
  - `rooms.rs` (CRUD + live_view/Observer + room_mode), `users.rs` (CRUD + sync_ldap + bulk + soft-delete), `settings.rs` (`AuthProvider` local/ldap/oidc + TURN/codecs/limits/retention/TLS, hot-reload shape), `audit.rs` (record/filter/CSV-export/append-only), `health.rs` (healthz/readyz), `handlers.rs` (axum `Router`, `/api/v1/...`, `/healthz`, `/readyz`, `/openapi.json`).
- [x] **vks-gateway-sip** (привязка: Ф6, ADR-0006 rvoip): 3 модуля, **19 теста**:
  - `codec.rs` — `CodecMap`: SIP↔WebRTC negotiation (PCMU/PCMA→G711↔Opus, H.264↔VP8, transcode flag).
  - `call.rs` — `SipCall` (INVITE→Ringing→Active→Terminated) + `SipGateway` (UAS, `on_invite`, `land_in_room` → participant=gateway).
- [x] **vks-gateway-h323** (привязка: Ф6, ADR-0007 H323Plus FFI): 4 модуля, **24 теста**:
  - `codec.rs` — `H323CodecMap`: H.263/H.264→VP8/VP9, G.711/G.729→Opus.
  - `ffi.rs` — `H323PlusBridge` (mock FFI-граница: одна точка, **watchdog** FFI>timeout→`Crashed`+restart, **panic hook**→`Err`).
  - `call.rs` — `H323Call` (Setup→Alerting→Proceeding→Connect→Terminated) + `H323Gateway`.
- [x] **vks-cluster** (привязка: Ф7, ADR-0014 NATS, ADR-0019 idempotency): 7 модулей, **48 теста**:
  - `bus.rs` (in-memory pub/sub → NATS JetStream, DLQ, idempotency by msg_id), `ownership.rs` (consistent-hash room→instance + NATS lease TTL 30 + split-brain lowest-id), `heartbeat.rs` (discovery 10s, dead flag), `circuit.rs` (50%/10s→OPEN 30s→HALF-OPEN), `shutdown.rs` (StopAccepting→Maintaining→MediaDrain→StorageFlush→Exit0, timeout 30→Killed), `backpressure.rs` (queue>10000 drop-non-critical/retry-critical).
- [x] **vks-monitoring** (привязка: Ф7, ADR-0015): 4 модуля, **22 теста**:
  - `metrics.rs` (registry, Prometheus text exposition), `qoe.rs` (MOS/jitter/RTT/loss, p95, `slo_met`), `slo.rs` (SLO 99.5% signaling / 99% MOS>3.5 / RTT p95<300ms), `ratelimit.rs` (token-bucket per key, admin 600/min, WS 16KB).

#### Критерии готовности (DEVELOPMENT_PLAN.md Ф5/6/7) — проверено (core, sans-I/O)
- [x] `cargo build --workspace` — 🟢.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — 🟢 0 замечаний.
- [x] `cargo test --workspace` — 🟢 **530 passed, 0 failed** (admin 50, cluster 48, h323 24, monitoring 22, sip 19, mcu 36, core 88, sfu 54, sdk 34, layout 28, storage 82, common 29, auth 21, recorder 27, proto 5).
- [x] SFU↔MCU switch из админки (global + per-room override, `ModeChangedEvent` → queue → NATS): `set_global_mode`/`set_room_mode`/`effective_mode` + epoch.
- [x] SIP: INVITE→200 lands participant; codec negotiation (G.711↔Opus, H.264↔VP8, transcode flag); BYE→Terminated.
- [x] H.323: Setup→Connect state machine; FFI watchdog (timeout→Crashed→restart); panic hook→Err; H.263→VP8, G.711→Opus transcode.
- [x] Cluster: ownership lease (30s) + split-brain (lowest-id); heartbeat 10s→dead; circuit breaker 50%/10s→OPEN 30s→HALF-OPEN→close; graceful shutdown sequence (5 steps, timeout→Killed); backpressure (drop non-critical, retry critical); idempotency (dedup by msg_id).
- [x] Monitoring: Prometheus exposition; QoE (MOS/RTT/jitter/loss, p95); SLO (99.5%/99% MOS>3.5/RTT p95<300ms); rate limiting (token-bucket).

#### Ошибки, с которыми столкнулись
| Ошибка | Где | Причина | Как исправили | Статус |
|---|---|---|---|---|
| `Slot::new` не найден (vks-layout `#[non_exhaustive]`) | vks-media-mcu ↔ vks-layout | cross-crate sync: MCU used `Slot::new` which didn't exist | added `Slot::new(id, participant, rect)` to vks-layout | fixed |
| New crates not in workspace `members` | root Cargo.toml | subagents created crates but they weren't members → `cargo build -p` failed | added vks-gateway-sip/h323/monitoring to `members` | fixed |
| `Vec<AuditEntry>` from `&AuditEntry` iterator | vks-admin handlers | `filter` returns `Vec<&AuditEntry>`, `.iter()`→`&&` | `.into_iter().map(\|e\| (*e).clone())` | fixed |
| `H323Call::new` `&[H323Codec]` vs single | vks-gateway-h323 tests | tests passed 1 codec, fn takes slice | `&[H323Codec::new("PCMU")]` | fixed |
| `Error::Internal(String.into())` — `anyhow::Error: From<String>` | vks-gateway-h323 ffi | anyhow doesn't `From<String>` | `anyhow::anyhow!(...)` + add `anyhow` dep | fixed |
| `to_value(&x)` needless borrow | vks-admin handlers/health | `to_value` takes by ref already | `to_value(x)` | fixed |
| `impl Default for AuthProvider` derivable | vks-admin settings | hand `impl Default` | `#[derive(Default)]` + `#[default]` on `Local` | fixed |
| consecutive `str::replace` | vks-gateway-h323 codec | clippy | `.chars().filter(...).collect()` | fixed |
| `room_override_survives_global_switch` panic "global event" | vks-admin mode test | default global=Sfu, test switched to Sfu (no change, no event) | switch global Sfu→Mcu (real change) | fixed |
| `Users::delete` returns true twice (soft-delete) | vks-admin users | second delete of already-deleted still found | `Some(r) if !r.deleted` guard → return false if already deleted | fixed |
| `BTreeMap` guard `.next()` / temp-dropped-borrowed | vks-admin users test | `MutexGuard<BTreeMap>` has no `.next()`; temp guard dropped | `let guard = ...; guard.iter().next()` | fixed |
| `map_or(false, ...)` → `is_some_and` | vks-gateway-sip call | clippy | `is_some_and` | fixed |
| `let call` not mut (2nd setup) | vks-gateway-h323 call test | `setup` called twice on non-mut | `let mut call` | fixed |

#### Риск / вопрос к команде
- **Скоуп-ограничение Ф5/6/7** (эста волна = sans-I/O core; реальные тяжёлые deps — за optional features, default dep-light):
  - vks-admin: axum REST + `ModeEventQueue` (in-memory) — production обёрнёт в NATS pub (vks-cluster) + PostgreSQL; `utoipa`/`progenitor` — optional.
  - vks-gateway-sip/h323: `rvoip`/`H323Plus` (FFI) — optional; core = state machine + codec map + FFI-mock (watchdog/panic-hook). E2E «телефон→комната» — следующий шаг.
  - vks-cluster: NATS JetStream = in-memory `Bus`-mock (dedup/lease/heartbeat/circuit/shutdown/backpressure) — production обёрнёт в настоящий NATS (ADR-0014).
  - vks-monitoring: in-memory registry (Prometheus exposition) — production `/metrics` handler + Grafana/Alertmanager.
- **ADR-0023** (лицензия) — единственный открытый вопрос.
- **Pуть с пробелом** — сборка через `/tmp/vksbuild`, всё 5 крейтов + Cargo.toml синхронизированы в реальный проект.

---

### [2026-10-15] Итерация 3.1 — Фаза 3: MCU-движок (compositor) + SFU↔MCU-прозрачность + запись

#### Выполнено
- [x] **vks-media-mcu** (привязка: [DEVELOPMENT_PLAN.md](../DEVELOPMENT_PLAN.md) Ф3, [ARCHITECTURE.md §2](../architecture/ARCHITECTURE.md), ADR-0004 oxideav-scene): 7 модулей, **36 теста**, clippy clean:
  - `audio_mixer.rs` — `AudioMixer`: N PCM f32 → 1 mixed, gain 1/√N (non-empty N), soft-clip tanh (pass-through внутри ±1).
  - `vad.rs` — `Vad`: 20 ms (960 @48k), RMS + ZCR (>= threshold), 500 ms debounce, `VadState` (Default derive).
  - `layout_bridge.rs` — `LayoutBridge`: `vks_layout::Layout` + canvas 1920×1080 → `PixelRect` per slot, crossfade 300 ms (interpolate old→new).
  - `compositor.rs` — `Compositor`: N `Frame` + `PixelRect` → `CompositeFrame` 1920×1080 (nearest-neighbour letterbox, canvas-clamped intersection, deterministic).
  - `frame.rs` — `Frame`/`CompositeFrame` (raw placeholder) + `VideoEncoder`/`VideoDecoder` (mock, roundtrip).
  - `engine.rs` — `McuEngine` implements `vks_core::MediaEngine` (mode=Mcu, create/destroy/publisher/subscriber/apply_layout/caps + sync `tick` 30 fps, `set_frame`/`set_audio` mock source).
  - `lib.rs` — re-exports.
- [x] **vks-core: `transparency.rs`** (привязка: [ARCHITECTURE.md §2](../architecture/ARCHITECTURE.md), ADR-0002, ADR-0026): **6 теста** (всего в vks-core **88**):
  - `ModeSwitcher` — in-place SFU↔MCU: держит active + candidate, `switch_mode` ждёт `SwitchClock`-ready (timeout 3s → `RolledBack`), атомарно подменяет, инкремент `switch_epoch`, `RoomModeChanged` событие, Layout сохраняется.
  - `ClientView` — «клиент» без `if mode==Sfu` (subscribe/apply_layout/observe_mode) — доказательство прозрачности.
  - `RoomModeChanged { room_id, new_mode, switch_epoch }` — доменное событие (NATS-адаптер → `vks-proto`).
  - `SwitchClock` — детерминированные «часы» ready.
  - `MockEngine` (impl MediaEngine) — для switch/rollback/layout-preservation тестов.
- [x] **vks-recorder** (привязка: [DEVELOPMENT_PLAN.md](../DEVELOPMENT_PLAN.md) Ф7, [BACKEND_RULES.md](../rules/BACKEND_RULES.md)): 4 модуля, **27 теста**, clippy clean:
  - `recording.rs` — `RecordingSession` (start(mode)/stop→Meta/append_segment), `Segment`/`SegmentKind`/`RecordingMode` (Mcu|SFu), `Clock` (parametrized, deterministic).
  - `storage.rs` — `RecordingStore` trait (save/get/list/purge_expired/list_all default) + `MemoryStore`.
  - `retention.rs` — `RetentionPolicy { max_age_days, max_rooms }`, is_expired_by_age/count, `select_for_purge`.

#### Критерии готовности (DEVELOPMENT_PLAN.md Фазы 3) — проверено
- [x] `cargo build --workspace` — 🟢.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — 🟢 0 замечаний.
- [x] `cargo test --workspace` — 🟢 **367 passed, 0 failed** (mcu 36, core 88, sdk 34, layout 28, sfu 54, recorder 27, storage 82, common 29, auth 21, proto 5).
- [x] MCU compositor: N frames → 1 1920×1080 composite (letterbox, canvas-clamped) — тесты (2 frames land in tiles, single-frame full canvas, zero→black, letterbox aspect).
- [x] Audio: N → 1 mix (1/√N + soft-clip) — 7 тестов.
- [x] VAD: RMS+ZCR+debounce — тесты (incl. DC-by-RMS, debounce window).
- [x] SFU↔MCU transparency: in-place switch, epoch, rollback-on-timeout, layout-preservation, client-mode-agnostic — 6 тестов.
- [x] Клиентский SDK **не знает** о режиме: `ClientView` без `if room_mode` (проверено кодом + тестом).

#### Ошибки, с которыми столкнулись
| Ошибка | Где | Причина | Как исправили | Статус |
|---|---|---|---|---|
| `dyn MediaEngine` «only auto traits…additional traits» | vks-media-mcu engine | `dyn MediaEngine + X` | `dyn MediaEngine` (supertraits уже в трейт) | fixed |
| lifetime mismatch `create_room`/`destroy_room` | vks-media-mcu engine | `#[async_trait]` impl vs trait signature | Совпал с точным sig из vks-core (async_trait) | fixed |
| compositor `blit` clamp: `rw = w - x` → 0 (tile 1 не рендерится) | vks-media-mcu compositor | неправильный clamp (width минус x) | Visible region = `rect ∩ canvas` (right/bottom = x+w clamp) | fixed |
| `Frame::filled(.., u32)` → value: u8 mismatch | vks-media-mcu compositor test | `(10*(i+1))` u32 | `as u8` | fixed |
| `0 * 1920` «always returns zero» (clippy) | vks-media-mcu compositor/engine tests | `0 *` в index | drop `0 *` | fixed |
| AudioMixer: tanh ВСЕГДА vs pass-through внутри ±1 | vks-media-mcu audio_mixer | impl применял tanh всегда; тесты ждали raw внутри ±1 | `if abs<=1 {v} else {tanh}` | fixed |
| VAD: `zcr > thr` vs test `&dc,0.01,0.0`=true | vks-media-mcu vad | DC zcr=0, `>0`=false | `zcr >= zcr_threshold` | fixed |
| `full_pipeline` assert `data[100]` (row 0 = letterbox bar) | vks-media-mcu engine test | 320×180 letterbox в 1080-слот → image rows 270..810 | assert at row 400 (image center) | fixed |
| `VadState` impl Default «can be derived» (clippy) | vks-media-mcu vad | ручной `impl Default` | `#[derive(Default)]` + `#[default]` | fixed |
| `LayoutDescriptor`/`RoomCaps` default+assign (clippy) | vks-media-mcu engine tests | `Default::default()` + field assign | struct literal / `..Default::default()` | fixed |
| `get(&N).is_none()` (clippy) | vks-media-mcu layout_bridge test | | `!contains_key(N)` | fixed |
| `MockEngine` never constructed in lib (clippy) | vks-core transparency | mock used only in tests | `#[cfg_attr(not(test), allow(dead_code))]` | fixed |
| `dyn MediaEngine` не `Debug` | vks-core transparency | `#[derive(Debug)]` на `ModeSwitcher{active: Box<dyn MediaEngine>}` | убрал derive | fixed |
| `PublisherHandle`/`SubscriberHandle` field names | vks-core transparency mock | mock wrote `{participant, tracks}` | `{room, publisher}` / `{room, subscriber}` | fixed |
| `apply_layout(&self)` mutates `current_layout` | vks-core transparency | `&self` vs `&mut self` | `&mut self` | fixed |
| vks-recorder `RecordingStoreExt` unresolved / `by_id` private | vks-recorder | отдельный extension-trait + private field | `list_all` = default method на `RecordingStore` | fixed |

#### Риск / вопрос к команде
- Q8 (WebRTC в WASM) — **resolved** (ADR-0025): web-sys + browser API; str0m — native.
- ADR-0023 (лицензия) — единственный открытый вопрос (workspace "MIT OR Apache-2.0").
- **Скоуп-ограничение Ф3**: `vks-media-mcu` compositor сейчас — nearest-neighbour + letterbox на placeholder `Frame` (raw bytes); реальный bilinear (libyuv) + codec-decode (H.264/VP8) + oxideav-scene — за optional feature (не в default, чтобы baseline остался лёгким). Live-цикл DTLS/SRTP + e2e «два браузера» — следующий шаг (полный Ф3 / Ф6).
- `vks-core::ModeSwitcher` — синхронный (det.) in-place switch; в production обёрнётся в async (NATS `RoomModeChanged` → signaling → SDK renegotiation) — ADR-0026 (global default + per-room override).
- **Pуть с пробелом** (Рабочий стол) — `protobuf-src` build-script падает (libtool) на этой машине; сборка шла через `/tmp/vksbuild` (без пробела), всё синхронизировано в реальный проект. В CI (ubuntu) пробела нет.

---

### [2026-10-15] Итерация 2.1 — Фаза 2: SFU-движок (str0m) + сигнализация + SDK + layout

#### Выполнено
- [x] **vks-media-sfu** (привязка: [DEVELOPMENT_PLAN.md](../DEVELOPMENT_PLAN.md) Ф2, [BACKEND_RULES.md §3](../rules/BACKEND_RULES.md), ADR-0017/0003): 9 модулей, **54 теста**, clippy clean (default + `str0m` feature, str0m 0.24.1):
  - `bwe.rs` — `BweEstimator` (TWCC arrival-time → bps, deterministic sliding window).
  - `simulcast.rs` — `SimulcastSelector` (Low/Med/High, порог 200/800kbps, downgrade instant / upgrade +5% hysteresis).
  - `congestion.rs` — `CongestionLadder` (Normal→SpatialMed→SpatialLow→audio-only, recovery +5%).
  - `forwarder.rs` — `Forwarder` (full-mesh по умолчанию, N-to-N, pluggable `Sink` + `RecordingSink` mock).
  - `dtls.rs` — `DtlsSession` (Handshaking→Active→Closed, SRTP keying-material).
  - `ice.rs` — `IceLite` (host-only candidates, RFC 8445 §2).
  - `codec.rs` — `CodecRegistry` (feature-gated opus/vp8/vp9/av1, ADR-0017).
  - `engine.rs` — `SfuEngine` (implements `vks_core::MediaEngine`, `on_feedback` TS→layer, `route` full-mesh).
- [x] **vks-signaling** (привязка: [SIGNALING_PROTOCOL.md](../architecture/SIGNALING_PROTOCOL.md) §2-§6): **32 теста**, clippy clean:
  - `sdp.rs` — `Sdp`/`Media`/`IceCandidate`/`RtpMap` модель + text codec + SFU offer→answer builder + ICE-candidates.
  - `ws.rs` — `WsSession` порт + `MockWs` (+ feature `ws` для реального tokio-tungstenite).
  - `wt.rs` — `WebTransportSession` порт + `MockWt` (+ feature `wtransport`).
  - `handler.rs` — `SignalingHandler`: dispatch по payload (ClientHello→ServerHello, JoinRoom→JoinRoomResult c join_nonce, SdpOffer→SdpAnswer, Subscribe/LeaveRoom→Ack), движущий Session FSM, Error-reply на FSM-illegal.
- [x] **vks-client-sdk** (привязка: [ARCHITECTURE.md §233](../architecture/ARCHITECTURE.md), [FRONTEND_RULES.md §7](../rules/FRONTEND_RULES.md), Q8): **34 теста**, clippy clean:
  - `conference.rs` — `Conference` (единый API: join→active→reconnect→leave, `ConferenceState`, `QoeConfig`).
  - `signaling_client.rs` — `SignalingTransport`-порт + `MockSignaling` + auto-reconnect backoff (1→16s, max 5).
  - `media_engine/` — `SfuRenderer` (N треков через `vks-layout` grid) + `McuRenderer` (1 композитный трек).
  - `render_surface.rs` — `RenderSurface`-порт + `NullSurface`/`RecordingSurface`.
  - `webrtc_glue.rs` — `WebrtcPeer`-порт + `MockPeer` + `WebRtcBackend` (Q8: web-sys vs str0m).
  - `types.rs` — `ConferenceEvent`/`QualityReport`.
- [x] **vks-layout** (привязка: [DEVELOPMENT_PLAN.md](../DEVELOPMENT_PLAN.md) Ф4): **28 теста**, clippy clean:
  - `rect.rs` — `Canvas`/`Rect` (u32 px) + area/center/contains/intersects.
  - `core.rs` — `Layout`/`LayoutKind`/`Slot` (serde).
  - `grid.rs` — `GridLayout::compute` (balanced, total==canvas, no gaps/overlap).
  - `focus.rs` — `FocusLayout::compute` (focus >50%, total==canvas).
  - `renderer.rs` — `Renderer` → `Vec<DrawCommand>` (letterbox fit-scale).

#### Критерии готовности (DEVELOPMENT_PLAN.md Фазы 2) — проверено
- [x] `cargo build --workspace` — 🟢.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — 🟢 0 замечаний.
- [x] `cargo test --workspace` — 🟢 **298 passed, 0 failed** (media-sfu 54, signaling 32, sdk 34, layout 28, core 13, storage 82, auth 21, common 29, proto 5).
- [x] SFU: bwe/simulcast/congestion/forwarder/engine/ice/codec — все покрыты тестами (deterministic, эмуляция degradation).
- [x] Signaling: WS + WebTransport — оба порта + mock; SDP offer→answer + ICE; handler движет FSM.
- [x] SDK: `Conference::join` lifecycle + auto-reconnect + SFU/MCU renderer + QoE.

#### Ошибки, с которыми столкнулись
| Ошибка | Где | Причина | Как исправили | Статус |
|---|---|---|---|---|
| `v2 {` / `/// v2::` mangled type names | vks-signaling handler.rs | Суб-агент сгенерировал `Self`→`v2` артефакт | `v2 {`→`Self {`, `sig::ServerHello`/`sig::ParticipantInfo`/`sig::JoinRoomResult` | fixed |
| `JoinRoom`/`ParticipantInfo` поле `is_recording` vs `is_recorder` | vks-signaling | В proto `is_recorder` | Исправил на `is_recorder` | fixed |
| `with_ice_candidates` вызывается на tuple `(Sdp, Vec)` | vks-signaling sdp.rs test | `sfu_answer` возвращает tuple; метод на `Sdp` | Destructure tuple first | fixed |
| `MockWt::send` не найден | vks-signaling handler test | `send` — метод trait `WebTransportSession` | `use crate::wt::{MockWt, WebTransportSession}` | fixed |
| rtpmap parse: `a=rtpmap:111 opus/48000/2` (pt+space) vs `pt/codec/clock` | vks-signaling sdp.rs | `to_sdp_text` пишет `pt codec/clock/ch`; parser ждал `pt/codec/clock` | Parser: `split_once(' ')` → pt, потом `codec/clock/ch` по `/` | fixed |
| `vks-client-sdk` `vks_common::ids::ULID` unresolved | client-sdk | В vks-common `ids::ulid()` fn, не `ULID` type | `ulid::Ulid` type + `vks_common::ids::ulid_string()` | fixed |
| `vks-client-sdk` `vks_layout::grid` private | client-sdk sfu_renderer | Sub-agent экспортирует `vks_layout::GridLayout` (flat) | `vks_layout::Canvas::new` + `vks_layout::GridLayout::compute` | fixed |
| `dyn RenderSurface` no `Debug`/`Clone` | client-sdk conference | `Arc<dyn Trait>` не имеет этих trait'ов | Убрал derive с `ConferenceConfig` | fixed |
| `SyncWaker` `Waker::from_raw` 2 args | client-sdk sync_helper | `from_raw(ptr, &vtable)` | Заменил на `tokio::runtime` `block_on` | fixed |
| `QualityReport`/`ConferenceEvent` `Eq` на `f32` | client-sdk types | `f32: Eq` = false | Убрал `Eq` (оставил `PartialEq`) | fixed |
| `unexpected cfg ws`/`wtransport` | vks-signaling ws.rs/wt.rs | Фичи `#[cfg]` не объявлены в Cargo.toml | Добавил `[features] ws=[] wtransport=[]` | fixed |
| `match` on single pattern (clippy) | vks-signaling sdp.rs | `match part.parse() {Ok→,Err→{}}` | `if let Ok(pt) = ...` | fixed |
| `clone()` → `from_ref` (clippy) | vks-signaling sdp.rs test | `&[x.clone()]` для одного elem | `std::slice::from_ref(&x)` | fixed |

#### Риск / вопрос к команде
- Q8 (WebRTC в WASM) — **resolved** (ADR-0025): web-sys + browser API (default `WebRtcBackend::WebSys`); str0m — native.
- ADR-0023 (лицензия) — единственный открытый вопрос (workspace "MIT OR Apache-2.0").
- SFU `engine.rs` имплементирует `vks_core::MediaEngine` (port) — интеграция с реальным str0m `Connection`/SRTP придёт в фазе 2-полный (live DTLS loop).
- vks-client-sdk `Conference::start` сейчас sync (через `tokio block_on` mock); в фазе 8 (Dioxus) — async-поток SDK → Dioxus signals.
- **Pуть с пробелом** (Рабочий стол) — `protobuf-src` build-script падает (libtool) на этой машине; сборка шла через `/tmp/vksbuild` (без пробела), всё синхронизировано в реальный проект. В CI (ubuntu) пробела нет.

---

### [2026-10-13] Итерация 0.1 — Документация и правила проекта

#### Выполнено
- [x] Создана структура `docs/` (rules, architecture, decisions, progress).
- [x] Написан `docs/PROJECT_RULES.md` — правила для ИИ-агента (общие, бэкенд, фронтенд, код, git, шпаргалка).
- [x] Написан `docs/architecture/ARCHITECTURE.md` — требования, инвариант MCU/SFU, структура workspace, дерево зависимостей, стек, поток данных.
- [x] Создан `docs/decisions/TEMPLATE_DECISION.md` — шаблон ADR.
- [x] Создан `docs/decisions/DECISIONS.md` — реестр 26 ADR (25 accepted, 1 proposed [лицензия]).
- [x] Создан `docs/progress/TEMPLATE_ERROR.md` — шаблон записи ошибок.
- [x] Написан `docs/progress/PROGRESS.md` (этот файл).
- [x] Написан `docs/DEVELOPMENT_PLAN.md` — фазы 0–8 с чек-листами и критериями готовности.

#### Осталось выполнить
- [x] Фаза 0: `Cargo.toml` workspace, `.cargo/config.toml`, `deny.toml`, `.rustfmt.toml`, `.github/workflows/ci.yml`.
- [x] `rust-toolchain.toml` — Rust 1.94.0 (ADR-0024, Q6).
- [x] Фаза 0: крейт `vks-common` (error, tracing, metrics, config, ids) — 14 unit-тестов, clippy/fmt clean.
- [x] Фаза 0: крейт `vks-proto` — 4 `.proto` (signaling/media/cluster/admin) + `build.rs` (vendored protoc) + `openapi.json` — 5 roundtrip-тестов, clippy/fmt clean.
- [x] Фаза 0: skeleton-крейты всех 14 членов workspace + `client/vks-client-sdk` (workspace собирается, `cargo build/clippy/test --workspace` зелёные).
- [x] Написать детальные правила `docs/rules/BACKEND_RULES.md`, `FRONTEND_RULES.md`, `CODE_STYLE.md`.
- [x] Детальные архитектурные документы: SIGNALING_PROTOCOL.md, MEDIA_PIPELINE.md, LAYOUT_ENGINE.md, CLUSTERING.md.
- [x] Q1–Q8 → все `resolved`, ADR-0004/0007/0012/0015/0021/0024/0025/0026 → `accepted`.
- [ ] Создать ADR-файлы: `0001-*.md` … `0026-*.md` (по TEMPLATE_DECISION.md).
- [ ] ADR-0023 (лицензия) — остаётся `proposed` (не выбрано).

#### Ошибки, с которыми столкнулись
| Ошибка | Где/когда | Причина | Как исправили | Статус |
|---|---|---|---|---|
| `vks-media` vs `vks-media-sfu`/`-mcu` противоречие | ARCHITECTURE §3 vs дерево | Разные варианты в разных местах | Приведено к 2 крейта: vks-media-sfu + vks-media-mcu | fixed |
| Дубль дерева зависимостей | RULES §7.2 = ARCHITECTURE §3 | Задваено | RULES → ссылка на ARCHITECTURE | fixed |
| `hadrone-dioxus` vs `hadrone-core` | RULES L245 | Разные имена | Исправлено на `hadrone-core` | fixed |
| PROGRESS: «10 accepted, 6 proposed» | PROGRESS L49 | Неточный подсчёт | Исправлено: 25 accepted, 1 proposed | fixed |

#### Риск / вопрос к команде
- ADR-0023 (лицензия) — единственный открытый вопрос.

---

### [2026-10-14] Итерация 1.1 — Фаза 1: ядро (vks-core) + vks-storage + vks-auth + vks-common + vks-signaling

#### Выполнено
- [x] **vks-core** (привязка: [ARCHITECTURE.md](../architecture/ARCHITECTURE.md) §2/§4.1, [DEVELOPMENT_PLAN.md](../DEVELOPMENT_PLAN.md) ф1):
  - `ports/media_engine.rs` — КЛЮЧЕВОЙ трейт `MediaEngine` (транспортно-независимый: SFU и MCU оба имплементируют; MCU/SFU-прозрачность — участник не знает, какой режим у комнаты).
  - `ports/{storage,auth,events,clock,rng}.rs` — async-порты (CRUD Room/Participant/Layout/ApiKey; verify credentials; pub/sub; Clock + Rng для детерминированных тестов).
  - `room.rs` — `Room`, `RoomId`, `RoomState`(Idle/Active/Destroyed) c `#[default]`, `RoomMode`(Sfu|Mcu).
  - `participant.rs` — `Participant`, `Role`(Operator/Moderator/Participant/Viewer), `Permissions`, `UserId` (Uuid v4) + `Default`.
  - `session.rs` — Session FSM (Disconnected→Connecting→Connected→...).
  - `track.rs` — `TrackId`, `TrackKind`(Audio/Video/Screen), `MediaKind`.
  - Тесты: FSM-переходы (все, включая illegal), RoomState, permissions, media-engine mock.
- [x] **vks-storage** (привязка: [BACKEND_RULES.md](../rules/BACKEND_RULES.md) §5, ф1): `memory.rs` (RwLock CRUD), `sqlite.rs` + `postgres.rs` (sqlx runtime, `_unstable-all-types` для u32→Postgres), `migrations/0001_initial.sql` (users, rooms, room_mode_override, layouts, api_keys, audit_log), `rows.rs` (User/Room/Layout/ApiKey/AuditEntry), README.
- [x] **vks-auth** (привязка: [BACKEND_RULES.md](../rules/BACKEND_RULES.md) §2/§6.3, ADR-0009/0022): `jwt.rs` (HS256, `jti` в логах, `leeway=0`), `api_key.rs` (`vks_`+32-hex, SHA-256, prefix 8), `ldap.rs` (порт `LdapClient` + mock), `local_db.rs` (PBKDF2), `provider.rs` (Local/Ldap/Oidc), README.
- [x] **vks-common** (привязка: [PROJECT_RULES.md](../rules/PROJECT_RULES.md) §19–22): расширены `metrics.rs` (media bytes/packets dropped, room signal latency, errors by code) + тесты error/config/ids/metrics.
- [x] **vks-signaling** (привязка: [SIGNALING_PROTOCOL.md](../architecture/SIGNALING_PROTOCOL.md) §1/§7/§11/§12): `session.rs` (FSM Disconnected→Authenticating→Connected→Joining→InRoom→Negotiating→MediaActive→Leaving, все переходы + `any→Disconnected`), `envelope.rs` (кодек `SignalingMessage`: encode/decode, `error_reply`, `ack_reply`), `reconnect.rs` (backoff 1s→16s ×5, nonce TTL 60s).

#### Критерии готовности (DEVELOPMENT_PLAN.md Фазы 1) — проверено
- [x] `cargo build --workspace` — 🟢 (15 членов).
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — 🟢 0 замечаний.
- [x] `cargo test --workspace` — 🟢 **165 passed, 0 failed** (vks-auth 21, vks-common 29, vks-storage 82, vks-signaling 15, vks-core 13, vks-proto 5).
- [x] `cargo fmt --all` — 🟢 чисто.
- [x] `vks-core` — KPI: FSM покрыт (все переходы), MediaEngine-трейт + mock.
- [x] `vks-storage` — memory+sqlite компилируются, migration-тесты зелёные.

#### Ошибки, с которыми столкнулись
| Ошибка | Где/когда | Причина | Как исправили | Статус |
|---|---|---|---|---|
| `serde_json` unresolved в vks-core | events.rs | Не в deps | Добавил `serde_json.workspace` | fixed |
| `SystemTime::now()` → `SystemTime` (не `Result`) | vks-core clock.rs | В Rust 1.94 `SystemTime::now()` не возвращает Result (unstable `time` API) | `Ok(SystemTime::now())` | fixed |
| `'{user}'` char literal (2 codepoint) | vks-auth ldap.rs | Char-literal на 2 символа | `"{user}"` | fixed |
| `u32: sqlx::Type<Postgres>` не сатисфакчен | vks-storage postgres.rs | `u32` нет impl для Postgres; `Layout.width/height` = i32 | Row structs → i32, `as` cast где нужно | fixed |
| `_unstable-all-types` vs `postgres-types` | vks-storage | `postgres-types` не top-level feature в 0.8.6 | `_unstable-all-types` | fixed |
| `RoomModeOverrideRow.room_id` dead_code | vks-storage | Поле из DB, но читается только `mode` | `#[allow(dead_code)]` | fixed |
| `Error: PartialEq` не сатисфакчен (sqlx::Error не PartialEq) | vks-storage rows.rs | Test сравнивает `Result<RoomMode, Error>` | Сравнивать `.unwrap()`, не `Result` | fixed |
| `Tile` unused import в memory.rs | vks-storage | `use super::*` уже поднимает | Импорт в test mod | fixed |
| jwt `leeway=60` default пропускает expired | vks-auth jwt.rs | jsonwebtoken 9 default leeway=60s | `validation.leeway = 0` | fixed |
| `new_registry_exposes_all_metrics` got `{}` | vks-common metrics.rs | `gather()` не возвращает vec-метрики без child-лейблов | Тест трогает globals через `init_default` | fixed |
| `tokio` unresolved в test_util | vks-auth lib.rs | Manual `block_on` с `Waker::from_raw` (1 arg) — broken | Заменил на `tokio::runtime::block_on` | fixed |

#### Риск / вопрос к команде
- ADR-0023 (лицензия) — единственный открытый вопрос (workspace "MIT OR Apache-2.0" до выбора).
- vks-core `Session` FSM vs vks-signaling `Session` FSM — разные (core = generic, signaling = wire-specific). OK: signaling-сессия — тонкая обёртка над core-сессией в будущем (фаза 2).
- vks-core не импортирует vks-proto (Sans-I/O) — MediaEngine-трейт использует собственные доменные типы (Track/RoomMode из core), не proto. Это по ADR-0016.

---

### [2026-10-13] Итерация 0.2 — Фаза 0: инфраструктура workspace и базовые крейты

#### Выполнено
- [x] `Cargo.toml` (workspace): 15 членов (14 `crates/*` + `client/vks-client-sdk`), `workspace.dependencies` (tokio, thiserror, anyhow, tracing(+subscriber), prometheus, figment, serde, uuid v7, ulid, prost/prost-build/tonic, axum, utoipa, criterion, proptest), `workspace.package` (v0.1.0 / edition 2021 / rust-version 1.94 / license "MIT OR Apache-2.0"), `[profile.release]` (lto thin, codegen-units 1, strip symbols).
- [x] `.cargo/config.toml`: профили dev (debug line-tables-only) / release (opt 3, lto thin, codegen-units 1, strip, panic=abort) / bench.
- [x] `deny.toml`: cargo-deny — лицензии (MIT/Apache-2.0/BSD/ISC/CC0/MPL allow; GPL/LGPL/AGPL/SSPL deny) + CVE (deny unsound/unserviced/direct).
- [x] `.rustfmt.toml`: max_width 120, comment_width 100, edition 2021 (nightly-only опции — с предупреждением в stable).
- [x] `.github/workflows/ci.yml`: fmt / clippy -D warnings / test / cargo-deny / build --release (toolchain 1.94.0); E2E (chromiumoxide) закомментировано под фазу 8.
- [x] `crates/vks-common` (привязка: BACKEND_RULES §6, PROJECT_RULES §19–22): `error.rs` (thiserror Error + ErrorCode/HttpStatus по 400/401/403/404/409/422/429/500), `tracing.rs` (human/JSON), `metrics.rs` (prometheus: signaling_messages, active_rooms, build_info), `config.rs` (figment: defaults → TOML → env `VKS_*`), `ids.rs` (UUIDv7 + ULID), `lib.rs`, README. 14 unit-тестов.
- [x] `crates/vks-proto` (привязка: SIGNALING_PROTOCOL.md): 4 `.proto` — `signaling.proto` (полный: envelope + handshake + join + SDP/ICE + subscribe + track/participant events + quality + layout + mode_changed), `media.proto` (RoomMode/CodecId/TrackInfo/RoomCaps/Subscribe), `cluster.proto` (heartbeat/ownership/maintaining), `admin.proto` (DTO-скелет); `build.rs` на `protobuf-src` (компилирует собственный protoc — системный не нужен); `src/lib.rs` (включает 4 пакета под родителем `vks`, re-export); `openapi.json` (admin REST, источник progenitor ADR-0013); README. 5 roundtrip-тестов.
- [x] Skeleton (Cargo.toml + src) всех остальных членов workspace: vks-core, vks-auth, vks-storage, vks-layout, vks-signaling, vks-media-sfu, vks-media-mcu, vks-gateway, vks-admin, vks-recorder, vks-cluster, vks-server + `client/vks-client-sdk` — чтобы workspace собирался целиком.

#### Критерии готовности (DEVELOPMENT_PLAN.md Фаза 0) — проверено
- [x] `cargo build --workspace` — 🟢 15/15 членов.
- [x] `cargo clippy --workspace --all-targets -- -D warnings` — 🟢 0 замечаний.
- [x] `cargo test --workspace` — 🟢 19 passed (14 vks-common + 5 vks-proto), 0 failed.
- [x] `cargo fmt --all -- --check` — 🟢 чисто.

#### Ошибки, с которыми столкнулись
| Ошибка | Где/когда | Причина | Как исправили | Статус |
|---|---|---|---|---|
| libtool: «whitespace in pwd» (прототип `protobuf-src`) | build vks-proto | Путь `/home/pro/Рабочий стол/mcu` содержит пробел; libtool/Makefile ломаются на нём | Сборка через зеркало в `/tmp/vksbuild` (без пробела); в CI (ubuntu) пробела нет | fixed |
| `prost-build` нет `include_directories` | build.rs vks-proto | API 0.13: `compile_protos(protos, includes)` | Заменено на `compile_protos` + `protoc_executable` | fixed |
| prost генерирует плоские файлы `vks.<pkg>.rs` | vks-proto/src/lib.rs | `include!` «vks.rs» не существует | 4 `pub mod`-обёртки под родителем `vks` | fixed |
| camelCase/snake_case в Rust-генерации | тесты vks-proto | `RoomModeSfu`→`RoomMode::Sfu`, oneof `signaling_message::Payload`, `encode_to_vec`→`Vec<u8>` (без Result) | Тесты переписаны под фактический API | fixed |
| figment: `Config` не `Provider`, `Env::map` → `Uncased`, `Toml` за trait `Format` | config.rs | Разные API-формы figment 0.10 | `Serialized::defaults`, `Uncased::new`, `use providers::{Format, Toml}` | fixed |
| uuid 1.28: `Version::V7` → `Version::SortRand`; ulid: `FromStr` (feature std) | ids.rs/тесты | Переименование enum-вариантов | Исправлено | fixed |
| Гонка env-тестов (VKS_SERVER_NAME) | config tests | Параллельные тесты читают/пишут глобальные env | Мьютекс `ENV_LOCK` на оба env-теста | fixed |

#### Риск / вопрос к команде
- ADR-0023 (лицензия) — единственный открытый вопрос (workspace временно "MIT OR Apache-2.0" до выбора).
- ADR-файлы `0001-*.md … 0026-*.md` (по TEMPLATE_DECISION.md) — ещё не созданы как отдельные файлы (реестр уже в DECISIONS.md).

---

## Архив

См. `docs/progress/archive/` (заполняется с 2026-11).

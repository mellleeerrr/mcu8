# L5–L7 · Инфраструктура: транспорт, запись, кластер, телеметрия, L0

**Дата:** 2026-10-29 · **HEAD:** `d82ce4d` · **Дерево:** чистое · **База:** Rust 1.94.0, 1425 passed / 0 failed / 8 ignored
**Зона:** `vks-signaling/{wt,ws,handler,envelope,session,sdp}.rs` + Cargo; `vks-server/{lib,main,recorder,cluster,frontend}.rs` (транспорт/запись/кластер/телеметрия); `vks-recorder/`; `vks-storage/` (миграции + код); `vks-auth/src/local_db.rs` + Cargo; `vks-cluster/`; `configs/*`, `deploy/`, `.github/workflows/ci.yml`; `vks-admin/src/api_v3/*` (videorecords).
**Режим:** read-only аудит. Сервис/БД/код не менялись. Тяжёлые сборки не запускались; версии и API крейтов проверялись по crates.io/docs.rs.

---

## §1 Вердикт

**Заявленное целевое состояние не реализовано ни по одному из шести пунктов; фактическое соответствие — 2,5/10.**
При этом инженерная база под реализацию хорошая (готовность к работе ≈8/10): порты, FSM, лимиты WS, детерминированные тесты, чистые clippy/fmt, doc-культура. Проблема не в качестве кода, а в **разрыве между спецификацией и кодом**: `SIGNALING_PROTOCOL.md:5`, `ARCHITECTURE.md:16` объявляют WebTransport «обязательным (R5)», `CLUSTERING.md`/`ADR-0014` — NATS, `BACKEND_RULES.md` §1.3 — Argon2id, `MASTER_TASK_LIST.md:55` — T10b. В `Cargo.lock` при этом **ноль** `quinn`/`rustls`/`wtransport`/`argon2`/`async-nats`/`opentelemetry`/`tonic` (проверено `grep -c`), а «реализация» — это mock-трейты, in-memory шина и PBKDF2.

Коротко по пунктам:

| # | Пункт заявки | Факт | Оценка |
|---|---|---|---|
| 1 | WebTransport/QUIC вместо mock | Только `MockWt` (`wt.rs:29`) + `MockWs` (`ws.rs:30`); HTTP-роута `/wt` нет, `Alt-Svc` нет, QUIC-стека нет. `ServerHello.room_mode` — **есть** (`handler.rs:288`) | 0,5/10 |
| 2 | RecorderProfile `W·H·fps·bpp 0.085` + запись + `/api/v3/videorecords` | Профиля нет вообще; запись никем не запускается (`start_recording` — только в тестах); записи **молча теряются** в SQLite/PG (`upsert_record` — no-op по умолчанию) | 1/10 |
| 3 | T10b: нормализация directory миграцией | Справочник целиком в одном JSON-документе (`resources.rs:241-269`); SQL-таблиц directory нет; миграции 0001–0005 к T10b отношения не имеют | 0/10 |
| 4 | Argon2id m=19456,t=2,p=1 + fallback pbkdf2 | Только PBKDF2-SHA256 100k (`local_db.rs:20`); `argon2` в lock нет; fallback-инфраструктуры нет (но и pbkdf2-строки пока единственный формат — миграция формата не начата) | 2/10 |
| 5 | NATS-шина + OTLP | `Bus` — `Arc<Mutex<…>>` in-memory (`bus.rs:35-40`, `cluster.rs:40`); телеметрия — только Prometheus (`main.rs:51`) | 1/10 |
| 6 | L0: sysctl + cgroup v2 для 40000/ICE/RTP | `LimitNOFILE=65536` есть; `Slice`/`CPUWeight`/`MemoryHigh`+`MemoryMax`/`LimitRTPRIO`/sysctl-профиля нет; в коде **нет** ни `SO_RCVBUF`, ни `sched_setscheduler` — лимиты некому использовать | 2,5/10 |

**Главный вывод:** три из шести пунктов (1, 5, 6) в текущей архитектуре не дают продукту ничего, пока не сделаны пункты 2 и 3 — запись и справочник. Их и надо делать первыми (порядок в §5). Пункт 6 без правок кода (SO_RCVBUF/RT-потоки) — чистый театр конфигурации и **не должен** попадать в отчёт как «выполненный».

---

## §2 Матрица: реализовано / mock / отсутствует

Легенда: **✅** реализовано и используется прод-кодом · **🟡** реализовано частично/не подключено · **🟨 mock** (тип есть, I/O нет) · **❌** отсутствует.

### 2.1 Транспорт сигнализации

| Элемент | Статус | Доказательство |
|---|---|---|
| WS-сервер `/ws` (axum) | ✅ | `vks-server/src/lib.rs:152` (`route("/ws", get(ws_handler))`), `lib.rs:383-443` |
| WS auth-at-handshake (fail-closed) | ✅ | `lib.rs:389-395` (503 без issuer), `396-430` (Bearer / `?token=`), `427-430` (`verify_token`) |
| WS Origin-check для `?token=` | ✅ | `lib.rs:409-426`, allowlist `VKS_WS_ORIGIN_ALLOW` (`lib.rs:419`) |
| WS лимит размера 64 КиБ + transport cap 256 КиБ | ✅ | `lib.rs:450-457`, `lib.rs:442`, `lib.rs:743-752` (close 4008) |
| WS rate-limit 60 msg/s | ✅ | `lib.rs:459-460`, `lib.rs:716-726` |
| WS keepalive 15 c / token-expiry / revocation | ✅ | `lib.rs:619-664` |
| WS per-IP connection cap | ❌ | в `/ws` нет; есть только в legacy `/api/v3/ws` (`api_v3/ws.rs:36,62-72`) |
| `WsSession` trait | 🟨 mock | `ws.rs:10-22`; прод-код его **не использует** (`handle_ws_socket` работает напрямую с `axum::extract::ws::WebSocket`, `lib.rs:608-609`) |
| `MockWs` | 🟨 mock | `ws.rs:30-77`; единственный тест создаёт и сразу выбрасывает (`handler.rs:537`: `let _mock = MockWs::new();`) |
| `WebTransportSession` trait | 🟨 mock | `wt.rs:9-21` |
| `MockWt` | 🟨 mock | `wt.rs:29-76`; используется в `handler.rs:616-625` |
| Реальный QUIC/WT listener | ❌ | нет роута `/wt`; `grep -rn '"/wt"' crates` → пусто; `grep -c 'name = "quinn"' Cargo.lock` → `0` |
| `Alt-Svc: h3` / HTTP/3 | ❌ | `grep -rni "alt-svc"` → только аудит 2026-10-25 |
| WT framing (bi-streams + datagrams) | ❌ | протокол объявляет «один фрейм = один protobuf» без length-prefix (`SIGNALING_PROTOCOL.md:14-16`) — для bi-stream нужен префикс, для datagram — лимит ≤1200 B |
| `ServerHello.room_mode` + `caps` | ✅ | `handler.rs:282-292` (`room_mode: self.config.room_mode` — 288), `handler.rs:96-108` |
| `ServerHello` версии протокола | ✅ (жёстко v1) | `handler.rs:21-22` `MIN_V2=MAX_V2=1`, `handler.rs:261-269` |
| Обработка ошибок/таймаутов транспорта | ❌ | трейт `send/recv` синхронный, `recv → Option<Vec<u8>>` — ошибка/таймаут/закрытие не выражаются типами (`ws.rs:12-21`, `wt.rs:11-20`) |
| Resume/reconnect по `resume_nonce` | ❌ | поле есть в proto (`signaling.proto:51`), но `on_client_hello` его не читает (`handler.rs:260-293`); FSM-переходы `ConnectionLost/Resumed` есть (`session.rs:133-137`), но прод-код их не вызывает |
| ICE-кандидаты клиента | 🟡 | `on_ice_candidate` только подтверждает (`handler.rs:374-389`), адрес не парсится/не применяется; парсер `parse_ice_candidate_addr` используется в другом месте (`lib.rs:463-473`) |
| ICE-креденшелы сервера | ❌ хардкод | `main.rs:356-371`: `"sfuufrag"`, `"sfupwd"`, `"sha-256 FF:00:11"` — одинаковые во всех установках, fingerprint фиктивный |

### 2.2 Запись

| Элемент | Статус | Доказательство |
|---|---|---|
| `RecordingSession` (start/stop/segments) | ✅ | `vks-recorder/src/recording.rs:216-402`; идемпотентный `stop` (`:333-366`) |
| `RecorderManager` (RoomHub-участник, события) | ✅ | `vks-server/src/recorder.rs:63-134`, `136-223` |
| `RecorderProfile` / формула `W·H·fps·bpp` | ❌ | нет ни типа, ни поля; `ContainerConfig` без битрейта (`container.rs:25-38`), default 1280×720@30 (`container.rs:40-51`) |
| Клампы профиля | ❌ | нет |
| Запуск записи из прод-кода | ❌ | `start_recording` вызывают только тесты (`vks-server/tests/network_integration.rs:829,1587`); `WsContext.recorder` (`lib.rs:148`) **нигде не читается**; HTTP-эндпоинта старта нет (`api_v3/mod.rs:179-184` — только GET/DELETE-501) |
| Поток данных кадров в запись | ❌ | `write_video_sample`/`write_audio_sample` не вызываются вне тестов (`grep` по `crates/` — только `recording.rs` + тест) |
| MP4-контейнер | 🟡 формально | `container.rs:146-185`; но `stsd/avc1` **без `avcC`** (`container.rs:318-331`) и payload ожидается length-prefixed, а ферма отдаёт Annex-B (`vks-media-mcu/src/farm.rs:225-258`) → файл структурно невалиден |
| `RecordEntry` в состоянии | ✅ (RAM) | `vks-admin/src/state.rs:230`; запись добавляется в `recorder.rs:183-190` |
| Персистентность записей в SQLite/PG | ❌ | `upsert_record/list_records/get_record/delete_record` — **дефолтные no-op** в трейте (`vks-core/src/ports/storage.rs:294-311`); переопределены **только** у `MemoryStorage` (`core_adapter.rs:205-223`). У `SqliteStorage`/`PostgresStorage` — наследуются пустышки |
| Гидратация записей при рестарте | ❌ | `sync_from_storage` не вызывает `list_records` (`vks-admin/src/state.rs:600-640`) |
| `/api/v3/videorecords` | 🟡 | `observability.rs:676-705`; `record_json` хардкодит `bitrate:0`, `resolution:[1280,720]`, `profile:null` (`observability.rs:646-665`) |
| `estimated_time` | 🟡 | `observability.rs:740-758`: константа 3600 при свободном месте ≥1 ГБ, битрейт не учитывается; `df` — блокирующий `Command` в async (`:749-758`) |
| Удаление/скачивание записи | ❌ | DELETE → 501 (`observability.rs:731-737`); эндпоинта скачивания нет (`src: record.id` — фикция, `:660`) |
| Retention (90 дней) | 🟡 мёртвый | `retention.rs` экспортируется (`vks-recorder/src/lib.rs:44-46`), но `select_for_purge`/`purge_expired` не вызываются нигде |
| Шифрование at rest / consent | ❌ | заявлено в `vks-recorder/src/lib.rs:4-5,19-21` как «позже» |
| Остановка записи при shutdown | ❌ | `main.rs:476-484` не финализирует сессии (впрочем, их и нет) |

### 2.3 T10b (нормализация справочника)

| Элемент | Статус | Доказательство |
|---|---|---|
| Справочник (roles/groups/accounts/addresses) | ✅ в JSON | `resources.rs:24-55` (`addresses` — 42, `directory` — 46); `directory.rs:1-18` — «источник истины — configuration-document» |
| Персистентность справочника | ✅ целиком документом | `resources.rs:241-269`: каждый commit пишет **весь** `pnm-definitions-v1` (resources + conference_settings + video_settings) |
| SQL-таблицы directory | ❌ | миграции 0001–0005 (`vks-storage/migrations/`): `users`, `rooms`, `layouts`, `api_keys`, `audit_log`, `core_*`, `user_credentials`, `configuration_documents` — ничего про directory |
| Unique index на canonical URI | ❌ | дедуп адресов — офлайн-утилита по документу (`vks-storage/src/dedup.rs:1-20`, `bin/vks-db-dedup.rs`) |
| Согласованность SQLite↔PostgreSQL | 🟡 частично | 0001 — `UUID PRIMARY KEY` (`0001_initial.sql:8,15,22,28,35,43`), 0002–0004 — `TEXT PRIMARY KEY` (`0002:4,12,19`, `0003:4,12`, `0004:4`): две несовместимые конвенции id в одной БД |
| Идемпотентность миграций | ✅ | все `CREATE TABLE/INDEX IF NOT EXISTS` |
| Rollback (down-миграции) | ❌ | sqlx не поддерживает down; в репозитории их нет — принятая политика forward-only + бэкап (`deploy/README.md`, `db_lock`) |
| Двойной источник (документ + `core_users` + `user_credentials`) | ⚠️ есть | идентичность — `core_users` (`0003`), пароли — `user_credentials` (`0004`), профиль/группы/роли — документ; удаление пользователя пишет в три места (`directory.rs:902-939`) |

### 2.4 Аутентификация

| Элемент | Статус | Доказательство |
|---|---|---|
| PBKDF2-SHA256 100k + 16B salt | ✅ | `local_db.rs:20`, `:306-325` |
| Constant-time сравнение | 🟡 | `local_db.rs:100,236`: `ct_eq` по **hex-строкам** (работает, но сравниваются 64-символьные ASCII вместо 32 байт; при смене формата на PHC — ломается) |
| Тайминг-защита от enum | ✅ | `local_db.rs:106-111,242-246` (dummy-hash) |
| `spawn_blocking` для async-путей | ✅ | `local_db.rs:206-268` (`set_password_async`, `authenticate_async`, `register_async`) |
| Все прод-пути асинхронны | ❌ | `accounts.rs:139` — `db.register(...)` (100k PBKDF2) прямо в async-хендлере; `operator_policy.rs:82` — sync `authenticate` (старт, допустимо) |
| Argon2id | ❌ | `argon2` нет в `Cargo.lock`; `vks-auth/Cargo.toml` — только `pbkdf2 0.12` |
| Fallback старых pbkdf2-хешей | ❌ | формата-маркера нет: `password_hash_hex` (`0004:5`) — «сырой» hex без алгоритма; PHC-строки негде распознать |
| Блокировка/лимит попыток | ✅ | `api_v3/auth.rs:261-265` (lockout), `:280-299` |
| Смена пароля инвалидирует токены | ✅ | `local_db.rs:189-203` (+1 сек), `auth.rs:302-360` |

### 2.5 Кластер и телеметрия

| Элемент | Статус | Доказательство |
|---|---|---|
| NATS-клиент | ❌ | `async-nats` нет в lock; `vks-cluster/Cargo.toml` — только `thiserror/tracing/uuid` |
| Шина pub/sub | 🟨 in-memory | `bus.rs:34-40` (`Arc<Mutex<BusState>>`), `vks-server/src/cluster.rs:40` (`Bus::new()`) |
| Дедуп по `message_id` | 🟡 опасно | `bus.rs:35-41`: `id = format!("msg-{seq}")` из process-local `AtomicU64` → на двух узлах id совпадают (`msg-0`, `msg-1`, …) → cross-node «duplicate» и потеря сообщений |
| Backpressure soft/hard + DLQ | ✅ модель | `bus.rs:214+`, `vks-server/src/cluster.rs:146-149` (`10_000/20_000`) |
| Ownership/lease/fencing/split-brain | ✅ модель | `vks-cluster/src/ownership.rs` (574 стр.), `cluster.rs:101-143` |
| Heartbeat/discovery | 🟡 локальный | `heartbeat.rs` + `main.rs:441-451` (тик 5 с, peers нет; `send_heartbeat` только self) |
| `ShutdownSequence` (5 шагов) | 🟡 не подключён | `shutdown.rs:26-32`; `main.rs:476-484` — ручной graceful shutdown axum, шаги не вызываются |
| Prometheus `/metrics` | ✅ | `lib.rs:195-196,361-367`; токен/loopback-гейт `lib.rs:236-273` |
| OTLP-экспорт | ❌ | `opentelemetry*`/`tonic` в lock нет; `vks_common/src/tracing.rs:26-41` — только fmt-layer |
| Trace-context в шине | ❌ | `Msg` без `traceparent` (`bus.rs:22-33`) |

### 2.6 L0 (sysctl + cgroup v2)

| Элемент | Статус | Доказательство |
|---|---|---|
| `LimitNOFILE` | ✅ | `deploy/systemd/vks-mcu-prod.service` (`LimitNOFILE=65536`) |
| Sandbox/hardening | ✅ | там же: `DynamicUser`, `ProtectSystem=strict`, `ProtectKernelTunables`, `CapabilityBoundingSet=` |
| `Slice=` / `CPUWeight=` / `MemoryHigh=` / `MemoryMax=` | ❌ | в юните нет; отдельного `vks.slice` нет |
| `LimitRTPRIO` | ❌ | нет; и `sched_setscheduler`/`SCHED_FIFO` в коде отсутствуют (`grep` → 0) |
| `LimitMEMLOCK` / `mlockall` | ❌ | нет ни в юните, ни в коде |
| sysctl-профиль | ❌ | `deploy/` содержит только `README.md`, `systemd/`, `vks.env.example`; `sysctl.d` нет |
| Порт 40000 / ICE / RTP | 🟡 | `media_listen_addr = 0.0.0.0:40000` (`configs/*.toml`) — один mux-UDP; реальный WebRTC — эфемерные/43000–43100 (`webrtc_runtime.rs:59-78`), т.е. «40000/ICE/RTP» в заявке не совпадает с фактической картой портов |
| SO_RCVBUF/SO_SNDBUF на медиа-сокете | ❌ | `udp_loop.rs:109-117`: `tokio::UdpSocket::bind(addr)` без `socket2`; `webrtc_room.rs:135,145` — `std::net::UdpSocket::bind` без буферов. Без этого `net.core.rmem_max` бесполезен |
| `IP_PKTINFO`/`SO_REUSEPORT` (мультипоток) | ❌ | нет |
| `AmbientCapabilities=CAP_NET_BIND_SERVICE` | ❌ | не нужно при 8080/40000 (>1024), но нужно `CAP_SYS_NICE` для будущего RTPRIO |

### 2.7 CI / supply chain

| Элемент | Статус | Доказательство |
|---|---|---|
| fmt/check/clippy/test/doc/build/deny | ✅ | `.github/workflows/ci.yml:24-302` |
| `--all-features --all-targets --locked` | ✅ | `ci.yml:53,71` |
| PG-интеграция (сервис `postgres:16`) | ✅ | `ci.yml:112-144`; тест гейтится `VKS_STORAGE_PG_URL` (`postgres.rs:374-380`) |
| Тесты NATS/OTLP/QUIC | ❌ | нет сервисов, нет джобов; появятся только с крейтами |
| `[bans]` (дубликаты версий) | ❌ | `deny.toml` содержит `[graph]/[licenses]/[advisories]/[sources]`, секции `[bans]` нет → `prost 0.13+0.14`, `sha2 0.10+0.11` пройдут незамеченными |
| MSRV-гейт для новых крейтов | ❌ | `rust-version = "1.94"` только в workspace; CI не проверяет MSRV зависимостей |

---

## §3 Дефекты

### P0 — потеря данных / ложная функциональность

**P0-1. Записи не сохраняются на реальной БД и всегда пусты после рестарта.**
`Storage::upsert_record/list_records` — дефолтные no-op (`vks-core/src/ports/storage.rs:294-311`). Их переопределяет только `MemoryStorage` (`core_adapter.rs:205-223`); `SqliteStorage` (242–…) и `PostgresStorage` (…) наследуют пустышки. `sync_from_storage` записей не читает (`state.rs:600-640`). Следствия: `POST`-старта записи нет вообще, а если бы запись создалась — `/api/v3/videorecords` на SQLite/PG вернёт `{count:0}` после рестарта; `DELETE`/download отсутствуют. Это не «недоделка рекордера», а дыра в контракте `Storage`: no-op-дефолты маскируют неполную реализацию (fail-open для данных).
*Доказательство:* `grep -n "fn upsert_record" crates/vks-storage/src/core_adapter.rs` → только 205 (в impl `MemoryStorage`).

**P0-2. `Msg::id` не уникален между узлами — дедуп шины «съест» межнодовые сообщения.**
`Msg::new` (`bus.rs:35-41`) генерирует `msg-{seq}` из `AtomicU64`, начиная с 0 в каждом процессе. При переходе на NATS (где dedup идёт по `message_id`/`Nats-Msg-Id`) узлы A и B оба пошлют `msg-0..msg-N` — получатели отбросят чужие сообщения как дубликаты (ADR-0019), а после рестарта счётчик обнулится и JetStream-дедуп окна 2 мин отбросит новые сообщения. Миграция на NATS **без** замены id на UUIDv7 превратит шину в источник тихой потери lease/room-migrated событий.

**P0-3. «Воспроизводимый MP4» невалиден: нет `avcC`, payload не length-prefixed.**
`build_video_trak` пишет `stsd/avc1` с фиксированными полями и **без** обязательного дочернего `avcC` (`container.rs:313-332`); `write_video_frame` принимает произвольные байты (`:84-102`), а ферма отдаёт Annex-B `AccessUnit` (`farm.rs:225-258`). Тест проверяет только сигнатуры `ftyp/moov/mdat/stbl` (`container.rs:576-621`) — то есть подтверждает наличие боксов, но не декодируемость. Заявление «recording → playable MP4» (M5.5) не доказано.

### P1 — безопасность / корректность / блокирующий I/O

**P1-1. Хардкод ICE-креденшелов и фиктивный fingerprint в прод-конфиге.**
`main.rs:356-371`: `ufrag="sfuufrag"`, `pwd="sfupwd"`, `fingerprint="sha-256 FF:00:11"` для всех инсталляций. При `VKS_WEBRTC_TRANSPORT=0` (дефолт) клиент получает SDP с недостоверными DTLS-параметрами; при включённом транспорте str0m подменяет answer (`lib.rs:882-907`), но legacy-путь остаётся с общими для всех секретами. Нужны `Uuid::new_v4()`-производные ufrag/pwd на сессию и fingerprint из реального сертификата, либо явный отказ от legacy-пути.

**P1-2. Argon2id с m=19456 — новый вектор memory-DoS без ограничителя параллелизма.**
Сейчас PBKDF2 не потребляет памяти; 19 МиБ × число одновременных `spawn_blocking` (дефолт tokio — до 512 потоков) = до ~9,7 ГиБ RSS при «распылении» логинов с разных IP/логинов (per-(login,ip) lockout, `auth.rs:261`, не мешает). Нужен `Semaphore` (2–4) вокруг verify/hash и лимит на размер пароля (сейчас не ограничен, `accounts.rs:122-132`).

**P1-3. Блокирующий PBKDF2 в async-хендлере.**
`accounts.rs:139` — `db.register()` (100k итераций, десятки мс) в async-задаче; рядом есть готовый `register_async` (`local_db.rs:252-268`). На 4 воркерах tokio несколько параллельных «создать учётку» заметно подвешивают сигнализацию.

**P1-4. `/ws` без ограничения числа соединений на IP/узел.**
Есть лимит сообщений на соединение (`lib.rs:716-726`) и размер кадра, но не число сокетов: 10k открытых `/ws` (каждое с подпиской на `room_hub` broadcast и таймерами) — штатный способ выесть память. В legacy `/api/v3/ws` лимит 100 conn/s на IP есть (`api_v3/ws.rs:36,62-72`) — в новом его нет.

**P1-5. Справочник: «последний писатель выигрывает» + split source of truth.**
Каждая мутация пишет весь документ (`resources.rs:241-269`); при T10b (directory в SQL, остальное в документе) любой рестарт/старый снимок документа воскресит удалённые directory-записи, если поле `directory` останется в `ResourceState` (`resources.rs:46`). План T10b обязан включать удаление поля и тест на «воскрешение».

**P1-6. `stop_recording` (sync) запускает `tokio::spawn` из не-async контекста и теряет ошибку персистентности.**
`recorder.rs:192-199`: `tokio::spawn(async move { let _ = st_clone.upsert_record(...) })` — ошибка записи игнорируется, а вызов из теста/не-runtime контекста паникует (`spawn` вне runtime). Async-вариант (`:233-243`) сделан правильно; sync-вариант — дубль, который надо удалить.

### P2 — гигиена/производительность/наблюдаемость

**P2-1.** `state_disk_free_gb` вызывает `std::process::Command::new("df")` в async-хендлере (`observability.rs:749-758`) — блокирующий fork/exec; и `df` может отсутствовать в `RestrictAddressFamilies`-окружении. Заменить на `libc::statvfs` (libc уже в lock 0.2.190) или `nix`.

**P2-2.** `videorecords`-ответ хардкодит `resolution: [1280,720]`, `bitrate: 0`, `profile: null` (`observability.rs:646-665`) — UI «Медиафайлы» показывает вымышленные параметры.

**P2-3.** `estimated_time` = 3600 с при свободном месте >1 ГБ (`observability.rs:740-747`) — не зависит ни от профиля, ни от числа параллельных записей.

**P2-4.** `HandlerConfig::new` читает `VKS_SFU_VIDEO_BITRATE_KBPS` из env (`handler.rs:43-45,79`) — библиотека читает окружение в конструкторе; конфиг-источников уже 50+ (`grep -rn 'std::env::var("VKS_'` → 50 вхождений в 14 крейтах), а `Config` (`vks-common/src/config.rs:14-55`) знает только 6 переменных.

**P2-5.** `LocalDb::authenticate` ищет `u.username == username` (регистрозависимо, `local_db.rs:94`), а `register` гарантирует уникальность регистронезависимо (`:73-76`) → логин `Admin` при пароле от `admin` даёт `NotFound` вместо `AuthFailed` (разный тип ошибки, но наружу маппится в 401 — `auth.rs:275`). Мелочь, но тест `authenticate_is_case_insensitive` отсутствует.

**P2-6.** `ct_eq` по hex-строкам (`local_db.rs:100,236`) — при переходе на PHC-строки переменной длины сравнение станет неконстантным по длине (раннее возвращение). Сравнивать надо 32 байта `hash_password_into`.

**P2-7.** Дублирование in-memory хранилищ записей: `AppState.records` (`state.rs:230`) и `MemoryStore` (`vks-server/src/recorder.rs:24`) + `MemoryStorage.core_records` — три копии одного списка.

**P2-8.** `ClusterManager` — три почти идентичных конструктора (`cluster.rs:24-42`, `45-62`, `65-79`); heartbeat-цикл в `main.rs:441-451` не останавливается при shutdown (утечка задачи, безвредно, но шумно).

**P2-9.** `deny.toml` без `[bans]` — дубликаты (prost 0.13/0.14, sha2 0.10/0.11) не ловятся; при добавлении OTLP и wtransport их станет больше.

**P2-10.** `MockWs`/`MockWt` — 205 строк почти побайтового дубля, не используемого прод-кодом (§6).

---

## §4 Разбор примеров заказчика

### 4.1 WebTransport (`wtransport`)

**Актуальная версия на crates.io — `wtransport 0.7.2` (2026-10-15, MSRV 1.88).** Пример из заявки («`Endpoint::server` / `accept_bi`») соответствует реальности **лишь частично**:

```rust
// 0.7.2 — так:
let identity = Identity::self_signed(["localhost"])?;           // Result<_, InvalidSan>
let config = ServerConfig::builder()
    .with_bind_default(4433)                                    // не 8443 в примере — ок, но порт не из конфига
    .with_identity(identity)
    .build();
let endpoint = Endpoint::server(config)?;                       // 0.7: СИНХРОННЫЙ Result, НЕ .await
loop {
    let incoming  = endpoint.accept().await;                    // IncomingSession
    let request   = incoming.await?;                            // IncomingRequest (SessionRequest)
    let connection = request.accept().await?;                   // Connection
    let (send, recv) = connection.accept_bi().await?;           // ← accept_bi есть, но на Connection
}
```

Что не так в типовом примере заказчика:
1. `Endpoint::server(config).await?` — **в 0.5** `server()` был async; в 0.6/0.7 — sync `Result`. Код, скопированный из старых примеров, не скомпилируется (E0599 «no method `await` on Result»).
2. `accept_bi()` — метод `Connection`, а не `Endpoint`/`IncomingSession`; пропуск двух `await` (`incoming` → `request` → `connection`) — самая частая ошибка.
3. `Identity::self_signed` требует SAN, а браузер **не примет** самоподписанный сертификат обычным путём: для Chromium нужен либо доверенный CA, либо JS-опция `serverCertificateHashes` (сертификат ≤14 дней, ECDSA). wtransport это поддерживает на клиенте: `ClientConfig::builder().with_server_certificate_hashes([Sha256Digest])` (docs.rs 0.7.2, `ClientConfigBuilder`), а для нативного SDK — `with_no_cert_validation()`.
4. Нет лимитов: `max_idle_timeout`, `keep_alive_interval` (`ServerConfigBuilder`), `max_datagram_size`/`receive_datagram` + контроль размера (SDP-answer ~1–3 КБ **не влезает** в датаграмму 1200 B → SDP только в bi-stream, датаграммы — ICE/пинг).
5. `Endpoint::server` в 0.7 сам по себе не аутентифицирует: токен должен проверяться **до** `request.accept()` (иначе любой UDP-пир создаёт Connection и держит память). Это и есть DoS-поверхность QUIC: `open_connections()` (`Endpoint`) надо ограничивать.

Зависимости: `wtransport 0.7.2` тянет `quinn 0.11.12+`, `rustls 0.23`, `rcgen 0.14.5` (в lock уже 0.14.10 — без дубля), `socket2 ^0.6` (в lock 0.6.5 — без дубля), `sha2 ^0.11` (**дубль** с 0.10.9), `x509-parser 0.18` (+`asn1-rs`, `der-parser`, `nom`, `oid-registry`). TLS-бэкенд: `default = ["self-signed","ring"]` → **ring**, без `aws-lc-rs`/cmake. Лицензии (MIT/Apache/ISC) проходят `deny.toml`. MSRV 1.88 ≤ 1.94 ✓.

### 4.2 Argon2 verify + fallback

Типовой пример заказчика:

```rust
let parsed = PasswordHash::new(&stored_hash)?;      // ← падает на legacy-строках
Argon2::default().verify_password(pwd.as_bytes(), &parsed)?;
```

**Здесь ровно тот баг, о котором предупреждает задание:** в БД лежит `password_hash_hex` (64 hex-символа, `0004_user_credentials.sql:5`) + `salt_hex`; `PasswordHash::new("a3f1…")` вернёт `Error::Password` → **все существующие пользователи мгновенно теряют доступ** (fail-closed на 100%), а «тихая» альтернатива (`unwrap_or(false)`) — fail-open. Правильно:

```rust
const ARGON2_M_COST: u32 = 19_456; // 19 MiB, OWASP Argon2id
let params = Params::new(ARGON2_M_COST, 2, 1, Some(32))?;      // m, t, p, out_len
let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
// 1) новый формат: PHC-строка в том же TEXT-столбце
if stored.starts_with("$argon2id$") {
    return argon2.verify_password(pwd.as_bytes(), &PasswordHash::new(stored)?);
}
// 2) legacy: pbkdf2_hmac::<Sha256>(pwd, salt_hex_decode, 100_000, &mut [u8;32]);
//    сравнить 32 байта (subtle::ConstantTimeEq), затем ЛЕНИВО перехешировать:
//    if ok { let phc = argon2.hash_password(pwd.as_bytes(), &salt_bytes)?.to_string();
//             persist_credentials(phc, /*salt*/""); }  // generation/пароль-маркер НЕ трогать
```

Нюансы, которых нет в примере заказчика:
- **Версия API.** `argon2 0.6.0` (MSRV 1.85) — это переписанный `password-hash 0.6`/`phc 0.6` (в 0.6.0 `password-hash` был yanked, актуален 0.6.1); примеры из интернета написаны под `0.5` и на 0.6 не компилируются. **Рекомендация: `argon2 = "0.5.3"`** (MSRV 1.65) — стабильный API, PHC на выходе, транзитивные `password-hash 0.5`, `blake2 0.10`, `base64ct` (уже 1.8.3), `cpufeatures 0.2` (уже), `zeroize` (уже), `subtle` (уже); **ни одного нового поколения `rand_core`** (0.6.4 уже в lock). Argon2 0.6 потребовал бы `phc 0.6`, `kdf 0.1`, `blake2 0.11` — дубли без выгоды.
- **Маркер формата.** Нужен self-describing префикс в `password_hash_hex` (имя столбца становится неточным — либо оставить и задокументировать, либо `ALTER TABLE ... RENAME COLUMN` в новой миграции; старый бинарь на той же БД читать перестанет — это допустимо по политике «не пересекать версии на одной БД», `deploy/README.md`).
- **Параметры.** `m=19456` — это KiB (19 MiB), а не байты; `t=2`, `p=1`, `out=32`. `Params::new(19456,2,1,None)` даёт дефолтную длину 32 — ок, но лучше `Some(32)` явно.
- **Рехеш при логине** не должен менять `password_changed_at`/`generation` (иначе инвалидация живых токенов при каждом входе).
- **Не надо** добавлять `algorithm`/`iterations`/`memory` колонки: PHC уже содержит все параметры, а лишние колонки — второй источник истины.

### 4.3 SQL-миграция T10b

Типовой пример заказчика (`directory_roles`/`directory_addresses` с `UUID`, FK и `ON DELETE CASCADE`) ломается о четыре факта репозитория:

1. **Две конвенции id.** `0001_initial.sql:8,15,22,28,35,43` — `UUID PRIMARY KEY`; `0002–0004` — `TEXT PRIMARY KEY`. На SQLite `uuid::Uuid` биндится как **16-байтовый BLOB** (sqlx-sqlite 0.8.6 `src/types/uuid.rs`: `Encode → SqliteArgumentValue::Blob`, `Type::compatible = Blob|Text`), а `TEXT`-таблицы — как строка. Смешивать нельзя: `WHERE id = $1` с `Uuid` не найдёт TEXT-строку и наоборот. **Директива: все новые таблицы — `TEXT`, значения — hyphenated UUID/строковые id; никаких `UUID` в миграциях.**
2. **FK-цель неоднозначна.** Есть `users` (`0001`, UUID) и `core_users` (`0003`, TEXT, «живая»). FK на `users` в SQLite будет ссылаться на BLOB-ключи, которых для directory-пользователей не существует (`user_id` в документе — строковый UUID из `UserId`), а `ON DELETE CASCADE` при удалении через `Users::delete_async` (`core_users`) **не сработает** → сироты. FK на `core_users(id)` — технически верно (TEXT), но: (а) удаление идёт в порядке LocalDb → core_users → документ (`directory.rs:902-939`), и добавление FK потребует переписать порядок на «сначала directory, потом пользователь»; (б) SQLite по умолчанию **не включает** `PRAGMA foreign_keys=ON` (sqlx тоже не включает) → FK не работают, пока не выставлено в `SqliteConnectOptions`. **Директива: FK только для чистых дочерних таблиц (`user_addresses`, `group_members`, `role_permissions`) на `TEXT`-родителей, и только вместе с `PRAGMA foreign_keys=ON` в `SqliteStorage::init_options`; для `directory_users → core_users` FK не добавлять (логическая ссылка + уборка в коде).**
3. **Идемпотентность/rollback.** `IF NOT EXISTS` обязателен (стиль 0001–0005 ✓). Down-миграций sqlx не поддерживает — принятая политика forward-only + бэкап (`vks-db-dedup --apply` делает `VACUUM INTO`, `deploy/README.md`). **Не надо** изобретать `-- +migrate Down`: откат = восстановление бэкапа + новая миграция с `DROP TABLE IF EXISTS` для dev.
4. **Перенос данных.** Соблазн сделать `INSERT ... SELECT json_extract(document_json,'$.directory.users')` — работает и в PG (`::jsonb`), и в SQLite (JSON1 в bundled sqlite есть), но (а) дублирует парсер `ResourceState` в двух диалектах, (б) не переживает битые записи (а `deserialize_addresses` терпим к ним, `resources.rs:57-69`), (в) требует двух разных SQL. **Директива: миграция создаёт только схему; перенос — Rust-функция `migrate_directory_from_document()` под тем же `flock`, что `vks-db-dedup` (транзакция, идемпотентность, отчёт), вызываемая один раз на старте при `directory_schema_version < 1`.** Это на ~60 строк больше SQL и на порядок меньше риска.
5. **Типы/индексы.** `BIGINT` для ms (как `0003:13`), `BOOLEAN`, `TEXT` для JSON-полей (`payload` адреса). Уникальность canonical URI — не `CREATE UNIQUE INDEX ON (lower(uri))` (не портируемо и не учитывает `canonical_uri()` из `vks_common::uri`), а **хранимый столбец `uri_canonical TEXT NOT NULL` + `UNIQUE`**, заполняемый в коде тем же `normalize_uri` (`dedup.rs:61-64`); в SQLite — обычный (не generated) столбец, чтобы избежать различий версий.
6. **Согласованность с документом.** После переноса поле `directory` обязано исчезнуть из `ResourceState` (`resources.rs:46`), иначе `persist()` снова запишет его и «воскресит» удалённые строки (P1-5). Тест: `old_document_with_directory_does_not_resurrect_rows`.

### 4.4 NATS

**Крейт/версия:** `async-nats 0.51.0` (2026, MSRV **1.89**), либо `0.50.0`. Оба **безальтернативно** тянут `tokio-rustls 0.26`/`rustls-webpki`/`rustls-native-certs` (TLS-стек появится в графе в любом случае — сегодня его нет, `deny.toml` прямо это фиксирует) и `rand 0.10` (в lock 0.10.3 ✓), `regex`, `portable-atomic`, `tryhard`, `time`. `default` включает `jetstream`, `kv`, `object-store`, `websockets` — для нас лишние ~40 транзитивных крейтов.

Что не так в типовом примере:
- `let js = jetstream::new(client)` + `js.publish()` — JetStream требует **stream + durable consumer** и даёт at-least-once; наша модель (`Bus`, `ownership`, `heartbeat`) — fire-and-forget pub/sub с dedup по id и DLQ. Для lease/heartbeat JetStream не нужен (и вреден: лишняя запись на диск/кворум для 5-секундных пингов). **Директива: core NATS для `vks.cluster.hb.*`/`lease.*`; JetStream — только для `vks.cluster.room.migrated` (нужна доставка после падения узла), если вообще нужна.**
- **Offline-поведение.** `async_nats::connect()` без сервера возвращает ошибку немедленно; reconnect-стратегия (`ConnectOptions::retry_on_initial_connect`) и `Event::Disconnected/Reconnected` обязательны, иначе single-node прод без NATS перестанет стартовать. **Директива: `ClusterBus` — трейт с двумя impl: `LocalBus` (текущий `Bus`, дефолт) и `NatsBus` (feature `nats`); `VKS_NATS_URL` пустой → `LocalBus`, ошибка connect → `LocalBus` + `warn!`, никогда не `exit(78)`.**
- **`message_id`** — см. P0-2: только UUIDv7 (`uuid` с `v7` уже в workspace).
- **Тестируемость.** NATS-сервера нет ни в `deploy/`, ни в CI. Без `services: nats` джоба крейт будет компилироваться, но не проверяться (`--all-features` в `check/clippy` — компиляция, не поведение). **Директива: `test-nats` джоб по образцу `test-postgres` (`ci.yml:112-144`) с `nats:2.10-alpine`, `continue-on-error: true` до стабилизации.**
- **Лицензия/версия:** Apache-2.0 ✓; MSRV 1.89 > 1.88 wtransport, < 1.94 ✓. Дублей `rand` не создаёт (0.10.3 уже есть), но создаёт `rustls` — общий с wtransport, т.е. **добавлять NATS и WT лучше одной волной**, чтобы TLS-стек появился один раз.

### 4.5 OTLP

**Версии:** `opentelemetry-otlp 0.33.1` + `opentelemetry 0.33.1` + `tracing-opentelemetry 0.34.0` (MSRV 1.75). `tracing-opentelemetry 0.34` требует `tracing-subscriber ^0.3.22` (в lock 0.3.23 ✓) и `opentelemetry ^0.33` — совместимо. НО:
- `grpc-tonic` тянет `tonic 0.14` + **`prost 0.14`** (в workspace `prost 0.13.5` для `vks-proto`) → неизбежный дубль `prost` (0.13 + 0.14). Это допустимо (разные semver-ветки), цена — время сборки; `deny.toml` без `[bans]` промолчит.
- `http-proto` тоже требует `prost 0.14` + `opentelemetry-http` (reqwest/hyper) — не лучше; `reqwest-blocking-client` в async-сервере **нельзя** (блокирующий клиент в tokio).
- Не хватает `TracerProvider::shutdown()` при SIGTERM (`main.rs:476-484`) — иначе последние спаны (включая причину падения) теряются. `BatchSpanProcessor` + `shutdown` обязательны.
- `vks_common::tracing::init` (`tracing.rs:26-41`) собирает registry **без** otel-слоя → нужен `Option<Layer>` (feature/`VKS_OTLP_ENDPOINT`), иначе «OTLP-экспорт» останется мёртвым кодом. `RUST_LOG`/`EnvFilter` уже есть, отдельный `OTEL_*` фильтр не нужен.
- **Не надо** тащить OTLP-метрики/логи (только traces): Prometheus (`/metrics`) уже закрывает метрики, а OTLP-logs дублируют JSON-логи. Это `features = ["trace","grpc-tonic","tls-ring"]`, `default-features = false`.
- Коллектора в `deploy/` нет — экспорт по умолчанию `off`, адрес только из env; в CI — без коллектора (юнит-тест на «слой собирается и shutdown не паникует»).

### 4.6 sysctl + cgroup v2 (L0)

Типовой пример заказчика (sysctl-профиль + `Slice/CPUWeight/MemoryHigh/MemoryMax/LimitNOFILE/LimitRTPRIO`) **сам по себе не даст эффекта**:
1. `LimitRTPRIO` не выдаёт RT-приоритет — он лишь разрешает `sched_setscheduler(SCHED_FIFO)`; в коде **нет** ни одного такого вызова (`grep -rn 'sched_setscheduler|SCHED_FIFO|setpriority'` → 0). Нужен либо реальный RT-поток микшера/таймера, либо `LimitRTPRIO` не нужен вообще. Плюс требуется `AmbientCapabilities=CAP_SYS_NICE` (или `CapabilityBoundingSet=CAP_SYS_NICE`) — сейчас `CapabilityBoundingSet=` пуст.
2. `net.core.rmem_max` бесполезен без `SO_RCVBUF` на сокете: `udp_loop.rs:109-117` и `webrtc_room.rs:135,145` биндят сокет без буферов. `socket2 0.6.5` **уже в lock** (через `dio`/`mio`-стек) → правка без новой зависимости.
3. `Slice=` в юните не создаёт cgroup-родителя: нужен `vks.slice` (drop-in `systemctl set-property`) или `Slice=vks.slice` + отдельный unit; `CPUWeight` работает только если родитель имеет cpu-контроллер и weight > 0 у конкурентов.
4. `MemoryHigh/Max` надо считать от реального потребления: 19 МиБ × параллельные Argon2 (P1-2) + буферы записи (`RecorderManager.containers` — безлимитный `HashMap<String, Vec<u8>>`, `recorder.rs:26`) + video farm. `MemoryMax` без `MemoryHigh` даёт OOM-kill вместо троттлинга; ставить `MemoryHigh=…`, `MemoryMax=…+25%`.
5. Порты: «40000/ICE/RTP» — это `media_listen_addr` (один mux-порт для legacy/SFU UDP, `configs/*.toml`), а реальный WebRTC — `VKS_WEBRTC_PORT_MIN..MAX` (43000–43100, `webrtc_runtime.rs:59-78`) или эфемерные. `net.ipv4.ip_local_reserved_ports` должен резервировать **оба** диапазона, иначе ядро отдаст 40000/43000 эфемерному соединению и `bind` упадёт.
6. `LimitNOFILE=65536` есть — не трогать (достаточно для 256 пиров × RTP).

---

## §5 План по пунктам (файлы/строки/тесты/версии)

Порядок — по зависимости, а не по номерам заявки: **2 → 3 → 4 → 1 → 6 → 5**. Пункты 1/5/6 без 2/3 не проверяемы.

### Шаг 1. Запись: персистентность + профиль (пункт 2)

**1.1. Закрыть дыру `Storage` (P0-1).**
- `crates/vks-core/src/ports/storage.rs:294-311`: убрать дефолтные реализации `upsert_record/list_records/get_record/delete_record` → сделать обязательными. Компилятор перечислит все неполные impl'ы (сейчас их 2: SQLite, PG).
- `crates/vks-storage/migrations/0006_video_records.sql`: `id TEXT PRIMARY KEY, room_id TEXT NOT NULL, started_ms BIGINT NOT NULL, duration_ms BIGINT NOT NULL, size_bytes BIGINT NOT NULL, width INTEGER NOT NULL, height INTEGER NOT NULL, fps INTEGER NOT NULL, bitrate_bps BIGINT NOT NULL, profile TEXT NOT NULL` + `CREATE INDEX IF NOT EXISTS idx_records_room ON video_records(room_id, started_ms)`.
- `core_adapter.rs`: реализовать 4 метода для `SqliteStorage` (после `impl` с `configuration_documents`, ~строка 250) и `PostgresStorage` (по аналогии, `$n`-плейсхолдеры, `ON CONFLICT (id) DO UPDATE` — как в `upsert_room`, `:267-270`).
- `crates/vks-admin/src/state.rs:600-640`: добавить гидратацию `state.records` из `storage.list_records(None)`.
- Тесты (`crates/vks-storage/tests/records.rs`): `records_survive_reopen_sqlite`, `records_upsert_is_idempotent`, `records_list_filters_by_room`; PG-вариант — под `VKS_STORAGE_PG_URL` (как `postgres.rs:374-380`), т.е. автоматически попадёт в `test-postgres` джоб.

**1.2. `RecorderProfile` (W·H·fps·bpp).**
- `crates/vks-recorder/src/profile.rs` (новый, ~90 строк):
  ```rust
  pub struct RecorderProfile { pub width: u16, pub height: u16, pub fps: u8,
                               pub bpp: f32, pub audio_kbps: u16 }
  impl Default for RecorderProfile { /* 1280×720@30, 0.085, 64 kbps */ }
  impl RecorderProfile {
      pub const MIN_W: u16 = 160; pub const MAX_W: u16 = 3840;
      pub const MIN_FPS: u8 = 1;  pub const MAX_FPS: u8 = 60;
      pub const MIN_BPP: f32 = 0.03; pub const MAX_BPP: f32 = 0.20;
      pub fn clamped(self) -> Self;            // чётные W/H, диапазоны
      pub fn video_kbps(&self) -> u32;         // W*H*fps*bpp/1000, clamp 64..20_000
      pub fn total_kbps(&self) -> u32;         // video + audio
      pub fn estimated_bytes(&self, secs: u64) -> u64;
      pub fn container(&self) -> ContainerConfig;
  }
  ```
  Единый источник и для записи, и для `/api/v3/videorecords`, и для `estimated_time` (сейчас три константы: `container.rs:40-51`, `observability.rs:62`, `:146`).
- Тесты (`profile.rs` inline): `default_profile_is_2_35_mbps` (1280·720·30·0.085/1000 ≈ 2350), `1080p_profile_is_5_29_mbps`, `odd_width_rounds_even`, `bpp_and_fps_clamp`, `estimated_bytes_matches_bitrate`.
- `vks-recorder/src/lib.rs:39-46`: `pub use profile::RecorderProfile;` (не плодить `ContainerConfig::default` вне профиля).

**1.3. Запуск записи из API и данные.**
- `vks-admin/src/api_v3/mod.rs:179-184`: добавить `POST /api/v3/conferences/{id}/videorecords` (start, Moderator+, тело `{profile?: {width,height,fps,bpp}}`) → `RecorderManager::start_recording(state, room, mode)`; `DELETE /api/v3/videorecords/{id}` — оставить 501 (файлов ещё нет), но `POST .../stop` не нужен: stop по `LeaveRoom`/`session stop`.
- `vks-server/src/lib.rs:148` (сейчас `recorder` в `WsContext` не читается): при успешном `JoinRoomResult` с `is_recorder` и на `LeaveRoom` вызывать `recorder.start/stop_recording_async` из `handle_ws_socket` — минимальная точка интеграции, без нового протокола.
- Данные: ферма отдаёт `AccessUnit` только подписчикам существующей группы (`farm.rs:492-538`); запись «без зрителя» невозможна. Минимальный tap — рекордер как скрытый viewer: `VideoFarm::viewer(key)` с `GroupSpec` из `RecorderProfile`, `AccessUnit → RecordingSession::write_video_sample(au.data(), au.pts_us()/1000, au.is_idr())`. Реализация — `vks-server/src/recorder_sink.rs` (~120 строк, `tokio::spawn` на комнату, `broadcast::Receiver<Arc<AccessUnit>>`).
- MP4: `container.rs:313-332` — добавить `avcC` (из `parameter_sets()`, `farm.rs:254-257`), конвертировать Annex-B → 4-байтовый length-prefix при записи; тест `finalized_mp4_has_avcc_and_length_prefixed_samples` + внешний гейт `ffprobe -v error -show_streams` на фикстуре (в CI — только если `ffprobe` есть, иначе `eprintln!`-skip, как в PG-тесте).
- Тест `record_without_viewers_writes_bytes` (soak 3 с, `size_bytes > 0`).
- `stop_recording` (sync, `recorder.rs:227-230`) + `containers` (`:26,53-60`) — удалить (см. §6).

**1.4. `/api/v3/videorecords` из профиля.**
- `observability.rs:646-665`: `record_json` берёт `width/height/fps/bitrate_bps/profile` из `RecordEntry` (добавить поля в `vks-core/src/ports/storage.rs:119-130` и в ре-экспорт `RecordEntry` (`state.rs:97`)).
- `observability.rs:740-758`: `estimated_time = free_bytes(statvfs) / (profile.total_kbps()*1000/8)`; заменить `df` на `libc::statvfs` (`libc` уже в `vks-storage/Cargo.toml`, добавить в `vks-admin` — версия из workspace `libc = "0.2"`).
- Тесты: `videorecords_report_profile_and_resolution`, `estimated_time_uses_bitrate_not_constant` (мок `free_bytes` через `cfg(test)`-параметр).

### Шаг 2. T10b: нормализация справочника (пункт 3)

**2.1. Схема.** `crates/vks-storage/migrations/0007_directory_normalized.sql` (~70 строк):
`directory_roles(id TEXT PK, display_name TEXT NOT NULL, description TEXT NOT NULL, permissions_json TEXT NOT NULL, undeletable BOOLEAN NOT NULL, created_at BIGINT NOT NULL)`;
`directory_groups(id TEXT PK, display_name TEXT NOT NULL, creator_id TEXT, created_at BIGINT NOT NULL)`;
`directory_profiles(user_id TEXT PK, login TEXT, display_name TEXT, email TEXT, phone TEXT, params_json TEXT NOT NULL, updated_at BIGINT NOT NULL)` (FK **нет** — см. §4.3);
`directory_group_members(group_id TEXT NOT NULL REFERENCES directory_groups(id) ON DELETE CASCADE, user_id TEXT NOT NULL, PRIMARY KEY(group_id,user_id))`;
`directory_user_roles(user_id TEXT NOT NULL, role_id TEXT NOT NULL REFERENCES directory_roles(id) ON DELETE RESTRICT, PRIMARY KEY(user_id,role_id))`;
`directory_addresses(id TEXT PK, owner_id TEXT, uri TEXT NOT NULL, uri_canonical TEXT NOT NULL, display_name TEXT NOT NULL, payload_json TEXT NOT NULL, created_at BIGINT NOT NULL)`;
`CREATE UNIQUE INDEX IF NOT EXISTS ux_address_owner_canonical ON directory_addresses(COALESCE(owner_id,''), uri_canonical)`;
`CREATE UNIQUE INDEX IF NOT EXISTS ux_address_global_canonical ON directory_addresses(uri_canonical) WHERE owner_id IS NULL` — **нельзя**: partial index есть в SQLite и PG, но `COALESCE`-выражение в индексе PG требует скобок; проще **два индекса**: `(owner_id, uri_canonical)` UNIQUE + `uri_canonical` UNIQUE `WHERE owner_id IS NULL` (portable).
Никаких `UUID`-типов, никаких `DEFAULT now()` (ms считает код), все `IF NOT EXISTS`.

**2.2. Перенос.** `crates/vks-storage/src/directory_migrate.rs` (~180 строк): читает `configuration_documents['pnm-definitions-v1']`, парсит `directory`+`addresses` структурами **admin**-крейта? Нет — `vks-storage` не должен зависеть от `vks-admin` (цикл). Поэтому перенос жить в `vks-admin/src/api_v3/directory_migrate.rs` и вызываться из `main.rs` после `sync_from_storage` под `DbLock` (уже удерживается, `main.rs:96-102`), с `directory_schema_version` в новой таблице `schema_state(key TEXT PK, version BIGINT)`. Транзакция, `INSERT ... ON CONFLICT DO NOTHING`, отчёт в лог, идемпотентность; `normalize_uri` — существующий (`dedup.rs:61-64`).

**2.3. Чтение/запись.** `Resources`/`DirectoryState` (`resources.rs:46`) → репозиторий `DirectoryRepo` (trait в `vks-admin`, impl поверх `Storage`-методов `list/upsert/delete_directory_*`). `ResourceState.directory` **удалить** (serde `#[serde(default, skip_serializing)]` на один релиз для чтения старых документов, затем убрать поле). `persist()` (`resources.rs:241-269`) перестаёт писать directory.

**2.4. Согласованность SQLite/PG.** `SqliteConnectOptions::foreign_keys(true)` в `sqlite.rs:86-91`; типы — только `TEXT/INTEGER/BIGINT/BOOLEAN`; в PG использовать `TEXT` (не `UUID`), чтобы один SQL работал в обоих; проверить `sqlx::migrate!` на обеих БД в `test-postgres`.

**2.5. Тесты** (`vks-admin/src/api_v3/directory_migrate_tests.rs`): `migrates_roles_groups_accounts_addresses`, `rerun_is_noop` (идемпотентность), `duplicate_canonical_uri_merged_and_links_rewritten`, `broken_address_record_skipped_not_fatal`, `old_document_with_directory_does_not_resurrect_rows`, `sqlite_and_pg_produce_same_rows` (под `VKS_STORAGE_PG_URL`), `delete_user_cascades_children_only`.
**Не надо:** FK на `core_users`, `ON DELETE CASCADE` от пользователя (P1-5), `JSONB`, `TIMESTAMPTZ`, down-миграции, SQL-парсинг документа.

### Шаг 3. Argon2id + fallback (пункт 4)

- `crates/vks-auth/Cargo.toml`: `argon2 = "0.5.3"`, `zeroize = "1"` (уже транзитивно), `rand = "0.8"` (уже).
- `crates/vks-auth/src/local_db.rs`:
  - `const ARGON2_M_COST: u32 = 19_456; ARGON2_T_COST: u32 = 2; ARGON2_P_COST: u32 = 1;`
  - `fn hash_password_argon2(pwd: &str) -> (String /*PHC*/, String /*""*/)`; `fn verify(stored: &str, salt_hex: &str, pwd: &str) -> Result<(), LocalDbError>` — префикс `$argon2id$` → Argon2; иначе PBKDF2 (`:314-319`) + **ленивый рехеш** (вызывающий `authenticate` возвращает флаг `needs_rehash`, `set_password`/persist делают запись).
  - `authenticate`/`authenticate_async` (`:91-113`, `:225-250`): сравнивать **32 байта** (`ct_eq(&derived, &expected)`), не hex.
  - `register`/`register_async`/`set_password*` → Argon2.
  - `restore_user` (`:271-293`) — без изменений (PHC-строка кладётся в `password_hash_hex`, `salt_hex` пустой для Argon2).
- `crates/vks-auth/src/lib.rs` или `provider.rs`: `static HASH_GATE: Semaphore` (2–4) вокруг любых `spawn_blocking(hash/verify)` — защита P1-2. Плюс лимит длины пароля (≤128 символов) в `accounts.rs:122-132` и `auth.rs` login.
- `accounts.rs:139`: `db.register(...)` → `db.register_async(...).await`.
- Миграция формата — **не нужна**: строка остаётся в `user_credentials.password_hash_hex` (`0004:5`), формат self-describing. Опционально `0008_credentials_comment.sql` с `--` комментарием (sqlx применит пустую миграцию) — лучше просто обновить комментарий в `0004` нельзя (контрольная сумма) → новый файл не нужен; задокументировать в `local_db.rs`.
- Тесты (`local_db.rs` inline + `tests/argon2_migration.rs`): `new_passwords_are_argon2id_phc` (префикс + `m=19456,t=2,p=1`), `legacy_pbkdf2_hash_still_authenticates`, `legacy_hash_rehashed_to_argon2_on_login`, `rehash_does_not_bump_generation_or_password_changed_at`, `wrong_password_constant_time_and_no_panic_on_bad_hex`, `argon2_memory_bounded_by_semaphore` (N=8 конкурентных verify, проверить, что одновременно ≤2).
**Не надо:** колонки `algorithm`/`iterations`; `Argon2::default()` (он m=19456,t=2,p=1 — да, но неявно; фиксируем явно); argon2 0.6 (см. §4.2); «тихий» fallback на pbkdf2 при ошибке парсинга PHC (fail-open!).

### Шаг 4. WebTransport (пункт 1)

- `crates/vks-signaling/Cargo.toml`: `wtransport = { version = "0.7.2", default-features = false, features = ["ring", "self-signed"] }` (TLS = ring; `rcgen`/`socket2` уже в lock).
- `ws.rs`+`wt.rs` → **один** `transport.rs` (см. §6) с async-трейтом:
  ```rust
  #[async_trait]
  pub trait EnvelopeTransport: Send {
      async fn send(&mut self, frame: &[u8]) -> Result<(), TransportError>;
      async fn recv(&mut self) -> Result<Option<Vec<u8>>, TransportError>; // None = закрыт
      fn is_closed(&self) -> bool;
      async fn close(&mut self);
  }
  ```
  `TransportError::{Closed, TooLarge{len,max}, Io, Timeout}`.
- `crates/vks-signaling/src/wt_server.rs` (новый, ~200 строк): `WtEndpoint::serve(cfg)`, `accept()` с лимитами (`max_idle_timeout(30s)`, `keep_alive_interval(15s)`, `Endpoint::open_connections() < MAX_WT_CONNS`, таймаут на handshake 10 с, `MAX_WT_FRAME = 64 KiB` = `MAX_WS_MESSAGE_BYTES`).
  Framing: bi-stream — `u32 BE len + protobuf`; datagrams — только фреймы ≤ `connection.max_datagram_size().unwrap_or(1200) - 4`, иначе принудительно bi-stream (SDP/answer никогда не в датаграмму).
- `crates/vks-server/src/lib.rs`: `route("/wt", get(wt_upgrade_info))` + отдельный UDP-листенер в `main.rs` (порт `VKS_WT_PORT`, дефолт 8443/UDP) — нельзя вешать QUIC на тот же порт, что TCP 8080; `Alt-Svc: h3=":8443"; ma=86400` в ответе `/` (только когда WT включён).
- Один и тот же `SignalingHandler` на оба транспорта: вынести цикл `handle_ws_socket` (`lib.rs:608-…`) в `signaling_session(transport: impl EnvelopeTransport, ctx)`; WS-адаптер — 30 строк поверх `axum::extract::ws`.
- Сертификаты: `VKS_WT_CERT`/`VKS_WT_KEY` (PEM) либо `Identity::self_signed(["<candidate_addr>"])` для dev; хеш DER (`sha2`) отдавать клиенту для `serverCertificateHashes`.
- Тесты: `mock_transport_roundtrip_and_errors` (замена двух mock-тестов), `frame_larger_than_datagram_goes_to_bistream`, `handshake_timeout_closes`, `connection_cap_rejects_extra`, `wt_and_ws_drive_same_fsm` (общий `signaling_session`).
- **Не надо:** `quinn` напрямую (wtransport уже инкапсулирует), `aws-lc-rs` (cmake/nasm), HTTP/3-роутинг (нам нужен только WT-сайт), второй экземпляр `SignalingHandler`/FSM, `tokio-tungstenite` (WS уже на axum).

### Шаг 5. L0: sysctl + cgroup + сокеты (пункт 6)

- `deploy/systemd/vks-mcu-prod.service`: `Slice=vks.slice`, `CPUWeight=800`, `MemoryHigh=75%`, `MemoryMax=85%`, `TasksMax=4096`, `LimitNOFILE=65536` (оставить), `LimitRTPRIO=50` + `AmbientCapabilities=CAP_SYS_NICE` **только если** появится RT-поток (иначе не добавлять — театр), `LimitMEMLOCK=64M` **если** появится `mlockall`.
- `deploy/systemd/vks.slice` (drop-in `[Slice] CPUWeight=… MemoryHigh=…`) либо `systemctl set-property vks.slice`.
- `deploy/sysctl.d/90-vks.conf` (новый): `net.core.rmem_max=33554432`, `net.core.wmem_max=33554432`, `net.core.netdev_max_backlog=5000`, `net.ipv4.udp_mem=…`, `net.ipv4.ip_local_reserved_ports=40000,43000-43100`, `fs.file-max=2097152`. Плюс `deploy/README.md` — раздел «применить: `sysctl --system`, проверить `sysctl net.core.rmem_max`».
- Код: `crates/vks-media-sfu/src/udp_loop.rs:109-117` — создать сокет через `socket2::Socket` (`socket2` уже в lock 0.6.5), `set_recv_buffer_size(8 MiB)`, `set_send_buffer_size(8 MiB)`, `set_reuse_address(true)`, затем `UdpSocket::from_std`; то же в `webrtc_room.rs:135,145`. Экспортировать фактический размер в `/metrics` (`vks_udp_rcvbuf_bytes`) — иначе «sysctl применён» недоказуемо.
- Тест: `udp_socket_requests_8mib_buffers` (unit, читает `getsockopt(SO_RCVBUF)` и проверяет ≥ запрошенного/2 — ядро может урезать).
- **Не надо:** `LimitRTPRIO` без RT-кода, `mlockall` без `LimitMEMLOCK`, `MemoryMax` без `MemoryHigh`, менять `LimitNOFILE` (уже ок), открывать 40000 в интернет.

### Шаг 6. NATS + OTLP (пункт 5)

- `crates/vks-cluster/Cargo.toml`: `async-nats = { version = "0.51", optional = true, default-features = false, features = ["ring"] }` (без jetstream/websockets/kv), `uuid` (уже), `[features] nats = ["dep:async-nats"]`.
- `bus.rs:35-41`: `Msg::new` → `uuid::Uuid::now_v7().to_string()` (**P0-2**).
- `crates/vks-cluster/src/transport.rs` (новый, ~150 строк): `trait ClusterTransport { async fn publish(&self, msg: &Msg) -> Result<(), TransportError>; fn subscribe(&self, topic) -> BoxStream<Msg>; }`; `LocalTransport` (текущий `Bus`, дефолт), `NatsTransport` (feature `nats`); reconnect + `Event::Disconnected` → деградация в `LocalTransport` с `warn!` (никогда `exit`).
- `vks-server/src/cluster.rs:36-41,65-79`: `ClusterManager::new` принимает `Arc<dyn ClusterTransport>`; `main.rs:439` — `VKS_NATS_URL` пуст → `LocalTransport`.
- `vks-server/src/main.rs:476-484`: добавить `ShutdownSequence` (`shutdown.rs:26-32`) в graceful shutdown: stop_accepting → maintaining → media_drain → **recorder stop_all** (новый метод) → storage_flush → exit. Заодно закрывает «потерю записей при shutdown».
- OTLP: `vks-common/Cargo.toml`: `opentelemetry = "0.33"`, `opentelemetry_sdk = { version = "0.33", features = ["rt-tokio"] }`, `opentelemetry-otlp = { version = "0.33", default-features = false, features = ["trace", "grpc-tonic", "tls-ring"] }`, `tracing-opentelemetry = "0.34"` — **под feature `otlp`** (иначе `--all-features` CI будет компилировать tonic всегда, это ок, но бинарник без фичи легче).
- `vks-common/src/tracing.rs:26-41`: `init_with_otlp(filter, format, Option<OtlpConfig>)`, `OtlpGuard` с `shutdown()`; в `main.rs:47` — читать `VKS_OTLP_ENDPOINT` (пусто → None), в shutdown — `guard.shutdown().await`.
- Тесты: `msg_ids_are_unique_across_processes` (два `Msg::new` → разные префиксы; сейчас `msg-0` и `msg-0` в разных процессах), `nats_transport_degrades_to_local_without_server`, `otlp_layer_builds_and_shuts_down` (без коллектора: экспорт в timeout, без паники).
- CI: `test-nats` джоб (`services: nats:2.10-alpine`, `continue-on-error: true`), `test-postgres` расширить на новые миграции.
- **Не надо:** JetStream для heartbeat/lease; заменять `Bus` целиком; OTLP-метрики/логи; `reqwest-blocking-client`; включать OTLP по умолчанию (коллектора нет).

### Сводка версий для добавления

| Крейт | Версия | MSRV | В lock сейчас | Риск |
|---|---|---|---|---|
| `wtransport` | 0.7.2 (`ring`,`self-signed`) | 1.88 | ❌ | средний: +quinn/rustls/x509-parser/sha2-0.11 |
| `argon2` | 0.5.3 | 1.65 | ❌ | низкий: `base64ct`/`cpufeatures`/`subtle`/`zeroize` уже есть |
| `async-nats` | 0.51.0 (или 0.50.0) | 1.89 | ❌ | средний: rustls-стек (общий с WT), `rand 0.10` уже есть |
| `opentelemetry-otlp` | 0.33.1 (`trace`,`grpc-tonic`) | 1.75 | ❌ | средний: `tonic 0.14` + дубль `prost 0.14` |
| `opentelemetry` / `_sdk` | 0.33.1 | 1.75 | ❌ | низкий |
| `tracing-opentelemetry` | 0.34.0 | 1.75 | ❌ | низкий: `tracing-subscriber 0.3.22` ✓ (0.3.23) |
| `socket2` | — (использовать существующий) | — | ✅ 0.6.5 | нет |
| `libc` | — (workspace `0.2`) | — | ✅ 0.2.190 | нет |
| `uuid` (v7) | — | — | ✅ | нет |

---

## §6 «Меньше кода»

| Что | Строк | Действие |
|---|---|---|
| `crates/vks-signaling/src/ws.rs` + `wt.rs` | 205 | **Слить в `transport.rs`**: один async-трейт + один `MemoryTransport` (VecDeque) вместо двух побайтовых дублей. Прод-код не использует ни один (`lib.rs:608` работает напрямую с axum). Правка тестов: `handler.rs:472-473,537,616-625` (3 ссылки) |
| `RecorderManager::containers` + `save_container/get_container` | 26 | **Удалить**: безлимитный `HashMap<String, Vec<u8>>` в RAM (`recorder.rs:26,53-60`), HTTP-потребителя нет, тест `network_integration.rs:1637-1638` — единственный. Байты — в `RecordingStore` (файл/объект-стор) |
| `RecorderManager::stop_recording` (sync) | 4 + вызовы | **Удалить**, оставить `stop_recording_async` (`:233-243`): sync-версия делает `tokio::spawn` с потерей ошибки (P1-6) |
| `ClusterManager::with_heartbeat_deadline` + `with_shared` | 35 | **Слить в один `new(node_id, deadline, transport)`**: три конструктора с идентичной регистрацией (`cluster.rs:24-79`) |
| `Msg::new` счётчик | 5 | **Заменить** на UUIDv7 (не «меньше», но убирает P0-2) |
| `videorecords_list` / `session_videorecords_list` | ~30 | **Слить**: один хендлер с `Option<Path<String>>`-фильтром (сейчас копия с `filter` на 3 строки) |
| `retention` (255 строк) | 0 вызовов | **Либо подключить** (purge-джоб на старте + раз в час, 15 строк), **либо удалить**: сейчас это экспортируемый мёртвый код |
| `MockWs` (тест `full_happy_path_over_mock_ws`) | 1 строка пользы | В тесте `let _mock = MockWs::new();` (`handler.rs:537`) — переменная не используется; убрать вместе с `MockWs` |
| `HandlerConfig::default_test` vs `main.rs:356-371` | ~25 | Не «меньше», но: убрать дубль констант (ufrag/pwd/fingerprint) — один `HandlerConfig::for_server(...)`, читающий реальные значения |
| `MemoryStore` (recorder) vs `state.records` vs `MemoryStorage.core_records` | ~60 | **Оставить один** слой (`Storage`), остальные два — удалить (P2-7) |

Итого без изменения поведения: **≈ −430 строк** и минус один in-memory реестр.

---

## §7 Риски и неизвестное

1. **Экосистема wtransport.** 0.7.2 выпущен 2026-10-15 (две недели назад), 0.7.0 — 1.83, 0.7.2 — 1.88. API менялся 3 раза за 2 года (0.5 async `server()` → 0.6/0.7 sync). Риск: примеры/статьи/LLM-память дают несовместимый код; нужен пин `=0.7.2` и smoke-тест «endpoint поднимается и принимает соединение» в CI.
2. **Браузерный сертификат.** Самоподписанный WT в Chromium без `serverCertificateHashes` не работает; Edge/Safari — по-разному. Неизвестно, поддерживает ли PNM-фронтенд `serverCertificateHashes` (в `client/pnm-frontend` следов нет). Без этого «WT для браузера» — только для нативного SDK.
3. **UDP-проникновение.** QUIC (4433/8443 UDP) в корпоративных сетях часто заблокирован; нужен fallback на WS — то есть WT не «замена», а «второй путь», и удалять WS нельзя (в §6 удаляются только mock-трейты, не `/ws`).
4. **Живой PostgreSQL.** В стенде PG нет (только CI-сервис). T10b + `0006`/`0007` будут проверены на `postgres:16` в CI, но не на прод-данных; риск миграции на живой БД (объём документа, битые записи) не измерен.
5. **NATS/OTLP-инфраструктуры нет** ни в `deploy/`, ни в CI. Добавление крейтов без сервера = непроверяемый код + рост времени сборки (`quinn`+`rustls`+`tonic` ≈ +40–60 с на джоб × 12 джобов).
6. **Argon2 memory.** 19 МиБ × параллельные логины; на стенде с `MemoryMax` (если его включить) возможен OOM-kill; нужен замер RSS под нагрузкой — сейчас данных нет.
7. **Дубли `prost` 0.13/0.14, `sha2` 0.10/0.11** — `deny.toml` без `[bans]`, CI не заметит; риск — несовместимость типов при попытке переиспользовать `vks-proto`-сообщения в OTLP (не нужно, но соблазн есть).
8. **MSRV-дрейф.** `async-nats 0.51` требует 1.89, `wtransport 0.7.2` — 1.88; workspace `rust-version = 1.94`, так что ок, но `rust-toolchain.toml` прибит к 1.94.0 — CI обновится только вручную.
9. **`VKS_*` env-источники (50+)** — при добавлении `VKS_NATS_URL`/`VKS_OTLP_ENDPOINT`/`VKS_WT_PORT` расхождение с `Config` усугубится; нужен один `Config`-объект (отдельная задача, не в этом скоупе).
10. **Неизвестно:** пропускная способность фермы при 1 скрытом зрителе-рекордере (группа = энкодер; на комнату с 20 участниками +1 группа может упереться в `VKS_VIDEO_FARM_MAX_GROUPS`, `vks-media-mcu/src/farm.rs:368+`).

---

## §8 Оценка

| Критерий | Балл | Комментарий |
|---|---|---|
| Соответствие заявке (6 пунктов) | **2,5/10** | 0,5 + 1 + 0 + 2 + 1 + 2,5 из 10 по пунктам §1 |
| Пункт 1 (WT) | 0,5/10 | mock-трейты + `room_mode`; стека нет |
| Пункт 2 (запись) | 1/10 | модель записи есть, данных/профиля/персистентности/API нет |
| Пункт 3 (T10b) | 0/10 | не начат; документ-монолит |
| Пункт 4 (Argon2id) | 2/10 | PBKDF2 + тайминг-защита + поколения; Argon2 нет |
| Пункт 5 (NATS+OTLP) | 1/10 | хорошая модель шины (dedup/DLQ/backpressure) в памяти; экспорта нет |
| Пункт 6 (L0) | 2,5/10 | systemd-hardening и NOFILE есть; cgroup/sysctl/сокеты/RT — нет |
| Качество существующего кода в зоне | **8/10** | FSM, лимиты WS, auth-at-handshake, детерминированные тесты, doc-культура; минусы — mock-порты, no-op-контракты, блокирующий I/O |
| Готовность к реализации плана §5 | **8/10** | есть порты, тесты, PG-CI, `flock`, `socket2`/`libc`/`uuid-v7` уже в lock |

**Итог: 2,5/10 по целевому состоянию** при 8/10 по инженерной базе. Главный риск — не «сложно», а «выдать театр за реализацию»: `LimitRTPRIO` без RT-кода, `net.core.rmem_max` без `SO_RCVBUF`, NATS-крейт без сервера, mock-трейт вместо QUIC. В §5 у каждого шага есть внешний, а не «сам себе», критерий приёмки (ffprobe, `getsockopt`, два узла, `count>0` после рестарта).

---

## §9 Спорные тезисы

### Т1. WebTransport не должен оставаться «обязательным» транспортом: для сигналинга VKS он даёт меньше, чем стоит
**ТЕЗИС.** В продукте, где медиа уже идёт через str0m/ICE-UDP, WebTransport нужен только для control-plane (SDP/ICE/join, единицы КБ). Выигрыш QUIC против WS/TCP для таких объёмов — доли миллисекунды; цена — TLS-стек (`rustls`+`quinn`+`x509-parser`+`sha2 0.11`), 4433/UDP, который режут фаерволы, и браузерный сертификат, который не примется без `serverCertificateHashes`. Спецификация (`SIGNALING_PROTOCOL.md:5`) должна либо пометить WT как `optional fast-path`, либо получить ADR с измеримой целью.
**ДОКАЗАТЕЛЬСТВО.** `quinn`/`rustls`/`wtransport` в lock — 0; HTTP-роута `/wt` нет; `Alt-Svc` нет; в `deploy/` нет ни UDP 8443, ни сертификатов; единственный реальный клиент (`tests/e2e/webrtc.js`) ходит по WS. То есть за два раунда разработки WT не понадобился ни одному потребителю.
**ЧТО ПЕРЕУБЕДИТ.** Замер p99 доставки SDP/ICE на lossy-канале (1–3 % потерь), где WS/TCP HOL даёт +200 мс на reconnect, или требование договора с заказчиком + подтверждённый браузерный сценарий (`serverCertificateHashes` в PNM), или отказ от WS в пользу WT как единственного транспорта (тогда — да, обязателен).

### Т2. Запись «без зрителя» в текущей архитектуре невозможна, и это блокер пункта 2, а не деталь
**ТЕЗИС.** Ферма отдаёт закодированный композит только подписчикам существующей группы (`Group` создаётся при подключении зрителя, `farm.rs:492-538`); рекордер, не зарегистрированный как viewer, получит **ноль** кадров. Значит «интеграция записи» — это не вызов `start_recording`, а создание скрытой группы-энкодера с профилем записи (и расход CPU/лицензий энкодера). Пока этого нет, любой `/api/v3/videorecords` с непустым списком — фикция.
**ДОКАЗАТЕЛЬСТВО.** `publish()` рассылает только в `groups`, отфильтрованные по `room`+`canvas_version` (`farm.rs:508-538`); `viewer()` — единственный путь получить `AccessUnit` (`farm.rs:492-502`); `write_video_sample` не вызывается вне тестов (grep).
**ЧТО ПЕРЕУБЕДИТ.** Soak-тест: комната без зрителей, `POST .../videorecords`, 60 с, затем `size_bytes > 0` и `ffprobe` видит ≥1 видеопоток. Или документированное решение «запись только в комнатах MCU, где композит кодируется всегда» с доказательством, что группа существует без зрителей.

### Т3. T10b нельзя делать как «миграцию directory в SQL», пока `layouts/links/history` остаются в том же документе
**ТЕЗИС.** После T10b появятся **два** источника истины с разными правилами конкурентности: directory — строки SQL, остальное — монолитный JSON, перезаписываемый целиком на каждую мутацию (`resources.rs:241-269`). Любой старый снимок документа, записанный после миграции, воскресит удалённые directory-строки, если поле `directory` не удалено из `ResourceState` (`resources.rs:46`). Поэтому T10b обязан включать **удаление** поля из документа, а не только создание таблиц; иначе выгода (уникальные индексы) меньше риска (split-brain).
**ДОКАЗАТЕЛЬСТВО.** `persist_with_video_settings` пишет весь `resources` одним `put_configuration_document` (`:250-266`); `commit` откатывает память, но не БД (`:271-281`); дедуп адресов уже вынесен в офлайн-утилиту именно потому, что in-place правка документа опасна (`dedup.rs:1-20`, `deploy/README.md` — «не пересекать версии на одной БД»).
**ЧТО ПЕРЕУБЕДИТ.** Дизайн, где `directory` удалён из `ResourceState` в том же PR, плюс тест `old_document_with_directory_does_not_resurrect_rows`, плюс замер: размер документа до/после (если directory <5 % документа — выгода только в индексах, и это надо признать).

### Т4. Argon2id с m=19456 без семафора — это регрессия доступности, а не улучшение безопасности
**ТЕЗИС.** Переход с PBKDF2 (0 МиБ/проверка) на Argon2id (19 МиБ/проверка) превращает login в аллокатор памяти: `spawn_blocking` в tokio по умолчанию до 512 потоков, т.е. теоретически ~9,7 ГиБ. Per-(login,IP) lockout (`auth.rs:261`) не мешает распылять попытки по логинам/IP. Без семафора на 2–4 конкурентные проверки Argon2id — это готовый memory-DoS, замаскированный под «мы внедрили OWASP-параметры».
**ДОКАЗАТЕЛЬСТВО.** `local_db.rs:20` (PBKDF2, 0 памяти) → план `Params::new(19_456, 2, 1)`; `local_db.rs:206-268` — все async-пути идут через `spawn_blocking` без ограничителя; `auth.rs:280-299` — блокировка по (login, ip), не глобальная.
**ЧТО ПЕРЕУБЕДИТ.** Замер пикового RSS при 100 параллельных login: с семафором(2) — плато ≤ базовый+40 МиБ; без — линейный рост. Или явное решение «≤4 конкурентных логина на узел» с enforce в коде.

### Т5. NATS и OTLP в этом репозитории — инфраструктурный долг, а не фича; их нельзя добавлять без сервера и коллектора в `deploy/` и CI
**ТЕЗИС.** Оба пункта требуют внешней инфраструктуры, которой нет: в `deploy/` только systemd/env/README, в CI — только `postgres`. Добавление `async-nats`/`opentelemetry-otlp` даст код, который компилируется (`--all-features`) и никогда не исполняется, плюс `rustls`/`tonic`/`prost 0.14` в граф. Пока кластер одноузловой (`cluster.rs:36-41`), шина in-memory достаточна; правильный шаг — трейт + `LocalTransport` по умолчанию, а NATS/OTLP — за фичами с e2e на двух узлах/коллекторе.
**ДОКАЗАТЕЛЬСТВО.** `ls deploy/` → `README.md, systemd/, vks.env.example`; `ci.yml` services → только `postgres:16` (`:116-128`); `deny.toml` фиксирует, что TLS-стека в графе нет; `vks_common/src/tracing.rs:26-41` — только fmt-слой.
**ЧТО ПЕРЕУБЕДИТ.** `docker-compose.yml` (nats + otel-collector + tempo) в `deploy/`, джоб `test-nats` с реальным переездом lease между двумя процессами и трейс в Tempo со стенда; тогда «фича» становится проверяемой и я снимаю тезис.

### Т6. «Воспроизводимый MP4» в `vks-recorder` не воспроизводим: нет `avcC`, payload не length-prefixed
**ТЕЗИС.** `PlayableContainerWriter` формирует валидные с точки зрения боксов `ftyp/moov/mdat` и таблицы `stbl`, но `stsd/avc1` без обязательного `avcC` (конфигурация SPS/PPS) — по ISO/IEC 14496-15 такой sample entry невалиден; плюс `write_video_frame` принимает Annex-B, тогда как MP4 требует length-prefixed NAL units. Тест проверяет только наличие четырёхбайтовых сигнатур (`container.rs:602-617`), т.е. подтверждает «файл похож на MP4», а не «файл играется». Заявление M5.5 «playable container» не доказано.
**ДОКАЗАТЕЛЬСТВО.** `container.rs:313-332` — `avc1` из фиксированных полей, дочерних боксов нет; `container.rs:84-102` — байты пишутся как есть; `farm.rs:225-258` — источник отдаёт Annex-B; фикстура теста — `[0xAA;512]`, не H.264.
**ЧТО ПЕРЕУБЕДИТ.** `ffprobe -v error -show_streams` на реальной записи (≥1 с) без ошибок + декод первого кадра (`ffmpeg -v error -i out.mp4 -frames:v 1 -f null -`). Если это проходит на текущем коде — я неправ, и `avcC` пишется где-то ещё (но grep по `avcC` в репозитории пуст).

### Т7. Потеря записей — дефект `Storage`-контракта, а не рекордера; no-op-дефолты в трейте маскируют неполные бэкенды
**ТЕЗИС.** `upsert_record`/`list_records` имеют реализации по умолчанию, возвращающие `Ok(())`/`Vec::new()` (`vks-core/src/ports/storage.rs:294-311`). `SqliteStorage` и `PostgresStorage` их не переопределяют, поэтому запись «успешно сохраняется» и «успешно не находится» одновременно. Это fail-open для данных: ошибка не логируется, API отдаёт `count:0`, тесты зелёные (тестируется только `MemoryStorage`). Такой контракт опаснее отсутствия метода.
**ДОКАЗАТЕЛЬСТВО.** `grep -n "fn upsert_record" core_adapter.rs` → одна строка (205, в impl `MemoryStorage`); `state.rs:600-640` не читает записи; `/api/v3/videorecords` собирает из `storage.list_records` (`observability.rs:667-674`) → на SQLite/PG всегда пусто.
**ЧТО ПЕРЕУБЕДИТ.** Тест `SqliteStorage`: `upsert_record` → переоткрыть файл БД → `list_records` содержит запись. Такого теста нет; если он есть где-то в `tests/` и проходит — значит я пропустил переопределение, и тезис снимается (я проверил grep'ом по всему `core_adapter.rs`).

### Т8. `LimitRTPRIO`/`MemoryHigh`/`net.core.rmem_max` без правок кода — конфигурационный театр
**ТЕЗИС.** Лимиты cgroup/RT имеют смысл только если процесс их использует. В коде **нет** `sched_setscheduler`, `setpriority`, `SO_RCVBUF`, `SO_SNDBUF`, `mlockall` (grep → 0). Значит `LimitRTPRIO` ничего не разрешает (нет вызова), `rmem_max` ничего не увеличивает (сокет не просит), `MemoryHigh` не на что троттлить (нет измерения). Пункт 6 в текущем виде — это `LimitNOFILE`, который уже был, плюс косметика юнита; выдавать его за «L0 выполнено» нельзя.
**ДОКАЗАТЕЛЬСТВО.** `grep -rn 'sched_setscheduler|SCHED_FIFO|SO_PRIORITY|setpriority|mlockall|SO_RCVBUF' crates/` → пусто; `udp_loop.rs:109-117` — `tokio::UdpSocket::bind` без `socket2`; `webrtc_room.rs:135,145` — `std::net::UdpSocket::bind`; `deploy/` без `sysctl.d`; `LimitNOFILE=65536` — единственный рабочий лимит.
**ЧТО ПЕРЕУБЕДИТ.** `strace -f -e trace=sched_setscheduler,setsockopt` на работающем сервере, показывающий реальные вызовы с `SO_RCVBUF=8 MiB` и `SCHED_FIFO=50`, плюс `systemctl show vks-mcu-prod -p MemoryHigh -p CPUWeight` с ненулевыми значениями. Тогда тезис снимается для тех пунктов, которые реально вызываются.

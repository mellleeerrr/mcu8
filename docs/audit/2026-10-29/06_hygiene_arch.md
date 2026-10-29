# 06 — Гигиена цепочки и архитектурная элегантность

> Саб-агент: `hygiene-arch`. Дата: 2026-10-29. HEAD `d82ce4d`, дерево чистое, 506 коммитов.
> Rust 1.94.0, baseline 1425 passed / 0 failed / 8 ignored.
> Метод: `grep` по 183 src-файлам (всего 198 `.rs`: 183 в `src/` + 15 в `tests/`) + `cargo metadata`/`cargo tree -d` + разбор `Cargo.lock`,
> `git ls-files`, `du`, `sha256sum -c MANIFEST.sha256`, чтение манифестов/конфигов/доков.
> **Ничего не удалялось и не перемещалось.** Живой сервис/БД не трогались. Единственный созданный
> файл — этот отчёт.
> Ограничение метода: nightly `cargo-udeps` не запускался (в CI нет, локально не ставился) —
> неиспользуемые зависимости определены регулярным анализом `use`/идентификаторов, что даёт
> верхнюю границу (false positive только для build.rs/tests — отдельно проверены и исключены).

---

## §1. Вердикт

**Гигиена кода — 7.5/10. Гигиена документации — 4/10. Гигиена артефактов — 5/10. Целостность
«док ≡ код» — 3.5/10.** Итог: **6.0/10**.

Что действительно хорошо (и это не формальность):

1. **Маркеров мёртвого кода почти нет.** 1 `TODO` на весь workspace
   (`crates/vks-server/src/gateway_video.rs:872`), 0 `FIXME`/`todo!()`/`unimplemented!()`,
   7 `#[allow(dead_code)]`, и все 7 — с обоснованием в комментарии. Для 96 799 строк Rust это
   исключительный показатель.
2. **`cargo-deny` настроен по-взрослому** (`deny.toml`: `all-features = true`, whitelist лицензий,
   `unknown-registry = deny`, `unused-ignored-advisory = deny`).
3. **Fail-closed конфигурация реализована честно** (`config.rs:268–330`, `DEV_JWT_SECRET`,
   `is_weak_password`, exit 78) и покрыта тестами, включая «prod.example.toml без секретов
   отвергается» — тесты `config.rs:581` и `crates/vks-server/tests/config_failclosed.rs:9`
   читают **реальный файл** `configs/prod.example.toml`, а не копию.
4. **`/metrics` и `m5_api` защищены** (`lib.rs:156–196`): токен + `Moderator+`. Тезис брифа
   «анонимные `/metrics`, `/mcu/farm`, `gateways/status`, `cluster/status`» на HEAD **неверен** —
   см. §13.1.
5. **Манифест фронтенда сходится**: `sha256sum -c MANIFEST.sha256` → 214/214 OK, дрейфа нет.

Что плохо, и это главный вывод:

1. **Мёртвый код не «россыпь», а целые модули — ~1 882 строки продакшн-кода** (3.0 % от 62 955
   строк prod-LOC), из которых 1 230 строк в пяти файлах — с **нулём** продакшн-потребителей:
   `vks-core/src/transparency.rs` (447), `vks-common/{qoe,slo,ratelimit}.rs` (504),
   `vks-gateway-h323/src/ffi.rs` (279). Все они «живы» ровно потому, что на них есть тест.
2. **Документация описывает другой репозиторий.** `ARCHITECTURE.md §3` — 10 из 15 строк дерева
   зависимостей не совпадают с манифестами, а 8 из перечисленных файлов не существуют. Это не
   «устарело на один коммит», это никогда не было верно.
3. **Есть работающие mock'и, помеченные как продакшн-функциональность**, и трекер оценивает их
   «9.5/10 закрыто» без слова «mock» (0 вхождений слова «mock» в `MASTER_TASK_LIST.md` на 976 строк).
   Самый острый — LDAP: `auth_provider = "ldap"` — это mock-клиент, принимающий **любой пароль**.
4. **«Меньше кода» — да, можно.** Реалистичная экономия без изменения поведения: **~1 500–1 600
   строк Rust (1.6 %)**, 19 дублирующих route-регистраций, 6.29 МБ legacy-бандлов в git и
   **65.6 ГБ** локальных артефактов сборки.

Ключевая мета-находка: **ревью-процесс оценивает diff, а не дерево.** «10.0/10 Minimization &
Elegance, Unconditional Approval» (`MASTER_TASK_LIST.md`, D6) стоит рядом с 1 230 строками
мёртвого кода и мёртвым MP4-писателем. Ни одна из шести ревизий (9.5–10.0) не задала вопрос
«кто вызывает этот `pub`-символ из другого крейта».

---

## §2. Таблица мёртвого кода

Легенда: **A** — 0 потребителей вне крейта (grep по `crates/` + `client/`); **B** — потребители
только в тестах; **C** — мёртвая ветка/фича. «Строк» — удаляемая оценка.

| # | Путь:строки | Строк | Класс | Доказательство |
|---|---|---|---|---|
| D1 | `crates/vks-core/src/transparency.rs:1–447` | **447** | B | `ModeSwitcher`, `SwitchOutcome`, `ClientView`, `SwitchClock` вне файла встречаются **только** в `crates/vks-server/tests/network_integration.rs:732,738,746`. Продакшн-переключение режима живёт в `crates/vks-admin/src/mode.rs` + `state.rs:105` (`ModeState::global_mode`). `pub use transparency::{…}` — `crates/vks-core/src/lib.rs:87` |
| D2 | `crates/vks-common/src/qoe.rs:1–175` | **175** | A | `grep -rn "qoe::\|QoeAggregator"` → только `crates/vks-common/src/lib.rs:27` (re-export). 0 потребителей |
| D3 | `crates/vks-common/src/slo.rs:1–145` | **145** | A | `SloEvaluator`/`SloTarget` → только `lib.rs:29`. 0 потребителей |
| D4 | `crates/vks-common/src/ratelimit.rs:1–184` | **184** | A | `RateLimiter`/`ADMIN_PER_MIN`/`WS_FRAME_MAX_BYTES` → только `lib.rs:28`. Реальный лимит WS — свой: `crates/vks-admin/src/api_v3/ws.rs:38` `WS_MAX_MESSAGES_PER_SEC = 100` |
| D5 | `crates/vks-gateway-h323/src/ffi.rs:1–279` | **279** | A | `H323PlusBridge`, `Watchdog`, `PRODUCTION_WATCHDOG_LIMIT` → только `crates/vks-gateway-h323/src/lib.rs:40` + док-ссылки `call.rs:143,153`. Продакшн-путь H.323 — Python-sidecar: `uac.rs:252,396`, `uas.rs:110,266` (`VKS_H323_HELPER`/`VKS_H323_PYTHON`) |
| D6 | `crates/vks-recorder/src/container.rs:1–633` + `recording.rs:178,231–234,279–284` | **~640** | B | `PlayableContainerWriter::new` — только `container.rs:590,629` (свои тесты) и `tests/network_integration.rs:1577`. Единственный активатор `RecordingSession::enable_container` **не вызывается нигде** ⇒ `container_writer == None` всегда, `container_bytes()` всегда `None`. `vks-recorder` не делает ни одного `fs::*`-вызова ⇒ запись в проде — метаданные в `MemoryStore` |
| D7 | `vks-signaling/src/ws.rs:30–77` (48) + `wt.rs:29–77` (49) + `lib.rs:25–26`; `client-sdk/src/signaling_client.rs:207–259` (53), `webrtc_glue.rs:44–147` (104) | **254** | B | Все 4 mock-реализации экспортированы в публичный API **без** `#[cfg(test)]`; потребители — только тесты (`handler.rs:472,537,618`; `signaling_client.rs:265–288`). Т.е. 254 строки компилируются в release и видны как часть SDK |
| D8 | `crates/vks-auth/src/jwt.rs:228–230` `duration_secs` | 3 | A | 0 вхождений по всему дереву; даже докстринг «Test helper» не соответствует — тесты его не зовут |
| D9 | `crates/vks-server/src/layout_signal.rs:84–86` `layout_kind` | 3 | A | `grep -rn "layout_kind"` → только объявление |
| D10 | `crates/vks-server/src/farm_metrics.rs:195–197` `snapshot_json` | 3 | A | 0 вхождений; живой путь — `json_from_stats` (`lib.rs:172`) |
| D11 | `crates/vks-media-mcu/src/engine.rs:1122–1124` `video_farm` | 3 | B | Тест-аксессор, продакшн использует `video_farm_stats()` |
| D12 | `crates/vks-media-mcu/Cargo.toml:40,44` + `lib.rs:65–66` фича `oxideav` | 3 + 1 стр. манифеста | C | Фича включает **только** `pub use oxideav_scene;`. Ни одного `#[cfg(feature = "oxideav")]` над кодом (grep даёт ровно 1 совпадение — сам `lib.rs:65`). «Реальный компоновщик за фичей» (README:69, `lib.rs:27`) **не существует** |
| D13 | `crates/vks-common/src/lib.rs:27–29` re-export'ы D2–D4 | 3 | A | Экспорт мёртвого как живого |

**Итого мёртвого продакшн-кода: ≈ 1 882 строки** (D1–D5 — 1 230 строк с нулём потребителей;
D6 — 640 строк, не мёртвых по коду, но недостижимых в проде; D7 — 254 строки моков в release;
D8–D11 — 12 строк; D12/D13 — фиктивная фича и реэкспорт мёртвого).

Проверено и **не** является мёртвым (не трогать): `vks-cluster` (1 868 строк — используется
`crates/vks-server/src/cluster.rs:11`: `Bus`, `RoomOwner`, `Lease`), `vks-layout::grid` vs `grids`
(разные алгоритмы: балансировщик N тайлов vs параметрические схемы), `build_pli`/`build_fir`
(тест-билдеры `rtcp.rs`), `scripts/patch-pnm-*.py` (load-bearing: `import-pnm-frontend.sh`).

### 2.1. Фронтенд: заявленные в брифе символы **отсутствуют** (уже удалены)

Проверено `grep -rn` по `client/`, `scripts/`, `tests/`, `docs/`, `deploy/`:

| Символ из брифа | Вхождений в коде | Вывод |
|---|---|---|
| `constructorCount` | **0** | Удалён; правки предыдущего аудита (`2026-10-28-fix` H-04, `2026-10-28` D1) применены в `codian-layouts.js` (mtime 28.10 19:34, коммит в HEAD) |
| `onConstructorActivate` | **0** | То же (H-05 / D2, −7 строк) |
| «перехват `calculate`» | `calculate` — 9 вхождений в `codian-layouts.js`, **все живые**: запрос `POST /api/v3/ui/video_layouts/calculate` для превью (`:668`), инвалидация кэша в axios-интерсепторе (`:1357`, `:1384`) | Перехвата-заглушки нет; удалять нечего |

Остаточные фронтенд-находки (не мёртвый код, а вес/риск):

- `codian-layouts.js` — **1 435 строк / 68 124 B**, 55 функций; монтируется в `index.html:33`
  (`<script src="/mcu/codian-layouts.js">`). Проверки синтаксиса в CI нет (C6).
- Дублирование монтирования: `frontend.rs:94–95` отдаёт один и тот же каталог по `/pnm/*`
  **и** `/mcu/*`, потому что `index.html` ссылается на ассеты абсолютным `/mcu/...`
  (`index.html:30–33`), а канонический вход — `/pnm/`. Работает (документировано `frontend.rs:13`),
  но это скрытая связка: убрать `/mcu/{*rest}` нельзя, не переписав `index.html`.
- `perf-overrides.js` (11 685 B) и `api-errors.js` (2 687 B) — проектные, живые, грузятся до бандла;
  `theme-toggle.js` (3 984 B) — живой (`index.html:32`).
- Legacy-сборка — 6.29 МБ (§5.2 п.2), единственная реальная «тяжесть».

---

## §3. Зависимости и манифесты

### 3.1. Неиспользуемые объявленные зависимости (10 манифест-строк)

Метод: для каждой записи `[dependencies]` (включая `.workspace = true`) — подсчёт вхождений
идентификатора в `src/**/*.rs` крейта. Проверены вручную все «нули».

| Крейт | Зависимость | Доказательство | Действие |
|---|---|---|---|
| `vks-cluster` | `tracing` | 0 вхождений `tracing::` в `src/` | удалить строку |
| `vks-cluster` | `uuid` | 0 вхождений `uuid`/`Uuid` | удалить строку |
| `vks-recorder` | `serde` | 0 вхождений (нет `Serialize`/`Deserialize` вообще) | удалить |
| `vks-recorder` | `tracing` | 0 вхождений | удалить |
| `vks-media-sfu` | `uuid` | 0 вхождений `uuid::`; `RoomId::from_uuid(...)` берёт `Uuid` из `vks-core` | удалить |
| `vks-media-mcu` | `serde` | 0 вхождений | удалить |
| `vks-media-mcu` (dev) | `prometheus` | 0 вхождений в `src/` включая `#[cfg(test)]` | удалить |
| `vks-media-mcu` (dev) | `serde_json` | 0 вхождений | удалить |
| `client/vks-client-sdk` | `tracing` | 0 вхождений | удалить |
| `client/vks-client-sdk` | `serde` | 0 вхождений | удалить |

**Ложные срабатывания, проверены и оставлены как есть:** `vks-proto` `prost-build` +
`protoc-bin-vendored` (используются в `build.rs:23,28`, вне `src/`), `vks-server` `tokio-tungstenite`
(`tests/network_integration.rs:23`), `getrandom` в `vks-common`/`vks-client-sdk` под
`cfg(target_arch = "wasm32")` (это feature-unifier для `uuid`/`rand`, кода не требует — но
заслуживает комментария в манифесте, сейчас его нет).

Экономия: 10 строк, ~0 байт бинарника (все крейты уже в графе), но это **ложь манифеста о
намерении** и лишняя поверхность для `cargo-deny`.

### 3.2. Дубли версий в `Cargo.lock`

434 пакета, 402 уникальных имени, **28 имён с двумя-тремя версиями**:

```
rand 0.8.8 / 0.9.5 / 0.10.3        getrandom 0.2.17 / 0.3.4 / 0.4.3
rand_core 0.6.4 / 0.9.5 / 0.10.1   rand_chacha 0.3.1 / 0.9.0
thiserror 1.0.69 / 2.0.21          syn 2.0.119 / 3.0.9
hashbrown 0.15.5 / 0.17.1          nom 7.1.3 / 8.0.0
toml_edit 0.22.27 / 0.25.17        winnow 0.7.15 / 1.0.4
chacha20 0.9.1 / 0.10.2            cpufeatures 0.2.17 / 0.3.1
windows-sys 0.48 / 0.52 / 0.61 (+10 windows-* дублей)
```

Корень `rand` ×3: `vks-auth` пинует `rand = "0.8"` (`crates/vks-auth/Cargo.toml:21`), `str0m`
тянет 0.9, `proptest` — 0.10. **Правка:** `vks-auth` → `rand = "0.9"` (API `thread_rng`/`gen` почти
не изменился; `rand::rng()` вместо `thread_rng()`), уходит одна копия `rand` + `rand_core 0.6` +
`getrandom 0.2`. `thiserror 1.0.69` приходит транзитивно (не наш) — не трогать.
`windows-*` — мусор для не-Windows целей, `deny.toml graph.targets` его уже игнорирует.

### 3.3. Тяжесть графа

`sqlx 0.8` с `postgres + sqlite + migrate` — самая тяжёлая ветка (≈120 пакетов из 434), но она
load-bearing (PG-путь в CI-джобе `test-postgres`). `str0m` (включён в `vks-server`) тянет
`dimpl`/`str0m-proto`/`chacha20poly1305` — тоже load-bearing (`webrtc_runtime.rs`).

Единственная **реально лишняя по смыслу** зависимость — `oxideav-scene` (D12): тянет внешний
крейт ради `pub use`, в коде не используется. `cargo check --all-features` его компилирует
(CI-джоба `check`), то есть **CI платит за пустую фичу**.

---

## §4. Конфиги, env, порты

### 4.1. Два источника истины на дефолты (5 расхождений из 17 полей)

| Поле | `Config::default()` (`config.rs`) | `configs/default.toml` | Последствие |
|---|---|---|---|
| `server_name` | `"vks"` (:196) | `"vks-mcu"` (:11) | логи/`/healthz` разные |
| `listen_addr` | `"0.0.0.0:443"` (:197) | `"0.0.0.0:8080"` (:12) | запуск без `--config` садится на **443** |
| `media_listen_addr` | `"127.0.0.1:0"` (:207) | `"0.0.0.0:40000"` (:13) | без конфига медиа только на loopback |
| `database_url` | `String::new()` (:199) | `sqlite://vks_data.db?mode=rwc` (:27) | без конфига — Memory/отказ |
| `ice_servers` | `Vec::new()` (:210) | `["stun:stun.l.google.com:19302"]` (:47) | без STUN ICE не соберётся |

Инвариант «TOML — единственное место» нарушен: `serde(default)` + `Default` создают **второй,
отличающийся** набор. Стоимость: оператор, забывший `--config`, получает сервер на 443 с
loopback-медиа и пустым ICE — и это **не** ошибка конфигурации, `validate()` пропустит.

Поведение `main.rs:64–73` усиливает проблему в обе стороны:

- **release** без `--config`/`VKS_CONFIG_PATH` → `Config::load(None)` → набор из таблицы выше
  (443 + loopback-медиа + пустой DSN). Сервер стартует, `/healthz` отвечает 200.
- **debug** без флагов молча подхватывает `configs/default.toml` из CWD, если он существует —
  то есть dev-секрет JWT (`DEV_JWT_SECRET`) и `admin/admin_password` включаются без единой
  строки в логе о том, *что* загружено (в логе только `path = Some("configs/default.toml")`).

### 4.2. 66 переменных `VKS_*` против 17 полей `Config`

- В коде читается **66** токенов `VKS_*` (65 переменных + динамическая маска
  `VKS_GATEWAY_VIDEO_*` из `gateway_video.rs:223`); в продакшн-файлах — **55**, остальные 11 —
  тестовые (`VKS_LIVE_*`, `VKS_SOAK_*`, `VKS_STORAGE_PG_URL`). 65 `env::var`-сайтов в prod-коде.
- Через `Config` (figment `Env::prefixed("VKS_")`, `config.rs:235`) проходит **17** полей.
- Документировано (`deploy/vks.env.example` + `configs/*.toml` + README) — **32**.
- **36 переменных не документированы**, из них ~22 продакшн-видимых:
  `VKS_GATEWAY_VIDEO_{PROFILE,LEVEL,WIDTH,HEIGHT,FPS,BITRATE_KBPS}`,
  `VKS_VIDEO_FARM`, `VKS_VIDEO_FARM_MAX_GROUPS`, `VKS_SFU_VIDEO_BITRATE_KBPS`,
  `VKS_WEBRTC_BWE_INITIAL_BPS`, `VKS_SDP_CODEC_MATRIX{,_LAB}`,
  `VKS_H323_UAS_{BIND,DEFAULT_ROOM}`, `VKS_H323_TRACE`, `VKS_ALLOW_MOCK_LDAP`.
  (~11 остальных — тестовые: `VKS_LIVE_*`, `VKS_SOAK_*`, `VKS_STORAGE_PG_URL`.)
- **Документирована, но не существует:** `VKS_API_TOKEN` (`deploy/README.md:197`, «bench harness
  passes the admin token to the farm endpoint via `VKS_API_TOKEN`») — в коде 0 вхождений.
  Это худший тип расхождения: production-док обещает оператору модель доступа, которой нет.
- Дефолты размазаны: `unwrap_or(...)` в 65 местах, каждый со своим числом
  (`ws.rs:38` 100/с, `farm.rs:1086`, `webrtc_room.rs:121` 256, `uas.rs:139,141` 64/8,
  `gateways.rs:2296` 64 …), и **ни один** не перечислен в одном месте.

### 4.3. Порты: четыре независимых источника

| Диапазон | Где задан | Конфигурируем | Документирован |
|---|---|---|---|
| HTTP 8080 | `configs/*.toml:12`, `run-local.sh` | да (`listen_addr`) | да |
| SIP UAS 5060 | `main.rs:416` (`VKS_SIP_UAS_BIND`) | да (env) | да |
| WebRTC UDP 43000–43100 | `webrtc_room.rs:103` (`VKS_WEBRTC_PORT_MIN/MAX`) | да (env) | да (`deploy/README.md:78`) |
| **SIP-RTP 41000–41200** | `crates/vks-gateway-sip/src/rtp_bridge.rs:22` — `Self::new(41000, 41200)`, **hardcode** | **нет** | **нет** |
| media 40000 | `configs/*.toml:13` | да | частично |
| H.323 1720 | sidecar | — | да |

**41000–41200 — дыра.** Это 100 чётных портов (41000…41198) для RTP/RTCP железа; в `deploy/README.md` нет ни
одного упоминания. Оператор, открывший только 43000–43100 (как написано), получит рабочий
WebRTC и **молча неработающий SIP** — без ошибки в логе (аллокация вернёт `None`).

Дублирование systemd: `deploy/systemd/vks-mcu-prod.service` и `vks-mcu.service` в двух местах
задают порты/флаги (43000/43100, `VKS_WEBRTC_TRANSPORT`, `VKS_SIP_UAS_ENABLED`), при этом в
`vks-mcu.service` комментарий утверждает «allowlist/лимиты — только один источник:
`deploy/vks.env.example`» — и тут же `Environment=` для портов. Комментарий противоречит телу файла.

### 4.4. `-C target-cpu=native` — цена больше, чем кажется

`.cargo/config.toml:31` фиксирует `rustflags = ["-C", "target-cpu=native"]` с обоснованием R-26
(воспроизводимость sha256). Фактические последствия:

1. `--all-features`/`--release` сборки **непереносимы** между машинами разработчика, CI и стендом.
   Бинарь с CI-runner (Zen/Skylake) на Haswell-стенде может дать SIGILL. Комментарий признаёт это
   («для прода — x86-64-v3»), но `x86-64-v3` **нигде не задан** — только в комментарии.
2. Кэш CI (`key: cargo-*-${{ hashFiles('**/Cargo.lock') }}`) **не учитывает** `.cargo/config.toml`
   и `rust-toolchain.toml` ⇒ смена `rustflags` молча переиспользует артефакты прошлого CPU-профиля.
3. `native` применяется и к wasm-джобе? Нет — секция `[target.x86_64-unknown-linux-gnu]`, корректно.

**Правка:** `rustflags = ["-C", "target-cpu=x86-64-v3"]` для сборок, `native` — только через
env-drop-in стенда (`RUSTFLAGS`), и добавить `.cargo/config.toml`/`rust-toolchain.toml` в ключ кэша.

---

## §5. Артефакты и git-гигиена

### 5.1. Локальный диск (не в git)

| Объект | Размер | В `.gitignore` | Оценка |
|---|---|---|---|
| `target/` | **55 G** (debug **50 G**, release 6.1 G, doc 57 M, wasm 229 M; 112 276 файлов) | да | 50 ГБ debug при 6 ГБ release — мусор, `cargo clean -p` не спасает |
| `target-impl/` | 5.9 G | да | альтернативный `--target-dir`, живого назначения нет |
| `target-proto/` | 4.7 G | да | то же |
| `docs/audit/2026-10-25/measure/` | **232 M** (10 бинарей `vks-server.iter*`) | да, `**/measure/vks-server.*` | 232 МБ измерительных бинарей 4-дневной давности |
| `backups/` | 30 M (бинарь + `vks_data.db` + SHA256SUMS) | да | ок, но 30 МБ |
| `client/pnm-frontend/archive/` | 30 M (zip 484 записи) | да | нужен `import-pnm-frontend.sh`, оставить |
| `tests/e2e/node_modules/` | 19 M | да | восстановимо `npm ci` |
| `scripts/__pycache__` + `scripts/bench/__pycache__` | 68 K + | да | регенерируемо |
| `.m7/` | 0 файлов (пустое дерево `tsi/{feedback,patterns}`, создано 28.10 17:09) | **нет в .gitignore** | ⚠️ мусор агентского инструмента: в git не попадёт (пустые каталоги не трекаются), но и не игнорируется — добавить в `.gitignore` или удалить |
| `vks_data.db` (3.2 M) + `vks_server.log` (1.3 M) | 4.5 M | да (`*.db`, `*.log`) | живые, не трогать |

**Итог:** 65.6 ГБ локальных артефактов при 51.8 МБ исходников — соотношение 1265:1.

### 5.2. Репозиторий

- **919 файлов, 51.8 МБ рабочего дерева, `.git` = 72 МБ** (pack 50.12 MiB, 6201 объектов).
- Распределение: `client/` 26.6 МБ (**51 %**), `docs/` 16.1 МБ (31 %), `crates/` 6.75 МБ (13 %),
  `tests/` 464 K, `scripts/` 553 K, `configs/` 4 K, `deploy/` 19 K.

Крупнейшие отслеживаемые файлы:

| Размер | Файл | Комментарий |
|---|---|---|
| 2.14 M | `docs/progress/evidence/t06b1/network.json` | сырой network-trace в git |
| 1.81 M | `client/pnm-frontend/deployed/pnm/chunks/index-legacy.bundle-2.2.1.66.js` | vendor legacy |
| 1.31 M | `…/chunks/index-legacy6.bundle-…js` | vendor legacy |
| 1.27 M | `crates/vks-gateway-h323/python/h323_asn.py` | ASN.1-таблица — обоснованно |
| 1.27 M | `…/chunks/index.bundle-2.2.1.66.js` | vendor |
| 1.25 M | `docs/progress/evidence/t16/http-trace.json` | сырой trace |
| 1.20 M | `…/chunks/index6.bundle-…js` | vendor |
| 921 K | `crates/vks-layout/tests/fixtures/grids_geometry_1280x720.json` | golden — обоснованно |
| 689 K | `docs/progress/evidence/t24/export.zip` | бинарный архив в git |

Проблемы:

1. **294 файла / 15 МБ evidence в git**, включая 18 `.log`, два сырых JSON-trace (3.4 МБ) и
   `export.zip`. Это не «доказательства приёмки», это дампы; `.gitignore` их **разрешает**
   (`!docs/progress/evidence/**/*.log`). Предложение: оставить `SUMMARY.md` + скриншоты, trace
   вынести в релиз-артефакт (или LFS).
2. **Legacy-сборка PNM: 32 файла / 6.29 МБ = 41 % `chunks`** — задокументировано в
   `client/pnm-frontend/NOTES.md` как осознанная плата за 1:1. Для MCU-панели 2026 года
   (`nomodule`-браузеры) это 6 МБ мёртвого груза на каждом клоне.
3. **2 stash'а** — `stash@{0}: temp-taskwork`, `stash@{1}: t1-wip`. Незакоммиченная работа,
   которая потеряется при `git stash drop`; ни в одном доке не упомянута.
4. `vks_data.db.lock` (0 байт, корень) — покрыт правилом `.gitignore:55` `vks_data.db.*`,
   не отслеживается; безвреден, но висит в корне рабочего дерева.
5. `.gitignore:24` `!docs/progress/evidence/**/*.log` — нег-паттерн работает только потому, что
   каталог не игнорируется целиком (`*.log` на `:22`); хрупко, но корректно. Правило `:55`
   `vks_data.db.*` дублирует `*.db` на `:27` — безвредное дублирование.

**Что закоммичено зря:** только `docs/progress/evidence/{t06b1/network.json,t16/http-trace.json,t24/export.zip}`
(4.1 МБ). Всё остальное крупное — либо vendor (обоснованно), либо golden (обоснованно).

---

## §6. Документация vs код — топ-10 опасных расхождений

| # | Утверждение в доке | Реальность | Опасность |
|---|---|---|---|
| **1** | `ARCHITECTURE.md:148` — «`ldap.rs # ldap3 + GSSAPI/Kerberos (FreeIPA)`»; `:375` — «Аутентификация: jsonwebtoken + **ldap3**»; R6 (обязательное) — «БД (своя или LDAP/FreeIPA)» | В `vks-auth/Cargo.toml` **нет `ldap3`**. `provider.rs:147` жёстко подставляет `crate::ldap::MockLdapClient::new()`, и `provider.rs:141` требует `allow_mock_ldap = true`, иначе `MockLdapProhibited` | 🔴 R6 «обязательное» не выполнено, а док утверждает обратное. LDAP-аутентификация в проде **невозможна** |
| **2** | `MASTER_TASK_LIST.md` — 0 вхождений слова «mock» на 976 строк при десятках «закрыт 9.5/10» | `handlers.rs:547–552` (`POST /api/v1/users/sync-ldap`) **безусловно** возвращает `MockLdapProvider::fetch()` — одного фейкового `ldap-user` — без всякого гейта | 🔴 API «синхронизация с LDAP» в проде отдаёт фикцию и рапортует `{"total":1}` |
| **3** | `vks-media-mcu/README.md:69`, `lib.rs:27` — «Real (behind `oxideav` feature): oxideav-scene codecs and the bilinear compositor» | Фича `oxideav` = `pub use oxideav_scene;`. Билинейного компоновщика нет: `compositor.rs:7` честно пишет «bilinear … sits behind the feature», но за фичей пусто | 🟠 док обещает качество картинки, которого нет; CI платит за компиляцию пустой фичи |
| **4** | `ARCHITECTURE.md:306–323` «Дерево зависимостей крейтов» (помечено «целевое», но на него ссылаются манифесты) | **10 из 15 строк не совпадают** с манифестами: `vks-common ← vks-core` (факт: 0), `vks-layout ← vks-core` (0), `vks-auth ← +vks-proto` (0), `vks-recorder ← 3 крейта` (0), `vks-cluster ← 2` (0), `vks-signaling ← 4` (факт: 1), `vks-media-mcu ← +vks-proto` (0), `vks-media-sfu` без `vks-common`, `vks-admin` без `vks-layout`, `vks-storage` без `vks-common` | 🟠 «Дерево» — главный архитектурный контракт; проверить его нельзя, не открыв 16 манифестов |
| **5** | `ARCHITECTURE.md:96–200` дерево файлов: `ports/signaling_transport.rs`, `layout/{layout,presets,mcu_renderer,client_renderer,storage}.rs`, `mcu/{decoder,encoder}.rs`, `storage/schema.sql`, `storage/src/migrations/`, `vks-proto/openapi.json` | **8 файлов не существуют**. Факт: `crates/vks-storage/migrations/` (не `src/`), `openapi.json` нет (маршрут `/openapi.json` собирается inline, `handlers.rs:254`), `vks-layout` = `{core,rect,grid,grids,focus,preset,codian,roles}` | 🟠 новому инженеру документ даёт ложную карту |
| **6** | `README.md:83` — «Тесты: **1333** (…0 сбоев, 8 ignored)» | 1425 passed (baseline), 1 432 `#[test]`-атрибута | 🟡 отставание на 92 теста (6.9 %) |
| **7** | `vks-media-mcu/src/lib.rs:31` — «workspace baseline (**298 tests**) stays green» | 1425 | 🟡 число из фазы 3 |
| **8** | `MASTER_TASK_LIST.md:11` — «Последний приёмочный прогон: **2026-10-21**» + «T01 закрыт 9.5/10, 970 passed» | baseline 2026-10-29 = 1425; после 21.10 были раунды 10-25, 10-28, 10-28-fix, 10-29 | 🟡 «последний» врёт на 8 дней и 455 тестов |
| **9** | `MASTER_TASK_LIST` T3 — «Глубокая очистка … **10.0/10**»: «удалены `scripts/__pycache__`, `vks_data.db(.lock)`, `vks_server.log`, `artifacts/`»; S30 — то же | Все четыре **на месте** (`scripts/__pycache__` 68 K, `vks_data.db` 3.2 M, `vks_server.log` 1.3 M) | 🟡 очистка регенерируемых артефактов выдана за достижение; оценка 10.0 за недолговечное состояние |
| **10** | `deploy/README.md:197` — «bench harness passes the admin token to the farm endpoint via **`VKS_API_TOKEN`**/`/tmp/vks.token`»; `configs/default.toml:8` — «All parameters can be overridden via env `VKS_*`» | `VKS_API_TOKEN` в коде **отсутствует**; `/api/v3/mcu/farm` требует JWT + `Moderator` (`lib.rs:188–194`). «All parameters» — 17 из 66 | 🟠 оператор настроит несуществующий токен и решит, что доступ выдан |

Дополнительно (не в топ-10, но в план): `vks-signaling/src/lib.rs:13` — «Дерево: vks-core,
vks-proto, vks-common, vks-auth», фактически один `vks-proto`; `vks-admin/Cargo.toml:2` — «Дерево:
…vks-storage», фактически `vks-storage` только в `[dev-dependencies]`; `vks-media-mcu/Cargo.toml:16`
`description = "… (oxideav-scene)"` — крейт не использует oxideav-scene.

---

## §7. Дубли источников истины

| # | Понятие | Сколько мест | Где должно быть одно |
|---|---|---|---|
| S1 | **Режим комнаты `room_mode`** | 6 представлений: `vks_core::RoomMode` (`room.rs:77`), proto `media::RoomMode` (3 значения, `media.proto:8`), строка `Config.default_room_mode` (`config.rs:24`), `AppState.mode.global_mode` (`state.rs:105`), per-room `Option<RoomMode>` в storage (`postgres.rs:208–246`, `memory.rs:132`), `"auto"` в payload раскладки (`shell.rs:946`, `Config::validate` его **отвергнет**) | `vks_core::RoomMode` как домен + конверсия в proto в одном месте; `"auto"` — отдельный `LayoutMode`, не `RoomMode` |
| S2 | **Дефолты конфигурации** | `Config::default()` + `configs/default.toml` + `configs/prod.example.toml` + 65 `unwrap_or(...)` в коде | Один файл (TOML) как единственный источник; `Default` — производный |
| S3 | **Порты** | 6 мест (§4.3), из них 41000–41200 — hardcode | `Config` + `deploy/vks.env.example`; RTP-пул — в конфиг |
| S4 | **Размер холста раскладки** | `1280×720` (`preset.rs:633–634,704–705` — эталон геометрии), `1920×1080` (`compositor.rs:19–20` `CANVAS_SIZE`), нормализованные `[0,1]` (`rect.rs:131`), «единицы сетки» (`GridSlot`, `preset.rs:947`) | Один `Canvas` (`rect.rs:20`) как валюта; пиксельные пресеты — производные от него |
| S5 | **Типы прямоугольника** | 5: `Rect{x,y,w,h:u32}` (`rect.rs:57`), `NormalizedRect` (`rect.rs:131`), `SlotRect{x,y,width,height:f64}` (`preset.rs:263`), `GridSlot` (`preset.rs:947`), `PixelRect{x,y,w,h,z}` (`layout_bridge.rs:26`) | `Rect` + `NormalizedRect` + `z` там, где нужен; `SlotRect`/`GridSlot`/`PixelRect` — алиасы/конверсии |
| S6 | **Лимиты частоты** | `vks-common/ratelimit.rs:12–13` (**не используется**), `api_v3/ws.rs:38` `100/с`, `gateway_video.rs`/`farm.rs` лимиты по env | `ratelimit.rs` оживить или удалить; лимиты — в `Config` |
| S7 | **Кодек-матрица** | ✅ почти хорошо: `vks-common/codec_capabilities.rs` — единый источник (`rtp_bridge.rs:482,574,588`, `uas.rs:193`). Но env-контракт `VKS_SDP_CODEC_MATRIX{,_LAB}`/`VKS_TRANSCODER_ENABLED` парсится **дважды** (`rtp_bridge.rs:563–567` и `shell.rs:277–279`) | Один геттер `MatrixMode::from_env()`; `shell.rs` вызывает его |
| S8 | **systemd-юниты** | 2 файла (`vks-mcu-prod.service`, `vks-mcu.service`) дублируют `Environment=` для портов/флагов | Один unit + drop-in'ы |
| S9 | **Переключение режима** | `vks-core/transparency.rs` (447 стр., только тест) **и** `vks-admin/mode.rs` + `state.rs` (прод) — две реализации одного инварианта ARCHITECTURE §2 | `transparency.rs` удалить, инвариант проверять на прод-пути |
| S10 | **Версия/сборка** | `env!("CARGO_PKG_VERSION")` (`shell.rs:293`), `vks_common::metrics::BUILD_INFO` (`metrics.rs:46,249`), версия фронтенда `2.2.1.66` в 30+ именах чанков, `MANIFEST.sha256` | ок: три источника не противоречат (crate-версия, метрика, имя бандла) |

---

## §8. Безопасность и гигиена

### 8.1. 🔴 Mock-LDAP = обход аутентификации в одном env-флаге

`crates/vks-auth/src/ldap.rs:94–104`:

```rust
match &*self.accept.lock().unwrap() {
    Some(expected) if expected == password => Ok(…),
    Some(expected) => Err(…),
    None => Ok(LdapBindResult { dn, groups: vec!["vks-users"] }),   // ← любой пароль
}
```

`provider.rs:147` создаёт клиент через `MockLdapClient::new()` — т.е. `accept == None` **всегда**.
Комбинация `VKS_ALLOW_MOCK_LDAP=1` + `auth_provider="ldap"` (`main.rs:223`) даёт: **любой
логин/любой пароль → успешная аутентификация с группой `vks-users`**. Гейт по умолчанию закрыт
(`provider.rs:141` → `MockLdapProhibited`), но:
- гейт не покрыт `Config::validate()` (проверяется только в фабрике);
- в `deploy/vks.env.example` флаг **не документирован** (§4.2), значит оператор не знает о нём
  и не знает, что его нельзя включать;
- сообщение об ошибке не пишет, что включение = «любой пароль принимается».

**Правка (P0):** `MockLdapClient` должен иметь `accept: Some(<ожидаемое>)` или возвращать
`LdapError::Client("mock client has no credentials source")` на `None`; `allow_mock_ldap`
проверять в `Config::validate()` (нельзя `true` при `allow_insecure_defaults = false`).

### 8.2. 🟠 `/api/v1/users/sync-ldap` — mock без гейта

`crates/vks-admin/src/handlers.rs:547–552`: `let ldap = MockLdapProvider::fetch();` — безусловно.
Эндпоинт под JWT+RBAC (это плюс), но возвращает фиктивного пользователя и рапортует успех.
**Правка:** 501 `Not Implemented` с явным телом или под `allow_mock_ldap`.

### 8.3. Что проверено и **в порядке**

- `/metrics` — `metrics_guard` (`lib.rs:196`, логика `:225–250`): без токена только loopback,
  с `VKS_METRICS_TOKEN` — обязателен для всех. Токен **не логируется**.
- `m5_api` (`gateways/status`, `mcu/farm`, `cluster/status`) — `token_middleware` + `Moderator+`
  (`lib.rs:188–194`, `require_moderator:217–233`), fail-closed при отсутствии `AuthContext`.
- `/api/v1/*` — `jwt_auth_middleware` + `v1_rbac_middleware` (`handlers.rs:758–760`).
- `/api/v3/*` — `token_middleware` + `fail_closed_middleware` (`api_v3/mod.rs:468,489`).
- `/healthz`, `/readyz` — публичные, отдают только `ok`/`503` (правильно).
- Секреты в логах: `tracing::*` не печатает токены/пароли; `Config: Debug` редактирует
  (`config.rs:170–190`), DSN и секреты маскируются (`redact_dsn:100`, `redact_secret`), поля выведены через них в `Debug` (`config.rs:170–190`); тесты `config.rs:449,528`.
- SQLite `vks_data.db` — **не** в git (`*.db`), 0 отслеживаемых `.db`.
- Dev-секрет `dev_ec99…` в `configs/default.toml:23` — осознанно, вынесен в `DEV_JWT_SECRET`
  и **отвергается** при `allow_insecure_defaults = false` (`config.rs:307`, тест `:641`).

### 8.4. Остаточные риски (не P0)

| Риск | Место | Комментарий |
|---|---|---|
| `?token=` в query WS | `lib.rs:400–411` | токен попадает в access-логи nginx/прокси и `Referer`; браузерный путь оправдан, но нужен `Cache-Control: no-store` на 401 и запрет логирования query |
| `/openapi.json` публичен | `handlers.rs:731` | раскрывает полный список путей и методов; для админ-API — лишнее, перенести под auth или отключить в prod |
| SIP UAS `0.0.0.0:5060` без TLS/auth | `main.rs:415–416`, `deploy/README.md:56` | задокументировано, off по умолчанию — приемлемо |
| H.323 plaintext | sidecar | задокументировано |
| `0.0.0.0` по умолчанию | `configs/*.toml:12`, `lib.rs` | заявлено «за reverse-proxy» — приемлемо при дисциплине |
| TLS отсутствует в сервере | — | осознанно (reverse-proxy), но нет ни строчки про `Strict-Transport-Security` для `/pnm/` |
| `VKS_H323_TRACE` недокументирован | `uac.rs:290`, `uas.rs:144` | при `=1` в лог идёт ASN.1-трейс вызова — потенциально PII/номера |

---

## §9. «Меньше кода» — можно ли было элегантнее

Ответ: **да, но не драматически.** Мёртвого/дублирующего продакшн-кода ~1 882 строки = **3.0 %**
от 62 955 строк prod-LOC (тесты — 33 844 строки, 34 % workspace'а). Крупные модули скорее
«раздутые», чем «лишние». Ниже 11 предложений, отсортированы по (экономия / риск).

| # | Предложение | Экономия | Риск | Как проверить |
|---|---|---|---|---|
| **L1** | Удалить `vks-core/src/transparency.rs` + `lib.rs:87`; тест `network_integration.rs:728–750` перенести на прод-путь `ModeService`/`ModeState` (он и так его проверяет) | **−447** | низкий (0 прод-потребителей) | `cargo test -p vks-server --test network_integration` |
| **L2** | Удалить `vks-common/{qoe,slo,ratelimit}.rs` + 3 re-export; если лимиты нужны — оживить `RateLimiter` в `api_v3/ws.rs` вместо своего счётчика | **−504** | низкий | `cargo test --workspace`; grep 0 ссылок |
| **L3** | Удалить `vks-gateway-h323/src/ffi.rs` + re-export (`lib.rs:40`); прод-путь — Python-sidecar, panic-hook/watchdog к нему не относятся | **−279** | низкий | grep `H323PlusBridge` = 1 объявление |
| **L4** | `MockWs`/`MockWt`/`MockSignaling`/`MockPeer` — под `#[cfg(test)]` или отдельный `testing`-модуль с `#[doc(hidden)]` | **−254 из release** | низкий | `cargo build --release` + `cargo test` |
| **L5** | Свести `Config::default()` к `include_str!("../../configs/default.toml")`-парсингу (или удалить `Default` и требовать файл); дефолты остаются в TOML | **−40** и −5 расхождений (§4.1) | средний (fail-closed тесты завязаны на `Config::default`) | `cargo test -p vks-common`, `--test config_failclosed` |
| **L6** | Один типизированный `RuntimeSettings` (десериализуется из тех же TOML/env через figment), вместо 65 `env::var(..).unwrap_or(..)` | **−120…−160** | средний-высокий (меняет пути чтения env) | тесты `env_override_*` + новый тест «все env-ключи из примера парсятся» |
| **L7** | Хелпер `fn route2(router, path, mr)` вместо 19 ручных пар `"/api/v3/x"` + `"/api/v3/x/"` (`api_v3/mod.rs`) | **−19…−40** | низкий | `cargo test -p vks-admin` (алиасы покрыты) |
| **L8** | Свести `SlotRect`/`GridSlot`/`PixelRect` к `Rect`+`Canvas` (или к `NormalizedRect` с одним `to_px`) | **−80…−120** | **высокий** (golden-фикстуры `grids_geometry_1280x720.json`, 54 Codian) | `cargo test -p vks-layout` + `codian_*_golden` |
| **L9** | MP4-писатель: **либо** подключить (`RecorderManager::start` → `enable_container`) — +5 строк, **либо** удалить `container.rs` + `enable_container` | −633 (если удалить) | средний (R12 «запись» в проде = метаданные) | `cargo test -p vks-recorder`; решить продуктово |
| **L10** | Удалить legacy-чанки PNM (32 файла, 6.29 МБ) — это не строки, это байты; требует ADR-ревизии §«legacy 1:1» | −6.29 МБ git | средний (старые браузеры) | `MANIFEST.sha256` перегенерировать |
| **L11** | `docs/progress/evidence`: 18 `.log` + 2 raw-trace + `export.zip` (4.1 МБ) → `SUMMARY.md` + артефакт релиза | −4.1 МБ git | нулевой (история остаётся) | `git rm --cached` + `.gitignore` |

**Реалистичный итог:** −1 500…−1 600 строк Rust (**−2.4 %** prod-LOC), −19 route-регистраций,
−6.3 МБ фронтенда, −4.1 МБ evidence, −65.6 ГБ локального диска. Поведение не меняется ни в одном
пункте, кроме L9 (продуктовое решение) и L10 (браузерная матрица).

**Где код раздут по существу, а не по мёртвости** (кандидаты на декомпозицию, не на удаление):

- `crates/vks-server/src/gateways.rs` — **6 337 строк** (3 853 прод + 2 484 теста) в одном файле:
  `mod aac`/`audio`/`video` внутри, SIP+H.323+порт-пул+ферма. Естественное деление — 3–4 файла.
- `crates/vks-admin` — **29 915 строк** (31 % всего Rust) на 38 файлов; `sessions.rs` 2 449 +
  `resources.rs` 2 546 + `tests.rs` 3 145. Для admin REST это много: ~210 маршрутов
  (`api_v3/mod.rs`) и много «алиасов вендора».
- `crates/vks-admin/src/api_v3/mod.rs:100–113` — `normalize_stray_brace_middleware`: 14 строк
  на исправление опечатки вендора `}/contacts`. Load-bearing, но заслуживает комментария
  «удалить при обновлении PNM».

---

## §10. CI-дыры

`.github/workflows/ci.yml` (11 джоб). Что **не** проверяется и что фиктивно:

| # | Дыра | Доказательство | Правка |
|---|---|---|---|
| C1 | **`test-external` полностью фиктивна** для MCU: `continue-on-error: true` + `--features oxideav`, за которой **нет кода** (D12). Обоснование в комментарии «требует GPU/видеоустройств» ложно — фича ничего не включает | `ci.yml:146–169` | убрать `vks-media-mcu --features oxideav`; для SFU оставить, но снять `continue-on-error` (str0m тесты сеть не требуют — они in-memory) |
| C2 | **`--all-features` только check/clippy, не test.** `vp9`/`av1` (кодек-реестр) проверяются компиляцией, но их тесты не идут. `str0m` — исключение: он включён через `vks-server/Cargo.toml:20` и потому тестируется унификацией фич в `cargo test --workspace` | `ci.yml:53` vs `:110` (`cargo test` без фич) | добавить `cargo test -p vks-media-sfu --all-features` (дёшево: 2 константы) |
| C3 | **`doc` — `continue-on-error`** с ~30 битыми intra-doc-ссылками; «гейт станет жёстким после починки» — висит с 10-23 | `ci.yml:216–223` | починить ссылки (или `#![allow(rustdoc::broken_intra_doc_links)]` с TODO) и снять флаг |
| C4 | **Один и тот же python-тест дважды**: шаг «Unit tests (PNM patch lib)» есть и в `sidecar`, и в `frontend-integrity` | `ci.yml:251` и `:270` | удалить дубль |
| C5 | **6 полных компиляций workspace за прогон**: `check`, `clippy`, `test`, `test-serial` (весь набор заново, `--test-threads=1`), `build --release`, `path-with-spaces` (полная сборка в каталоге с пробелом) | `ci.yml` джобы | `test-serial` → только `-p` с известными гонками (или `-- --test-threads=1` на 3 пакета); `path-with-spaces` → `cargo check -p vks-proto` вместо `build --workspace` |
| C6 | **Нет проверки JS-синтаксиса** патч-слоя: `codian-layouts.js` (68 K, 55 функций), `theme-toggle.js`, `perf-overrides.js` — только `sha256` + idempotency патчей | `ci.yml:255–270` | `node --check` на 3 файла (5 строк) |
| C7 | **Нет гейта на неиспользуемые зависимости/мёртвый `pub`** — 10 лишних манифест-строк (§3.1) и 1 230 строк мёртвого кода прошли все 6 ревизий | — | `cargo udeps` (nightly, отдельная allowed-to-fail джоба) или скрипт «grep pub-символов»; минимум — `cargo machete` |
| C8 | **Кэш CI не учитывает `rustflags`/toolchain** ⇒ `target-cpu=native` меняется без инвалидации | `key: hashFiles('**/Cargo.lock')` во всех джобах | `hashFiles('**/Cargo.lock', '.cargo/config.toml', 'rust-toolchain.toml')` |
| C9 | **`deny.toml`** содержит 5 лицензий, которых нет в графе (OpenSSL, Zlib, CC0, MPL, Unicode-DFS) — шум, маскирующий реальные warning'и | `deny.toml:31–43` | урезать до фактического union'а |
| C10 | **wasm только `check`**, без `test`; `rust-toolchain.toml` объявляет `wasm32` компонент, но тесты SDK под wasm не идут | `ci.yml:73–90` | `cargo test -p vks-client-sdk --target wasm32` невозможен без `wasm-bindgen-test` — зафиксировать как известное ограничение, а не молчать |
| C11 | **E2E отсутствует** (`# E2E (chromiumoxide) добавляется в фазе 8`), при этом 7 Playwright-сьютов в `tests/e2e/` существуют и не запускаются в CI | `ci.yml:304` | добавить job с `npx playwright test` (артефакты уже в репо) |

Что CI делает **хорошо** (не трогать): `--locked` везде, `--no-fail-fast`, PG-service с healthcheck,
checkout в каталог с пробелом, `cargo-deny --all-features`, pinned runner `ubuntu-24.04`,
`RUSTDOCFLAGS=-D warnings`.

---

## §11. План гигиены по приоритетам

**P0 — правдивость и безопасность (0.5 дня, без риска для поведения)**

1. `MockLdapClient::bind` при `accept == None` → ошибка (§8.1); `allow_mock_ldap` в `Config::validate`.
2. `POST /api/v1/users/sync-ldap` → 501 или под `allow_mock_ldap` (§8.2).
3. В `MASTER_TASK_LIST.md` — раздел «Mocks in production» с 5 строками: LDAP-аутентификация,
   `sync-ldap`, H.323 FFI, oxideav-композитор, MP4-контейнер. Без этого «9.5/10» — дезинформация.
4. `ARCHITECTURE.md §3` — привести дерево зависимостей и файловое дерево к факту (§6.4, §6.5).

**P1 — удаление мёртвого кода (1 день, три коммита, каждый обратимый)**

5. Коммит 1: `transparency.rs` + re-export + перенос теста на прод-путь (L1).
6. Коммит 2: `qoe.rs`/`slo.rs`/`ratelimit.rs` + re-export'ы (L2).
7. Коммит 3: `ffi.rs`, `duration_secs`, `layout_kind`, `snapshot_json` (L3, D8–D10);
   моки — под `#[cfg(test)]` (L4).
8. 10 неиспользуемых зависимостей (§3.1) + `rand` 0.8→0.9 в `vks-auth` (§3.2).

**P2 — единый источник истины конфигурации (2 дня, средний риск)**

9. `Config::default` ← TOML (L5); убрать `serde(default)` или сделать его производным.
10. `RuntimeSettings` для 22 недокументированных env (L6); добавить в `deploy/vks.env.example`
    **все** ключи; RTP-пул 41000–41200 — в конфиг (§4.3).
11. `rustflags` → `x86-64-v3` + ключ кэша CI (§4.4).

**P3 — артефакты и CI (0.5 дня)**

12. `cargo clean` для `target-impl`/`target-proto` (решение владельца — 10.6 ГБ);
    `docs/audit/2026-10-25/measure` — 232 МБ (уже игнорируется, можно удалить локально).
13. `git rm --cached` для 2 raw-trace + `export.zip`; legacy-чанки — по ADR (L10, L11).
14. CI: C1, C2, C4, C6, C8 (5 точечных правок).

**P4 — структурная элегантность (по желанию, отдельный ADR)**

15. `gateways.rs` 6 337 строк → 4 файла; `Rect`-унификация (L8, высокий риск — только с golden-тестами);
    MP4-контейнер — продуктовое решение (L9).

**Чего НЕ трогать (доказанно load-bearing):** `vks-cluster` (используется `server/src/cluster.rs`),
`vks-recorder` `recording.rs`/`storage.rs` (метаданные записи живые), `vks-layout/codian.rs` +
`grids.rs` (54+65 раскладок, golden-фикстуры), `scripts/patch-pnm-*.py` (идемпотентность
проверяется CI), `normalize_stray_brace_middleware` (обход опечатки вендора), `vks.env.example`
(единственный SSOT лимитов H.323), `MANIFEST.sha256` (214/214 OK).

**Риски плана:** L6/L8 могут задеть fail-closed и golden-геометрию — делать отдельными коммитами
с прогоном `cargo test --workspace` + `--test config_failclosed` + `codian_*_golden`.
L9/L10 меняют продуктовое поведение — только с явным решением владельца.

---

## §12. Оценка

| Разрез | Оценка | Обоснование |
|---|---|---|
| Мёртвый код Rust | **7.5/10** | 7 `allow(dead_code)`, 1 TODO — образцово; но 1 230 строк мёртвых модулей и фиктивная фича |
| Мёртвый код фронтенда | **8.0/10** | `constructorCount`/`onConstructorActivate` уже удалены (H-04/H-05 закрыты), MANIFEST 214/214; минус — 6.29 МБ legacy и 0 проверки JS в CI |
| Зависимости | **7.0/10** | 10 неиспользуемых строк, `rand`×3, `getrandom`×3; `cargo-deny` — сильный |
| Конфиги/env/порты | **5.5/10** | 66 env vs 17 полей, 5 расхождений дефолтов, RTP-диапазон невидим оператору |
| Артефакты/git | **5.0/10** | 51.8 МБ чисто, но 65.6 ГБ локально, 232 МБ measure, 2 stash'а, 4.1 МБ trace в git |
| Документация vs код | **3.5/10** | 10 расхождений, 3 из них 🔴; ARCHITECTURE §3 описывает несуществующий репозиторий |
| Дубли SSOT | **5.0/10** | 10 дублей, включая 5 типов прямоугольника и 6 представлений `room_mode` |
| Безопасность | **7.0/10** | эндпоинты закрыты, секреты редактируются; минус — mock-LDAP = обход аутентификации |
| «Меньше кода» | **7.0/10** | 3.0 % мёртвого prod-кода — хороший результат для 96.8k строк; но 31 % LOC в одном крейте и 6.3k строк в одном файле |
| CI | **6.5/10** | 11 джоб, `--locked`, PG, path-with-spaces — сильно; минус 2 `continue-on-error`, 1 фиктивная джоба, нет all-features test, дубль python-шага |
| **ИТОГО** | **6.0/10** | код опрятный, документация — нет; гигиена процессов (ревью diff, а не дерева) — главный дефект |

---

## §13. Спорные тезисы

### Т.1. «`/metrics`, `/mcu/farm`, `gateways/status`, `cluster/status` анонимны» — **неверно на HEAD**

**Доказательство:** `crates/vks-server/src/lib.rs:188–194` — `m5_api` под
`route_layer(token_middleware)` + `route_layer(require_moderator)`; `require_moderator:217–233`
возвращает 401 при отсутствии `AuthContext` и 403 при роли < `Moderator`. `/metrics` —
`route_layer(metrics_guard)` (`:196`), логика `:225–250`: с `VKS_METRICS_TOKEN` токен обязателен
даже для loopback. Тест `unauthenticated_v1_request_fails_with_401` (`handlers.rs:896`) фиксирует
401 для v1.
**Что переубедит:** `curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:8080/api/v3/mcu/farm`
без заголовков → 200 (а не 401). Если стенд отдаёт 200 — значит на нём старый бинарь, и это
отдельная находка о рассинхроне «дерево ↔ задеплоено».

### Т.2. «Мёртвый код — разрозненные хвосты, а не модули» — **неверно: 1 230 строк в пяти файлах**

**Доказательство:** §2, D1–D5: 1 230 строк в 4 файлах с нулём продакшн-потребителей, каждый
Re-export'ится через `lib.rs` (то есть выглядит как часть API). `ModeSwitcher` (447 строк)
вызывается **только** из `tests/network_integration.rs:738`.
**Что переубедит:** `grep -rn "ModeSwitcher\|QoeAggregator\|SloEvaluator\|RateLimiter\|H323PlusBridge"
crates --include=*.rs | grep -v "src/"` → пусто или только тесты. Если найдётся прод-вызов
в `crates/vks-server/src/**` — я ошибся в карте вызовов.

### Т.3. «"Закрыто 9.5/10" в трекере несовместимо с mock'ами в проде»

**Доказательство:** `MASTER_TASK_LIST.md` — 976 строк, **0** вхождений «mock»; при этом
`handlers.rs:549` (mock-LDAP без гейта), `provider.rs:147` (mock-LDAP-аутентификация),
`lib.rs:65` (пустая фича «реального компоновщика»), `recording.rs:231` (`enable_container`
без вызовов). Шесть ревизий 9.5–10.0 (T01–T10a, D1–D6) эти файлы не пометили.
**Что переубедит:** ссылка на строку в `MASTER_TASK_LIST.md` или ADR, где сказано «LDAP —
mock, R6 не выполнен»; либо `git log -S "MockLdapProhibited"`, показывающий, что гейт добавлен
именно как осознанная замена LDAP с записью в трекере.

### Т.4. «Документация отстала на один коммит» — **неверно, она никогда не соответствовала коду**

**Доказательство:** `ARCHITECTURE.md:148` требует `ldap3`, которого нет в `Cargo.toml` с первого
коммита крейта; `:318` «`vks-signaling ← vks-core, vks-common, vks-auth`» — в манифесте
`vks-signaling` только `vks-proto`; 8 файлов из дерева §3 не существовали ни в одном коммите
(`git log --diff-filter=A -- '*signaling_transport.rs'` пуст).
**Что переубедит:** `git log --oneline -- crates/vks-core/src/ports/signaling_transport.rs`
с непустым выводом (файл существовал и был удалён) — тогда это «отставание», а не фикция.

### Т.5. «`Config::default()` — безопасный дефолт» — **он не безопасный и не согласован**

**Доказательство:** `config.rs:197` `listen_addr = "0.0.0.0:443"` при `configs/default.toml:12`
`8080`; `:207` `media_listen_addr = "127.0.0.1:0"` при `:13` `0.0.0.0:40000`; `:211`
`ice_servers = []` при `:47` STUN. `validate()` все четыре пропустит — это валидные значения.
`main.rs:64–73`: в **release** `config_path == None` достижим (нет ни `--config`, ни
`VKS_CONFIG_PATH`, `cfg!(debug_assertions) == false`) → `Config::default()` уходит в прод.
**Что переубедит:** запрет запуска без явного конфига в release (сейчас его нет) или
`validate()`, отвергающий `listen_addr` с портом 443/`media_listen_addr` на loopback при
`allow_insecure_defaults = false` — тогда дефолт перестаёт быть достижимым в проде.

### Т.6. «41000–41200 не проблема, это внутренний диапазон» — **это молчаливый отказ SIP**

**Доказательство:** `rtp_bridge.rs:22` `Self::new(41000, 41200)`, hardcode, без env/конфига;
`deploy/README.md` (268 строк) не содержит «41000»; `RtpPortPool::allocate_pair` возвращает
`Option` (`:47`), вызывающий `gateways.rs:2640` пишет `let Some(...) else { … }` — отказ
обрабатывается как «нет портов», а не «порт закрыт firewall'ом».
**Что переубедит:** строка в `deploy/README.md` или `configs/*.toml`, документирующая диапазон
RTP-моста, **и** конфигурируемость (env/Config). Пока её нет — оператор, открывший 43000–43100,
получит неработающий SIP.

### Т.7. «Тесты — половина кода, поэтому проект раздут» — **неверно, 34 %, и это не проблема**

**Доказательство:** prod ≈ 62 955 строк, тесты ≈ 33 844 (34 %); 1 432 `#[test]`-атрибута на
1 425 passed + 8 ignored. Для проекта с 16 крейтами, FFI-границей, протоколами и геометрией
это **нормальная** пропорция, а не раздувание.
**Что переубедит:** доля тестов, которые не проверяют поведение (snapshot-тесты на себя), > 20 %
от набора — тогда «много тестов» = «мало сигнала» и цифру можно резать.

### Т.8. «`target-cpu=native` — осознанный выбор стенда, не гигиена» — **он уже влияет на CI**

**Доказательство:** `.cargo/config.toml:31` применяется ко **всем** сборкам, включая CI-джобу
`build --release` (`ci.yml:283–295`) с кэшем `hashFiles('**/Cargo.lock')` — `rustflags` вне ключа.
Один и тот же commit+lock на двух runner'ах даёт разные ISA-инструкции; артефакт CI формально
непереносим, а R-26 («sha256 воспроизводим») выполняется только на одной машине.
**Что переубедит:** `sha256sum` release-бинаря с двух машин с разными CPU при одном commit+lock —
если совпал, значит `native` фактически не влияет (например, из-за `lto`/`codegen-units`), и
претензия снимается.

### Т.9. «Фича `oxideav` безвредна — она просто пустая» — **она стоит денег и лжёт в доках**

**Доказательство:** `vks-media-mcu/Cargo.toml:44` `oxideav = ["dep:oxideav-scene"]` тянет
внешний крейт; `lib.rs:65–66` — единственное `cfg(feature="oxideav")`; CI-джоба `check`
(`--all-features`) компилирует его на каждом прогоне; `README.md:69` и `lib.rs:27` обещают
«real codecs and the bilinear compositor».
**Что переубедит:** `grep -rn "oxideav_scene::" crates | grep -v "lib.rs"` с непустым выводом —
тогда фича что-то включает, и D12 снимается.

### Т.10. «Ревью 9.5–10.0 означает, что дерево в порядке» — **оно означает, что в порядке diff**

**Доказательство:** D6 в `MASTER_TASK_LIST.md` — «10.0/10 Minimization & Elegance, Unconditional
Approval» по фронтенду; в тот же период в дереве жили `transparency.rs` (447), `qoe/slo/ratelimit`
(504), `ffi.rs` (279), мёртвый MP4-писатель (633) и 10 неиспользуемых зависимостей — всё это
не входило в diff'ы задач и потому не было замечено ни одним ревью.
**Что переубедит:** протокол ревью, в котором есть шаг «grep каждого нового `pub`-символа вне
крейта» и «`cargo tree -d` до/после» — если такой протокол существует и эти файлы были в скоупе,
мой вывод неверен.

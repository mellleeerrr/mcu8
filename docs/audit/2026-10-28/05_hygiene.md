# 05 — Гигиена: мёртвое/живое, конфиги, зависимости, версии, CI

**Агент:** `hygiene` · **Дата:** 2026-10-28 · **Режим:** read-only (единственная запись — этот файл)
**Стенд:** PID 2965299 жив, не перезапускался · `vks_data.db` не менялась · `cargo build --release` не запускался
**Предшественник:** `docs/audit/2026-10-25/05_hygiene_regressions.md` (K-01…K-12). Ниже — только **дельты**,
новые измерения и то, что я **опроверг** у себя же; повторно найденное помечено `[=K-xx]`.

## 0. Блокер рантайм-выводов (читать первым)

| Факт | Доказательство |
|---|---|
| Живой сервис крутит бинарь **25 окт 20:37**, старт **26 окт 11:20:20 UTC** (= 14:20 MSK) | `ls -la target/release/vks-server` → `27178544 Oct 25 20:37`; `ps -o lstart= -p 2965299` → `Пн окт 26 14:20:19`; `grep '2026-10-26T11:20:20' vks_server.log` → `configuration applied` |
| В рабочем дереве **7** `.rs` новее бинаря | `find crates -name '*.rs' -newer target/release/vks-server` → `vks-media-mcu/src/{lib,engine}.rs`, `vks-layout/src/grids.rs`, `vks-layout/tests/{grids,grids_geometry_golden}.rs`, `vks-server/src/{gateway_video,gateway_audio}.rs` |
| Незакоммичено **21 файл, +4510/−846** | `git diff --stat` |
| `/proc/2965299/exe` → `/home/pro/mcu/…`, cwd → `/home/pro/Рабочий стол/mcu` | симлинк `/home/pro/mcu → /home/pro/Рабочий стол/mcu` (создан 21 окт 19:39) — неявная зависимость dev-юнита `[=K-05]` |

**Следствие:** всё, что наблюдаемо в рантайме (раскладки, farm-метрики, RTCP, `gateway_*`), относится
к коду **до** правок 26–27 окт. Любой вывод «в рантайме X» из этого аудита — про устаревший бинарь.
Пересборка `cargo build --release --locked -p vks-server` + sha256 + повторная проверка
`/presets`, save/reload, звонков — обязательный предусловие приёмки.

## 1. Удалить / оставить (с доказательством)

### 1.1 Удалить — безопасно, доказано отсутствие вызовов

| # | Объект | Объём | Доказательство | Риск |
|---|---|---|---|---|
| D1 | `client/pnm-frontend/deployed/pnm/codian-layouts.js:104` `var constructorCount = 0;` | 1 стр. | `grep -c constructorCount` = **1** (только объявление). В HEAD их 6 (`git show HEAD:…` → 103,608,614,615,632,965); в незакоммиченном рефакторе роль перешла к `counterValue` (WT:109,539,1111,1130; `+ counterValue = count;` в diff) → объявление **забыто**, восстанавливать использование не нужно | нет |
| D2 | `codian-layouts.js:1378–1383` `onConstructorActivate()` | 6 стр. | Единственное вхождение в **всём репо** (`grep -rn` по `client/ tests/ scripts/ docs/ deploy/ configs/` + `--include=*.js --include=*.html --include=*.py --include=*.sh --include=*.md`) — само объявление. Нет `addEventListener(..., onConstructorActivate)`; клик идёт через document-capture `onVendorModeClick` (:1819), клавиатура — `onConstructorKeydown`, привязан в :1330. Мёртв **и в HEAD** (там на :1030) | нет |
| D3 | `crates/vks-cluster/Cargo.toml`: `tracing`, `uuid` | 2 стр. | `grep -rn 'tracing\|uuid' crates/vks-cluster/src/` → **0** вхождений (всего 1868 стр.) | нет |
| D4 | `crates/vks-recorder/Cargo.toml`: `serde`, `tracing` | 2 стр. | `grep` по `crates/vks-recorder/src/` → **0** вхождений (1627 стр.) | нет |
| D5 | `crates/vks-server/Cargo.toml`: `bytes` | 1 стр. | `grep -rn 'bytes::' crates/vks-server/src/` → **0** (совпадения только в именах полей `audio_rx_bytes`, `gateways.rs:186`) | нет |
| D6 | `crates/vks-media-sfu/Cargo.toml`: `uuid` | 1 стр. | `uuid::` в `src/` → **0**; `uuid` приходит транзитивно через `vks-core` (`RoomId::from_uuid`) | нет |
| D7 | `client/vks-client-sdk/Cargo.toml`: `tracing`, `serde` | 2 стр. | `grep -rn 'serde\|tracing' client/vks-client-sdk/src/` → **0** на 2294 строках | нет |
| D8 | `crates/vks-common/Cargo.toml`: `tracing` | 1 стр. | Крейт использует только `tracing_subscriber` (`src/tracing.rs:9,31,35,39`); имя `tracing` занято **собственным** `pub mod tracing` (`src/lib.rs:21`), т.е. extern-crate недостижим по имени | нет |
| D9 | `crates/vks-media-mcu/Cargo.toml`: `serde` → перенести в `[dev-dependencies]` | 1 стр. | В `src/` 0 вхождений; используется только `tests/mcu_tick_soak.rs:27,229,231` (`use serde::Serialize`) | нет |
| D10 | `crates/vks-proto/openapi.json` | 6417 Б | Ни `include_str!`, ни чтения из build.rs (`crates/vks-proto/build.rs` компилирует только 4 `.proto`); в Rust/TOML/sh ссылок нет. `GET /openapi.json` отдаёт стаб из `vks-admin/src/handlers.rs:254` `[=K-08]` | низкий: возможно, задуман как спека для будущего `utoipa` |
| D11 | `target-impl/` + `target-proto/` | **10.6 ГБ** | Оба содержат `CACHEDIR.TAG` (cargo-маркер кэша) и полный `debug/` `release/` `.fingerprint/` с бинарями `vks-server` от 25 окт 18:05/19:37 → альтернативные `CARGO_TARGET_DIR` прогоны, воспроизводимы. **Не в `.gitignore`** (см. C-16) | средний: evidence 25 окт ссылается на *бинари*, не на каталоги — сверить `docs/audit/2026-10-25/06_perf_lab.md`/`measure/` перед `rm -rf` |
| D12 | `target/debug` | **44 ГБ** (из 50 ГБ всего `target`) | `du -sh target/*` → `debug 44G`, `release 5.6G`, `wasm32 225M`, `doc 57M` | низкий: удалять **после** финальной release-верификации; `target/release/vks-server` не трогать |

Итого по D3–D10: **~11 строк манифестов + 6.4 КБ + 12 зависимостей** меньше в графе.
Метод: `cargo machete`/`cargo udeps` на стенде **отсутствуют** (`cargo machete --version` → `no such command`),
поэтому это grep-эвристика + ручная проверка каждой строки; требуется подтверждение `cargo +nightly udeps`
на внешнем раннере (в отчёте — в §7 как непроверенное).

### 1.2 Удалить/решить — но НЕ молча (сначала решение Lead)

| # | Объект | Объём | Почему не «просто удалить» |
|---|---|---|---|
| S1 | `oxideav-scene` + фича `oxideav` + re-export | 4 стр. (`media-mcu/Cargo.toml:40,44`, `lib.rs:67-68`) + 1 CI-джоба | В коде **только** `pub use oxideav_scene;`; `grep -rn 'oxideav_scene::' .` → **0**. Т.е. «реальный композитор» из ADR-0004 не написан. **Но:** `[=K-11]`, трекер S15 уже фиксирует «`oxideav` — no-op фича», в lock 12 крейтов `oxideav-*`. Рекомендация: **оставить как reserved, но (а) убрать джобу `test-external`/`--features oxideav`, (б) пометить `reserved // ADR-0004 not implemented`, (в) добавить CI-инвариант `cargo tree --all-features -i oxideav-scene` → 1 узел.** Удаление dep/фичи ломает `--all-features` во всех трёх джобах одновременно и требует правки ADR-0004/0029. Внешних потребителей в репо 0 (workspace-member только `vks-server`, без фичи) |
| S2 | `crates/vks-gateway-sip/src/rtcp.rs` (277 стр., untracked) | 277 стр. | Подключён (`lib.rs:26 pub mod rtcp;`), но **ни одного** `rtcp::` вне самого файла: `grep -rn 'rtcp' crates/` даёт только `rtp_bridge.rs`/`uas.rs` (своя SDP-логика, поле `rtcp_port`). Т.е. модуль компилируется и не используется. **Решение:** либо подключить к `rtp_bridge` (это работа `gateways`), либо не коммитить. Удалять не мне — untracked-работа другого агента |
| S3 | 5 × `tests/e2e/test_challenger*.js` (untracked, 62 КБ) | ~62 КБ | Дублируют сценарии `layout_constructor`/`geometry`; ни один не упомянут в `tests/e2e/package.json` (`scripts` = только `node live.js`). Либо коммитить + добавить в npm-скрипты, либо удалить вместе с PR — решить с `layout-ui` |
| S4 | Клиентский синтез `calculate` в `codian-layouts.js` (`formatCalculatedSlots`, `synthXhr`, 2 calculate-ветки) | **−~70 строк** | Серверный маршрут **существует**: `api_v3/mod.rs:354` → `post(shell::video_layouts_calculate)` (`shell.rs:694–766`); живьём: 401 без токена, геометрия с токеном. Детали и гейты — **§8.1**. Ветку сброса `N` после save (`:1782`) при этом **не трогать** |

### 1.3 НЕ трогать (и почему)

| Объект | Почему |
|---|---|
| `crates/vks-media-mcu/src/farm.rs` (untracked) | **Подключён**: `media-mcu/src/lib.rs:45 pub mod farm;`, `:58 pub use farm::{…}`, `engine.rs:28 use crate::farm::{EncoderFarm,…}`, `vks-server/src/gateway_video.rs:514-521` (`farm_spec_for`, `FarmCodec`) |
| `crates/vks-server/src/farm_metrics.rs` (untracked) | **Подключён**: `vks-server/src/lib.rs:9 pub mod farm_metrics;`, вызовы `:136` `install()`, `:171` `json_from_stats`, `:273` `update()`; тест `tests/network_integration.rs:2044` |
| Перехват `fetch`/XHR в `codian-layouts.js:1716–1793` — **ветка сброса кэша `N` после save** (`:1741`, `:1782`, `serverCountKey=''; loadLayoutCount()`) | Единственный триггер перезагрузки авторитетного `N` с сервера после сохранения раскладки; без него счётчик в конструкторе остаётся устаревшим. **Не трогать** |
| `formatCalculatedSlots` (:1648–1688) + `synthXhr` (:1689–1715) + `calculate`-ветки перехвата (:1722–1739 fetch, :1768–1781 XHR) | ~~Удалять нельзя: маршрута в Rust нет~~ — **ИСПРАВЛЕНО 28.10:** маршрут **есть**, `api_v3/mod.rs:354` → `post(shell::video_layouts_calculate)` (`shell.rs:694–766`). Клиентский синтез — **дублирующее** звено и кандидат на удаление (**−~70 строк**) при двух условиях: (1) вендорский UI действительно ходит на серверный `/api/v3/ui/video_layouts/calculate` (проверить XHR-логом, а не по коду), (2) id/форма ответа совпадают (`slots[]` с `x,y,width,height,z_order,position` — сервер их отдаёт, проверено живьём). Консистентность calculate↔apply_layout (`:84`) при этом обеспечивается сервером, а не клиентом |
| `crates/vks-admin/src/api_v3/*_tests.rs` (6 файлов, 5669 стр. в `src/`) | **Не сироты**: все объявлены через `#[cfg(test)] #[path=…] mod` — `mod.rs:29,32`, `directory.rs:970`, `history.rs:785`, `call_strings.rs:1329`, `resources.rs:2201`. Не в бинаре (cfg(test)). Перенос в `tests/` — косметика, не сокращение |
| 22 файла `tests/e2e/*.js` | Evidence UI-регрессий; отсутствие CI-джобы — дыра (§4), но файлы нужны |
| `docs/audit/2026-10-25/` (7 файлов, 700 КБ), `scripts/bench/` (5.4 МБ), `docs/architecture/{ADR_DRAFTS,ARCHITECTURE_MASTER_PLAN}.md` | Untracked, но на `ARCHITECTURE_MASTER_PLAN.md` ссылается **закоммиченный** `.cargo/config.toml:18`, и на `docs/audit/2026-10-25/*` — сам мастер-план. Это «документ, на который ссылается трекаемый файл» ⇒ **коммитить**, не удалять |
| `deny.toml` `[licenses] allow` (6 «про запас») | Формально мёртвые (warning `license-not-encountered`), но удаление сделает будущее добавление зависимости падающим по лицензии. Оставить, убрать только комментарий-ложь про TLS (C-07) |
| `configs/prod.example.toml` | Не стартует by design (`CHANGE_ME` → `NoPersistentStorage`, `config.rs` validate) — это фича, не баг |

## 2. Конфликты и дубли конфигов

| # | Конфликт | Доказательство | Рекомендация |
|---|---|---|---|
| C-01 | **Две несогласованные портовые политики.** `media_listen_addr=0.0.0.0:40000` объявляется в SDP как `candidate_port=40000`; при этом `VKS_WEBRTC_PORT_MIN/MAX=43000/43100` — отдельный диапазон для str0m, и **только он** документирован в юните («Firewall/NAT must open this UDP range») | `configs/default.toml:15`; `crates/vks-server/src/main.rs:282–307` (`candidate_port = local_addr.port()`); лог: `bound UDP media socket 0.0.0.0:40000`, `candidate_port=40000`, `WebRTC preflight OK min=43000 max=43100`; `deploy/systemd/vks-mcu.service:16-22`; `vks-media-sfu/src/webrtc_room.rs:103` | Прописать в `deploy/README.md` таблицу «порт → носитель»; либо привести `media_listen_addr` внутрь 43000–43100. **Не менять без `gateways`** (у них ICE-путь) |
| C-02 | `-C target-cpu=native` (uncommitted) заявлен как гарантия воспроизводимого sha256 — **ложь**: `native` зависит от CPU хоста | `.cargo/config.toml:22-23` (diff) + комментарий «sha256 бинаря воспроизводим» там же; противоречит собственной строке «Для прода — x86-64-v3» | Для CI/прода — `target-cpu=x86-64-v3`; `native` оставить только в локальном `[env]`/профиле стенда. Иначе кэш `target/` (ключ только по `Cargo.lock`!) может принести артефакты с чужой микроархитектуры → SIGILL |
| C-03 | Версия тулчейна в **3 местах**: `rust-toolchain.toml:2`, `Cargo.toml` `rust-version="1.94"`, `ci.yml` `RUST_TOOLCHAIN: 1.94.0` | все три файла | Оставить `rust-toolchain.toml` источником; в CI убрать `env.RUST_TOOLCHAIN` (пусть `dtolnay/rust-toolchain` читает файл) |
| C-04 | `edition` в 2 местах: `.rustfmt.toml:8` и `Cargo.toml` workspace | — | Убрать из `.rustfmt.toml` (rustfmt берёт из манифеста) |
| C-05 | Список целей в 2 местах: `deny.toml [graph].targets` и `rust-toolchain.toml targets` | `deny.toml:15-18`, `rust-toolchain.toml:4-7` | Один источник; дрейф молча расширит/сузит проверяемый граф |
| C-06 | `deny.toml [graph] all-features = true` **и** `ci.yml args: --all-features` — дубль | `deny.toml:20`, `ci.yml` deny-джоба | Оставить в конфиге |
| C-07 | Комментарий «После удаления rvoip TLS-стек в графе отсутствует» — устарел: `ring 0.17.14` в графе через `str0m/rust-crypto` | `deny.toml:61-64`; `Cargo.lock:ring 0.17.14`; `vks-media-sfu/Cargo.toml` `str0m = { features… }` | Поправить комментарий |
| C-08 | CI-комментарий «E2E (chromiumoxide) добавляется в фазе 8» — устарел: в репо Playwright 1.64.1, 22 e2e-скрипта | `ci.yml` последняя строка; `tests/e2e/package.json` | Заменить комментарий и добавить джобу (§4) |
| C-09 | `MANIFEST.sha256:210` — строка **без** `./`, в отличие от остальных 213 и от генератора | `git diff MANIFEST.sha256` (`-4133d3…  ./codian-layouts.js` → `+1f6491…  codian-layouts.js`); генератор `scripts/import-pnm-frontend.sh:289` всегда пишет `./` (через `find .`) | Правка была **ручной**. Содержимое при этом верно: `sha256sum -c` из `deployed/pnm` → **214/214 OK**. Перегенерировать командой из :289 |
| C-10 | `default_room_mode` в конфиге **не авторитетен** после первого старта | `main.rs:253–258`: значение из TOML применяется только если в `server_settings` ещё `Sfu`. Сейчас `server_settings` пуста (проверено read-only через `sqlite3`-модуль Python), поэтому фактический режим = `sfu` = конфиг — совпадает случайно | Зафиксировать в ADR: БД приоритетнее TOML |
| C-11 | Dev-юнит **не стартует сам по себе**: `VKS_WEBRTC_TRANSPORT=1` + `candidate_addr=""` → exit 78 | `deploy/systemd/vks-mcu.service:19`; `configs/default.toml:22` `candidate_addr = ""`; `webrtc_runtime.rs:33–37` | Оставить (fail-closed by design, `[=K-03/K-04]`), но в `deploy/README.md` — явный шаг drop-in |
| C-12 | `deploy/vks.env.example` (`TRANSPORT=0`) ↔ dev-юнит (`=1`) | два файла в одном каталоге | Разные назначения — добавить в шапку каждого «для какого юнита» |
| C-13 | `media_listen_addr` дефолт в коде `127.0.0.1:0` (эфемерный порт ⇒ случайный SDP-кандидат) vs `0.0.0.0:40000` в TOML | `config.rs:210` vs `default.toml:15` | `[=K-01]`: источник истины — TOML |
| C-16 | `.gitignore` покрывает **только** `/target/`; `target-impl/` и `target-proto/` (10.6 ГБ) не игнорируются | `.gitignore:2`; `git check-ignore -v target-impl target-proto` → пусто; `git status` → `?? target-impl/`, `?? target-proto/` | Добавить 2 строки `/target-impl/`, `/target-proto/` — безопасно **сразу** |
| C-17 | `run-local.sh:33` собирает `cargo build --release -p vks-server` **без `--locked`** — локальная сборка может молча переписать `Cargo.lock` | `scripts/run-local.sh:33` | `[=K-12]`; добавить `--locked` |
| — | Дубли фикстур — **опровергнуто** | 6 файлов `crates/*/tests/fixtures/*`, `sha256sum` → 0 дублей | — |
| — | Мёртвые `[workspace.dependencies]` — **опровергнуто** | все 22 записи используются ≥1 членом (`grep 'workspace = true'`): tokio 13, serde 12, uuid 14, thiserror 15, … | — |
| — | Дубли `[profile.*]` в `.cargo/config.toml` — **опровергнуто** | в `crates/*/Cargo.toml` нет `[profile`; профили только в корне | — |

## 3. Версии стека

Факт: `rustc 1.94.0 (4a4ef493e 2026-03-02)`, `cargo 1.94.0`, `node v24.21.0`, `Python 3.14.4`, `ffmpeg 8.0.1-3ubuntu2`.
`cargo metadata`: **16** workspace-членов, у всех `edition = "2021"`, `rust-version = 1.94`.
`Cargo.lock`: **434** пакета (совпадает с заявленным в трекере).

| Пакет | lock | Пакет | lock |
|---|---|---|---|
| tokio | 1.53.3 | tower-http | 0.6.11 |
| axum | 0.8.9 | hyper | 1.12.0 |
| sqlx (+sqlite/+postgres) | 0.8.6 | http | 1.5.0 |
| str0m | 0.24.1 | mio | 1.2.4 |
| oxideav-scene | 0.1.4 | ring | 0.17.14 |
| oxideav-core | 0.1.40 | tokio-tungstenite / tungstenite | 0.29.0 |
| prost / prost-build | 0.13.5 | libsqlite3-sys | 0.30.1 |
| protoc-bin-vendored | 3.3.0 | chrono | 0.4.45 |
| serde / serde_json | 1.0.229 / — | prometheus | 0.14.0 |
| tracing | 0.1.44 | figment | 0.10.19 |
| uuid | 1.28.0 | sha2 / base64 | 0.10.9 / 0.22.1 |
| rand | 0.10.3 | ulid | 1.2.1 |
| getrandom | 0.4.3 | thiserror / anyhow | 2.0.21 / 1.0.104 |
| wasm-bindgen | 0.2.129 | async-trait / bytes / libc / subtle | 0.1.92 / 1.12.1 / 0.2.190 / 2.6.1 |
| proptest | 1.12.0 | uncased | 0.9.10 |

Отсутствуют в графе (важно для `deny.toml`): `reqwest`, `rustls`, `webpki-roots`, `openssl`, `h2`, `tonic`, `opentelemetry`.
12 крейтов `oxideav-*` присутствуют в lock, но поставляемый бинарь их не содержит
(`strings target/release/vks-server | grep -c oxideav-scene` = 0 `[=K-11]`).
Цель `wasm32-unknown-unknown` собрана в `target/wasm32-unknown-unknown` (225 МБ) — джоба `wasm-sdk` реальна.

## 4. CI-реальность: что падает, что пропускает тесты, чего нет

Работают (12 джоб: fmt, check, clippy, wasm-sdk, test, test-postgres, test-external, test-serial,
path-with-spaces, doc, sidecar, deny, build).

| # | Дыра | Доказательство | Последствие |
|---|---|---|---|
| G1 | **E2E Playwright отсутствует в CI полностью** | `grep -c 'e2e\|playwright\|node ' .github/workflows/ci.yml` → **0**; при этом 22 файла `tests/e2e/*.js`, `package.json` `playwright 1.64.1`, `npm test = node live.js` | UI-регрессии (раскладки, конструктор, тёмная тема) ловятся только руками — ровно поэтому в дереве 5 untracked `test_challenger*.js` |
| G2 | **`#[ignore]`-тесты не запускаются нигде**: 8 штук | `grep -rn '^\s*#\[ignore' crates/` → 8: `mcu_tick_soak.rs:339` (release-only SLO), `g726.rs:215`, `live_dial.rs:42`, `grids_geometry_golden.rs:142`, `live_h323.rs:18`, `gateways.rs:4078`, `gateway_video.rs:1764,1982` | Soak/SLO-гейт (`mcu_tick_soak`) **никогда** не исполняется в CI; golden-генератор и live-звонки — тоже |
| G3 | **Нет `--release`-тестов** | ни одна джоба не делает `cargo test --release`; при этом единственный timing-гейт помечен release-only (`mcu_tick_soak.rs:339`) | timing/GC-регрессии невидимы |
| G4 | `test` идёт **без `--all-features`**, т.е. поставляемый граф ≠ тестируемый | `ci.yml` job `test`: `cargo test --workspace --locked --no-fail-fast`; `[=K-12]` | str0m/oxideav-пути тестируются только в soft-джобе |
| G5 | `test-external` — `continue-on-error: true` | `ci.yml` job `test-external` | падение str0m/oxideav **не блокирует** merge |
| G6 | `doc` — `continue-on-error: true` (≈30 битых intra-doc-ссылок) | `ci.yml` job `doc` | `-D warnings` декоративен |
| G7 | **Python-сайдкар: CI 3.13, стенд 3.14.4** — матрицы нет | `ci.yml` `python-version: "3.13"`; `python3 -V` → 3.14.4 | расхождение версий не обнаруживается; `pycrate==0.8.1` (pinned + `--require-hashes`) на 3.14 не проверялся. Локально `.h323-venv` **отсутствует** → `live_h323`-тесты невоспроизводимы |
| G8 | Нет линтеров для не-Rust: `shellcheck` (20+ скриптов), `ruff/black` (Python), prettier/eslint (22 e2e js) | `ls scripts/*.sh` | опечатки в `scripts/bench/*.sh` и `tests/e2e/*.js` не ловятся |
| G9 | Нет проверки, что `MANIFEST.sha256` соответствует `deployed/pnm` | C-09: расхождение реально есть (ручная правка, 214/214 OK по содержимому) | дрейф UI-артефакта невидим; **дешёвая и ценная джоба** |
| G10 | Нет проверки идемпотентности `scripts/import-pnm-frontend.sh` (есть только юнит-тесты `pnm_patch_lib`) | `ci.yml` sidecar job, второй шаг | импорт может тихо перезаписать проектный слой |
| G11 | Кэш CI ключуется **только** по `Cargo.lock` | все `key: cargo-*-${{ hashFiles('**/Cargo.lock') }}` | `.cargo/config.toml` (C-02, `target-cpu=native`) и `rust-toolchain.toml` в ключ не входят → возможен чужой/устаревший `target/` |
| G12 | `path-with-spaces` собирает workspace в **debug** и не прогоняет clippy/test | `ci.yml` job `path-with-spaces` | джоба проверяет только «libtool не сломался», не код |
| G13 | `cargo audit` отсутствует | — | **не дыра**: `cargo-deny` `[advisories]` покрывает RustSec (`vulnerability/unsound` = error, `unmaintained = "workspace"`). Дублировать не нужно |

### Минимальный, но полный CI (предложение)

1. **`manifest`** (новый, 15 с): `cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256` → закрывает G9.
2. **`e2e-smoke`** (новый, обязательный): `actions/setup-node@v4` (node 24), `npm ci` в `tests/e2e`, поднять `vks-server` из собранного release-бинаря с `VKS_ALLOW_INSECURE_DEFAULTS=1` на временном `VKS_DATABASE_URL`/`:memory:`-эквиваленте, прогнать **подмножество**: `test_layout_constructor.js` + `crawl_all_tabs.js` → закрывает G1.
3. **`test-release-soak`** (новый, P2): `cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored` → закрывает G2/G3 для единственного timing-гейта.
4. **`test` += `--all-features`** (правка 1 строки) и снять `continue-on-error` с `test-external` → закрывает G4/G5. Перед этим — решение S1 по `oxideav`.
5. **`sidecar`:** матрица `python-version: ["3.13", "3.14"]` → закрывает G7.
6. **`lint-scripts`** (новый, P3): `shellcheck scripts/*.sh`, `ruff check scripts crates/vks-gateway-h323/python` → G8.
7. Кэш: `key: …-${{ hashFiles('**/Cargo.lock', '.cargo/config.toml', 'rust-toolchain.toml') }}` → G11.
8. `doc`: убрать `continue-on-error` **после** починки 30 ссылок (отдельный коммит) → G6.

## 5. План мелких коммитов чистки (порядок безопасный)

| # | Коммит | Состав | Гейт перед коммитом |
|---|---|---|---|
| 1 | `chore(gitignore): ignore stray CARGO_TARGET_DIR trees` | C-16: 2 строки в `.gitignore` | `git status` больше не показывает `target-impl/`,`target-proto/` |
| 2 | `chore(ci): fix stale comments + cache key + manifest check` | C-08, G11, G9 | `sha256sum -c` локально зелёный |
| 3 | `chore(cargo): drop unused deps` | D3–D9 (11 строк) | `cargo check --workspace --all-features --all-targets`; если доступен — `cargo +nightly udeps`; **отдельно** от п.4 |
| 4 | `chore(media-mcu): serde to dev-deps` | D9 | `cargo test -p vks-media-mcu` |
| 5 | `fix(pnm-ui): drop leftover constructorCount + dead onConstructorActivate` | D1, D2 (**7 строк**) | `node --check codian-layouts.js`; перегенерировать `MANIFEST.sha256` (§6); затем п.6 CI-джоба `manifest` должна быть зелёной; e2e `test_layout_constructor.js` |
| 5b | `refactor(pnm-ui): use server video_layouts/calculate` | S4 (**−~70 строк**), отдельно от п.5 — иначе регресс превью не отделить от чистки | XHR-лог: UI идёт на `/api/v3/ui/video_layouts/calculate`; e2e: 6 режимов × `vks_grid_43`/`codian_07`, превью == сохранённые слоты; `MANIFEST.sha256` |
| 6 | `docs: commit 2026-10-25 audit + master plan + ADR drafts` | untracked `docs/audit/2026-10-25/`, `docs/architecture/{ADR_DRAFTS,ARCHITECTURE_MASTER_PLAN}.md`, `scripts/bench/` | `.cargo/config.toml:18` уже ссылается на мастер-план ⇒ ссылка перестанет быть висячей |
| 7 | `feat(sip): wire or drop rtcp.rs` | S2 — **решение `gateways`**, не гигиены | `cargo check -p vks-gateway-sip` |
| 8 | `chore(build): target-cpu policy` | C-02 | CI-кэш не должен переиспользовать `native`-артефакты |

Коммиты 1, 2, 6 — можно **сейчас**, они не касаются кода сервиса.
Коммиты 3–5 требуют отдельного прогона (и п. 5 — обязательной пересборки UI-манифеста).

## 6. Как обновлять `MANIFEST.sha256` (обязательно при правке `codian-layouts.js`)

`scripts/import-pnm-frontend.sh` **не имеет** режима «только манифест» (флаги: `--force`, `--with-source`, `--gzip`;
`grep -n 'manifest-only'` → 0), и он пересобирает `deployed/` из архива `archive/PNM-Frontend-20261016.zip` (30 МБ),
предварительно **сохраняя** проектный слой (`:133-136`, `:163`, `:224`: `perf-overrides.js`, `api-errors.js`,
`codian-layouts.js`). Т.е. полный прогон = риск. Эквивалент строки `:289`, без импорта:

```bash
cd client/pnm-frontend/deployed/pnm \
  && find . -type f -print0 | sort -z | xargs -0 sha256sum > ../../MANIFEST.sha256
cd .. && sha256sum -c MANIFEST.sha256 | grep -v ': OK$'   # должно быть пусто
```

Иначе UI-фикс не попадёт в деплой (сейчас манифест уже разошёлся по формату — C-09).

## 7. Дебаты (обязательный протокол)

**Ограничение:** `subagent` в этой сессии **недоступен** — `spawn_teammate`/`subagent` из depth-1 агента даёт
`Error: subagent depth 2 exceeds maxDepth 1`. Прямое указание Lead: не тратить время, дебаты — режим
«внутренний адвокат дьявола + независимая проверка», внешний челленджер запускает Lead.
Ниже — протокол: сильнейший контраргумент → моя проверка → итог.

| Раунд | Тезис `hygiene` | Сильнейший контраргумент | Проверка | Итог |
|---|---|---|---|---|
| 1 | `constructorCount` мёртв | «В HEAD он живой (6 использований) — значит удалять нельзя, это регресс» | `git show HEAD:… \| grep -c` = 6; `grep -c` в WT = 1; diff показывает `- constructorCount = count;` и `+ counterValue = count;` | **Согласен частично:** это не «исторический мусор», а **хвост незакоммиченного рефактора**. Удаляем только объявление; использование восстанавливать **не нужно** (роль передана `counterValue`) |
| 1 | То же | «Удаление случайно — восстанови чтения на 608/614/615/632/965» | `counterValue` в WT используется 4×, включая `if (count < 1) count = counterValue;` (:1111) и `counterValue = value;` (:1130) — это тот же счётчик, что и `constructorCount` в HEAD | **Не согласен с восстановлением.** Восстановление вернёт дублирующее состояние; правильный фикс — удалить объявление |
| 2 | `onConstructorActivate` мёртв | «Может вызываться строкой: `window['onConstructor'+'Activate']`, inline `onclick=`, или из HTML» | `grep -rn` по всему репо (`*.js/*.html/*.py/*.sh/*.md`) → 1 вхождение = объявление; динамической сборки имён в файле нет (IIFE, `var`-стиль, нет `window[`); DOM строится кодом (`createElement`), inline `onclick` в PNM не используется | **Согласен с удалением.** 6 строк |
| 2 | Перехват `fetch`/XHR можно сократить целиком | «Он несущий: сброс кэша `N` после save» | **Ошибка в моей проверке:** я сделал `grep -rn "video_layouts" crates/vks-admin/src/api_v3/ \| head -10` — строка `mod.rs:354` попала за границу `head`, и я заключил «маршрута нет». Факт: `mod.rs:354` `.route("/api/v3/ui/video_layouts/calculate", post(shell::video_layouts_calculate))`, `shell.rs:694–766`; живьём: без токена **401**, с токеном `vks_grid_43` → `{"slots":[{…"x":0.0,"y":0.0,"width":142.0,"height":144.0,"z_order":0,"position":1},…]}`. `/api/v3/health` → 501 — это **другой** путь (catch-all), к calculate не относится | **Позиция изменена (по поправке Lead):** клиентский `calculate`-синтез **удаляем** (−~70 строк) при подтверждённой серверной замене; ветку сброса кэша `N` после save (`:1782`) — **не трогать** |
| 3 | `target-impl`/`target-proto` — мусор | «Это evidence-артефакты 25 окт, на них могут ссылаться отчёты» | `CACHEDIR.TAG` + полные `debug/ release/ .fingerprint/` + бинари 25 окт 18:05/19:37 | **Согласен частично:** в `.gitignore` — сразу; `rm -rf` — только после сверки `docs/audit/2026-10-25/*` и `measure/` |
| 3 | `oxideav` — пустая фича, удалить | «ADR-0004 её требует; это запланированный композитор, а не мусор» | `grep -rn 'oxideav_scene::'` = 0; `lib.rs:67-68` только re-export; ADR-0004 в `docs/decisions` описывает целевое состояние; трекер S15 «no-op фича»; `[=K-11]` | **Позиция изменена:** не удалять, а **заморозить** (убрать CI-джобу, пометить `reserved`, добавить инвариант `cargo tree`). Удаление = правка ADR + риск для `--all-features` |
| 3 | «Дублей фикстур нет» | «6 фикстур на 16 крейтов — подозрительно мало, наверняка копии» | `sha256sum` всех 6 → **0** повторов | Тезис подтверждён, контраргумент отклонён |
| 4 | «Мёртвых workspace-зависимостей нет» | «`proptest`, `subtle`, `prost-build`, `uncased` выглядят сиротами» | `grep 'workspace = true'` по 15 манифестам: у каждой ≥1 потребитель (proptest 1, subtle 1, prost-build 1, uncased 1) | Тезис подтверждён |

**Что дало дебаты:** четыре позиции изменены (S1 `oxideav`; клиентский `calculate` — удалять, а не защищать;
`constructorCount` не восстанавливать; ветку сброса `N` — не трогать); две гипотезы опровергнуты
(дубли фикстур, мёртвые workspace-deps); одна ошибка предотвращена (`*_tests.rs` — не сироты,
а `#[cfg(test)] #[path]`-модули).

**Erratum (28.10, поправка Lead).** В первой редакции §1.2/§7 утверждалось, что маршрута
`/video_layouts/calculate` в Rust нет. Это **неверно** и вызвано усечением вывода
(`grep … | head -10` отрезал `mod.rs:354`). Факт: маршрут есть (`api_v3/mod.rs:354` →
`post(shell::video_layouts_calculate)`, `shell.rs:694–766`), проверено живьём (401 без токена,
геометрия с токеном). Вывод перевёрнут: клиентский синтез `calculate` — **избыточное дублирование**,
кандидат на удаление ~70 строк (S4 ниже), а не «несущий код».
Уроки метода: (1) не делать вывод «маршрута нет» по усечённому `head`; (2) проверять HTTP-поведение
пробой, а не только grep; (3) `501` на `/api/v3/health` — свойство catch-all, не аргумент про другие пути.

### 8.1 S4 — удалить клиентский `calculate`-синтез (−~70 строк)

| Объект | Строки | Условие безопасности |
|---|---|---|
| `formatCalculatedSlots` | `codian-layouts.js:1648–1688` (41) | Серверный `POST /api/v3/ui/video_layouts/calculate` возвращает те же `slots[]` (проверено: `x,y,width,height,z_order,position`) |
| `synthXhr` | `:1689–1715` (27) | Вызывается только из calculate-веток ⇒ уходит вместе с ними |
| calculate-ветка fetch | `:1722–1739` | Подтвердить XHR-логом, что UI реально идёт на `/api/v3/ui/…`, а не на вендорский относительный путь |
| calculate-ветка XHR | `:1768–1781` | То же |
| `formatSlots`-хелперы, оставшиеся без вызова | проверять `grep` после удаления | `node --check` + e2e `test_layout_constructor.js` |

**Итого −~70 строк JS** (1833 → ~1763). Обязательно: пересчёт `MANIFEST.sha256` (§6)
и e2e-проверка превью конструктора (`vks_grid_43`, `codian_07`, 6 режимов).

## 8. Непроверенное / ограничения

1. **`cargo machete`/`cargo udeps` недоступны** (`no such command`), nightly-тулчейна нет (`rustup toolchain list` → stable + 1.94.0). Список D3–D9 — grep-эвристика с ручной валидацией; `udeps` может найти ещё (feature-only, build-deps) или опровергнуть часть.
2. **Ничего не собиралось** (запрет). Все выводы о компиляции — по исходникам и lock, не по факту сборки. `cargo metadata --no-deps` — единственная запущенная cargo-команда.
3. **Не проверял** `deploy/README.md` (11.9 КБ) построчно и `client/pnm-frontend/NOTES.md` (36 КБ) — прочитаны выборочно (`grep` по ключевым словам).
4. **Портовая политика C-01** — не моя зона: нужен вердикт `gateways` (ICE/str0m-путь).
5. **`scripts/bench/`** (5.4 МБ, 21 файл) — только инвентаризация; являются ли `drafts/`, `media/` воспроизводимыми — не проверял.
6. **`target` = 50 ГБ** — политика «когда удалять `debug`» согласуется с `perf_lab`; безопасно только после release-верификации.
7. **`vks_data.db`** читалась строго read-only через `sqlite3`-модуль Python (URI `?mode=ro`); `server_settings` пуста. Никаких записей не производилось.
8. `smoke_v3.sh` (15 КБ) и `run-local.sh` (8.7 КБ) — прочитаны только `grep`-ом по ключевым паттернам (`cargo build`, порты).

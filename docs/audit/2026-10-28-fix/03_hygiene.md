# 03 — Гигиена: мёртвое, конфиги, зависимости, CI (fix-проход)

> **Исторический снимок:** H-20 о полном покрытии сетки закрыт в S6 (неполный ряд центрируется, docs обновлены).

**Агент:** `hygiene` · **Дата:** 2026-10-28, срез 17:35–17:46 MSK · **Режим:** read-only (единственная запись — этот файл)
**Прочитано целиком (`cat`):** корневой `Cargo.toml`, все 16 манифестов, `rust-toolchain.toml`, `.cargo/config.toml`,
`deny.toml`, `.rustfmt.toml`, `.gitignore`, `configs/default.toml`, `configs/prod.example.toml`,
`.github/workflows/ci.yml`, все 4 файла `deploy/**`, `deploy/README.md`, `crates/vks-media-mcu/src/frame.rs`,
`crates/vks-layout/src/grid.rs`, `docs/audit/2026-10-28/05_hygiene.md`, `docs/CLEANUP_AUDIT_2026-10-23.md`,
`docs/CONFIG_PACKAGE_AUDIT_2026-10-23.md`; `Cargo.lock` — программный разбор всех 434 пакетов;
`client/pnm-frontend/NOTES.md` (36 КБ) — по разделам + адресные проверки; `scripts/*.sh`, `scripts/*.py`,
`scripts/bench/*` — синтаксис + инвентаризация; `index.html` — все `<script>`; `MANIFEST.sha256` — структура + `-c`.

> **Дерево живое.** Пока шёл аудит, параллельные агенты правили файлы: `resources.rs` (17:40),
> `codian-layouts.js` + `MANIFEST.sha256` + **два вендорских чанка** (17:43:41–17:43:56). Все выводы ниже
> привязаны к срезу 17:35–17:46; для изменяемых файлов указан таймстемп. Это не мелочь: один из
> параллельных фиксов **нарушил инвариант «vendor не трогаем»** — см. H-13/§5.

---

## 1. TL;DR + топ-10

Состояние: **сборка формально живая, но дерево не проходит `cargo fmt --check`, 41 незакоммиченный
элемент (+4677/−848), 10.6 ГБ мусорных target-деревьев и 233 МБ бинарей в untracked-докax.** Мёртвого
кода ~**570 строк Rust + ~80–105 строк JS**, мёртвых зависимостей **12 рёбер в 6 манифестах**,
конфликтов конфигов **17** (из них 5 — про порты), дыр CI **13** (2 маскируются `continue-on-error`).

| # | Топ-10 | Эффект |
|---|---|---|
| 1 | **Портовых политик не две, а пять**, и ни одна не задокументирована в `deploy/`: media 40000 (SDP-кандидат), RTP-пул шлюза **41000–41200 (hardcoded, ключа нет)**, ICE 43000–43100 (только в unit), «ICE 42000–65000 / RTP 20000–39999» (мастер-план), «40000–60000» (BACKEND_RULES). Firewall по `deploy/` → медиа шлюзов молча не проходит | P0-риск для приёмки |
| 2 | **`-C target-cpu=native` в трекаемом `.cargo/config.toml`** с ложным комментарием «sha256 воспроизводим», при этом прод-политика `x86-64-v3` существует только в тексте; CI-кэш ключуется лишь по `Cargo.lock` | SIGILL/неатрибутируемые замеры |
| 3 | **`cargo fmt --all -- --check` падает прямо сейчас** (2 файла: порядок `pub mod rtcp;`, форматирование `rtcp.rs`) ⇒ джоба `fmt` красная, а «все гейты зелёные» в трекере — не про это дерево | блокер коммита |
| 4 | **Мёртвый Rust:** `vks-layout/src/viewer.rs` 215 стр (0 потребителей вне крейта), mock-кодеки `frame.rs:64–193` ≈130 стр (0 потребителей), 4 из 6 функций `rtcp.rs` ≈130 стр | −~476 строк |
| 5 | **`target-impl/` 5.9 ГБ + `target-proto/` 4.7 ГБ** не в `.gitignore`; в `target-impl/release/vks-server` лежит evidence-бинарь `05f1ce81…` (совпал с паспортом) | 10.6 ГБ + 2 строки |
| 6 | **`docs/audit/2026-10-25/` = 233 МБ**, из них `measure/` — **9 копий бинаря `vks-server`** по ~27 МБ. Прошлый аудит советовал это «коммитить» | нельзя в git |
| 7 | **42 из 44 `env::var("VKS_…")` не выразимы в TOML** (в `Config` — 16 полей, пересечение = 2, и оба тестовые); при этом `config.rs` грузит TOML через `Toml::file` — **figment молча игнорирует отсутствующий файл** и ищет относительный путь вверх по дереву | конфиг-ловушка |
| 8 | **Вендорские чанки пропатчены вручную** (17:43:55): в `index6.bundle` добавлен `||[]`-guard. `MANIFEST` пересчитан ⇒ джоба «manifest OK» это не поймает | нарушен слой проекта |
| 9 | **CI: 13 дыр**; ключевые — нет E2E Playwright (25 файлов, 0 упоминаний), нет проверки `MANIFEST`, нет `cargo test --release` (единственный timing-гейт `#[ignore]`), `doc` и `test-external` — `continue-on-error`, Python 3.13 vs 3.14.4 на стенде, кэш без `.cargo/config.toml` | регрессии невидимы |
| 10 | **Доки врут про структуру:** `ARCHITECTURE.md §3` описывает `ports/signaling_transport.rs`, `decoder.rs`, `encoder.rs`, `vks-server/src/{config,bootstrap}.rs`, `vks-server/configs/{dev,production}.toml`, `rvoip`, `utoipa` — **ничего из этого не существует**; `vks-proto/README.md` обещает `protobuf-src`, а в build.rs `protoc-bin-vendored` | доки как источник=false |

---

## 2. Таблица находок

Категория · что · доказательство (команда/файл:строка) · действие · риск · экономия.

### 2.1 Мёртвый код (доказано отсутствием потребителей)

| # | Что | Доказательство | Действие | Риск | Экономия |
|---|---|---|---|---|---|
| H-01 | `crates/vks-layout/src/viewer.rs` — `plan_viewer`, `ViewerPlan`, `PlanCell`, `ViewerContext`, `SelfViewPolicy` | `grep -rn "plan_viewer\|ViewerPlan\|ViewerContext\|SelfViewPolicy\|PlanCell" crates/ client/vks-client-sdk/src tests/ scripts/ --include=*.rs --include=*.js --include=*.py` → **все хиты внутри `viewer.rs` + 1 строка** `vks-layout/src/lib.rs:50` (re-export). `grep -rn "viewer::"` → только `lib.rs:50`. Публичный API крейта, но ни одного вызова вне | удалить `viewer.rs` + строку `lib.rs:50`; ADR-0029 §2 упоминает «план для зрителя» — сослаться на `engine.rs:998 frame_for_viewer` как на фактическую реализацию | низкий (0 вызовов) | 215+1 стр |
| H-02 | `crates/vks-media-mcu/src/frame.rs:64–193` — `CodecError`, traits `VideoEncoder`/`VideoDecoder`, `MAGIC`/`HEADER_LEN`, `MockVideoEncoder`/`MockVideoDecoder` + 5 тестов | `grep -rn "MockVideoEncoder\|MockVideoDecoder\|VideoEncoder\|VideoDecoder\|CodecError" crates/ client/ --include=*.rs` → **только `lib.rs:61`** (re-export). Живёт лишь `Frame` + `CANVAS_*` | удалить блок; оставить `Frame`/`CANVAS_*`/`is_yuv420p`/`pixel_count`; из `lib.rs:61` убрать 5 имён | низкий, но: ADR-0004 «codec abstraction» — зафиксировать, что абстракция появится вместе с реальным кодеком | ~130 стр |
| H-03 | `crates/vks-gateway-sip/src/rtcp.rs`: `is_rtcp` (:36), `parse_feedback` (:46) + `VideoFeedback`/`FeedbackKind`, `build_pli` (:156), `build_fir` (:167) + их тесты | `for n in is_rtcp parse_feedback build_pli build_fir VideoFeedback FeedbackKind; grep -rn "\b$n\b" crates/ --include=*.rs \| grep -v src/rtcp.rs` → **0** для всех шести. **Не мёртв целиком:** `build_rr`/`parse_compound` используются в `crates/vks-server/src/gateways.rs:359,377` (новый незакоммиченный `spawn_video_rtcp`, `+142…+184` в `git diff`) | удалить 4 функции и 2 типа; `rtcp.rs` оставить (277→~140 стр). PLI/FIR-счётчики уже идут через `seen.pli/seen.fir` из `parse_compound` | средний: файл untracked, работа другого агента — не удалять без Lead | ~130 стр |
| H-04 | `client/pnm-frontend/deployed/pnm/codian-layouts.js:141` `var constructorCount = 0;` | `grep -c constructorCount` = **1** (срез 17:43). В HEAD роль играл `constructorCount` (6 вхождений), в WT — `counterValue` | удалить строку | нет | 1 стр |
| H-05 | `codian-layouts.js:1420–1426` `onConstructorActivate()` | `grep -rn onConstructorActivate client/ tests/ scripts/ deploy/ configs/ crates/` → **1 вхождение = объявление**. Клик идёт через `onVendorModeClick` (document-capture), клавиатура — `onConstructorKeydown` | удалить функцию | нет | 7 стр |
| H-06 | `crates/vks-proto/openapi.json` (6417 Б) | `grep -rn openapi.json crates/vks-proto scripts/ .github/` → только `crates/vks-proto/README.md:15`. `build.rs` компилирует 4 `.proto`, файл не читает. `GET /openapi.json` отдаёт **свой** стаб: `crates/vks-admin/src/handlers.rs:253–254` | удалить файл + абзац README; либо подключить как `include_str!` и отдавать вместо стаба | низкий | 6.4 КБ |
| H-07 | `crates/vks-media-mcu/src/engine.rs:1180 layout_descriptor` | все 16 вызовов — в тестах (`crates/vks-server/src/gateways.rs:4388…5054` внутри `#[cfg(test)]` c :3858, и `engine.rs:2131…3187`). Прод читает `layout_snapshot` (:1204), док самого метода: «(test/inspector)» | **не удалять** (тест-API), но пометить `#[cfg(test)]`-видимость или `#[doc(hidden)]`; в отчётах не считать мёртвым | нет | 0 (уточнение) |
| H-08 | 8 e2e-скриптов без единого упоминания | `for f in tests/e2e/*.js; grep -rl $f docs/ .github/ scripts/ README.md \| wc -l` → 0 для `adversarial_forms_challenger.js`, `adversarial_visual_challenger.js`, `test_challenger{1,2}_*.js` (5 файлов, untracked, 68 КБ), `test_modal_address_manager.js` | 5 untracked — удалить вместе с PR или внести в `package.json`; 3 tracked — либо в npm-скрипты, либо удалить | низкий | ~70 КБ, 8 файлов |
| H-09 | `scripts/bench/drafts/vks-sipbot.rs` (17.7 КБ), `scripts/bench/__pycache__` (352 КБ), `scripts/__pycache__` (44 КБ) | `ls`, `du -sh`; `__pycache__` уже в `.gitignore:38`, но лежит на диске | удалить кэши; `drafts/` — либо в git, либо удалить (эксперимент, нигде не вызывается) | нет | ~410 КБ |
| H-10 | `.m7/tsi/{feedback,patterns,registry,stats}` — пустое дерево, 32 КБ, создано 28.10 17:09 | `find .m7 -type f` → **0 файлов**; `git check-ignore .m7` → не игнорируется | удалить + `.gitignore` | нет | 32 КБ, шум в `git status` |

### 2.2 Неиспользуемые зависимости (эвристика: `cargo metadata --no-deps` + grep по `src/`, `tests/`, `build.rs`)

| # | Крейт | Лишнее | Доказательство | Действие | Риск |
|---|---|---|---|---|---|
| H-11 | `vks-cluster` | `tracing`, `uuid` | скан манифестов vs текста `crates/vks-cluster/src/*.rs` → 0 вхождений `tracing::`/`uuid::` | убрать 2 строки | низкий |
| H-12 | `vks-recorder` | `serde`, `tracing` | `crates/vks-recorder/src/*` → 0 | убрать 2 | низкий |
| H-13 | `vks-server` | `bytes` | `grep -rn "bytes::" crates/vks-server/src/` → 0 (совпадения — имена полей `*_bytes`) | убрать 1 | низкий |
| H-14 | `vks-media-sfu` | `uuid` | `uuid::` в `src/` → 0; `uuid` приходит транзитивно через `vks-core` | убрать 1 | низкий |
| H-15 | `client/vks-client-sdk` | `serde`, `tracing` | `grep -rn "serde\|tracing" client/vks-client-sdk/src/` → 0 на 2294 стр | убрать 2 | низкий |
| H-16 | `vks-common` | `tracing` | занято **собственным** `pub mod tracing` (`crates/vks-common/src/lib.rs:21`), extern-crate недостижим по имени | убрать 1 | низкий |
| H-17 | `vks-media-mcu` | `serde` (normal → dev) | в `src/` 0; только `tests/mcu_tick_soak.rs` | перенести в `[dev-dependencies]` | низкий |
| — | `getrandom` в `vks-common`/`vks-client-sdk` — **ложное срабатывание** | не упоминается кодом, но нужен как wasm-бэкенд (`features=["wasm_js"]`) | **оставить** | — |

Итого **11 строк манифестов, −12 рёбер** графа. Требует подтверждения `cargo +nightly udeps`:
`cargo-udeps`/`cargo-machete`/`cargo-audit`/`cargo-deny` на стенде **отсутствуют** (`command -v` → ABSENT),
nightly-тулчейна нет (`rustup toolchain list` → `stable`, `1.94.0`).

### 2.3 Дубли кода и «можно было написать меньше»

| # | Что | Доказательство | Действие | Риск | Экономия |
|---|---|---|---|---|---|
| H-18 | **Role-нормализация 4×** в `crates/vks-storage/src/core_adapter.rs:514,546,974,1006` (+`:516,548,976,1008` `viewer`) — один и тот же `match` на 5 ветвей | `grep -rn '"operator" =>' crates/ --include=*.rs` → ровно 4 сайта, все в одном файле. В `vks-core` есть только `Role::as_str` (`participant.rs:164`), обратного парсера нет | добавить `Role::from_str` (или `impl FromStr`) в `vks-core`, 4 сайта → 1 вызов | низкий | −12…−18 стр |
| H-19 | **Клиентский синтез `calculate`** дублирует серверную геометрию: `formatCalculatedSlots` (`codian-layouts.js:1690`, ≈41 стр) + `synthXhr` (`:1732`, ≈27) + 2 ветки перехвата fetch/XHR (`:1762…`, `:1808…`) | серверный маршрут есть и покрыт тестами: `crates/vks-admin/src/api_v3/mod.rs:354` → `shell.rs:694–766`; тесты `api_v3/tests.rs:2550,2592`, parity — `vks-layout/src/preset.rs:1242`. **Но:** перехватчик сейчас подменяет ответ, т.е. UI в проде **никогда** не сверяется с сервером | удалить синтез, оставив перехват только для сброса кэша `N` (`:1782`) | **средний**: нужен XHR-лог, что UI ходит на `/api/v3/ui/video_layouts/calculate`, и совпадение `slots[]` | −~70…100 стр JS |
| H-20 | `grid.rs` (226 стр) vs `grids.rs` (705 стр) — **не дубль** | `GridLayout::compute` (`grid.rs:105`) — 4 потребителя: `media-mcu/src/engine.rs:281,718,1847`, `client-sdk/.../sfu_renderer.rs:25`. `grids.rs` — конструктор `vks_grid_*/vks_pin_*/vks_dual_*` с 16:9 и зазорами (`geometry_for`:201). Разные контракты (полное покрытие canvas vs сохранение 16:9) | **не объединять**: слияние = регресс геометрии и golden-тестов | — | 0 |
| H-21 | `vks-proto/README.md` (+ `docs/architecture/*`) описывают несуществующее | см. §3, строка «доки» | править текст | нет | — |

### 2.4 Мусор рабочего дерева

| # | Объект | Размер | Доказательство | Действие | Комментарий |
|---|---|---|---|---|---|
| H-22 | `target/debug` | **46 ГБ** | `du -sh target/*` → `debug 46G`, `release 6.1G`, `wasm32 225M`, `doc 57M`; итого `target` 52 ГБ | удалять **последним**, после приёмки release | игнорируется (`.gitignore:2 /target/`) |
| H-23 | `target-impl/` + `target-proto/` | **10.6 ГБ** | `du -sh` → 5.9G + 4.7G; оба с `CACHEDIR.TAG` (cargo-маркер) → альтернативные `CARGO_TARGET_DIR` | `.gitignore` — сразу; `rm -rf` — после копирования evidence | **evidence:** `target-impl/release/vks-server` sha256 `05f1ce8180e43a74…` (совпал с паспортом мастер-плана); `target-impl/debug/…` 170 МБ и `target-proto/debug/…` 169 МБ — копировать не нужно |
| H-24 | `docs/audit/2026-10-25/` | **233 МБ** | `du -sh docs/audit/2026-10-25`; крупнейшие: `measure/vks-server.iter{2..7}`, `.iter4a`, `.iter3b`, `.frozen` — **9 бинарей × ~27 МБ** | **в git не коммитить**: оставить `measure/SHA256SUMS` + 1 бинарь (`.frozen`) или только sha; остальное — на диск/артефакты | прямая поправка к прошлому аудиту (§6.2) |
| H-25 | `vks_data.db` 1.0 МБ, `vks_data.db.lock`, `vks_server.log` 1.1 МБ | — | все три **игнорируются** (`*.db`, `vks_data.db.*`, `*.log`) | не трогать (рабочая БД/лог живого сервиса) | — |
| H-26 | `tests/e2e/node_modules` 19 МБ, `client/pnm-frontend/archive/` 30 МБ | — | `git check-ignore` → игнорируются | не трогать | — |
| H-27 | `scripts/bench` 5.4 МБ (untracked) | 5.4 МБ | `scripts/bench/media/.gitignore` (`*.h264` и др.) → при `git add scripts/bench` медиа **не** попадёт; реально в git уйдёт ~700 КБ кода+README | коммитить код, не медиа | вложенный `.gitignore` работает |

`.gitignore` — 4 дыры (проверено `git check-ignore -v`): `target-impl`, `target-proto`, `.m7`, `docs/audit/*/measure`
(+ правило на `*.iter*`/`*.frozen`). `artifacts` — **игнорируется корректно** (`.gitignore:43 /artifacts/`;
мой первый `check-ignore artifacts` без слэша дал ложный «NOT IGNORED»).

### 2.5 CI-дыры (`.github/workflows/ci.yml`, 13 джоб)

| # | Дыра | Доказательство | Что добавить | Стоимость |
|---|---|---|---|---|
| C-1 | **E2E Playwright отсутствует** | `grep -c 'e2e\|playwright\|node ' ci.yml` → **0**; при этом `tests/e2e/*.js` = 25, `playwright 1.64.1`, `node_modules` 19 МБ | джоба: `setup-node@v4` (24), `npm ci`, поднять release-бинарь с `VKS_ALLOW_INSECURE_DEFAULTS=1`, прогнать `test_layout_constructor.js` + `crawl_all_tabs.js` | ~3 мин |
| C-2 | **`MANIFEST.sha256` не проверяется** | `sha256sum -c` локально → **214/214 OK**, но CI об этом не знает | `cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256` | 15 с |
| C-3 | **`chunks/` не защищены от правок** (сейчас правка есть!) | `git diff --stat -- client/pnm-frontend/` → `chunks/index6.bundle…js \| 2 +-`, `chunks/index-legacy6…js \| 2 +-` (mtime 17:43:55) | `git diff --exit-code -- client/pnm-frontend/deployed/pnm/chunks/` — вендор неизменяем | 5 с |
| C-4 | `doc` — `continue-on-error: true` | `ci.yml:223` | снять после починки ~30 битых intra-doc-ссылок | — |
| C-5 | `test-external` — `continue-on-error: true` | `ci.yml:153` | снять; либо удалить джобу вместе с фичей `oxideav` (§5) | — |
| C-6 | `cargo test` без `--all-features` | `ci.yml:110` `cargo test --workspace --locked --no-fail-fast` | `--all-features` (после решения по `oxideav`) | +время |
| C-7 | **Нет `cargo test --release`** — единственный timing-гейт никогда не бежит | `grep -n "cargo test" ci.yml` → 6 строк, `--release` нет; `crates/vks-media-mcu/tests/mcu_tick_soak.rs:339 #[ignore = "release-only timing gate…"]` | джоба `cargo test --release -p vks-media-mcu --test mcu_tick_soak -- --ignored` | ~2 мин |
| C-8 | 8 `#[ignore]` не запускаются нигде | `grep -rn "#\[ignore" crates/ --include=*.rs` → 8 атрибутов (комментарии не считая) | 1 нужен CI (C-7); 3 `live_*` — только вручную; 2 ffmpeg; `grids_geometry_golden` — генератор | — |
| C-9 | **Python: CI 3.13 vs стенд 3.14.4** | `ci.yml:238 python-version: "3.13"`; `python3 -V` → 3.14.4 | матрица `[3.13, 3.14]` | ×2 на джобу |
| C-10 | Кэш ключуется **только** по `Cargo.lock` | все `key: cargo-*-${{ hashFiles('**/Cargo.lock') }}`; `.cargo/config.toml` (`target-cpu=native`) и `rust-toolchain.toml` в ключ **не входят** | добавить оба файла в `hashFiles`, в идеале `restore-keys` | 0 |
| C-11 | Нет линта не-Rust | 25 JS + 20 SH + 12 PY; синтаксис-проверка: `node --check` × 25 → **0 ошибок**, `bash -n` × 20 → **0**, `py_compile` × 12 → **0** | `node --check`, `bash -n`, `ruff check` — дешёвые гейты | 20 с |
| C-12 | `path-with-spaces` собирает только debug и без clippy/test | `ci.yml:190–214` | добавить `cargo test -p vks-proto -p vks-common` | ~1 мин |
| C-13 | **`cargo-deny` без `[bans]`** | `deny.toml` секции `[graph]/[licenses]/[advisories]/[sources]` — `[bans]` нет; при этом в lock **28 имён с 2+ версиями** | `[bans] multiple-versions = "warn"` + `skip` для `windows-sys`/`windows-targets` и т.п. | 0 |
| — | `cargo audit` — **не дыра** | `[advisories]` покрывает RustSec (`vulnerability/unsound` = error) | не добавлять | — |
| — | **Текущее дерево не проходит `fmt`** | `cargo fmt --all -- --check` → diff в `crates/vks-gateway-sip/src/lib.rs:23,32` (порядок `pub mod rtcp;`) и `rtcp.rs:69` (схлопывание `VideoFeedback{…}`) | починить до коммита | 1 мин |

---

## 3. Полный список конфигов: вердикт

| Файл | Вердикт | Комментарий / конфликт |
|---|---|---|
| `Cargo.toml` (workspace) | **ок, 1 конфликт** | 16 членов = 15 `crates/*` + `client/vks-client-sdk` ✔ (`cargo metadata` → 16). Профили только здесь ✔. Конфликт: `rust-version = "1.94"` дублирует пин тулчейна (см. `.cargo`) |
| `crates/*/Cargo.toml` ×15 + `client/vks-client-sdk` | **6 — править, 10 — ок** | лишние deps: `vks-cluster`, `vks-recorder`, `vks-server`, `vks-media-sfu`, `vks-client-sdk`, `vks-common`, `vks-media-mcu` (H-11…H-17). `vks-gateway-sip` пинит `oxideav-*`/`rusty-opus` через `=0.0.7` — намеренно, оставить |
| `Cargo.lock` | **ок, не трогать** | 434 пакета; 28 имён с 2+ версиями — transitive (`windows-sys` 0.48/0.52/0.61, `rand` 0.8/0.9/0.10, `getrandom` 0.2/0.3/0.4, `syn` 2/3, `thiserror` 1/2, `toml_edit` 0.22/0.25). Единственный «наш» источник дубля — `crates/vks-auth/Cargo.toml:20 rand = "0.8"` (→0.8.8, но `sqlx-postgres` всё равно тянет 0.8.8, выигрыш нулевой). Вердикт: **не трогать**, вместо этого `[bans]` (C-13) |
| `rust-toolchain.toml` | **ок (источник истины)** | `channel = "1.94.0"`, `components = [clippy, rustfmt]`, `targets = [x86_64, wasm32]`. Факт: `rustc 1.94.0 (4a4ef493e 2026-03-02)`, активен по `override` из этого файла ✔ |
| `.cargo/config.toml` | **КОНФЛИКТ (P0)** | `[target.x86_64-unknown-linux-gnu] rustflags = ["-C","target-cpu=native"]` (:22–23) + комментарий «sha256 бинаря воспроизводим» — **ложь** (native зависит от CPU хоста); противоречит собственной строке «для прода — x86-64-v3» и `ARCHITECTURE_MASTER_PLAN.md:145–147`. Файл **не в CI-кэш-ключе** ⇒ артефакты чужой микроархитектуры → `SIGILL`. Вердикт: `native` убрать из трекаемого файла (в `[env]`/`config.stand.toml` вне git), в CI/прод — `x86-64-v3` |
| `deny.toml` | **ок, 3 правки** | нет `[bans]` (C-13); комментарий «после удаления rvoip TLS-стек отсутствует» устарел — `ring 0.17.14` в графе через `str0m/rust-crypto`; `[graph].targets` дублирует `rust-toolchain.toml` (дрейф безвреден, но лучше один источник) |
| `.rustfmt.toml` | **объединить** | `edition = "2021"` дублирует манифест (rustfmt берёт оттуда) — 1 строка вон; `max_width = 120` противоречит `docs/rules/CODE_STYLE.md:8` («rustfmt со стандартными настройками», дефолт 100) — привести правило к 120 |
| `.gitignore` | **КОНФЛИКТ (4 дыры)** | добавить `/target-impl/`, `/target-proto/`, `/.m7/`, `docs/audit/*/measure/*.iter*`, `*.frozen`. Остальное корректно (проверено `check-ignore` по 9 путям) |
| `configs/default.toml` | **ок для dev, 2 замечания** | 16/16 полей ✔. Расхождения с `Config::default()` — **9 из 16** (`listen_addr` 8080↔`0.0.0.0:443`, `media_listen_addr` 40000↔`127.0.0.1:0`, `database_url`, `jwt_secret`, `admin_password`, `allow_insecure_defaults`, `ice_servers`, `server_name`, `candidate_addr`). Опасны два: **(а)** код-дефолт `0.0.0.0:443` — привилегированный порт; **(б)** `media_listen_addr = "0.0.0.0:40000"` объявляется в SDP как `candidate_port`, но в unit документирован диапазон 43000–43100 |
| `configs/prod.example.toml` | **ок** | 14/16 полей; отсутствуют `jwt_secret` и `admin_password` — **намеренно** (только env). Не стартует by design (`CHANGE_ME` → exit 78) ✔ |
| `configs/` (прочее) | — | ничего нет (только 2 файла) ✔ |
| `.github/workflows/ci.yml` | **КОНФЛИКТ (13 дыр, §2.5)** | 13 джоб; 14×`--locked`; 2×`continue-on-error`; комментарий в конце «E2E (chromiumoxide) добавляется в фазе 8» устарел (в репо Playwright) |
| `deploy/systemd/vks-mcu.service` | **ок, 1 замечание** | Ставленный юнит `~/.config/systemd/user/vks-mcu.service` **побайтово совпал** с шаблоном (`diff` → IDENTICAL). `Documentation=file://%h/mcu/…` — зависит от симлинка `/home/pro/mcu → /home/pro/Рабочий стол/mcu` (создан 21.10). Юнит **сам по себе не стартует**: `VKS_WEBRTC_TRANSPORT=1` + `candidate_addr=""` → exit 78; значения приходят из drop-in `stand.conf` (вне git) |
| `deploy/systemd/vks-mcu-prod.service` | **ок** | hardening полный; `LoadCredential`; `RestartPreventExitStatus=78`; закомментированный ICE-диапазон |
| `deploy/vks.env.example` | **ок, 1 замечание** | `TRANSPORT=0` vs dev-юнит `=1` — разные назначения, но шапки «для какого юнита» нет; нет и не может быть `VKS_RTP_PORT_*` — ключа не существует |
| `deploy/README.md` | **устарел/неполон** | таблицы портов нет; **RTP-пул 41000–41200 не упомянут ни разу**; заявлено «the only end-to-end client is tests/e2e/webrtc.js; the PNM UI is not yet wired» |
| `client/pnm-frontend/NOTES.md` | **ок, 2 неточности** | «210 строк» манифеста (`:65`) — фактически **214**; провенанс/пересборка описаны верно |
| `client/pnm-frontend/MANIFEST.sha256` | **ок по содержимому** | 214 строк / 214 файлов, `sha256sum -c` → **214/214 OK** (срез 17:45); формат `./`-префикса теперь однороден (был ручной дефект) |
| `client/pnm-frontend/deployed/pnm/index.html` | **ок** | порядок: `api-errors.js` → `perf-overrides.js` → `theme-toggle.js` → `codian-layouts.js` → вендорский `index.bundle-2.2.1.66.js` (module). Проектный слой до вендора — обязательно так. CSP-хэши inline-скриптов считает сервер при старте (`sha2`/`base64`) |
| `tests/e2e/package.json` | **править** | `scripts` = `{test: "node live.js"}` при 25 файлах; 7 скриптов не упомянуты нигде (H-08) |
| `docs/architecture/ARCHITECTURE.md` | **устарел** | §3: `.cargo/config.toml # wasm rustflags` (:100), `ports/signaling_transport.rs`, `decoder.rs`, `encoder.rs`, `vks-admin … + utoipa`, `vks-gateway-sip (rvoip)`, `vks-server/src/{config,bootstrap}.rs`, `vks-server/configs/{default,dev,production}.toml` — **не существует**; `openapi.json` подан как рабочая спека |
| `docs/architecture/ARCHITECTURE_MASTER_PLAN.md` | **конфликт** | `:151` «ICE 42000–65000, RTP 20000–39999 (после P0-патча; сейчас 41000–41200)» — третья портовая политика, не совпадающая ни с кодом, ни с unit; `:118` «rustc 1.94.0 (LLVM 21.1.8)» — верно |
| `docs/architecture/ADR_DRAFTS.md` | **ок** | честно фиксирует `41000–41200` = 100 пар = 50 видеозвонков, отсутствие env-ключа |
| `DARK_THEME_PNM_PLAN.md` (корень) + `docs/DARK_THEME_PNM_PLAN.md` | **объединить/удалить 1** | md5 разные, `diff` = **68 добавленных строк** в корневой версии (секция D1–D5). Корневая — надмножество. Оставить одну (в `docs/`), корневую удалить; обе ссылаются на удалённый `scripts/qwen.sh` (6 отсылок) |
| `docs/CLEANUP_AUDIT_2026-10-23.md`, `docs/CONFIG_PACKAGE_AUDIT_2026-10-23.md` | **ок, но дубль жанра** | 24 и 21 строка, повторяют выводы; `CONFIG_PACKAGE_AUDIT` уже содержит неточность (перечисляет `parking_lot` как рабочий стек — он транзитивный) |
| `docs/progress/MASTER_TASK_LIST.md` (976 стр) | **править** | устаревшие утверждения: «устранены новые Clippy lints **Rust 1.99.0**» (стек — 1.94.0), «27 экранов/334 кнопки» → позже «29/383», «**вендорные чанки на 100% нетронуты (0 изменений)**» — **уже неверно** (17:43:55), «`sha256sum -c` 100% OK» — верно |

---

## 4. Безопасный порядок очистки

### Волна A — можно сразу, без риска (≈15 мин, ничего не компилируется)

> **Срез 17:47 (после моего замера):** параллельный агент уже внёс в `.gitignore` три из четырёх правил —
> `/target-impl/` (:5), `/target-proto/` (:6), `docs/audit/**/measure/vks-server.*` (:57). Осталось:
> `/.m7/` (проверено: `git check-ignore .m7` → NOT IGNORED). Пункты 1 и 13 ниже считать частично
> закрытыми; `du -sh docs/audit/2026-10-25` по-прежнему 233 МБ на диске — правило лишь не даёт им попасть в git.


1. `.gitignore`: осталось `+ /.m7/` (остальные три правила уже внесены параллельно, см. врезку выше).
2. `rm -rf .m7 scripts/__pycache__ scripts/bench/__pycache__` (пустое дерево + 396 КБ кэшей).
3. `cargo fmt --all` (или ручная правка `gateway-sip/src/lib.rs:23,32` + `rtcp.rs:69`) — **иначе CI красный**.
4. Удалить `openapi.json` (H-06) и абзац в `crates/vks-proto/README.md:15–16`.
5. Удалить `codian-layouts.js:141` и `:1420–1426` (H-04/H-05) → **пересчитать `MANIFEST.sha256`** (см. ниже).
6. `node --check` на изменённом JS; `sha256sum -c` 214/214.

Пересчёт манифеста без импорта (у `scripts/import-pnm-frontend.sh` режима «только манифест» нет — флаги только
`--force/--with-source/--gzip`, строка генерации `:289`):
```bash
cd client/pnm-frontend/deployed/pnm \
  && find . -type f -print0 | sort -z | xargs -0 sha256sum > ../../MANIFEST.sha256 \
  && cd .. && sha256sum -c MANIFEST.sha256 | grep -v ': OK$'   # должно быть пусто
```

### Волна B — после проверки/согласования (1 коммит = 1 смысл)

7. Мёртвый Rust: H-01 (`viewer.rs`), H-02 (mock-кодеки), H-03 (4 функции `rtcp.rs`) — **отдельно** от правок конфигов. Гейт: `cargo check --workspace --all-features --all-targets --locked` + `cargo test -p vks-layout -p vks-media-mcu -p vks-gateway-sip`.
8. Роль-нормализация H-18 (поведенчески нейтрально): гейт `cargo test -p vks-storage -p vks-admin`.
9. Лишние deps H-11…H-17 (по одному крейту): гейт — тот же `check` + `cargo tree -i <dep>` до/после.
10. `.cargo/config.toml`: убрать `target-cpu=native` из трекаемого файла → `x86-64-v3` в CI; `native` — в незакоммиченный `config.stand.toml` или `[env]` стенда. Гейт: sha256 бинаря до/после на **одной** машине (изменится — это ожидаемо), и `cargo build --release --locked` без env.
11. CI: C-2 (manifest), C-3 (chunks guard), C-10 (cache key), C-11 (`node --check`/`bash -n`) — дешёвые и не блокирующие.
12. `target-proto/` (4.7 ГБ) — удалить **после** копирования `target-impl/release/vks-server` (sha `05f1ce81…`) в `docs/progress/evidence/` или `/tmp/vks-evidence/`.

### Волна C — только с решением Lead (§5)

13. `docs/audit/2026-10-25/measure/` — оставить 1 бинарь + `SHA256SUMS`, остальные 8 удалить (233 МБ → ~28 МБ) **до** любого `git add`.
14. `target-impl/` (5.9 ГБ) — `rm -rf` только после п.12.
15. `target/debug` (46 ГБ) — **последним**, после финальной release-верификации; `target/release/vks-server` не трогать.
16. `docs/architecture/*`, мастер-план, `DARK_THEME_PNM_PLAN.md` — текстовые правки одним docs-коммитом.

### Нельзя трогать

- `target/release/vks-server` (живой бинарь, sha `94484d42…`, 28.10 11:44) — сервис PID 1418719 работает из него.
- `vks_data.db`, `vks_data.db.lock`, `vks_server.log` — рабочее состояние живого стенда.
- `crates/vks-media-mcu/src/farm.rs` + `crates/vks-server/src/farm_metrics.rs` — **несущие** (проверено: `media-mcu/src/lib.rs:45,58`, `engine.rs:28`, `server/src/lib.rs:9,136,171,273`, `gateway_video.rs:514–521`).
- `crates/vks-gateway-sip/src/rtcp.rs` — **не удалять целиком**: `build_rr`/`parse_compound` уже подключены (`gateways.rs:359,377`).
- `deploy/systemd/vks-mcu.service` и drop-in `~/.config/systemd/user/vks-mcu.service.d/stand.conf` — согласованы (`diff` → identical).
- `docs/audit/2026-10-28/evidence/*.png` — evidence текущего прохода.

---

## 5. Что требует решения Lead

| # | Вопрос | Почему без Lead нельзя | Моя рекомендация |
|---|---|---|---|
| L-1 | **Портовая политика** (5 источников) | Любой сдвиг `media_listen_addr`/ICE ломает SDP-кандидат и ICE-путь шлюзов; нужен вердикт `gateways`+network | Не менять значения. Сделать **одну таблицу** в `deploy/README.md` (порт → носитель → ключ → default) + `VKS_RTP_PORT_MIN/MAX` в `gateways.rs` вместо `RtpPortPool::default()` (41000–41200 hardcoded) |
| L-2 | **`target-cpu`**: `native` (стенд, максимум perf, непереносимо) vs `x86-64-v3` (воспроизводимость) | Прямо влияет на замеры perf и на атрибутируемость sha256 | В git — `x86-64-v3`; `native` — только локальный неотслеживаемый файл; в CI — `x86-64-v3`; паспорт сборки в evidence |
| L-3 | **`oxideav` (пустая фича)**: заморозить или удалить | ADR-0004; удаление ломает `--all-features` в 3 джобах одновременно | Заморозить: убрать `--features oxideav` из CI, пометить `reserved`, добавить инвариант `cargo tree -i oxideav-scene` = 1 |
| L-4 | **Патч вендорского чанка** (17:43:55) | Это чужая незакоммиченная работа; `import-pnm-frontend.sh --force` её **сотрёт** | Перенести guard `(this.sessionLayoutListByType.default\|\|[])` в проектный слой (`codian-layouts.js`), чанки вернуть `git checkout --`; иначе фикс исчезнет при следующем импорте |
| L-5 | **Судьба untracked-кода**: `farm.rs`, `farm_metrics.rs`, `rtcp.rs`, `scripts/bench/`, `docs/audit/*` | В `HEAD` их нет → checkout «сломан» для сборки | Коммит 1: `farm.rs`+`farm_metrics.rs`+`rtcp.rs`; коммит 2: `docs/architecture/*` + `scripts/bench` (без медиа); `docs/audit/2026-10-25` — **без** `measure/*.bin` |
| L-6 | **233 МБ бинарей в `docs/audit/2026-10-25/measure/`** | git-история необратима (bfg/filter-repo) | Хранить sha в `SHA256SUMS` + 1 бинарь; остальные 8 — удалить до `git add` |
| L-7 | **`media_listen_addr` default в коде** (`127.0.0.1:0` → случайный SDP-кандидат) | Менять код-дефолт = менять поведение прод-конфигов, где ключ забыт | Сделать загрузку strict: если TOML задан и в нём нет ключа — error (сейчас figment молча берёт код-дефолт) |
| L-8 | `cargo test --all-features` в блокирующей джобе | Может открыть предсуществующие падения | Включить после L-3 и прогона на раннере |

---

## 6. С чем не согласен с аудитом `docs/audit/2026-10-28`

### 6.1 `rtcp.rs` — «ни одного `rtcp::` вне файла» — **неверно**

`05_hygiene.md:51` (S2): «Подключён… но **ни одного** `rtcp::` вне самого файла: `grep -rn 'rtcp' crates/`
даёт только `rtp_bridge.rs`/`uas.rs`». Факт (28.10, 17:40):

```
$ grep -rn "\bparse_compound\b\|\bbuild_rr\b" crates/ --include=*.rs | grep -v gateway-sip/src/rtcp.rs
crates/vks-server/src/gateways.rs:359:        let report = vks_gateway_sip::rtcp::build_rr(ssrc);
crates/vks-server/src/gateways.rs:377:                    let seen = vks_gateway_sip::rtcp::parse_compound(&buffer[..count]);
```

Это новый незакоммиченный `spawn_video_rtcp` (`git diff gateways.rs` → `+142…+184`), т.е. на момент прошлого
аудита вызовов действительно не было, **но вывод «мёртв» уже неверен и его нельзя протаскивать в план**:
мёртвы 4 функции из 6 (`is_rtcp`, `parse_feedback`, `build_pli`, `build_fir`), живут `build_rr`/`parse_compound`.
Формулировка «load-bearing, коммитить, не удалять» (`00_MASTER_PLAN.md:364`; «без них рабочее дерево не собирается» — `:293–294`) верна только для двух функций — я против
удаления файла и за удаление PLI/FIR-части.

### 6.2 «`docs/audit/2026-10-25/` — 7 файлов, 700 КБ, коммитить, не удалять» — **неверно в 300 раз**

```
$ du -sh docs/audit/2026-10-25
233M
$ find docs -type f -size +2M -printf "%s %p\n" | sort -rn | head -3
27178544 docs/audit/2026-10-25/measure/vks-server.iter7
27173424 docs/audit/2026-10-25/measure/vks-server.iter6
27162640 docs/audit/2026-10-25/measure/vks-server.iter5
```

9 бинарей `vks-server` (~27 МБ каждый: `.frozen`, `.iter2…iter7`, `.iter3b`, `.iter4a`). Рекомендация
«коммитить» невыполнима: это ~225 МиБ в git-истории навсегда. Заменяю на: **`SHA256SUMS` + 1 бинарь**.

### 6.3 «Блокер: живой сервис крутит бинарь 25 окт» — **снят, но появился новый**

```
$ ls -la target/release/vks-server → 27192592 Oct 28 11:44
$ sha256sum target/release/vks-server → 94484d4287c71e1c…2af0bc7e
$ systemctl --user status vks-mcu → Active: running since 2026-10-28 11:44:12 MSK, Main PID 1418719
```

Т.е. бинарь свежее подавляющего большинства кода (новее — только `crates/vks-admin/src/api_v3/resources.rs`,
17:40). Прошлый аудит строил все рантайм-выводы на «устаревшем бинаре» — теперь наоборот: **его собственные
измерения (PID 2965299, sha `ef37731f…`) устарели**, а мастер-план (`00_MASTER_PLAN.md:391`) до сих пор
сравнивает sha, которого на диске нет.

### 6.4 «44 `env::var` против 16 полей Config» — **цифра верна, вывод смещён**

Пересечение — не «много», а **2 ключа** (`VKS_JWT_SECRET`, `VKS_JWT_TTL_SECS`, и оба — в `#[cfg(test)]`
`config.rs:375–400`). Значит: **42 ключа вне TOML**, из них 9 тестовые (`VKS_LIVE_*` ×6, `VKS_SOAK_OUT`,
`VKS_STORAGE_PG_URL`, +`VKS_SDP_*` частично) ⇒ **~33 реальные runtime-ручки** (SIP/H.323/WebRTC/codec/farm)
физически невыразимы в `configs/*.toml`. Правильная формулировка — «TOML не является источником истины для
двух третей настроек», а не «44 vs 16».

### 6.5 «Две несогласованные портовые политики» (C-01) — **их пять**

1. `configs/default.toml:13` → `0.0.0.0:40000` (и этот порт уходит в SDP: `main.rs:282–307 candidate_port = local_addr.port()`);
2. `crates/vks-gateway-sip/src/rtp_bridge.rs:20–22` → `Self::new(41000, 41200)` — **hardcoded, env-ключа нет** (подтверждено: `grep -rn "VKS_RTP_PORT" crates/` → 0);
3. `deploy/systemd/vks-mcu.service:27–28` → `VKS_WEBRTC_PORT_MIN/MAX=43000/43100` (в коде дефолт — эфемерный `bind(0)`, `webrtc_room.rs:103–105`);
4. `docs/architecture/ARCHITECTURE_MASTER_PLAN.md:151` → «ICE 42000–65000, RTP 20000–39999 (после P0-патча)»;
5. `docs/rules/BACKEND_RULES.md:86` → «RTP 40000–60000», и там же 443/8443/3478/5349/9090/4222/5432 — ни одного из этих портов сервер не слушает.

Опаснее всего не «две политики», а то, что **41000–41200 не упомянут в `deploy/` ни разу** (проверено
`grep -n "41000" deploy/ -r` → 0): firewall по документации откроет 43000–43100 и 40000, и звонки шлюзов
поедут без медиа.

### 6.6 Мелочи, которые считаю неверными/неточными

- **C-05** («дубль `[graph].targets` vs `rust-toolchain.toml`»): не конфликт — дрейф даёт лишь
  `warning[license-not-encountered]`, exit 0. Понижаю до info.
- **«`layout_descriptor` (pending) без прод-потребителей»** (`00_MASTER_PLAN.md:141, :298`): метод помечен
  «(test/inspector)» в самом коде (`engine.rs:1178`) и используется 16 тестами; это **осознанный тест-API**,
  а не забытый код. Не кандидат на удаление — кандидат на `#[doc(hidden)]`.
- **G2** («8 `#[ignore]` не запускаются нигде»): верно, но из 8 только **один** стоит CI-денег
  (`mcu_tick_soak`, release); 3 — живые звонки на физические терминалы, 3 — ffmpeg, 1 — генератор golden.
  Формулировка «8 тестов невидимы» преувеличивает риск.
- **«`fmt` зелёный»** нигде не проверялось: `cargo fmt --all -- --check` сейчас **падает** (2 файла).
  Это не «предсуществующее», а следствие незакоммиченного `rtcp.rs` — т.е. блокер коммита, а не долг.
- **«Закоммитить `rtcp.rs`, иначе `cargo build` сломан»**: сборка не «сломана» — файл на диске, `mod rtcp;`
  объявлен; ломается только **checkout** (`HEAD` без файла). Формулировку надо разделить.

### 6.7 С чем согласен (проверил сам)

Дубли фикстур — нет (`sha256sum` 6 файлов → 0 повторов). Мёртвых `[workspace.dependencies]` — нет (каждая
из 22 записей имеет ≥1 потребителя). `*_tests.rs` — не сироты (`#[cfg(test)] #[path]`). `farm.rs`/`farm_metrics.rs`
— несущие. `MANIFEST` — 214/214 OK. `oxideav` — пустая фича (`grep -rn "oxideav_scene::"` → 0). Пустой
`prod.example.toml` не стартует — by design.

---

## 7. Что не проверено (границы достоверности)

1. **Ничего не собиралось и не запускалось** (запрет). `cargo metadata --no-deps`, `cargo tree --offline`,
   `cargo fmt --check` — единственные запущенные cargo-команды. Оценки «скомпилируется после удаления»
   не проверены компилятором.
2. `cargo-udeps`/`machete`/`deny`/`audit` — **отсутствуют**; H-11…H-17 — grep-эвристика (помечено).
3. `docs/progress/evidence/**` (15 МБ) — не разбирался построчно.
4. `scripts/bench/*.py` (3.6 тыс. строк: `sipbot.py` 1038, `sipbot_reorder.py` 1068, `h323bot.py` 700) —
   проверен только синтаксис и наличие README; корректность методики — зона `perf_lab`.
5. `client/pnm-frontend/NOTES.md` — 36 КБ прочитаны по разделам и адресным проверкам, не построчно.
6. `docs/audit/2026-10-28/{01,02,03,04,06,07}*.md` — использованы адресно (для §6), не рецензировались целиком.
7. Количество битых intra-doc-ссылок («≈30» из прошлого аудита) не перемерялось: `cargo doc` не запускался.

---

### Приложение: ключевые команды среза

```bash
cargo metadata --no-deps --offline        # 16 членов, edition 2021, rust_version 1.94
cargo tree -i rand@0.8.8 --offline        # rand 0.8.8 ← sqlx-postgres + vks-auth(rand="0.8")
cargo fmt --all -- --check                # FAIL: gateway-sip/src/lib.rs:23,32; rtcp.rs:69
grep -rho 'env::var("VKS_[A-Z0-9_]*"' crates/ --include=*.rs | sort -u | wc -l   # 44
git check-ignore -v target-impl target-proto .m7 docs/audit/2026-10-25           # ни одного правила
sha256sum target-impl/release/vks-server   # 05f1ce81… (evidence, копировать перед rm -rf)
sha256sum target/release/vks-server        # 94484d42… (живой бинарь, 28.10 11:44)
cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256 | grep -c ': OK$'   # 214
node --check tests/e2e/*.js; bash -n scripts/*.sh; python3 -m py_compile scripts/*.py         # 0 ошибок
```

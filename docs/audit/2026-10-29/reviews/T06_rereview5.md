# T06 — повторное ревью №5 (HEAD `25c9d99`, фикс P1 из rr3/rr4)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, ветка `master`.
- **Проверяемый HEAD:** `25c9d99` «fix(T06,T07): declared capacity growth + empty AUTO windows…»
  (поверх `0febfe5`/`214a673`; reviewed commit, не dirty worktree).
- **Предыдущие ревью:** `T06_rereview3.md` — 6.5, done no (2 P1: pre-merge ceiling 409 на рост;
  occupancy vs validate); `T06_rereview4.md` — 8.8, done no (P1-3: пустые AUTO вне ёмкости
  отравляют PATCH).
- **Полигон:** `git archive 25c9d99 | tar -x -C /dev/shm/t06r5` (без `.git`/`target`),
  scratch-копия `/dev/shm/t06r5_scratch` для route/engine-проб и негативных контролей.
  `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr5_target` (гейты) и
  `/home/pro/.cache/t06_rr5_scratch_target` (пробы), Rust **1.94.0** из `rust-toolchain.toml`,
  `cargo --offline`; `/tmp` не использовался.
- **Live-среда:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на
  `:8080`/`:18082` не запускался/не опрашивался, browser e2e не запускался. В live-репо записан
  только этот файл.

## Вердикт

**Verdict: 9.0 / 10 — done: no. 0 P0, 1 P1, 2 P2, 4 P3.**

Все проверяемые P1 из rr3/rr4 **закрыты и подтверждены исполнением** на реальных route-хендлерах:
рост `vks_grid_5→vks_grid_9` (200/size 9), `13→43` (200/size 43), mode `preset_equal_25` на 13
(200/size 25), detach 9 + `preset_equal_5` + rename (200/200), grid-4 + пустые AUTO 1..5 + rename
(200, prune до 4), explicit junk (409/422), contact-only legacy 9 → shrink 5 → size 9 → rename 200,
seeded legacy `abonent_id` 9 → rename 200, attach 1..9 = 200 и 10-й = 409, MCU рисует 9 окон.
Гейты зелёные (fmt 0, clippy 0, layout 54, admin 354, server 147+3 ignored).

Но остаётся **один P1 того же класса «ёмкость растёт без объявления»**, теперь на mode-эндпоинте:
`declared_capacity` там берётся из `inferred_size`, в который входит `slots.len`
(`shell.rs:985-1010`), поэтому mode-запрос **без preset/size** с занятыми слотами расширяет
`size` (13→25, 5→25), а mode с `preset_equal_25` + явным слотом 26 принимает запрос 200 и молча
вырезает слот вместо 409. PATCH ведёт себя правильно (409); mode расходится с ним и с commit
message «only when the request declares it (preset/size)». Поэтому done: no.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** (P1-5: mode-эндпоинт объявляет ёмкость через `slots.len`) |
| **P2** | **2** (устаревший §4.6; UI full-save легаси-записи с эхом пустых AUTO) |
| **P3** | **4** |

---

## 1. Приёмка по списку rr3/rr4 — исполнение на pristine `25c9d99`

Все пробы — scratch-тесты в `/dev/shm/t06r5_scratch` через реальные
`create` / `edit_global` / `edit_linked` / `attach` / `attach_layout_abonent` /
`detach_layout_abonent` / `switch_video_layout_mode`; вывод — `--nocapture`.

| # | Сценарий (реальные хендлеры) | Факт | Статус |
|---|---|---|---|
| 1 | PATCH `vks_grid_5` → `vks_grid_9`, 9 AUTO-слотов | **200**, `size=9`, slots 9 | **PASS** |
| 2 | PATCH `vks_grid_13` → `vks_grid_43`, 43 AUTO-слота | **200**, `size=43`, slots 43 | **PASS** |
| 3 | mode `preset_equal_25` на «Общей» 13 (25 слотов) | **200**, `size=25`, slots 25 | **PASS** |
| 4 | mode preset-only `preset_equal_25` (без slots) | **200**, `size=25` | **PASS** |
| 5 | detach всех 9 + `preset_equal_5` + rename | 204×9, PATCH shrink **200**, size 5/slots 5, rename **200** | **PASS** |
| 6 | ровно порядок rr4: shrink 5 → attach → detach all → rename | size 9→5, slots 5, rename **200** | **PASS** |
| 7 | один detach p9 у legacy 9 (preset 5) → rename | 204, size 8/slots 8, rename **200** | **PASS** |
| 8 | grid-4 + пустые AUTO 1..5 → rename | **200**, `size=4`, slots prune 5→4 | **PASS** |
| 9 | explicit junk (PATCH): slots-only 1..9; preset9+pos10 (empty/occupied); dup; pos0; pos string; missing pos; non-object slot | **409** (`outside layout of 5/9`, `duplicate slot position 9`, `must be >= 1`), **422** (`must be an integer`, `is required`, `items must be objects`), запись не изменена | **PASS** |
| 10 | contact-only legacy 9 → `preset_equal_5` → rename | size **9**, slots 9, rename **200** | **PASS** |
| 11 | seeded legacy `abonent_id` 9 (stale size 5) через `put_configuration_document`+`sync_from_storage` | `size_after_sync=9`; rename **200**; attach 1..9 = **[200×9, 409]**; slots 9 | **PASS** |
| 12 | engine: `GatewayManager::apply_layout` + `McuEngine`, 9 contact-only слотов, `preset_equal_9` | `assignments=9 geometry=9 slot_rects=9`, `preset=preset_equal_9`, `mode=manual` | **PASS** |
| 13 | rename-only PATCH (без payload) | **200**, size/slots не изменились | **PASS** |
| 14 | PATCH через `edit_linked` 5→9 (scoped, COW-путь) | **200**, size 9, slots 9 | **PASS** |

Фрагменты вывода проб:

```text
RR5 GROWTH preset 5->9 status=200 OK size=9 slots=9
RR5 GROWTH preset 13->43 status=200 OK size=43 slots=43
RR5 GROWTH edit_linked 5->9 status=200 OK size=9 slots=9
RR5 MODE preset_equal_25 on 13 status=200 OK size_before=13 size=25 slots=25
RR5 LEGACY AUTO grid4+auto1..5 rename status=200 OK size=4 slots=4
RR5 DETACH all detach_fail=0 size_after_detach=9 slots=9
RR5 DETACH shrink 9->5 status=200 OK size=5 slots=5
RR5 DETACH rename status=200 OK
RR5D shrink-before-detach status=200 OK size=9 slots=9
RR5D after detach-all size=5 slots=5
RR5D rename status=200 OK
RR5D detach-one size=8 slots=8 status=204 No Content
RR5 CONTACT shrink 9->5 status=200 OK size=9 slots=9
RR5 CONTACT rename status=200 OK
RR5 SEED abonent9 size_after_sync=9 slots=9
RR5 SEED rename status=200 OK
RR5 SEED attach statuses=[200, 200, 200, 200, 200, 200, 200, 200, 200, 409] size=9 slots=9
RR5 NEG slots-only 1..9 empty: status=409 ... unchanged=true
RR5 NEG preset9 + empty pos10: status=409 ... unchanged=true
RR5 NEG preset9 + duplicate pos: status=409 ... unchanged=true
RR5 NEG preset9 + pos string: status=422 ... unchanged=true
RR5 ENGINE assignments=9 geometry=9 preset=preset_equal_9 mode=manual
RR5 ENGINE slot_rects=9
```

Вывод: заявленные фиксы (a) declared growth для PATCH, (b) игнор/prune пустых AUTO вне ёмкости,
(c) prune при detach/mode/update, (d) 4 route-теста — работают; helper `explicit_layout_capacity`
(`resources.rs:436-448`) и `validate_layout_assignments_with_incoming` (`:1532-1572`) действительно
несущие (см. NC-A/NC-B в §3).

## 2. P1-5 — mode-эндпоинт расширяет ёмкость без preset/size (открыт)

`shell.rs:985-1010`: `inferred_size` — это `size → slots.len → preset → сохранённый size`,
и в `declared_capacity = Some(inferred_size)` (`:1010`) попадает **`slots.len`**. Тогда
`validation_capacity = max(stored, declared)` (`:1026-1030`) и `recalc_layout_size` дают рост.
PATCH так не умеет: `explicit_layout_capacity` (`resources.rs:436-448`) явно **не** включает
`slots.len` («сюда НЕ входит fallback slots.len»).

### 2.1 Воспроизведение (pristine, реальный mode-хендлер)

1. Создать раскладку с ёмкостью 13:
   `POST /api/v3/video_layouts {"display_name":"D13occ","payload":{"type":"individual","mode":"auto","preset":"preset_default_13","size":13,"slots":[]}}` → 201.
2. Привязать к сессии: `POST /api/v3/sessions/{room}/video_layouts/{id}` → 204.
3. `POST /api/v3/ui/video_layouts/mode` с телом
   `{"session_id":…,"video_layout_id":…,"mode":"manual","payload":{"mode":"manual","slots":[{"position":1,"role":"PEER","abonent_id":"ab-1"},…,{"position":25,…"ab-25"}]}}`
   — **без preset и без size**.

Факт: **200 OK**, в записи `payload.size = 25`, 25 слотов. Ожидание по контракту фикса:
**409** (ёмкость не объявлена). То же для mode-only на легаси-записи без распознаваемого preset
(`preset:""`, size 5): **200**, size 5→**25**. На канонической «Общей» без занятых слотов рост
не закрепляется только потому, что `recalc_layout_size` возвращает preset 13, а слоты 14..25
молча вырезаются (200/size 13) — то есть поведение зависит от того, заняты ли «лишние» слоты.

### 2.2 Вторая грань: explicit junk на mode не даёт 409

`POST /ui/video_layouts/mode` c `payload":{"mode":"manual","preset":"preset_equal_25","slots":[1..26 AUTO]}`
(26-й слот — явный мусор за пресетом) → **200 OK**, `size=25`, 25 слотов (позиция 26 молча
вырезана). Причина та же: `slots.len=26` попадает в `declared_capacity`, а не ёмкость пресета 25.
Эквивалентный PATCH с `preset:"vks_grid_9"` + pos10 даёт **409** (проверено). Это прямо нарушает
требование (b) «explicit slots in a request beyond capacity still 409» на одном из двух write-путей.
Duplicate-позиции на mode при этом корректно дают **409** (`RR5C MODE preset25 + dup5 status=409`).

### 2.3 Предлагаемый фикс (проверен как NC-D)

В `shell.rs` считать `declared_capacity` только из preset/size (как `explicit_layout_capacity`),
не из `inferred_size`/`slots.len`; `inferred_size` оставить только для записи `payload.size`:

```rust
declared_capacity = preset_field.as_ref().and_then(Value::as_str).and_then(|preset| {
    vks_layout::grids::slot_count(preset)
        .or_else(|| vks_layout::codian::slot_count(preset))
        .or_else(|| vks_layout::preset_size(preset))
}).map(|n| n as u64).or(size);
```

Проверено на pristine-копии (NC-D): mode `preset_equal_25` + 25 слотов → **200/size 25**;
preset25 + pos26 → **409**; slots-only 25 (empty и occupied) → **409**; detach/легаси-сценарии
не затронуты. Рекомендуется также переиспользовать `resources::explicit_layout_capacity`
(сделать `pub(super)`), чтобы правило было одно, и добавить committed-тесты:
`mode_slots_only_growth_is_409`, `mode_preset_growth_with_junk_is_409`, `mode_preset_growth_13_to_25`.

## 3. Негативные контроли (pristine scratch, реальные хендлеры)

| # | Что откатили | Результат | Вывод |
|---|---|---|---|
| **NC-A** | `validation_capacity` в `update_resource` → только `stored_capacity` (игнор declared) | рост 5→9 и 13→43 → **409**, size 5/13; mode preset 25 остался 200/25 | фикс (a) несущий |
| **NC-B** | убран `if !from_request && !slot_holds_participant { continue; }` (`resources.rs:1567-1570`) | grid-4 + AUTO 1..5 rename → **409** `slot position 5 is outside layout of 4 windows` | фикс (b) несущий |
| **NC-C** | mode: `declared_capacity` только при наличии size/preset, но по-прежнему из `inferred_size` | slots-only → 409 ✔, но preset25+pos26 → **200** ✘ | недостаточный фикс |
| **NC-D** | mode: `declared_capacity` из preset/size (см. §2.3) | slots-only 25 (empty/occupied) → **409**; preset25+pos26 → **409**; preset25 → **200/25** | **предлагаемый фикс корректен** |

## 4. Regression risk (запросы)

| Проверка | Факт |
|---|---|
| PATCH slots-only 1..9 без preset/size (empty и occupied) на ёмкости 5 | **409**, запись не изменена |
| PATCH preset-рост + duplicate/pos0/pos string/missing/non-object | **409/409/422/422/422**, без частичного merge (`unchanged=true`) |
| PATCH preset9 + pos10 (empty или occupied) | **409**, запись не изменена |
| PATCH rename-only | **200**, size/slots не меняются |
| PATCH `size`-only рост (size 9, 9 AUTO) на `vks_grid_5` | **200**, но `size=5`, слоты 6..9 вырезаны (P3-1) |
| mode slots-only (см. §2) | **200** и рост при занятых слотах / без preset (P1-5) |
| mode preset25 + pos26 | **200**, слот 26 вырезан (P1-5) |
| mode preset25 + duplicate | **409** |
| mode `size:25` + 25 слотов на preset 13 | **200**, но size 13 (тихий no-op, P3-1) |

## 5. Документация

- **`BACKEND_RULES.md` §4.6 (`:192-195`) — P2, устарело/вводит в заблуждение.**
  «attach, validate и mode используют один и тот же резолвер ёмкости, причём validate берёт ёмкость
  из СОХРАНЁННОЙ записи (до merge), чтобы новые позиции в запросе не расширяли ёмкость» — после
  `25c9d99` validate берёт `max(stored, declared)`, и рост через объявленный preset/size разрешён.
  Не описаны также: пропуск пустых AUTO вне ёмкости, `prune_empty_slots_beyond_capacity`, prune при
  detach/mode/update. Остальные утверждения §4.6 (`slot_holds_participant`, 8 ключей, «пустые AUTO
  не раздувают») соответствуют коду.
- **`LAYOUT_ENGINE.md` §12 (`:239-250`) — PASS**: 3×2, `426+426+428`, `x=214`, высоты 360,
  площадь `767 520`, `83.28 %` — совпадает с S6-тестом (1 passed).
- **`LAYOUT_ENGINE.md` §13 (`:252-268`) — PASS**: `is_known_preset` = manual ∪ Codian, `Default`
  не входит; `preset_default_13` → 400 в `calculate` — committed-тест
  `pnm_calculate_uses_reference_preset_geometry` 1 passed.

## 6. Гейты (pristine `25c9d99`, offline, Rust 1.94.0)

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all --check` | **0** | без вывода |
| `cargo clippy -p vks-layout -p vks-admin -p vks-server --all-targets --offline -- -D warnings` | **0** | предупреждений нет |
| `cargo clippy --workspace --all-features --all-targets --locked --offline -- -D warnings` | **0** | `Finished dev profile in 54.26s` |
| `cargo test -p vks-layout --lib --offline` | **0** | **54 passed**, 0 failed |
| `cargo test -p vks-admin --lib --offline` | **0** | **354 passed**, 0 failed (34.73 s) |
| `cargo test -p vks-server --lib --offline` | **0** | **147 passed, 3 ignored**, 0 failed (20.05 s) |
| `-- --list` (layout/admin) | 0 | **54 / 354**, дублей имён нет |
| 4 новых route-теста в `--list` | — | все присутствуют: `declared_preset_growth_is_accepted_and_validated`, `legacy_empty_auto_slots_beyond_capacity_do_not_block_rename`, `detached_empty_windows_do_not_poison_record`, `mode_preset_switch_recalculates_capacity` |
| точечно `grid_five_slots_at_1280x720_fill_is_83_28_percent` | 0 | 1 passed |
| точечно `contact_only_legacy_slots_keep_capacity_across_preset_shrink` | 0 | 1 passed |
| точечно `pnm_calculate_uses_reference_preset_geometry` | 0 | 1 passed |
| engine-проба (scratch `vks-server`) | 0 | `assignments=9 geometry=9 slot_rects=9` |

Числа сходятся с ожиданием: admin 354 = 350 (rr4) + 4 новых; layout 54 без изменений;
server 147+3 ignored (+1 к rr4 за счёт T07-тестов в том же коммите).

## 7. Остаточные P2/P3

| # | Sev | Проблема | Файл:строки |
|---|---|---|---|
| P1-5 | **P1** | mode: `declared_capacity` включает `slots.len` → рост без preset/size и 200 вместо 409 на explicit junk | `shell.rs:985-1010`, `:1026-1030` |
| P2-1 | P2 | §4.6 описывает pre-merge stored capacity, но код использует `max(stored, declared)`; не описаны skip/prune пустых AUTO | `docs/rules/BACKEND_RULES.md:192-195` |
| P2-2 | P2 | UI full-save легаси-записи: PATCH с эхом сохранённых пустых AUTO 1..5 за ёмкостью 4 → **409** (`size=4 slots=5`), т.к. они приходят «из запроса»; rename-only при этом 200. Закреплено только rename-only-тестом | `resources.rs:1567-1570`; `resource_tests.rs:2629` |
| P3-1 | P3 | `size`-only рост (без preset) на preset-записи: 200, но size остаётся пресетным, лишние слоты вырезаны — тихий no-op при объявленной ёмкости | `resources.rs:436-448`, `:1599`, `:1615-1623`; `:470-479` |
| P3-2 | P3 | `position == 0` → 409 `Conflict` вместо 422 `Invalid` (pre-existing, rr4 P2-4) | `resources.rs:1558-1561` |
| P3-3 | P3 | `normalize_loaded` меняет только память; storage хранит stale `size` до первого commit (pre-existing rr4 P2-3) | `state.rs:711`; `resources.rs:1201` |
| P3-4 | P3 | Пробелы committed-тестов: mode-тест проверяет только shrink 13→5 (не рост 13→25 и не slots-only); `declared_preset_growth…` не проверяет junk, хотя назван `and_validated`; `legacy_empty_auto…` — только rename; `detached…` использует порядок detach→shrink, а не rr4-порядок shrink→detach | `resource_tests.rs:2600`, `:2629`, `:2664`, `:2721` |

## 8. Ограничения и гигиена

- Browser e2e не запускался (по заданию); вывод по e2e-оракулу — только статический, приёмка
  «оракул стабилен» исполнением не подтверждена. Engine-проба — scratch-тест на
  `GatewayManager::apply_layout` + `McuEngine`, не live-сервер.
- Live-БД `vks_data.db`: sha256 `687b758f…caa81ce`, mtime `1793266711`, size 3248128 — **до и после
  совпадают**; SQLite не открывался. Порты `:8080` (:3255206) и `:18082` (:3253281) — те же PIDs,
  сервер не стартовал. `backups/` (mtime 07:32) не тронут.
- В live-репо записан только `docs/audit/2026-10-29/reviews/T06_rereview5.md`. Прочие изменения
  worktree (`M Cargo.lock`, `M crates/...`, `?? docs/...`) — не мои, существовали до ревью.
- Scratch-копии (`/dev/shm/t06r5`, `/dev/shm/t06r5_scratch`, `/dev/shm/t06r5_logs`) и
  `CARGO_TARGET_DIR` (`t06_rr5_target`, `t06_rr5_scratch_target`) удалены после сдачи; `/tmp` не
  использовался. Перед пробами файлы pristine-копии сверены с `25c9d99` (`cmp` — OK после
  восстановления NC).

## 9. Счёт

**9.0 / 10, done: no.**

**+**
- Все 12 пунктов приёмки rr3/rr4 пройдены исполнением на реальных route-хендлерах, включая
  ранее падавшие 5→9, 13→43, mode→25, detach+shrink+rename, grid-4+AUTO+rename, seeded legacy,
  attach 1..9/10-й и MCU 9/9/9.
- NC-A/NC-B доказывают, что declared growth и skip/prune пустых AUTO — несущие, а не декоративные.
- Гейты зелёные, 4 committed route-теста, дублей тестов нет; §12/§13 точны.

**−**
- **P1-5:** mode-эндпоинт расширяет ёмкость через `slots.len` без preset/size и принимает explicit
  junk за пресетом (200 вместо 409) — расходится с PATCH, commit message и требованием (b).
  Предложенный фикс проверен (NC-D), но в коммит не входит.
- §4.6 устарел относительно нового `max(stored, declared)`; UI full-save легаси-записи с эхом
  пустых AUTO по-прежнему 409.

До done нужно: закрыть P1-5 по §2.3 и добавить committed-тесты на mode slots-only / preset+junk /
рост 13→25; переписать §4.6. До 9.5+ — ещё решить P2-2 (dedupe «эхо сохранённого слота» или
задокументировать двухшаговый протокол) и P3-4.

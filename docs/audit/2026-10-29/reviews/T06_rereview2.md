# T06 — повторное ревью №2 layout-правок (`c030dff`, HEAD `57c2b4b`)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **HEAD:** `57c2b4b5a377195bb73fb57278b98a1d998d2eda` "docs(webrtc): T11a — document ICE side effect
  of rejected re-offers".
- **Проверяемый фикс:** `c030dff55b1e077127fb01dc7a3df0c14e6118ee` "fix(layout): T06 review pass 2 —
  never shrink capacity below occupied slots" (2026-10-29 11:55:30 +0300). Между `c030dff` и HEAD
  только SDK/WebRTC-коммиты; layout-код не менялся.
- **Предыдущее ревью:** `docs/audit/2026-10-29/reviews/T06.md` — 8.0/10, open P1 (normalize сжимает
  legacy-ёмкость), open P2-13 (tiny canvas panic/underflow), §13 неточен, `BACKEND_RULES` §4.6
  отсутствует, e2e-оракул не доказан.
- **Чистое дерево:** `git archive 57c2b4b | tar -x -C /dev/shm/t06rr_1793264375_4188889`; все сборки,
  тесты и пробы — только в этой копии, `CARGO_TARGET_DIR` внутри неё, `--locked --offline`; `/tmp` не
  использовался. Копия удалена после ревью.
- **Live-БД/сервер:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на
  `:8080` не запускался и не опрашивался. E2E в браузере не запускался.
- **Единственная запись в live-репо:** этот файл. Чужие изменения в worktree
  (`M docs/audit/.../pitfalls/T06.md`, `?? T08.md`, `?? T09a_rereview.md`, `?? T11a_rereview3.md`)
  не мои и на HEAD/архив не влияют.

## Verdict

**Score: 7.5 / 10. T06 НЕ done.**

`c030dff` действительно меняет `payload.size` при загрузке legacy-записи с 5 на 9 и убирает
underflow-панику `gallery`. Но **P1 не закрыт**: ёмкость, которую применяют PATCH и attach, по-прежнему
считается старым резолвером (`preset_size`/`derived_layout_size`), а не новым `recalc_layout_size`.
Запись с `preset_equal_5`/`vks_grid_5` + 9 слотами после normalize имеет `size = 9` в payload, но PATCH
rename даёт **409** «slot position 6 is outside layout of 5 windows», attach принимает **только 5**
абонентов, а MCU рисует **9**. То есть в админке теперь две истины (payload 9, enforcement 5), и
заявленное в commit message «a rename no longer 409s» неверно.

| Критерий приёмки | Вердикт | Где доказано |
|---|---|---|
| P1: normalize не сжимает legacy-ёмкость | **FAIL по поведению** | §1: load size 9, но PATCH 409 и attach limit 5 |
| P1: PATCH rename не 409, attach до 9, engine/admin agree | **FAIL** | §1.2–1.3: rename 409, attach `[200×5, 409×5]`, engine 9 |
| P2-13: tiny canvas без panic/underflow, count == requested, rects внутри | **PARTIAL** | §2: panic/count исправлены; на 1×1 42/43 rect вне холста |
| Docs: §13 и §4.6 верны, без противоречий | **PARTIAL** | §3: §13 исправлен, §4.6 добавлен, но его never-shrink-утверждение ложно |
| S6-тест точен, clippy `-D warnings`, `--list` без дублей, admin lib зелёный | **PASS** | §4 |
| Новые P0/P1 | **нет новых P0; P1-1 остаётся** | §5 |

- **Open P0: 0.**
- **Open P1: 1** — P1-1: `recalc_layout_size` пишет 9, но attach/validate применяют ёмкость пресета 5;
  PATCH rename 409, attach лимит 5, MCU 9.
- **Open P2: 5.**
- **Open P3: 7.**

---

## 1. P1 — **НЕ ИСПРАВЛЕН** (acceptance-проба)

### 1.1 Что изменено и чего не хватает

`c030dff` добавил в `recalc_layout_size` (`crates/vks-admin/src/api_v3/resources.rs:1378-1401`) учёт
`max(slot.position)`:

```rust
let size = match (derived, occupied) {
    (Some(derived), Some(occupied)) => Some(derived.max(occupied).min(MAX_WINDOWS)),
    ...
};
```

Это меняет только сохраняемое поле `payload.size`. Пути, которые **принимают решение** о допустимости
слота, не тронуты:

| Путь | Код | Ёмкость | Использует `max(position)`? |
|---|---|---|---|
| attach | `resources.rs:960` | `layout_size(...).unwrap_or(13)` | **Нет** |
| validate (PATCH/mode) | `resources.rs:1450-1454` | сначала `preset_size(preset)`, иначе `layout_size` | **Нет** |
| `layout_size`/`derived_layout_size` | `resources.rs:376-395` | preset capacity → `payload.size` → `slots.len` | **Нет** |
| `recalc_layout_size` | `resources.rs:1378-1401` | derived **max** `max(position)` | Да |

`preset_size` (`crates/vks-layout/src/lib.rs:59-70`) сначала вызывает `grids::slot_count` и
`codian::slot_count`, поэтому `preset_size("vks_grid_5") == Some(5)` и
`preset_size("preset_equal_5") == Some(5)`; в validate этот вызов стоит **раньше** `layout_size`,
поэтому даже записанный `size = 9` не учитывается. `recalc` идёт перед validate, но validate его
результат игнорирует.

### 1.2 Пробы через admin-роуты (pristine-копия, `MemoryStorage`)

Запись засеяна в configuration-document (`CONFIGURATION_KEY`) как
`{"preset": <p>, "size": 13, "slots": [position 1..9, role AUTO]}` и загружена через
`AppState::sync_from_storage()` (реальный `normalize_loaded`). Абоненты `p1..p10`
зарегистрированы в комнате; PATCH — через `edit_linked` (реальный PATCH-роут сессии).

| Шаг | `preset_equal_5` | `vks_grid_5` |
|---|---|---|
| `payload.size` после load | **9** | **9** |
| слотов после load | 9 | 9 |
| PATCH `{"display_name":"Renamed"}` | **409** `slot position 6 is outside layout of 5 windows` | **409**, тот же текст |
| attach #1..#10 | `[200,200,200,200,200,409,409,409,409,409]` | то же |
| занятых окон / `size` после attach | 5 / 9 | 5 / 9 |
| layout без слотов + `vks_grid_5` (stale size 13) | **5** | **5** |
| detach всех 9 + PATCH `{"preset":"vks_grid_5","slots":[]}` | 200, **size 5**, затем attach `[200×5, 409]` | то же |

То есть «size становится 9» и «законное сжатие до 5» работают, а **rename и attach — нет**. Запись
остаётся нередактируемой ровно как в T06 §1: PATCH 409 на существующих позициях 6..9, attach
«full» после 5, при этом `payload.size` и UI говорят 9.

### 1.3 Engine/admin agreement — **FAIL**

Scratch-тест в `vks-server` (копия; `GatewayManager::apply_layout` + `McuEngine::new()`, 9
забридженных участников, 9 слотов с `abonent_id`, `mode:"auto"`, `size:9`):

| Preset | `descriptor.geometry` | `assignments` | `slot_rects` |
|---|---|---|---|
| `preset_equal_5` | 0 (auto) | 9 | **9** |
| `vks_grid_5` | 9 (regrid) | 9 | **9** |

MCU рисует 9 окон, admin attach разрешает 5. Расхождение не только осталось, но стало заметнее:
payload админки теперь тоже 9, а attach/validate — 5, то есть противоречие внутри админки.

### 1.4 Почему тесты этого не ловят

- `recalc_layout_size_распознаёт_конструктор` (`resource_tests.rs:2352-2390`) проверяет только JSON
  после `recalc_layout_size` (`size == 9`, `no_slots == 5`), но не PATCH/attach.
- `tiny_canvas...` и остальные новые тесты к P1 отношения не имеют.
- Интеграционного теста «legacy 9 слотов → load → rename проходит, attach лимит 9, движок 9» в
  коммите нет. Поэтому зелёный `cargo test -p vks-admin --lib` (348 passed) P1 не опровергает.

### 1.5 Рекомендация

Сделать `layout_size` (или единый резолвер) учитывающим `max(position)`:
`size = max(preset capacity, payload.size, max(position), slots.len)`, затем использовать его в
attach (`:960`) и validate (`:1450-1454`) **без** отдельного `preset_size`-вызова. Либо в validate
сравнивать `position` с `payload.size` после `recalc` (и всё равно перевести attach на тот же
резолвер). Добавить роут-тест: load legacy (9 слотов, preset 5) → PATCH rename 200 → attach 1..9 OK,
#10 409 → повторный normalize идемпотентен.

---

## 2. P2-13 — **PARTIAL**: panic/underflow исправлены, «rects внутри» — нет

Пробы через реальный `video_layouts_calculate` (admin-роут, debug):

| Запрос | count | panic | outside canvas | max bottom | canvas |
|---|---|---|---|---|---|
| `vks_grid_5`, 9×10 | **5** | нет | **0** | 8 | 9×10 |
| `vks_grid_43`, 1×1 | **43** | нет | **42** | **43** | 1×1 |
| `vks_grid_100`, 0×0 | **100** | нет | **0** | 720 | 1280×720 (fallback) |

Дополнительно `cargo test -p vks-layout --test t06_rr_probe` в копии с
`RUSTFLAGS="-C overflow-checks=off"` — те же результаты: **паники/overflow нет, count совпадает**.
Underflow `count - row*cols` действительно закрыт `saturating_sub` (`grids.rs:262`), margin
клампится (`:246`).

Остаток: на холсте 1×1 `cell_h`/`cell_w` зажаты `.max(1.0)` (`grids.rs:257-258`), поэтому 43 окна
выстраиваются в 43 ряда по 1 px и уходят вниз до `y = 42` при холсте высотой 1. Никакого клампа
rect к холсту в `gallery` нет (в `preset::geometry` он есть через `scale_rect`, в `grids::gallery` —
нет). Это не паника и не порча данных, но контракт «rect внутри холста» для 1×1 нарушен (P2).

**Дефект нового теста:** `tiny_canvas_never_panics_or_overflows_the_grid`
(`grids.rs:770-790`) в assert'е проверяет только `x`/`width`:

```rust
rect.x >= 0.0 && rect.y >= 0.0 && rect.x + rect.width <= width.max(1.0) + 1e-9
```

`y + height <= height` не проверяется, поэтому тест **зелёный**, хотя на 1×1 42 из 43 rect вне холста,
а на 0×0 (прямой вызов `gallery`) `max y+h` равен 100. Комментарий теста «Every rect must stay finite
and inside the canvas» и commit message «no rect leaves the canvas for tiny/degenerate sizes»
не соответствуют поведению.

Рекомендация: либо масштабировать ячейки ниже 1 px на вырожденных холстах, либо клампить
`x/y/w/h` к холсту и пересчитывать `y`; в тесте проверять обе оси (`x+w <= w`, `y+h <= h`) и
`assert_inside`.

---

## 3. Документация — **PARTIAL**

### 3.1 `LAYOUT_ENGINE.md` §13 — **исправлен**

`docs/architecture/LAYOUT_ENGINE.md:252-268` больше не утверждает «у calculate нет таблицы
координат». Проверено по коду: `preset::geometry` для `PresetScheme::Default` действительно даёт
`equal_slots(count)` (`crates/vks-layout/src/preset.rs:647`), `equal_slots` есть (`:361`), а 400 —
политика реестра (`is_known_preset` = manual ∪ Codian; `Default` не входит). Формулировка про
«равный фолбэк существует, но calculate не публикует не-реестровые id» соответствует коду.
Противоречий в §13 не найдено.

### 3.2 `BACKEND_RULES.md` §4.6 — **добавлен, но не полностью верен**

`docs/rules/BACKEND_RULES.md:174-184` действительно содержит §4.6: порядок резолвера
(`grids::slot_count` → `codian::slot_count` → `preset_size` → `payload.size` → `slots.len`),
кламп `preset_size` 1..=100, 400 для `preset_default_13`. Порядок совпадает с
`derived_layout_size` (`resources.rs:385-395`).

Но:

1. **Утверждение «`size` никогда не опускается ниже `max(slot.position)`… не должна ломать
   PATCH/attach» ложно для фактического поведения.** `payload.size` действительно не опускается, но
   attach/validate его не читают (см. §1): PATCH rename 409, attach лимит 5. Документ описывает
   инвариант, которого нет на API-границе, — это прямое противоречие коду и причине P1.
2. **Нарушен порядок подразделов:** §4.6 стоит **перед** §4.5 (`BACKEND_RULES.md:174` vs `:186`),
   хотя §4.4 → §4.6 → §4.5. Косметика, но для нормативного документа — P3.
3. В списке резолвера нет `max(slot.position)` — новый never-shrink не описан как часть ёмкости,
   хотя ниже он заявлен.

Итог: «docs now true / no contradictions» не выполнено из-за п.1 (P2-уровень, поскольку это лицо
незакрытого P1).

---

## 4. S6, тесты, clippy — **PASS**

| Проверка | Результат |
|---|---|
| `cargo test -p vks-layout --lib -- --list` | **54 tests**, дублей нет (`uniq -d` пуст), оба S6-имени по одному разу |
| `cargo test -p vks-layout --lib grid_five` | **2 passed, 0 failed** |
| S6-тест `grid_five_slots_at_1280x720_fill_is_83_28_percent` (`grid.rs:186-201`) | точные rect'ы (включая `x=640`, `w=428`, `x=214`), `assert_no_overlap`/`assert_inside`, площадь `767_520`, отношение `0.8328125` с `1e-9` — **не изменён и точен** |
| `cargo test -p vks-layout --locked` | lib **54** + integration **31** = **85 passed, 1 ignored** |
| `cargo test -p vks-admin --lib --locked` | **348 passed, 0 failed** |
| `cargo clippy -p vks-layout --all-targets --locked -- -D warnings` | **rc=0** |
| `cargo clippy -p vks-admin --all-targets --locked -- -D warnings` | **rc=0** |
| `rustfmt --check` (`grids.rs`, `grid.rs`, `resources.rs`, `resource_tests.rs`) | **чисто** |

Примечание: 54-й lib-тест — новый `tiny_canvas_never_panics_or_overflows_the_grid`; он зелёный,
но его оракул неполный (§2), поэтому «зелёный тест» не доказывает заявленное свойство.

---

## 5. Новые P0/P1

**Новых P0 нет.** **Новых P1 нет**, но P1-1 из T06 остаётся открытым (это не регрессия `c030dff` —
фикс просто не дошёл до enforcement-путей). Дополнительный негативный эффект: `c030dff` создал
вторую истину в админке (`payload.size=9` vs attach/validate=5) и добавил документ/тест, которые
заявляют несуществующее поведение.

---

## 6. Остаточные P2/P3

| # | Sev | Проблема | Файл:строки |
|---|---|---|---|
| P1-1 | **P1** | `recalc` пишет 9, но attach/validate применяют ёмкость пресета 5 → PATCH rename 409, attach лимит 5, MCU 9 | `resources.rs:960`, `:1450-1454`, `:1378-1401`, `:385-395`; `lib.rs:59-70` |
| P2-1 | P2 | normalize memory-only: storage держит stale size до первого commit | `state.rs:708-712`; `resources.rs:257-280` |
| P2-2 | P2 | Резолвер `layout_size` не учитывает `max(position)`: sparse `[{"position":4}]` → `Some(1)`, attach лимит 1 (корень P1) | `resources.rs:385-395` |
| P2-3 | P2 | `position == 0` → 409 `Conflict`, хотя malformed payload → должен 422 `Invalid` | `resources.rs:1463-1464` |
| P2-4 | P2 | tiny canvas: на 1×1 42/43 rect вне холста; тест проверяет только `x/w`, не `y/h` | `grids.rs:246-268`, `:779-786` |
| P2-5 | P2 | E2E-оракул: `vendor400Count >= 1`, `calculate.length >= 7`, фиксированные `waitForTimeout`, default `:8080`; вживую не проверялся | `tests/e2e/layout_constructor_ui.js:21`, `:156-170`, `:207`, `:293`, `:315`, `:317` |
| P3-1 | P3 | `preset_size` не валидирует схему: `preset_evil_4` → `Some(4)`, `calculate` → 400 | `lib.rs:59-70` |
| P3-2 | P3 | `§4.6` стоит перед `§4.5` — нарушен порядок подразделов | `docs/rules/BACKEND_RULES.md:174`, `:186` |
| P3-3 | P3 | validate дублирует `preset_size` перед `layout_size` — второй путь резолва, разошедшийся с `recalc` | `resources.rs:1450-1454` |
| P3-4 | P3 | mode-endpoint: inline `inferred_size` (`size` первым) полностью перезаписывается `recalc` — мёртвая ветка | `shell.rs:983-1017` |
| P3-5 | P3 | attach-fallback 13 vs validate `None` — асимметрия для legacy без preset/size/slots | `resources.rs:960`, `:1450` |
| P3-6 | P3 | `remove_global` глушит результат применения (`let _ = push_layout_to_engine`), не отдаёт `engine/pending/saved-only` | `resources.rs:1755-1761` |
| P3-7 | P3 | Список резолвера в §4.6 не включает `max(slot.position)`, хотя ниже он заявлен как инвариант | `docs/rules/BACKEND_RULES.md:174-180` |

Пункт P3-2 прошлого ревью (`payload.size` без клампа) больше не воспроизводится путём записи: `recalc`
клампит `size` до `MAX_WINDOWS` (`resources.rs:1393-1396`), поэтому `u64::MAX` из PATCH сохраняется
как 100. Отдельного пункта не держу (hardening `layout_size` без клампа остаётся, но достижимого
пути не видно).

---

## 7. Команды и окружение

Из `/dev/shm/t06rr_1793264375_4188889` (`git archive 57c2b4b`), `CARGO_TARGET_DIR` внутри копии,
`--locked --offline`.

| Команда | Результат |
|---|---|
| `cargo test -p vks-layout --lib -- --list` | 54 tests, дублей нет |
| `cargo test -p vks-layout --lib grid_five` | 2 passed |
| `cargo test -p vks-layout` | 85 passed, 1 ignored |
| `cargo test -p vks-admin --lib` | 348 passed |
| `cargo clippy -p vks-layout -p vks-admin --all-targets -- -D warnings` | rc=0 (по отдельности) |
| `rustfmt --check` | чисто |
| scratch: load `preset_equal_5`/`vks_grid_5` + 9 слотов + size 13 | size 9; rename 409; attach `[200×5,409×5]`; MCU rects 9 |
| scratch: load `vks_grid_5` без слотов (size 13) | size 5 |
| scratch: detach 9 + PATCH `{"preset":"vks_grid_5","slots":[]}` | 200, size 5, attach 5 затем 409 |
| scratch: `calculate` 9×10/1×1/0×0 | 5/43/100; outside 0/42/0; panic нет; с `-C overflow-checks=off` то же |
| scratch: `GatewayManager::apply_layout` + 9 участников | `preset_equal_5`: rects 9; `vks_grid_5`: geometry 9, rects 9 |

Все scratch-правки (resource_tests, gateways.rs, `tests/t06_rr_probe.rs`) внесены только в
`/dev/shm`-копию, после проб файлы восстановлены из `57c2b4b`, копия удалена. Live-репо не менялся,
кроме этого отчёта.

### Ограничения

- Live-БД не открывалась: неизвестно, есть ли в проде записи с `capacity < max(position)`. P1 доказан
  на синтетической legacy-записи через реальный load/PATCH/attach и на коде, который применяется ко
  всем загружаемым документам.
- Engine-проба — scratch-тест в копии (`GatewayManager::apply_layout` + `McuEngine`), не live server;
  она воспроизводит именно `gateways.rs:1855-1937` и авто-regrid движка.
- E2E в браузере не запускался; вывод по оракулу — статический (P2-5).

---

## Score: 7.5 / 10

- **+** `normalize_loaded` больше не сжимает `payload.size` ниже `max(position)`: загрузка legacy
  9 слотов даёт 9, запись без слотов — 5, законное сжатие (detach all + `slots: []` + grid-5) — 5.
- **+** Underflow-паника `gallery` закрыта (`saturating_sub`, кламп margin); `calculate` 9×10, 1×1,
  0×0 больше не падает, count совпадает, overflow-checks=off не меняет результат.
- **+** §13 `LAYOUT_ENGINE` исправлен по факту (`equal_slots` есть, 400 — политика реестра);
  `BACKEND_RULES` §4.6 появился; S6-тест по-прежнему точный; clippy/fmt/list/admin lib зелёные.
- **−** **P1 не закрыт:** `recalc` и enforcement используют разные резолверы. PATCH rename 409,
  attach лимит 5, MCU 9 — ровно исходный дефект, плюс новая внутренняя противоречивость
  (`payload.size=9` vs attach=5). Commit message «a rename no longer 409s» неверен.
- **−** P2-13 закрыт частично: на 1×1 42/43 rect вне холста, а новый тест этого не проверяет
  (нет `y + height`), хотя комментарий и commit message обещают «no rect leaves the canvas».
- **−** §4.6 содержит never-shrink-утверждение, которого нет в поведении, и стоит перед §4.5.
- **−** P2-1…P2-5 (кроме закрытой паники) остаются; e2e-оракул не доказан.

До 9.5+ не хватает: (1) провести `max(position)` через **все** пути ёмкости (`layout_size`/attach/
validate), а не только через `recalc`, и покрыть роут-тестом rename/attach; (2) довести tiny-canvas
до «rect внутри холста» и проверять обе оси в тесте; (3) привести §4.6 в соответствие с поведением
и порядком разделов; (4) закрыть P2-3/P2-5. P1-1 — блокирующий для «done».

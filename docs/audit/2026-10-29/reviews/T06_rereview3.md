# T06 — повторное ревью №3 layout-правок (`7199818`, HEAD `4686ac8`)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **HEAD:** `4686ac8` "docs(webrtc)+test: T11a comment accuracy and TrackAdded regression".
- **Проверяемый фикс:** `7199818` "fix: close T06/T08/T09a re-review blockers" (2026-10-29 12:20:45 +0300).
  `git diff 7199818 4686ac8 -- crates/vks-admin/src/api_v3/resources.rs crates/vks-admin/src/api_v3/shell.rs
  crates/vks-layout/src/grids.rs crates/vks-layout/src/grid.rs docs/rules/BACKEND_RULES.md
  docs/architecture/LAYOUT_ENGINE.md` пуст: layout-код и оба документа после фикса не менялись.
- **Предыдущее ревью:** `docs/audit/2026-10-29/reviews/T06_rereview2.md` — 7.5/10, open P1 (attach/validate
  применяют ёмкость пресета 5 после recalc 9), P2-13 partial (rect вне холста на 1×1), §4.6 false,
  docs-порядок нарушен.
- **Чистое дерево:** `git archive 4686ac8 | tar -x -C /dev/shm/t06rr3_110045`; все сборки, тесты и
  route-пробы — только в этой копии, `CARGO_TARGET_DIR` внутри неё, `--locked --offline`; `/tmp` не
  использовался. Scratch-тесты после проб восстановлены из архива, копия удалена после ревью.
- **Live-БД/сервер:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на `:8080`
  не запускался и не опрашивался. E2E в браузере не запускался. Live worktree на момент ревью содержал
  чужие изменения других агентов; я их не трогал.
- **Единственная запись в live-репо:** этот файл.

## Verdict

**Score: 8.5 / 10. T06 НЕ done.**

`7199818` действительно закрывает ровно тот сценарий, который был в T06_rereview2: legacy-запись с
`preset_equal_5`/`vks_grid_5` и **девятью слотами с `abonent_id`** после normalize имеет `size = 9`,
PATCH rename отвечает 200, attach принимает 1..9 и отклоняет 10-го, mode-эндпоинт работает, а MCU рисует
девять окон. P2-13 тоже закрыт: degenerate canvas (`1×1`, `9×10`) уходит в 1280×720, count совпадает,
все rect'ы внутри холста по обеим осям, тест проверяет `y + height`.

Но **P1 не закрыт для другого легального представления занятого слота**. `max_slot_position` считает
занятыми только слоты с непустым `abonent_id` (`resources.rs:394-404`), тогда как `ensure_slot_roles`
(`resources.rs:1390-1392`) и движок считают занятыми также `contact_id`/`call_string_id`/camelCase.
Legacy-раскладка «preset 5 + 9 слотов, занятых только через `contact_id`/`call_string_id`» после
normalize получает `size = 5`, PATCH rename — **409**, а MCU по этим же девяти слотам рисует **9**
окон. Более того, это состояние достижимо обычной последовательностью успешных API-вызовов без правки
БД: create `preset_equal_9` → PATCH 9 contact-only слотов (200) → PATCH `preset_equal_5` (200,
`size=5`, слотов 9) → следующий PATCH rename **409**. То есть запись «отравляется» штатным PATCH'ем,
ровно как в исходном P1.

| Критерий приёмки | Вердикт | Где доказано |
|---|---|---|
| P1: legacy 9 слотов с `abonent_id`, PATCH rename 200, attach 6..9 и 10-й 409, mode, engine/admin agree | **PASS** | §1.1–1.3: size 9; rename 200; attach `[200×9, 409]`; mode 200; engine rects 9 |
| P1-негативы: `preset_equal_2`+pos3 → 409, create `preset_equal_1`+slot5 → 409, grid-4 + пустой AUTO pos5 не раздувает | **PASS** | §1.4: 409/409; attach занял 1,2, size 4, pos5 остался пустым |
| P2-13: 1×1/grid-43 и 9×10/grid-5 через route, count == requested, fallback 1280×720, rect внутри | **PASS** | §2: 43/5; точное совпадение с 1280×720; `x+w`, `y+h` внутри; тест проверяет height |
| Ёмкость: attach/validate/mode/recalc через одни helpers; новые позиции не расширяют capacity | **PARTIAL** | §1.5: PATCH pos10 → 409, mode size13 → 9; но occupancy-предикат неполный → P1-2 |
| Docs: BACKEND_RULES §4.6 true/ordered, LAYOUT_ENGINE §13 true | **PARTIAL** | §3: §4.6 после §4.5, но `max(slot.position)` буквально ложен; §13 подтверждён route-пробой 400 |
| S6 exact, нет дублей `#[test]`, clippy `-D warnings`, admin lib + layout tests green | **PASS** | §4: 54/349 тестов, списки без дублей, clippy rc=0, fmt rc=0, S6 2 passed |
| Новые P0/P1 | **P0 = 0; P1 = 1 новый** | §5: P1-2 contact-only occupancy |

- **Open P0: 0.**
- **Open P1: 1** — P1-2: occupancy для capacity учитывает только `abonent_id`; contact-only legacy
  (и достижимая PATCH-последовательность) снова даёт size 5 / rename 409 / MCU 9.
- **Open P2: 6.**
- **Open P3: 6.**

---

## 1. P1 — исходный сценарий **PASS**, contact-only **FAIL**

### 1.1 Legacy 9 слотов с `abonent_id` — все route-пробы зелёные

Запись засеяна в configuration-document (`CONFIGURATION_KEY`) как
`{"preset": <p>, "size": 13, "slots": [position 1..9, abonent_id p1..p9]}` и загружена через
`AppState::sync_from_storage()` (реальный `normalize_loaded`). Проброс — полный HTTP-router
(`tests::app` + login admin) на pristine-копии:

| Шаг | `preset_equal_5` | `vks_grid_5` |
|---|---|---|
| `payload.size` после load | **9** | **9** |
| PATCH `{"display_name":"Renamed"}` | **200**, size 9 | **200**, size 9 |
| attach p1..p10 | **`[200×9, 409]`** | **`[200×9, 409]`** |
| занятых окон после attach | 9 (10-е не добавлено) | 9 |
| mode (`new_mode:"manual"`, payload size 13, 9 слотов) | **200**, size 9 | **200**, size 9 |
| PATCH с новой позицией 10 | **409** | **409** |

### 1.2 Engine/admin agreement — **PASS** для `abonent_id`

Scratch-тест в `vks-server` (`GatewayManager::apply_layout` + `McuEngine`, 9 забридженных участников,
9 слотов с `abonent_id`, `mode:"manual"`, `size:9`):

| Preset | `descriptor.assignments` | `slot_rects` | Admin attach limit |
|---|---|---|---|
| `preset_equal_5` | **9** | **9** | **9** |
| `vks_grid_5` | **9** | **9** | **9** |

### 1.3 Почему P1 всё же открыт: occupancy только по `abonent_id`

`max_slot_position` (`resources.rs:394-404`) фильтрует исключительно
`slot.get("abonent_id").and_then(Value::as_str).is_some()`. Собственный код админки определяет
занятость шире — `ensure_slot_roles` (`resources.rs:1390-1392`) проверяет непустые
`abonent_id`, `contact_id`, `abonentId`, `contactId`, `call_string_id`, `callStringId`. Движок также
принимает contact-only слоты.

Пробы (pristine-копия, реальные route-handlers/HTTP):

| Сценарий | Результат |
|---|---|
| seed: `preset_equal_5`, `size:13`, 9 слотов с `contact_id`/`call_string_id`, без `abonent_id` → `sync_from_storage` | **`payload.size = 5`**, PATCH rename **409** |
| тот же payload → `GatewayManager::apply_layout` + 9 участников | `assignments = 9`, `slot_rects = 9` |
| **без правки БД:** POST create `preset_equal_9` | 201, size 9 |
| PATCH 9 слотов с `contact_id`/`call_string_id` | 200, size 9 |
| PATCH `{"preset":"preset_equal_5"}` | **200**, в ответе `size=5`, слотов 9 |
| PATCH `{"display_name":"Renamed"}` | **409** `slot position 6 is outside layout of 5 windows` |

Это тот же класс дефекта, что T06 P1: успешная запись делает запись нередактируемой, а admin capacity
(5) расходится с MCU (9). `contact_id`/`call_string_id` — не «пустой AUTO»: negative-требование
«пустой AUTO position 5 не раздувает capacity» должно остаться выполненным, но не должно исключать
contact-only занятые окна.

**Рекомендация:** считать occupancy тем же предикатом, что `ensure_slot_roles` (непустой любой из
шести ключей), а не только `abonent_id`; пустой AUTO по-прежнему не считать. Добавить route-регрессию
на последовательность `create equal_9 → PATCH 9 contact-only → PATCH preset_equal_5 → rename 200`, а
также на contact-only seed после `sync_from_storage`.

### 1.4 Негативные пробы — **PASS**

| Проба | Результат |
|---|---|
| create `preset_equal_2`, `slots:[]`; PATCH `slots:[{"position":3}]` | **409** |
| create `preset_equal_1` + `slots:[{"position":5}]` | **409** |
| seed `vks_grid_4` + EMPTY AUTO positions 1..5 (`size:13`); normalize | `size = 4` (pos5 не раздувает) |
| attach двух участников | заняты позиции **1,2**, pos5 остался пустым; size 4 |

### 1.5 Incoming positions не расширяют capacity — **PASS**

- PATCH legacy-9 с дополнительной позицией 10 → **409** (stored capacity 9 захвачен до merge,
  `resources.rs:1507`).
- mode с `payload.size:13` и 9 слотами → **200**, но `payload.size` после recalc = **9** (13 не
  расширил ёмкость).
- create `preset_equal_1` + slot5 → 409 (нет stored capacity, fallback на `derived_layout_size`).

---

## 2. P2-13 — **PASS**

Пробы через реальный `POST /api/v3/ui/video_layouts/calculate`:

| Запрос | count | panic | Внутри 1280×720 (`x+w`, `y+h`) | Геометрия |
|---|---|---|---|---|
| `vks_grid_43`, 1×1 | **43** | нет | все 43 | **точно равна** calculate 1280×720 |
| `vks_grid_5`, 9×10 | **5** | нет | все 5 | **точно равна** calculate 1280×720 |
| `preset_default_13` | — | нет | — | **400 `invalid.preset`** |

`gallery` теперь при `width/height < 16`, `0` или non-finite подставляет `(1280, 720)`
(`grids.rs:241-249`). Committed-тест `tiny_canvas_never_panics_or_overflows_the_grid`
(`grids.rs:773-799`) проверяет `x >= 0`, `y >= 0`, `x + width <= canvas_w`,
**`y + height <= canvas_h`** — то есть обе оси, а не только `x/w` как в прошлом ревью. Для 1×1
42/43 rect'а вне холста больше не воспроизводятся.

---

## 3. Документация — **PARTIAL**

### 3.1 `BACKEND_RULES.md` §4.6 — порядок исправлен, утверждение всё ещё ложно

Раздел теперь стоит **после** §4.5 (`BACKEND_RULES.md:179` vs `:174`), и это закрывает прошлый P3-2.
Но:

1. `:183-185` утверждает «`size` никогда не опускается ниже `max(slot.position)`». Буквально это
   неверно в двух проверенных случаях:
   - grid-4 + пустой AUTO position 5 → `size = 4 < max(position) = 5` (это ожидаемое поведение,
     negative-требование);
   - contact-only 9 слотов + preset 5 → `size = 5 < max(position) = 9` (это P1-2).
   Документ должен говорить «max(position) по **непустым занятым** слотам», а не по всем слотам.
2. Список резолвера `:180-182` (`grids → codian → preset_size → payload.size → slots.len`) не включает
   шаг `max(position занятых)`, хотя именно он определяет capacity в `layout_size`
   (`resources.rs:380-388`). Это прошлый P3-7, он остался.
3. Формулировка «attach, validate и mode используют один и тот же резолвер» в основном верна:
   attach читает `layout_size` (`resources.rs:986`), validate/mode — `stored_capacity` из
   `layout_size` до merge (`resources.rs:1507`, `shell.rs:1010`). Но create валидирует через
   `derived_layout_size` без stored capacity (`resources.rs:1568`), а mode сохраняет параллельный
   inline `inferred_size` (`shell.rs:984-1008`) — это P3-2 ниже.

### 3.2 `LAYOUT_ENGINE.md` §13 — **PASS**

`§13` (`LAYOUT_ENGINE.md:252-268`) соответствует коду: `is_known_preset` = manual ∪ Codian
(`preset.rs:681-683`), `Default` не входит; `equal_slots` существует (`preset.rs:361`), но
`calculate` не публикует non-registry id. Route-проба `preset_default_13` даёт **400
`invalid.preset`**, как и описано. Противоречий не найдено.

---

## 4. S6, тесты, clippy — **PASS**

| Проверка | Результат |
|---|---|
| `cargo test -p vks-layout --lib -- --list` | **54 tests**, `uniq -d` пуст |
| `cargo test -p vks-admin --lib -- --list` | **349 tests**, `uniq -d` пуст |
| Дубли `#[test]`/`#[tokio::test]` (статический разбор) | нет; повтор имени `create_get_roundtrip` только в разных модулях |
| `cargo test -p vks-layout --lib grid_five` | **2 passed, 0 failed** |
| S6 `grid_five_slots_at_1280x720_fill_is_83_28_percent` (`grid.rs:186-201`) | точные rect'ы, `x=640`, `w=428`, `x=214`, площадь `767_520`, ratio `0.8328125` с `1e-9` — **не изменён и точен** |
| `cargo test -p vks-layout --locked --offline` | lib **54** + integration **31** = **85 passed, 1 ignored** |
| `cargo test -p vks-admin --lib --locked --offline` | **349 passed, 0 failed** |
| `cargo clippy -p vks-layout --all-targets --locked --offline -- -D warnings` | **rc=0** |
| `cargo clippy -p vks-admin --all-targets --locked --offline -- -D warnings` | **rc=0** |
| `rustfmt --check` (`resources.rs`, `shell.rs`, `resource_tests.rs`, `grids.rs`) | **rc=0** |

Примечание: 349-й admin-тест — новый `legacy_layout_capacity_never_drops_below_occupied_slots`
(`resource_tests.rs:2514`), но он проверяет helpers (`layout_size`/validate), а не route-путь
`sync_from_storage → PATCH/attach/mode`; см. P2-2.

---

## 5. Новые P0/P1

**P0 нет.** **Новый P1 один — P1-2** (contact-only occupancy; §1.3). Исходный P1 для `abonent_id`
закрыт; это остаточная дыра того же корня, достижимая штатным PATCH-сценарием, поэтому она
блокирующая.

---

## 6. Остаточные P2/P3

| # | Sev | Проблема | Файл:строки |
|---|---|---|---|
| P1-2 | **P1** | Capacity occupancy только `abonent_id`; contact-only legacy/последовательность PATCH даёт size5, rename409, MCU9 | `resources.rs:394-404`; `:1390-1392`; `:986` |
| P2-1 | P2 | §4.6 буквально лжёт про `max(slot.position)` (пустой AUTO и contact-only), список резолвера без occupancy-шага | `docs/rules/BACKEND_RULES.md:180-185` |
| P2-2 | P2 | Нет committed route-регрессии на P1: новый тест только helper-level; attach/validate/mode wiring может сломаться незаметно | `resource_tests.rs:2514-2530`; `:2754` |
| P2-3 | P2 | normalize memory-only: storage держит stale `size` до первого commit; после рестарта нормализуется снова, но внешнее чтение БД видит старое | `state.rs:708-712`; `resources.rs:257-280` |
| P2-4 | P2 | `position == 0` → **409 Conflict**, хотя это malformed payload → должен быть 422 Invalid | `resources.rs:1481-1483` |
| P2-5 | P2 | E2E-оракул не доказан: default `:8080`, фиксированные `waitForTimeout`, `vendor400Count >= 1`, `calculate.length >= 7`; вживую не запускался | `tests/e2e/layout_constructor_ui.js:21`, `:156-170`, `:207`, `:293`, `:315`, `:317` |
| P2-6 | P2 | detach не пересчитывает `size`: после detach всех legacy-9 окон `payload.size` остаётся 9, attach-лимит уже 5; две истины до следующего PATCH | `resources.rs:1093-1118` |
| P3-1 | P3 | `preset_size` не валидирует схему: `preset_evil_4` → `Some(4)`, `calculate` → 400 | `lib.rs:59-70` |
| P3-2 | P3 | mode-endpoint: inline `inferred_size` полностью перезаписывается `recalc` — мёртвая ветка | `shell.rs:984-1008` |
| P3-3 | P3 | attach-fallback 13 vs validate `None` — асимметрия для legacy без preset/size/slots | `resources.rs:986`, `:1472` |
| P3-4 | P3 | `remove_global` глушит результат применения (`let _ = push_layout_to_engine`), не отдаёт `engine/pending/saved-only` | `resources.rs:1777` |
| P3-5 | P3 | `max_slot_position` использует `is_some()`, поэтому `abonent_id:""` считается занятым, а attach (`is_none_or(str::is_empty)`) — свободным | `resources.rs:400` vs `:1005-1007` |
| P3-6 | P3 | Два подраздела `### 4.3` подряд (preexisting, но нормативный документ) | `docs/rules/BACKEND_RULES.md:153`, `:161` |

---

## 7. Команды, окружение и ограничения

Из `/dev/shm/t06rr3_110045` (`git archive 4686ac8`), `CARGO_TARGET_DIR` внутри копии,
`--locked --offline`, `CARGO_INCREMENTAL=0` для финальных прогонов.

| Команда/проба | Результат |
|---|---|
| `cargo test -p vks-layout --locked --offline` | rc=0: 54 lib + 31 integration, 1 ignored |
| `cargo test -p vks-admin --lib --locked --offline` | rc=0: 349 passed |
| `cargo clippy -p vks-layout -p vks-admin --all-targets -- -D warnings` | rc=0 (по отдельности) |
| `cargo test -p vks-layout --lib grid_five` | 2 passed |
| `cargo test -p vks-layout --lib -- --list` / admin `--list` | 54 / 349, дублей нет |
| Scratch: seed legacy `preset_equal_5`/`vks_grid_5` + 9 `abonent_id` → HTTP PATCH/attach/mode | size 9; rename 200; `[200×9,409]`; mode 200 |
| Scratch: engine `apply_layout` + 9 участников, оба пресета | assignments 9, rects 9 |
| Scratch: seed contact-only legacy 9 слотов → normalize/rename | size 5; rename 409; engine rects 9 |
| Scratch: create equal_9 → PATCH 9 contact-only → PATCH equal_5 → rename | 201/200/200/**409** |
| Scratch: negative `preset_equal_2`/pos3, create `preset_equal_1`/slot5, grid-4 + AUTO pos5 | 409/409; attach 1,2; size 4 |
| Scratch: calculate `vks_grid_43` 1×1, `vks_grid_5` 9×10, `preset_default_13` | 43/5/400; rect'ы внутри; fallback 1280×720 |

Все scratch-правки (`resource_tests.rs`, `gateways.rs`) внесены только в `/dev/shm`-копию, после проб
файлы восстановлены из `4686ac8`, копия удалена. Live-репо не менялся, кроме этого отчёта.

### Ограничения

- Live-БД не открывалась: неизвестно, есть ли в проде contact-only legacy-записи. P1-2 доказан на
  синтетическом seed и на достижимой последовательности реальных route-вызовов, а также на коде,
  который применяется ко всем загружаемым документам.
- Engine-пробы — scratch-тесты в копии (`GatewayManager::apply_layout` + `McuEngine`), не live server;
  они воспроизводят ветку `gateways.rs` и авто-regrid движка.
- E2E в браузере не запускался; вывод по оракулу — статический (P2-5).

---

## Score: 8.5 / 10

- **+** Исходный P1 (legacy 9 слотов с `abonent_id`) закрыт на реальных route-пробах: normalize 9,
  rename 200, attach 1..9, 10-й 409, mode 200, engine/admin agree 9.
- **+** P2-13 закрыт: degenerate canvas уходит в 1280×720, count совпадает, rect'ы внутри по обеим
  осям, committed-тест проверяет height.
- **+** Негативы (equal_2/pos3, equal_1/slot5, grid-4 + пустой AUTO pos5) дают 409/409/attach 1,2.
- **+** §4.6 переставлен после §4.5; §13 верен; S6 точен; clippy/fmt/list/admin lib/layout tests
  зелёные.
- **−** **P1-2:** occupancy считается только по `abonent_id`; contact-only legacy и штатная
  PATCH-последовательность снова дают size5 / rename409 / MCU9.
- **−** §4.6 всё ещё буквально ложен про `max(slot.position)` и не описывает occupancy-шаг резолвера.
- **−** Нет route-регрессии на P1; остаются P2-3…P2-6 и P3-1…P3-6.

До 9.5+ не хватает: (1) считать occupancy тем же предикатом, что `ensure_slot_roles`, сохранив
negative для пустого AUTO, и покрыть route-регрессией contact-only последовательность; (2) привести
§4.6 к фактическому резолверу; (3) добавить route-тест на legacy-9 → rename/attach/mode.

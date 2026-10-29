**Verdict: 6.5 / 10 — done: no. 0 P0, 2 P1, 6 P2, 5 P3.**

# T06 — независимое ревью №3 (HEAD `2442a16`, layout-фикс `7199818`)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, branch `master`.
- **HEAD:** `2442a1645f8f4efc53b3959a03aab918f03c4ec1` «docs(tracker): close T08 (9.6) and T09a (9.0)…».
- **Layout-фикс под ревью:** `7199818` «fix: close T06/T08/T09a re-review blockers». Между `7199818` и
  HEAD layout-код и оба нормативных документа не менялись (`git diff --stat 4686ac8..2442a16` —
  только `vks-server/frontend.rs`, `lib.rs`; `crates/vks-layout`, `vks-admin`, `BACKEND_RULES`,
  `LAYOUT_ENGINE` не тронуты), поэтому пробы на HEAD применимы к `7199818` один-к-одному.
- **Предыдущие ревью:** `T06.md` — 8.0/10 (P1: normalize сжимает size), `T06_rereview2.md` — 7.5/10
  (P1: attach/validate берут ёмкость пресета 5 после recalc 9), незакоммиченный prior-набросок
  `T06_rereview3.md` (8.5, P1: occupancy только `abonent_id`) — учтён, но **не является** этим отчётом:
  здесь другой HEAD и другие пробы. Настоящий файл заменяет его.
- **Чистое дерево:** `git archive 2442a16 | tar -x -C /dev/shm/t06rr3_1793266862_223284`;
  `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr3_target`, `--offline`; `/tmp` не использовался.
- **Live-БД/сервер:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на
  `:8080`/`:18082` не запускался и не опрашивался. E2E в браузере не запускался.
- **Единственная запись в live-репо:** этот файл. Чужие изменения в worktree (`M pitfalls/T06.md`,
  `?? T08_rereview2.md`, `?? T09a_rereview*.md`, `?? T11a_rereview*.md` и прежний набросок этого файла)
  не мои и на HEAD/архив не влияют.
- **Live-репо ушёл вперёд во время ревью:** после `2442a16` появились `0febfe5 fix(T06): one occupancy
  predicate…` и документальные коммиты `0999267`/`214a673`. Предмет этого отчёта — `2442a16`, но
  фактическое состояние tip'а проверено отдельно и вынесено в §9: часть P1-2 (contact-only) там уже
  закрыта, **P1-1 и «пустая» часть P1-2 остаются живыми**.

## Вердикт кратко

`7199818` действительно закрывает **исходный** P1 на route-уровне: legacy-запись с 9 **занятыми**
слотами (`abonent_id`) получает `size = 9`, PATCH rename → 200, attach принимает 1..9, 10-й → 409.
`preset_size` зажат в 1..=100, `gallery` больше не паникует и даёт точный count, §12/§13
`LAYOUT_ENGINE` соответствуют коду, §4.6 переставлен после §4.5, e2e-оракул больше не требует ровно
один 400. Гейты зелёные: fmt rc=0, clippy rc=0, layout lib **54**, admin lib **349**.

Но:

1. **P1-1 (новая регрессия `7199818`).** Ёмкость для validate берётся **до** merge запроса, поэтому
   **легальное расширение раскладки отклоняется 409**: PATCH `vks_grid_5` → `vks_grid_9` (9 слотов)
   → `409 slot position 6 is outside layout of 5 windows`; `vks_grid_13` → `vks_grid_43` → 409; mode
   `preset_equal_25` на 13-оконной «Общей» → 409. Именно этот одним запросом и делает UI: в manual
   режиме vendor-клиент отправляет `payload.slots` из store (N окон нового пресета), а store, как
   подтверждает сам e2e, содержит ровно N слотов. До фикса (validate по merge-пресету) те же запросы
   проходили — доказано негативным контролем. «Создать» (POST) работает, «Сохранить»/«Применить»
   (PATCH) на рост числа окон — нет.
2. **P1-2 (та же дыра не закрыта для другого представления слота).** `layout_size` считает занятыми
   только слоты с `abonent_id`, а validate проверяет **все** слоты против этой ёмкости. Поэтому
   запись с пустым окном/окном только по `call_string_id` выше итоговой ёмкости становится
   **нередактируемой**: один detach девятого участника legacy-записи → rename **409**; сценарий
   собственного теста `t00_attach_skips_auto_outside_capacity` (grid-4 + пустой AUTO pos5) → rename
   **409**; contact-only 9 окон + preset 5 → size 5, rename 409. То есть «запись не отравляется»
   из комментария теста неверно для PATCH.

| Критерий приёмки | Вердикт | Где доказано |
|---|---|---|
| P1: legacy 9 **занятых** окон, PATCH/attach/mode не ломаются | **PASS** | §1.1: size 9, rename 200, `[200×9, 409]` |
| P1: ёмкость никогда не ниже занятых окон после любых операций | **FAIL** | §1.2: detach → 409; contact-only → 409 |
| Один резолвер `layout_size` для attach/validate/recalc/normalize/mode | **PARTIAL** | §1.3: вызовы едины, но validate использует **pre-merge snapshot**, create — post-merge, attach — текущий payload |
| Pre-merge capacity не даёт новым позициям расширять раскладку | **PASS для негатива / FAIL для легального роста** | §2.1: pos10 → 409 ✔; рост 5→9, 13→43, mode→25 → 409 ✘ |
| mode-recalc после merge | **PASS по поведению, не покрыт тестом** | §2.3 / NC-C: 5 vs 13 без recalc, `m4_…` зелёный без строки |
| P2-13: tiny canvas без panic/underflow, count == requested | **PASS (P3: silent fallback)** | §3.2: 43@1×1, rect'ы внутри 1280×720 |
| S6 n=5@1280×720, 83.28 % | **PASS** | §3.1: exact rects, 767 520, 0.8328125 |
| `preset_size` clamp 1..=100 | **PASS** | §3.3 (код + lib-тесты) |
| Docs §12/§13/§4.6 truthful | **PARTIAL** | §4: §12/§13 ✔, §4.6 `max(slot.position)` ложно |
| E2E-оракул: expected 400 == factual | **PASS статически, вживую не проверен** | §5.2: `:315` exact-consistency, `>=1` |

- **Open P0: 0.**
- **Open P1: 2** — P1-1 (pre-merge ceiling блокирует рост), P1-2 (occupancy vs validate mismatch).
- **Open P2: 6**, **Open P3: 5** (таблица §6).

---

## 1. Исходный P1 и его остаточная дыра

### 1.1 Исходный сценарий (9 **занятых** окон через `abonent_id`) — закрыт

Запись засеяна в `resources.layouts` и прогнана через реальный `normalize_loaded`
(`resources.rs:1139`), затем привязана к сессии и пройдена реальными route-хендлерами
(`attach_layout_abonent`, `edit_linked`, `detach_layout_abonent`) на pristine-копии:

| Шаг | `preset_equal_5` |
|---|---|
| `payload.size` после `normalize_loaded` | **9** |
| слотов после load | 9 |
| PATCH `{"display_name":"Renamed"}` | **200** |
| attach p1..p10 | **`[200×9, 409]`** |
| detach p9 | 204 |
| PATCH `{"display_name":"Renamed after detach"}` | **409** `slot position 9 is outside layout of 8 windows` |

Первые четыре строки — ровно то, что требовало `T06_rereview2` §1.5: `layout_size` учитывает
`max(position)` занятых окон (`resources.rs:376-388`), attach берёт тот же лимит (`:986`),
update_resource валидирует с `stored_capacity` (`:1507`/`:1526`). Это **закрыто**.

### 1.2 Почему P1 всё ещё открыт: capacity игнорирует пустые окна, validate — нет

`max_slot_position` (`resources.rs:394-404`) фильтрует только
`slot.get("abonent_id").and_then(Value::as_str).is_some()`. `detach` же не удаляет окно, а оставляет
`{"position": N, "role": "AUTO"}` (`resources.rs:1094-1099`), и `recalc_layout_size` при detach не
вызывается. `validate_layout_assignments_with_capacity` (`:1465-1497`) проверяет **каждый** слот
(включая пустой) против `size`: `if position > size { 409 }`. Отсюда три доказанных сценария:

| Сценарий (pristine, реальные вызовы) | Результат |
|---|---|
| legacy `preset_equal_5` + 9 слотов, занятых только `call_string_id` → normalize | `size = 5`, PATCH rename **409** (`position 6 outside 5`) |
| legacy `preset_equal_5` + пустые позиции 1..9 (`role:AUTO`, без `abonent_id`) → normalize | `size = 5`, PATCH rename **409** |
| `vks_grid_4` + пустой AUTO `position:5` → attach участника → PATCH rename | `payload.size = 4`, **409** `slot position 5 is outside layout of 4 windows` |

Третий сценарий — это **ровно** `t00_attach_skips_auto_outside_capacity`
(`resource_tests.rs:2754-2790`): тест создаёт `vks_grid_4`, руками кладёт пустой слот 5, attach
аккуратно не занимает его (фильтр `1..=limit` в `attach`, `:1005-1007`), после чего проверяет только
позиции `[1,2,5]`. Он **не делает PATCH**, хотя его комментарий обещает «запись не отравляется».
Фактически после такого attach любой PATCH (даже rename) даёт 409, потому что validate видит
позицию 5, а ёмкость — 4. То есть «не отравляется» неверно; тест закрепляет только половину
инварианта.

Первый сценарий — это P1-2 из prior-наброска (occupancy только `abonent_id`), он остаётся открытым:
собственный `ensure_slot_roles` (`resources.rs:1390-1392`) считает занятостью непустые
`abonent_id`/`contact_id`/`abonentId`/`contactId`/`call_string_id`/`callStringId`, движок принимает
contact-only слоты, а capacity — нет.

**Рекомендация:** либо считать занятость тем же предикатом, что `ensure_slot_roles`, и/или в
validate пропускать пустые слоты с `position > stored_capacity` (они всё равно не используются
attach'ем — `:1005-1007`), либо нормализовать/удалять такие окна при load/detach. Плюс route-тест:
`legacy 9 occupied → detach одного → rename 200` и `grid-4 + AUTO pos5 → attach → rename 200`.

---

## 2. P1-1 — новая регрессия: pre-merge ceiling блокирует легальный рост

### 2.1 Пробы (pristine + scratch route-тесты)

| Запрос (одна реальная запись, реальные хендлеры) | Результат |
|---|---|
| запись `vks_grid_5` (size 5), PATCH `payload{preset:"vks_grid_9", slots:[1..9 AUTO]}` | **409** `slot position 6 is outside layout of 5 windows`; запись не изменена |
| запись `vks_grid_13` (size 13), PATCH `payload{preset:"vks_grid_43", slots:[1..43 AUTO]}` | **409** `slot position 14 is outside layout of 13 windows` |
| каноническая «Общая» (`preset_default_13`, size 13), mode `payload{preset:"preset_equal_25", slots:[1..25 AUTO]}` | **409** `slot position 14 is outside layout of 13 windows`; size остался 13 |
| **NC-B:** те же три запроса, но validate без `stored_capacity` (post-merge, как было до `7199818`) | **200 / 200 / 200**; сохранены `size` 9 / 43 / 25 |

NC-B доказывает, что именно `stored_capacity`-снапшот (`resources.rs:1507`/`:1526`,
`shell.rs:1010`/`:1021`) блокирует рост, и что до фикса эти запросы проходили. То есть это
**регрессия**, а не pre-existing дефект.

### 2.2 Это реальный UI-путь, а не синтетика

- Вендорский payload-маппер `Th(e)` в `index.bundle-2.2.1.66.js` для **manual** режима отдаёт
  `payload = {mode, preset, type, slots}`, где `slots` — **все** слоты store (`Fee.manual = kee`,
  `slots: ({slots}) => slots.map(...)`).
- `updateSessionLayout` (`index.bundle…js`) делает
  `PATCH /api/v3/ui/sessions/{room}/video_layouts/{id}` с этим телом → `edit_linked` →
  `update_resource` → `stored_capacity` → 409.
- Проектный слой `client/pnm-frontend/deployed/pnm/codian-layouts.js` (`select`, `selectConstructor`)
  кладёт в store ровно N слотов выбранного пресета; e2e это же и проверяет:
  `tests/e2e/layout_constructor_ui.js:207-215` — «N=12: store содержит ровно 12 слотов, позиции 1..12».
- У «Общей» ёмкость 13 (`canonical_default_payload` — `preset_default_13`, `size:13`), поэтому
  переключение на 25 окон (вендорский manual-дефолт `preset_equal_25`) даёт 409 на позиции 14.

Итого: «Создать» (POST, validate post-merge, `resources.rs:1568`) новый layout на 43 окна создаёт,
а «Сохранить»/«Применить» на существующем — 409, как только новое число окон больше текущей ёмкости.
Ограничение commit message («new positions cannot widen the layout») задумано как защита от
контрабанды позиций, но реализовано как запрет **любого** роста в одном запросе, даже когда запрос
явно объявляет новый `preset`/`size`. E2E этого не ловит: он меняет N только до 12 при ёмкости 13 и
ничего не сохраняет.

**Рекомендация:** потолок для входящих слотов = `max(stored_capacity, declared payload.size,
capacity(declared payload.preset))`, при этом позиции, которые не покрыты ни stored-, ни объявленной
ёмкостью, по-прежнему 409; пустые слоты нового пресета не должны считаться «контрабандой». Либо
задокументировать двухшаговый протокол (сначала PATCH без слотов, потом PATCH со слотами) и
переучить UI. Нужен route-тест `5 → 9` и `13 → 25/43` (сейчас ни одного нет).

### 2.3 mode-recalc после merge — код есть, теста нет

Mode-эндпоинт вызывает `recalc_layout_size` после merge (`shell.rs:1018`). Проба: запись
`vks_grid_5` со **stale** `payload.size:13` в памяти (seed без normalize), mode-only запрос
(`{mode:"auto"}`, без size/slots/preset) → сохранён `size = 5`. **NC-C:** если удалить строку
`shell.rs:1018`, тот же запрос сохраняет `size = 13`. При этом `cargo test -p vks-admin --lib --offline
m4_abonents_lifecycle_and_commands` (единственный тест с mode-ассертами, `tests.rs:1575-1690`)
**зелёный без recalc**: все его mode-запросы несут `preset`/`size`, поэтому inline `inferred_size`
маскирует отсутствие recalc. То есть заявленный фикс «mode-endpoint recalc after merge» не закреплён
ни одним тестом.

---

## 3. Что действительно закрыто (с исполнением)

### 3.1 S6 и раскладки — PASS

`grid_five_slots_at_1280x720_fill_is_83_28_percent` (`grid.rs:186-201`) не менялся: точные rect'ы
(`426/426/428`, `x=640`, `x=214`, высоты 360), `assert_no_overlap`/`assert_inside`, площадь
`767_520`, отношение `0.8328125` с допуском `1e-9`. `grid_five` — **2 passed**. Тест вызывает
`grid_rects`, а `GridLayout::compute` делегирует в него (`grid.rs:110-115`) — расхождения нет.

### 3.2 P2-13 (tiny canvas) — underflow/panic закрыт, count точный; остаётся P3

`gallery` (`grids.rs:241-249`) при `width/height < 16`, `0` или non-finite подставляет `(1280,720)`;
`count - row*cols` больше не underflow'ит (`saturating_sub`, `:262-267`), count == requested,
тест проверяет обе оси (`x+w`, `y+h`, `grids.rs:774-800`). Проба: `gallery(43, 1×1)` → 43 rect'а,
`max x+w = 1272`, `max y+h = 712` — внутри 1280×720. Но **запрошенный** холст не соблюдается:
`calculate` принимает `width/height ≥ 1` (`shell.rs:745-752`) и не возвращает canvas, поэтому клиент
`1×1` молча получает геометрию 1280×720; для `preset_*`/`codian_*` подстановки нет (там `geometry`
строит от запрошенного холста) — поведение зависит от типа пресета (P3, §6).

### 3.3 `preset_size` clamp — PASS

`crates/vks-layout/src/lib.rs:59-72`: `grids::slot_count` → `codian::slot_count` → tail `preset_*_N`
с фильтром `(1..=MAX_WINDOWS)`; `MAX_WINDOWS = 100`. Тесты `preset_size_tests` в lib (54) зелёные.
Схема по-прежнему не валидируется (`preset_evil_4 → Some(4)`) — P3-3.

### 3.4 Docs §12/§13 — PASS

§12 (`LAYOUT_ENGINE.md:239-250`) точен: 3×2, ширины `426+426+428`, `x=214`, `2×426`, 360, 83.28 %,
`767 520 / 921 600`; формулировка «первая строка — 3 окна, вторая — 2 окна по центру» соответствует
коду. §13 (`:252-268`) точен: `Default` не входит в `is_known_preset` (manual ∪ Codian, `preset.rs`),
`equal_slots` существует и используется manual, `calculate` не публикует non-registry id,
`preset_default_13 → 400 invalid.preset` — политика реестра. Оракул описан как «ожидаемое ==
фактическое число 400», что совпадает с кодом e2e.

---

## 4. Документация §4.6 — `max(slot.position)` по-прежнему ложен

`docs/rules/BACKEND_RULES.md:179-185`:

1. **Ложное утверждение:** «`size` никогда не опускается ниже `max(slot.position)`». Буквально
   неверно в трёх проверенных случаях: (а) `vks_grid_4` + пустой AUTO pos5 → `size = 4 < 5`
   (это ожидаемый негатив); (б) legacy 9 окон только по `call_string_id` + preset 5 → `size = 5 < 9`;
   (в) 9 пустых окон + preset 5 → `size = 5 < 9`. Правильная формулировка — «ниже `max(position)` по
   **непустым занятым** слотам».
2. **Список резолвера** (`:180-182`) не содержит шага «max(position) занятых», хотя именно он
   определяет capacity в `layout_size` (`resources.rs:380-388`). Это прошлый P3-7, он остался.
3. **Не задокументирован pre-merge снапшот:** §4.6 говорит «после merge запроса size
   пересчитывается», но умалчивает, что **validate** использует ёмкость **до** merge. Это и есть
   корень P1-1; сейчас документ создаёт впечатление, что объявленный в запросе новый size/preset
   учитывается. Нужно явно описать: потолок входящих слотов = stored capacity, рост требует
   отдельного запроса/объявленной ёмкости.
4. «attach, validate и mode используют один и тот же резолвер ёмкости» — формально верно, но с
   разными снимками (attach — текущий payload, validate/mode — pre-merge), и create валидирует
   post-merge (`resources.rs:1568`). Это надо оговорить.

§4.5 стоит перед §4.6 (`:174` vs `:179`) — прошлый P3-2 закрыт.

---

## 5. Гейты, негативные контроли, пробы

### 5.1 Гейты (pristine-копия, чистый target после `cargo clean -p vks-layout -p vks-admin`)

| Команда | Результат |
|---|---|
| `cargo fmt --all --check` | **rc=0** |
| `cargo clippy -p vks-layout -p vks-admin --all-targets --offline -- -D warnings` | **rc=0** (один прогон, оба крейта) |
| `cargo test -p vks-layout --lib --offline -- --list` | **54**, дублей нет |
| `cargo test -p vks-layout --lib --offline` | **54 passed, 0 failed** |
| `cargo test -p vks-admin --lib --offline` | **349 passed, 0 failed** |
| `cargo test -p vks-layout --offline` | lib 54 + integration 31 = **85 passed, 1 ignored** (ручной `regenerate_golden`) |
| `cargo test -p vks-layout --lib grid_five` | **2 passed** |
| `node --check tests/e2e/layout_constructor_ui.js` | **OK** (в браузере не запускался) |

**Гигиена прогонов (важно):** при повторном прогоне в том же `CARGO_TARGET_DIR` после сборок
scratch/NC-копий cargo переиспользовал устаревший тест-бинарь: layout показал **55** тестов (мой
scratch-модуль), а admin — падение `t00_attach_skips_auto_outside_capacity` с `[1,5]` (бинарь из
NC-копии с удалённым `abonent_id`-фильтром). Это не флейк продукта, а загрязнение общего target.
После `cargo clean -p vks-layout -p vks-admin` и повторного прогона на pristine-исходниках получены
числа выше (54/349). Первый прогон (до scratch/NC) тоже давал 54/349; clippy/fmt — rc=0 в обоих
чистых прогонах.

### 5.2 Негативные контроли (в scratch-копиях, затем удалены)

| # | Что откатили | Результат |
|---|---|---|
| NC-A | `layout_size` → только `derived_layout_size` (без `max(position)` занятых) | `legacy_layout_capacity_never_drops_below_occupied_slots` **FAIL**: `left: Some(5) right: Some(9)` — тест действительно пинит фикс |
| NC-B | validate без pre-merge `stored_capacity` (post-merge) | рост `5→9`, `13→43`, mode `→25` дают **200** (size 9/43/25) — доказывает, что 409 в §2.1 создан именно снапшотом |
| NC-C | удалить `recalc_layout_size` в mode (`shell.rs:1018`) | mode-only запрос сохраняет stale `size = 13` вместо **5**; при этом `m4_abonents_lifecycle_and_commands` остаётся **зелёным** — recalc не покрыт тестом |
| NC-D | убрать `.filter(abonent_id)` в `max_slot_position` | `t00_attach_skips_auto_outside_capacity` **FAIL** (`[1,5]` vs `[1,2,5]`) — фильтр пинится; `legacy_layout_capacity…` и `recalc_layout_size…` при этом зелёные (они не про пустые окна) |

### 5.3 Route-пробы (pristine + scratch-тесты, реальные хендлеры)

| Проба | Результат |
|---|---|
| legacy 9 `abonent_id` + preset 5 → normalize / rename / attach 1..10 | size 9 / **200** / `[200×9, 409]` |
| detach p9 → rename | **409** `position 9 outside layout of 8 windows` |
| legacy 9 empty / contact-only + preset 5 → rename | size 5 / **409** |
| `vks_grid_4` + AUTO pos5 → attach → rename | size 4 / **409** `position 5 outside layout of 4` |
| PATCH `vks_grid_5`→`vks_grid_9` (9 слотов), `13`→`43`; mode `→preset_equal_25` | **409 / 409 / 409** |
| mode-only на `vks_grid_5` со stale size 13 | **200**, size **5** |
| `calculate` grid-43 на 1×1 (`gallery`) | 43 rect'а, `max x+w = 1272`, `max y+h = 712` |

---

## 6. Остаточные P2/P3

| # | Sev | Проблема | Файл:строки |
|---|---|---|---|
| P1-1 | **P1** | pre-merge `stored_capacity` блокирует легальный рост (PATCH 5→9/13→43, mode→25 → 409); UI шлёт `payload.slots` нового пресета одним запросом | `resources.rs:1507,1526`; `shell.rs:1010,1021`; UI: `index.bundle…js` `Th`/`kee`, `codian-layouts.js` |
| P1-2 | **P1** | capacity считает занятость только по `abonent_id`, validate проверяет все слоты → пустой/contact-only слот выше ёмкости делает запись нередактируемой (detach → 409) | `resources.rs:394-404`, `:1465-1497`, `:1094-1099`, `:1390-1392` |
| P2-1 | P2 | §4.6: `max(slot.position)` буквально ложен, список резолвера без occupancy-шага, pre-merge снапшот не описан | `docs/rules/BACKEND_RULES.md:179-185` |
| P2-2 | P2 | Нет committed route-регрессии на исходный P1: тест только helper-level, `t00_attach_skips_auto…` не делает PATCH, хотя обещает «не отравляется» | `resource_tests.rs:2514-2530`, `:2754-2790` |
| P2-3 | P2 | mode-recalc после merge не покрыт тестом: удаление `shell.rs:1018` не роняет ни один тест (NC-C) | `shell.rs:1018`; `tests.rs:1575-1690` |
| P2-4 | P2 | E2E-оракул вживую не проверен (Playwright не запускался), `vendor400Count >= 1`, стабильность сценария не доказана | `tests/e2e/layout_constructor_ui.js:315` |
| P2-5 | P2 | detach не пересчитывает `size` и оставляет пустое окно на прежней позиции → две истины (`size` vs attach-лимит) и триггер P1-2 | `resources.rs:1093-1118` |
| P2-6 | P2 | `gallery` молча подменяет холст <16 px на 1280×720; `calculate` принимает такие размеры и не сообщает о подмене; для `preset_*`/`codian_*` подмены нет | `grids.rs:241-249`; `shell.rs:745-752` |
| P3-1 | P3 | `position == 0` → 409 `Conflict`, хотя это malformed payload → должен быть 422 `Invalid` (pre-existing) | `resources.rs:1482` |
| P3-2 | P3 | `normalize_loaded` меняет только память; storage хранит stale `size` до первого commit | `state.rs:711`; `resources.rs:257-280` |
| P3-3 | P3 | Три снимка ёмкости (attach — текущий, validate/mode — pre-merge, create — post-merge); attach-fallback 13 vs validate `None`; `preset_size` не валидирует схему (`preset_evil_4 → Some(4)`) | `resources.rs:986`, `:1472`, `:1568`; `lib.rs:59-72` |
| P3-4 | P3 | E2E операционно: default `:8080` (живой сервер/БД), фиксированные `waitForTimeout`, `calculate.length >= 7`, в CI не входит | `tests/e2e/layout_constructor_ui.js:21,166,170,207,293,317` |
| P3-5 | P3 | Тест-качество: tiny-canvas тест хардкодит тот же порог 16.0, что и реализация; `remove_global` глушит apply (`let _ = push_layout_to_engine`) | `grids.rs:774-800`; `resources.rs:1777` |

---

## 7. Счёт и обоснование

**6.5 / 10, done: no.**

**+**
- Исходный P1 (9 занятых окон с `abonent_id`) закрыт на реальных route-пробах: load 9, rename 200,
  attach 1..9, 10-й 409; NC-A подтверждает, что тест пинит фикс.
- `gallery` больше не паникует/underflow'ит, count == requested, тест проверяет обе оси.
- `preset_size` зажат 1..=100; §12/§13 `LAYOUT_ENGINE` точны; §4.5/§4.6 упорядочены; e2e-оракул
  сверяет expected с фактическим числом 400 (не «ровно один»).
- S6-тест точен и не изменён; fmt/clippy/layout 54/admin 349 — зелёные на чистом target;
  NC-C/NC-D показывают, что mode-recalc и `abonent_id`-фильтр действительно несущие.

**−**
- **P1-1:** pre-merge ceiling отклоняет легальный рост раскладки (5→9, 13→43, mode→25), то есть
  основной сценарий конструктора «выбрать больше окон и сохранить/применить» ломается 409; NC-B
  доказывает, что до фикса эти запросы проходили. Это не описано ни в документах, ни в тестах.
- **P1-2:** та же «отравляющая» петля осталась для пустых и contact-only окон: один detach на
  legacy-записи → rename 409; сценарий собственного теста `t00_attach_skips_auto…` → 409. Комментарий
  теста «запись не отравляется» не соответствует поведению.
- §4.6 по-прежнему содержит буквально ложное `max(slot.position)` и не описывает ни occupancy-шаг,
  ни pre-merge снапшот.
- mode-recalc и route-acceptance P1 не закреплены тестами (NC-C; helper-only тест).
- E2E-оракул не исполнялся, стабильность сценария не доказана.

До 9.5+ нужно: (1) разрешить рост до **объявленной** ёмкости запроса (preset/size), оставив 409 для
позиций вне и stored-, и объявленной ёмкости, и покрыть route-тестами `5→9`, `13→43`, mode→25;
(2) привести occupancy к предикату `ensure_slot_roles` и/или пропускать пустые слоты вне ёмкости в
validate — с route-тестом `detach → rename 200`; (3) переписать §4.6 по фактическому резолверу;
(4) закрепить mode-recalc тестом; (5) прогнать e2e на ephemeral-инстансе.

---

## 8. Гигиена

- Все сборки/тесты/пробы — только в `/dev/shm`-копиях из `git archive`; live-репо не менялся, кроме
  этого файла. Live-БД `vks_data.db` не открывалась, сервер на `:8080`/`:18082` не стартовал и не
  опрашивался, E2E в браузере не запускался.
- Scratch-копии (`/dev/shm/t06rr3_scratch`, `/dev/shm/t06rr3_nc`) с пробами/негативными контролями,
  pristine-копия и `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr3_target` удалены после ревью;
  `/tmp` не использовался.
- Найденная особенность окружения: общий `CARGO_TARGET_DIR` между разными checkout'ами приводит к
  переиспользованию устаревших тест-бинарей (наблюдалось 55 тестов layout и «падение» NC-сборки в
  admin). Числа гейтов в §5.1 получены после `cargo clean -p` на pristine-исходниках; при повторных
  ревью в этом же target это надо делать обязательно (или использовать отдельный target на копию).

---

## 9. Дополнение: состояние tip'а (`0febfe5`, `214a673`) на момент сдачи отчёта

Пока шло ревью, live-репо ушёл вперёд: `0febfe5` («one occupancy predicate») закрывает
contact-only часть P1-2, `214a673` — только документы/T11a. Проверено пробами уже на `214a673`
(архив `git archive 214a673`, те же route-хендлеры):

| Проба на tip `214a673` | Результат | Статус |
|---|---|---|
| contact-only legacy 9 окон (`contact_id`) + preset 5 → normalize → rename | `size = 9`, **200** | **P1-2 (contact-only) закрыт** — `slot_holds_participant` (`resources.rs:391-411`) един для capacity и ролей; route-тест `contact_only_legacy_slots_keep_capacity_across_preset_shrink` есть |
| PATCH `vks_grid_5` → `vks_grid_9` (9 слотов) | **409** `position 6 outside layout of 5 windows` | **P1-1 жив** |
| mode `preset_equal_25` (25 слотов) на «Общей» 13 | **409** `position 14 outside layout of 13 windows` | **P1-1 жив** |
| legacy 9 занятых → detach p9 → rename | **409** `position 9 outside layout of 8 windows` | **P1-2 (пустые окна) жив** |
| `vks_grid_4` + пустой AUTO pos5 → attach → rename | **409** `position 5 outside layout of 4 windows` | **P1-2 (пустые окна) жив**; ровно сценарий `t00_attach_skips_auto_outside_capacity` |
| `BACKEND_RULES` §4.6 на tip | переписан: «после самого дальнего **ЗАНЯТОГО** окна», `slot_holds_participant`, пустой AUTO не раздувает, validate берёт ёмкость **до** merge | **P2-1 на tip снят**; в §4.6 осталось только неописанное следствие: рост в одном PATCH запрещён (это теперь подразумевается формулировкой «новые позиции не расширяют ёмкость», но UI этого не соблюдает) |

Вывод для tip'а: открытых P1 остаётся **два** — P1-1 (рост раскладки 409) и P1-2 в сузившемся виде
(пустые AUTO-окна выше ёмкости: detach и сценарий `t00_attach_skips_auto…`). Для «done» tip'а нужно:
разрешить рост до объявленной в запросе ёмкости (`payload.size`/`preset`) и не валидировать пустые
слоты вне ёмкости (или чистить их при attach/detach), плюс route-тесты на оба сценария.

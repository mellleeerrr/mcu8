# T06 — повторное ревью №4: фикс P1-2 (`0febfe5`)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, ветка `master`.
- **Проверяемый HEAD:** `0febfe5` «fix(T06): one occupancy predicate for layout capacity and slot roles»
  (поверх `2442a16`).
- **Предыдущее ревью:** [`T06_rereview3.md`](T06_rereview3.md) — 8.5/10, done no, open P1-2:
  capacity считался только по `abonent_id`, contact-only 9 слотов + `preset_equal_5` давали size 5 /
  rename 409 при 9 окнах MCU.
- **Дата/время:** 2026-10-29, 12:47–13:10 MSK.
- **Полигон:** `git archive 0febfe5 | tar -x -C /dev/shm/t06rr4_1793267255_3471` (без `.git`/`target`),
  `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr4_target`, Rust **1.94.0** из `rust-toolchain.toml`,
  `--offline`; `/tmp` не использовался. Все правки — только в `/dev/shm`-копии; после проб
  `resources.rs`, `resource_tests.rs`, `gateways.rs` восстановлены и сверены по sha256 с `0febfe5`
  (три `OK`).
- **Live-среда:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на
  `:8080`/`:18082` не запускался и не опрашивался. Пока шло ревью, live-ветку другие агенты
  продвинули `0febfe5 → 214a673`; `git diff 0febfe5 214a673` по `resources.rs`,
  `resource_tests.rs`, `BACKEND_RULES.md`, `LAYOUT_ENGINE.md`, `grid.rs` **пуст**, поэтому все
  ссылки/evidence относятся к проверяемому коду. В live-репо записан только этот файл.

## Вердикт

**Verdict: 8.8 / 10 — done: no. 0 P0, 1 P1, 3 P2, 7 P3.**

P1-2 из ревью №3 **закрыт полностью и доказан исполнением**: один предикат занятости
(`slot_holds_participant`, 8 ключей) используется и capacity, и ролями; contact-only 9 слотов после
`preset_equal_5` дают `size = 9`, rename 200, attach принимает 1..9 и отклоняет 10-го; MCU рисует 9
окон; revert предиката к `abonent_id`-only валит новый committed-тест (`left: Some(5)`,
`right: Some(9)`) и воспроизводит 409. §4.6 теперь соответствует коду, §12/§13 точны, гейты зелёные.

Но остаётся **P1-3 того же класса «успешная запись/загрузка отравляет запись»**: пустые AUTO-окна
вне ёмкости ёмкость не раздувают (это требование выполнено), однако `validate_layout_assignments_with_capacity`
проверяет **все** сохранённые слоты и возвращает 409 на позиции за ёмкостью. В результате:
(а) legacy `vks_grid_4` + пустые AUTO 1..5 после normalize имеет size 4, но **rename → 409**
`slot position 5 is outside layout of 4 windows`; (б) ровно сценарий P1-2 после attach 9 и detach всех
9 (слоты становятся AUTO) имеет stored size 9 / resolver 5 и **rename → 409**
`slot position 6 is outside layout of 5 windows`. Запись нередактируема обычным PATCH, committed-тесты
этот путь не покрывают. Поэтому done: no.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **1** (P1-3, новый/переклассифицированный P2-6) |
| **P2** | **3** |
| **P3** | **7** |

---

## 1. P1-2 — **PASS** (реальные handlers/route, pristine `0febfe5`)

Проброс — committed-тест и scratch-тесты в pristine-копии через `create` / `edit_global` /
`get_global` / `attach` / `attach_layout_abonent` / `detach_layout_abonent` (не через helper
`layout_size`). Сборка `cargo test -p vks-admin --lib ... --offline` в
`CARGO_TARGET_DIR=/home/pro/.cache/t06_rr4_target`.

### 1.1 Последовательность P1-2 по всем 8 ключам — 200/200/9/200

На каждый ключ: `create` `preset_equal_9` (201) → PATCH 9 слотов с этим ключом (200) →
PATCH `{"preset":"preset_equal_5"}` (200) → чтение state + GET → PATCH `display_name` (200).

| Ключ | size после shrink | slots | PATCH pos10 (incoming `vks_grid_13`) | rename | GET occupied |
|---|---:|---:|---|---:|---:|
| `abonent_id` | **9** | 9 | **409** | **200** | 9 |
| `contact_id` | **9** | 9 | **409** | **200** | 9 |
| `call_string_id` | **9** | 9 | **409** | **200** | 9 |
| `connection_id` | **9** | 9 | **409** | **200** | 9 |
| `abonentId` | **9** | 9 | **409** | **200** | 9 |
| `contactId` | **9** | 9 | **409** | **200** | 9 |
| `callStringId` | **9** | 9 | **409** | **200** | 9 |
| `connectionId` | **9** | 9 | **409** | **200** | 9 |

Вывод проб (фрагмент):

```text
RR4 KEY=contact_id size_after_shrink=Some(9) slots=Some(9)
RR4 KEY=contact_id preset13_plus_pos10=409 Conflict
RR4 KEY=contact_id rename=200 OK
RR4 KEY=contact_id get_size=Number(9) get_occupied=9
...
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 350 filtered out
```

Committed-тест `contact_only_legacy_slots_keep_capacity_across_preset_shrink`
(`resource_tests.rs:2549`) отдельным прогоном: **1 passed, 0 failed**.

### 1.2 Legacy seed после `sync_from_storage` (реальный `normalize_loaded`)

Configuration-document с записью `preset_equal_5`, `size:13`, 9 слотами только по одному ключу →
`MemoryStorage::put_configuration_document` → `AppState::sync_from_storage()` → PATCH rename:

```text
RR4 SEED key=contact_id size_after_sync=Some(9) first_role=Some("PEER") rename=200 OK
RR4 SEED key=abonent_id size_after_sync=Some(9) first_role=Some("PEER") rename=200 OK
RR4 SEED key=callStringId size_after_sync=Some(9) first_role=Some("PEER") rename=200 OK
RR4 SEED key=connectionId size_after_sync=Some(9) first_role=Some("PEER") rename=200 OK
```

### 1.3 Attach и MCU — admin и engine согласованы на 9

Legacy 9 слотов с `abonent_id`, `preset_equal_5`, attach к сессии, 10 зарегистрированных
участников:

```text
RR4 ATTACH p1=200 OK ... p9=200 OK
RR4 ATTACH p10=409 Conflict
RR4 ATTACH final size=Some(9) occupied=9 slots=9
```

MCU-проба (scratch-тест в `vks-server`, `GatewayManager::apply_layout` + `McuEngine`, 9
забридженных participants, layout `preset_equal_5`, 9 contact-only слотов):

```text
RR4 ENGINE assignments=9 geometry=9
RR4 ENGINE slot_rects=9
test result: ok. 1 passed; 0 failed
```

То есть устранён исходный разрыв «админка 5 / MCU 9».

### 1.4 Negative control — committed-тест действительно ловит регрессию

В pristine-копии предикат capacity возвращён к старому виду
`.filter(|slot| slot.get("abonent_id").and_then(Value::as_str).is_some())` (при сохранённом общем
`slot_holds_participant` для ролей):

| Прогон | Факт |
|---|---|
| `contact_only_legacy_slots_keep_capacity_across_preset_shrink` | **FAILED**: `capacity keeps 9 occupied contact-only windows`, `left: Some(5)`, `right: Some(9)` (`resource_tests.rs:2584`) |
| scratch P1-2 route (contact-only) | `size_after_shrink=Some(5)`, `rename=409 Conflict` |
| scratch all-keys route | `contact_id: size_after_shrink=Some(5)` → panic; `abonent_id` остался 9/200 |

После восстановления предиката (и принудительной пересборки) тот же route-симптом даёт
`size_after_shrink=Some(9) rename=200 OK`, а committed-тест — **ok**.

---

## 2. P1-3 — пустые AUTO вне ёмкости отравляют PATCH (новый блокер)

Ёмкость считается правильно: пустые AUTO не раздувают `payload.size`. Но валидация назначений
(`validate_layout_assignments_with_capacity`, `resources.rs:1486-1514`) проверяет **каждый** слот
против ёмкости и не различает занятое окно и пустой AUTO. Любая запись, где сохранены пустые
AUTO-окна за ёмкостью, после normalize/загрузки или после detach становится нередактируемой.

### 2.1 Legacy `vks_grid_4` + пустые AUTO 1..5

Seed: `preset:"vks_grid_4"`, `size:13`, slots 1..5 `{"position":N,"role":"AUTO"}` (без id) →
`sync_from_storage` → size 4 (требование «не раздувать» выполнено) → PATCH только `display_name`:

```text
RR4 SEEDED AUTO rename=409 Conflict
body={"error":{"code":409,"description":"slot position 5 is outside layout of 4 windows"}}
```

### 2.2 Ровно сценарий P1-2 + detach всех окон

`create preset_equal_9` → PATCH 9 contact-only слотов (200) → PATCH `preset_equal_5` (200,
size 9) → attach 9 участников (200×9) → detach всех 9 (204×9). Все слоты становятся
`{"position":N,"role":"AUTO"}`:

```text
RR4 DETACH CONTACT slots=[(1,AUTO,None,None) ... (9,AUTO,None,None)] preset="preset_equal_5"
RR4 DETACH CONTACT stored_size=Some(9) resolver=Some(5) next_rename=409 Conflict
```

То же для legacy с `abonent_id` (attach-all → detach-all):

```text
RR4 DETACH after all: stored_size=Some(9) resolver_capacity=Some(5) occupied=0 slots=9
RR4 DETACH next_rename=409 Conflict
body={"error":{"code":409,"description":"slot position 6 is outside layout of 5 windows"}}
```

### 2.3 Почему это P1, а не P2

- Это тот же класс, что исходный P1: **успешная** операция (загрузка/normalize или detach)
  оставляет запись, которую любой последующий PATCH (rename, сохранение раскладки UI) отвергает
  409; пользователь не может переименовать/сохранить раскладку.
- `payload.size` (9) расходится с ёмкостью резолвера (5), т.е. нарушен принцип «одного резолвера»:
  capacity пустые AUTO не считает, а validate эти же слоты считает фатальными.
- Достижимо штатной последовательностью API из §2.2 без правки БД; detach всех участников —
  легальная операция.
- Committed-тесты не ловят: helper `legacy_layout_capacity_never_drops_below_occupied_slots`
  проверяет только `layout_size == 4`; новый route-тест покрывает contact-only occupied, но не
  пустые AUTO и не detach-all. Negative control из §1.4 эту ветку не затрагивает.

**Направление фикса:** либо при detach/normalize удалять пустые AUTO-слоты за пределами ёмкости
(и/или пересчитывать `size`/слоты согласованно), либо в валидации игнорировать пустые AUTO-слоты
вне ёмкости (занятые — по-прежнему 409), сохранив требование «пустой AUTO не раздувает capacity».
Нужен committed route-тест: seed grid-4 + AUTO pos5 → rename 200; и P1-2 → attach 9 → detach all →
rename 200.

---

## 3. Остальные проверки приёмки

| Критерий | Результат | Доказательство |
|---|---|---|
| Пустой AUTO вне сетки не раздувает capacity | **PASS по capacity** (`size=4`), **FAIL по редактируемости** (rename 409) | §2.1; после attach 2 участников: `size=4`, occupied=2, pos5 `AUTO`/`None` |
| Legacy `abonent_id` сценарий | **PASS** | §1.3: attach 1..9 = 200, 10-й = 409, size 9, slots 9 |
| Preset shrink использует pre-merge stored capacity | **PASS** | после shrink 9 занятых: PATCH `preset=vks_grid_13` + pos10 → **409** (stored 9 победил resolver 13); PATCH pos10 без preset → 409 |
| Реальный route, не helper | **PASS** | все пробы §1.1–1.4 через `create/edit_global/get_global/attach/...` |
| S6 точный | **PASS** | `grid_five_slots_at_1280x720_fill_is_83_28_percent` — 1 passed; §12 совпадает с тестом (426+426+428, x=214/640, area 767520, ratio 0.8328125) |
| `preset_default_13` → 400 | **PASS** | committed `pnm_calculate_uses_reference_preset_geometry` — 1 passed; `preset::is_known_preset` без Default |

---

## 4. Гейты на pristine `0febfe5`

```bash
export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TARGET_DIR=/home/pro/.cache/t06_rr4_target
cd /dev/shm/t06rr4_1793267255_3471
```

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all --check` | **0** | без вывода |
| `cargo clippy -p vks-layout -p vks-admin --all-targets --offline -- -D warnings` | **0** | `Finished dev profile ... in 1m 13s`, предупреждений нет |
| `cargo test -p vks-layout --lib --offline` | **0** | **54 passed**, 0 failed |
| `cargo test -p vks-admin --lib --offline` | **0** | **350 passed**, 0 failed (36.52 s) |
| `cargo test -p vks-admin --lib -- --list` | 0 | **350 tests**; `uniq -d` по именам пуст |
| `cargo test -p vks-layout --lib -- --list` | 0 | **54 tests**; `uniq -d` пуст |
| `cargo test -p vks-server --lib --offline` | **0** | **146 passed, 3 ignored**, 0 failed (20.04 s) — изменения `vks-admin` не сломали сервер |
| `cargo test -p vks-layout --lib grid_five_slots_at_1280x720_fill_is_83_28_percent` | **0** | 1 passed |
| `cargo test -p vks-admin --lib contact_only_legacy_slots_keep_capacity_across_preset_shrink` | **0** | 1 passed |

Флейков не было: полный admin-lib прогон 36.52 s, повторный запуск не потребовался; точечные
тесты перезапускались многократно (positive/negative control) — стабильно. Admin 350 = 349 из
ревью №3 + новый route-тест; layout 54 без изменений. Дублей `#[test]` нет.

---

## 5. Документация

- **`BACKEND_RULES.md` §4.6 (`:179-198`)** — **PASS, overclaim не найден.**
  `effective_capacity = max(резолвер, max(position) по слотам с участником)` совпадает с
  `layout_size` (`resources.rs:376-389`); предикат назван и перечисляет ровно 8 ключей
  (`abonent_id/contact_id/call_string_id/connection_id` + camelCase) — совпадает с
  `slot_holds_participant` (`:400-416`); «пустые AUTO не раздувают» — правда; «validate берёт
  ёмкость из сохранённой записи до merge» — правда для PATCH (`resources.rs:1528`) и mode
  (`shell.rs:1010`), create сохранённой записи не имеет (контекст корректен).
- **`LAYOUT_ENGINE.md` §12 (`:239-250`)** — **PASS**: 3×2, ширины 426/426/428, вторая строка
  2×426 при x=214, высоты 360, площадь 767520, 83.28% — всё совпадает с S6-тестом.
- **`LAYOUT_ENGINE.md` §13 (`:252-268`)** — **PASS**: `is_known_preset` = manual ∪ Codian, Default
  не входит; `calculate` на `preset_default_13` → 400, committed-тест зелёный.

Остаточная doc-неточность только уровня P3: §4.6 не упоминает, что attach при `layout_size == None`
берёт fallback 13, а validate при `None` не ограничивает позиции (P3-3), и что create валидирует
без stored capacity. Это не ложь о проверенном поведении.

---

## 6. Судьба P2/P3 из ревью №3

| # | Статус | Sev | Блокирует T06? |
|---|---|---|---|
| P2-1 §4.6 лжёт про `max(slot.position)` | **Закрыт**: §4.6 переписан, code-match | — | нет |
| P2-2 нет committed route-регрессии на P1 | **Закрыт**: `contact_only_legacy_slots_keep_capacity_across_preset_shrink` ловит revert (§1.4) | — | нет |
| P2-3 normalize memory-only | Открыт: `normalize_loaded` меняет только память (`state.rs:711`), storage держит stale `size` до первого commit; рестарт нормализует снова, in-process расхождения нет | **P2** | нет |
| P2-4 `position == 0` → 409 | Открыт: route-проба `status=409`, body `slot position must be >= 1`; malformed payload должен быть 422 | **P2** | нет |
| P2-5 e2e-оракул не доказан | Открыт: браузерный e2e не запускался (нужен live `:8080`); статически — `BASE` default `:8080`, фиксированные `waitForTimeout`, `vendor400Count >= 1`, `calculate.length >= 7` (`layout_constructor_ui.js:21,156-170,315,317`) | **P2** | нет (но приёмка «оракул стабилен» исполнением не подтверждена) |
| P2-6 detach не пересчитывает size | **Переклассифицирован в P1-3**: после detach всех окон stored size 9 / resolver 5, следующий PATCH 409 (§2.2) | **P1** | **да** |
| P3-1 `preset_size` без схемы (`preset_evil_4` → 4) | Открыт | P3 | нет |
| P3-2 mode: inline `inferred_size` перезаписывается `recalc_layout_size` — мёртвая ветка | Открыт (`shell.rs:982-1018`) | P3 | нет |
| P3-3 attach fallback 13 vs validate `None` | Открыт (`resources.rs:986` vs `:1489`) | P3 | нет |
| P3-4 `remove_global` глушит результат apply | Открыт | P3 | нет |
| P3-5 whitespace-id: capacity trim, attach `is_empty` | Открыт: `" "` считается пустым в capacity, но занятым в attach | P3 | нет |
| P3-6 два подраздела `### 4.3` | Открыт (`BACKEND_RULES.md:153,161`) | P3 | нет |
| P3-7 committed route-тест покрывает только `contact_id` | Открыт: helper покрывает 5 ключей + `abonent_id`; `connectionId`/`abonentId` — только scratch-пробы этого ревью | P3 | нет |

---

## 7. Ограничения и гигиена

- Браузерный e2e не запускался (нет live-сервера); вывод по оракулу — статический. P2-5.
- Engine-проба — scratch-тест в pristine-копии (`GatewayManager::apply_layout` + `McuEngine`), не
  live-сервер; она воспроизводит ветку применения раскладки и авто-геометрию.
- Live-БД не открывалась; наличие таких legacy-записей в проде неизвестно. P1-3 доказан на
  синтетическом seed и на достижимой последовательности реальных route-вызовов.
- Все scratch-правки (`resource_tests.rs`, `gateways.rs`) удалены, файлы сверены с `0febfe5` по
  sha256; scratch-каталог и `CARGO_TARGET_DIR` удалены. Замечание методики: `git archive` при
  восстановлении ставит старую mtime, и cargo может переиспользовать stale-артефакт; после
  восстановления файлы принудительно `touch`-ились и пересобирались (обнаружено на первом
  пост-восстановительном прогоне, который ещё показывал size 5; повторный — 9/200).
- В live-репо записан только `docs/audit/2026-10-29/reviews/T06_rereview4.md`; live HEAD во время
  ревью продвинулся до `214a673` другими агентами, но проверяемые файлы не менялись.

---

## 8. Итог

**Verdict: 8.8 / 10 — done: no. 0 P0, 1 P1, 3 P2, 7 P3.**

- **+** P1-2 закрыт: единый предикат (8 ключей) для capacity и ролей; contact-only 9 слотов +
  `preset_equal_5` → size 9, rename 200; attach 1..9/10-й 409; MCU рисует 9; seeded
  `sync_from_storage` даёт 9/PEER/200; negative control валит committed-тест (5 vs 9) и
  воспроизводит 409.
- **+** Пустой AUTO не раздувает capacity; pre-merge stored capacity не даёт новым позициям
  расширять ёмкость; legacy `abonent_id` сценарий зелёный.
- **+** §4.6 теперь соответствует коду, §12/§13 точны; fmt/clippy/layout 54/admin 350/server
  146+3 ignored — зелёные, дублей тестов нет.
- **−** **P1-3:** пустые AUTO за ёмкостью (legacy grid-4 + AUTO pos5; и ровно сценарий P1-2 после
  detach всех окон) делают запись нередактируемой: stored size 9 / resolver 5, rename 409.
  Committed-тесты этот путь не покрывают.
- **−** P2-3/P2-4/P2-5 остаются; P3-1…P3-7 — долги.

Для done нужно закрыть P1-3 и добавить committed route-регрессии на пустой AUTO и detach-all; для
9.5+ — ещё и закрыть/явно принять P2-5 (исполнимый e2e-оракул) и P2-3/P2-4.

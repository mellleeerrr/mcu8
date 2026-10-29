# T06 — повторное ревью №8 (`3e2983c`, P2-1, committed-тест и полный acceptance)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, ветка `master`.
- **Проверяемый HEAD:** `3e2983c` «fix(T04b): derive production RoomCaps from the codec SSOT»
  (включает `8b47dfe` — clippy-alias `StoppedRecord` и единый preset-first резолвер mode/PATCH).
- **История:** `T06_rereview5` — 9.0, no (P1-5); `fff51a7` — фикс P1-5; `T06_rereview6` — 5.0,
  no (build-break); `T06_rereview7` — 8.5, no (P1 clippy `type_complexity` + P2-1
  precedence mode/PATCH).
- **Полигон:** `git archive 3e2983c | tar -x -C /dev/shm/t06rr8/src` (без `.git`/`target`),
  scratch-копии `/dev/shm/t06rr8/scratch` (пробы) и `/dev/shm/t06rr8/mut` (мутационные
  проверки). Rust **1.94.0** (`rust-toolchain.toml`), все сборки `--offline`, `/tmp` не
  использовался. `CARGO_TARGET_DIR` уникальные: `/dev/shm/t06rr8/target` (fmt/clippy),
  `/home/pro/.cache/t06_rr8_target` (пробы/мутации), `/home/pro/.cache/t06_rr8_target_std`
  (стандартный прогон server). Один первый прогон server был прерван средой
  (`No space left on device` в `/dev/shm`), затем повторён на root-cache; это не код.
- **Live-среда:** `vks_data.db` не открывалась, сервер на `:8080`/`:18082` не запускался,
  browser e2e не запускался. В live-репо записан только этот файл.

## Вердикт

**Verdict: 9.2 / 10 — done: no. 0 P0, 0 P1, 1 P2, 4 P3.**

Поведение T06 на exact `3e2983c` подтверждено **исполнением на реальных route-хендлерах**:
P2-1 по результату исправлен (mode и PATCH дают идентичные 200/25 и 409/13/0, тихого prune
нет), P1-5 и все ранее закрытые P1 проходят, движок получает 9 assignments/geometry/slot_rects.
Оба обязательных clippy-гейта на exact HEAD **зелёные** (`StoppedRecord` alias закрыл
`type_complexity`). Но **committed-теста, который падает при регрессии precedence, нет** —
вопреки утверждению в задании. Мутационная проверка это доказывает: возврат mode к
size-first (или резолвера к size-first) оставляет **368/368** admin-тестов зелёными. Это
единственный незакрытый пункт acceptance, поэтому done: no.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** (нет committed-теста P2-1; утверждение задания ложно) |
| **P3** | **4** (size>MAX, size-only no-op, pos0→409, неточная скобка §4.6) |

---

## 1. Гейты exact `3e2983c` (Rust 1.94.0, `--offline`)

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all --check` | **0** | без вывода |
| `cargo clippy -p vks-layout -p vks-admin -p vks-server --all-targets --offline -- -D warnings` | **0** | `Finished` 1m27s, 0 warnings |
| `cargo clippy --workspace --all-features --all-targets --locked --offline -- -D warnings` | **0** | `Finished` 55.61s, 0 warnings |
| `cargo test -p vks-layout --lib` | **0** | **54 passed**, 0 failed |
| `cargo test -p vks-admin --lib` | **0** | **368 passed**, 0 failed, 0 ignored (35.53s) |
| `cargo test -p vks-server --lib` | **0** | **150 passed**, 0 failed, **3 ignored** (20.04s) |
| scratch admin-пробы T06 | **0** | **9 passed**, 0 failed |
| scratch engine-проба | **0** | **1 passed**; `assignments=9 geometry=9 slot_rects=9` |

`type_complexity` действительно закрыт алиасом: `stopped_cache` имеет тип
`Arc<Mutex<HashMap<RoomId, StoppedRecord>>>` (`crates/vks-server/src/recorder.rs:136`),
`type StoppedRecord = (RecordingMeta, Option<String>)` (`:535`). Targeted и workspace clippy
проходят без `#[allow]` и без warning'ов.

Примечание по среде: первый `cargo test -p vks-server --lib` с `CARGO_TARGET_DIR` на
`/dev/shm` упал на ассемблере `No space left on device` (tmpfs был заполнен параллельными
сборками), не на коде. Повтор на `/home/pro/.cache/t06_rr8_target_std` со стандартным
профилем — **150 passed / 3 ignored**. Более ранний повтор на debug=0 дал тот же результат.

---

## 2. P2-1: поведение исправлено, но committed-теста НЕТ

### 2.1 Код — единый preset-first резолвер

`switch_video_layout_mode` больше не считает объявленную ёмкость по `size` раньше пресета:
`declared_from_request` собирает `{preset, size}` и вызывает
`super::resources::explicit_layout_capacity` (`crates/vks-admin/src/api_v3/shell.rs:1010-1022`).
PATCH использует тот же резолвер (`crates/vks-admin/src/api_v3/resources.rs:478-489`, вызов
`:1599`), валидация — `max(сохранённая, объявленная)` (`:1615-1619`). Порядок резолвера:
`grids → codian → preset_size → size`, затем `.min(MAX_WINDOWS)`; `slots.len` не входит.

### 2.2 Исполнение: mode и PATCH идентичны

Обе записи — `individual`, `preset_default_13`, `size=13`, привязаны к сессии; payload
подавался в mode и в PATCH отдельно:

```text
RR8 PRECEDENCE-A mode=200 OK size=Some(25) slots=Some(25) | patch=200 OK size=Some(25) slots=Some(25)
RR8 PRECEDENCE-B mode=409 Conflict size=Some(13) slots=Some(0) | patch=409 Conflict size=Some(13) slots=Some(0)
```

- `preset_equal_25` + `size:13` + занятые слоты 1..25 → **оба 200**, size 25, slots 25.
- `preset_default_13` + `size:25` + занятые слоты 1..25 → **оба 409**, size 13, slots 0;
  запись не изменена, тихого prune 14..25 нет.

### 2.3 Committed-теста нет — главный дефект приёмки

`git show --numstat 8b47dfe` содержит только `shell.rs`, `recorder.rs` и четыре review-дока;
`3e2983c` — только `main.rs`/`handler.rs`. Файл `resource_tests.rs` этими коммитами не менялся.
Все committed-вызовы mode:

- `mode_capacity_is_declared_only_by_size_or_preset` (`resource_tests.rs:2725-2767`) подаёт
  slots-only, затем только preset, затем preset+slot26 — **без `size`**;
- `t7_mode_only_preserves_preset_type_size` (`:1332`) подаёт `preset:"vks_grid_9", size:9` —
  поля согласованы, precedence не различается;
- `mode_preset_switch_recalculates_capacity` (`:2773`), `t00_mode_preset_switch_uses_preset_capacity`
  (`:2989`) — только preset;
- прямого теста `explicit_layout_capacity` нет (`grep` по `resource_tests.rs`/`tests.rs` — 0).

Мутация (scratch `/dev/shm/t06rr8/mut`, exact `3e2983c` плюс один откат):

1. `declared_from_request` возвращён к старому size-first (как в `f602f72`, divergence
   mode/PATCH) → `cargo test -p vks-admin --lib` = **368 passed, 0 failed**.
2. `explicit_layout_capacity` возвращён к size-first для обоих путей → **368 passed, 0 failed**.

То есть ни один committed-тест не падает при регрессии precedence. Требование задания
«committed test exists and fails if precedence regressed» **не выполнено**; это P2, а не P3,
потому что это обязательный пункт acceptance и утверждение в задании ложно. Нужны два теста
с одновременными `preset+size` и проверкой неизменности записи на 409 (как в пробах выше).

---

## 3. P1-5 и все ранее закрытые P1 — подтверждены исполнением

Все строки ниже — вывод scratch-проб на реальных route-хендлерах (9 admin + 1 engine), не
рассуждение по коду.

| # | Сценарий | Факт | Статус |
|---|---|---|---|
| 1 | mode slots-only 1..25, пустые AUTO, база 13 | **409**, size 13, slots 0, запись не изменена | **PASS** |
| 2 | mode slots-only 1..25, база 13 с 13 занятыми | **409**, size 13, slots 13 | **PASS** |
| 3 | mode `preset_equal_25` | **200**, size 25, slots 0 | **PASS** |
| 4 | mode `preset_equal_25` + слот 26 | **409**, size 25, slots 0, слот 26 не записан | **PASS** |
| 5 | PATCH `vks_grid_5` → `vks_grid_9`, 9 занятых | **200**, size 9, slots 9 | **PASS** |
| 6 | PATCH `preset_default_13` → `vks_grid_43`, 43 занятых | **200**, size 43, slots 43 | **PASS** |
| 7 | `edit_linked` 5→9 (scoped/COW) | **200**, size 9, slots 9 | **PASS** |
| 8 | contact-only legacy 9 → shrink `preset_equal_5` → rename | size **9**, slots 9; rename **200** | **PASS** |
| 9 | detach всех 9 → shrink на 5 → rename | detach 9×**204**; shrink **200** size 5/slots 5; rename **200** | **PASS** |
| 10 | detach одного у legacy 9 (preset 5) → rename | **204**, size 8/slots 8; rename **200** | **PASS** |
| 11 | grid-4 + пустые AUTO 1..5 → rename | **200**, size 4, slots prune 5→4 | **PASS** |
| 12 | junk: slots-only 9; preset9+pos10; duplicate pos9; pos0; pos string; missing pos; non-object | **409/409/409/409/422/422/422**, запись не изменена после каждого | **PASS** |
| 13 | seeded legacy `abonent_id` 9 (storage + `sync_from_storage`, stale preset 5) | после load size 9/slots 9; rename **200** | **PASS** |
| 14 | attach 1..9 на preset 9 / 10-й | `[200×9, 409]`, slots 9 | **PASS** |
| 15 | full-save echo после load: grid-4 + AUTO 1..5 в storage → reload → PATCH эхом | после load size 4/slots 4; full-save **200**, slots 4 | **PASS** |
| 16 | engine: `GatewayManager::apply_layout` + `McuEngine`, 9 слотов | `assignments=9 geometry=9 slot_rects=9` | **PASS** |

Фрагмент `--nocapture`:

```text
RR8 MODE-SLOTS-EMPTY status=409 size=13 slots=0
RR8 MODE-SLOTS-OCCUPIED status=409 size=13 slots=13
RR8 MODE-PRESET-25 status=200 size=25 slots=0
RR8 MODE-PRESET-25-SLOT26 status=409 size=25 slots=0
RR8 PATCH-5-9 status=200 size=9 slots=9
RR8 PATCH-13-43 status=200 size=43 slots=43
RR8 EDIT-LINKED status=200 size=9 slots=9
RR8 CONTACT-LEGACY size=9 slots=9 rename=200
RR8 DETACH-ALL size=5 slots=5 rename=200
RR8 DETACH-ONE size=8 slots=8 rename=200
RR8 GRID4-PRUNE status=200 size=4 slots=4
RR8 JUNK 7/7 statuses and no mutation
RR8 SEEDED-LEGACY after load size=9 slots=9 rename=200
RR8 ATTACH statuses=[200, 200, 200, 200, 200, 200, 200, 200, 200, 409] slots=9
RR8 FULL-SAVE after load size=4 slots=4 status=200
RR8 ENGINE assignments=9 geometry=9 slot_rects=9
```

Committed-тесты `declared_preset_growth_is_accepted_and_validated`,
`legacy_empty_auto_slots_beyond_capacity_do_not_block_rename`,
`detached_empty_windows_do_not_poison_record`,
`contact_only_legacy_slots_keep_capacity_across_preset_shrink`,
`mode_capacity_is_declared_only_by_size_or_preset`, `mode_preset_switch_recalculates_capacity`,
`t00_mode_preset_switch_uses_preset_capacity` входят в 368 и проходят.

---

## 4. P3 (не блокируют T06-логику, но честно фиксируются)

| # | Sev | Проблема | Файл:строки | Подтверждение |
|---|---|---|---|---|
| P3-1 | P3 | `size > MAX_WINDOWS` (100): mode фильтрует `size` в `1..=100` и считает его отсутствующим → **409**, PATCH принимает `size:1000`, клампит до 100 и (из-за preset) молча сохраняет size 13/slots 13 → **200**. Для невалидного входа mode/PATCH расходятся, PATCH делает silent prune 14..25 | `crates/vks-admin/src/api_v3/shell.rs:939-941`; `crates/vks-admin/src/api_v3/resources.rs:487-488` | `RR8 SIZE>MAX mode=409 Conflict size=Some(13) slots=Some(0) | patch=200 OK size=Some(13) slots=Some(13)` |
| P3-2 | P3 | `size`-only рост на preset-записи — тихий no-op: mode и PATCH дают 200, но `size` остаётся пресетным, слоты 14..25 вырезаются. Это остаток rr7 P3-1, не регрессия `3e2983c` | `resources.rs:1610,1622-1623`; `shell.rs:1025-1050`; `docs/rules/BACKEND_RULES.md:193-196` | `RR8 SIZE-ONLY mode=200 size=Some(13) slots=Some(13) | patch=200 size=Some(13) slots=Some(13)` |
| P3-3 | P3 | `position == 0` → **409 Conflict** вместо 422 Invalid (pre-existing, не менялся) | `crates/vks-admin/src/api_v3/resources.rs:1559-1560` | junk-проба: pos0 **409**, запись не изменена |
| P3-4 | P3 | `§4.6:202` формулирует mode как `max(сохранённая, size/пресет)` и «те же правила»; для валидных payload'ов это теперь верно, но precedence (пресет раньше size) в скобке не назван, а для `size>MAX` «те же правила» неверны (P3-1) | `docs/rules/BACKEND_RULES.md:202`; `:193-196` | код + пробы §2.2/§4 |

---

## 5. Документация

- **`BACKEND_RULES.md` §4.6** (`:179-206`): порядок резолвера `grids → codian → preset_size
  → payload.size → slots.len` совпадает с `derived_layout_size` (`resources.rs:467-476`);
  `effective_capacity = max(резолвер, max(position) занятых)` и 8 ключей
  `slot_holds_participant` — с `layout_size`/`slot_holds_participant` (`resources.rs:415-431`);
  `validation_capacity = max(сохранённая, объявленная)`, где объявленная — `size>0`/пресет,
  а не `slots.len` — с PATCH (`:1592-1621`) и mode (`shell.rs:1010-1022`);
  строгая проверка позиций из запроса и prune на load/write — с `:1544-1580`, `:1622`,
  `:1263-1265`. **Mode-фраза `:202` теперь соответствует поведению** для валидных payload'ов;
  остаётся P3-4 (нет явного precedence и edge `size>MAX`).
- **`LAYOUT_ENGINE.md` §12/§13** (`:239-269`): `git diff fff51a7 3e2983c` по файлу пуст —
  разделы не менялись. §12 (S6 3×2, 83.28 %) закреплён тестом
  `grid_five_slots_at_1280x720_fill_is_83_28_percent` (`crates/vks-layout/src/grid.rs:186`),
  который проходит в 54 layout-тестах; §13 (`preset_default_13` → 400) не противоречит
  исполнению. Overclaim не найден.

---

## 6. Гигиена

- Live-БД `vks_data.db`: sha256 `687b758f6e3c6dd31021109c05fa626f19dfb3f2e1fd6c69d3f3f1c90caa81ce`,
  size `3248128`, mtime `1793266711` — совпадает с rr7; за ревью не открывалась.
- Порты `:8080` (PID `3255206`) и `:18082` (PID `3253281`) — те же PIDs, сервер не
  стартовал; browser e2e не запускался.
- `backups/` (mtime `2026-10-29 07:32:22`) не тронут.
- В live-репо записан только `docs/audit/2026-10-29/reviews/T06_rereview8.md`; прочие
  изменения worktree — других задач, не мои.
- `/tmp` не использовался. Pristine `/dev/shm/t06rr8`, scratch, mut, target-кэши
  (`/home/pro/.cache/t06_rr8_target*`) и логи удалены после сдачи; live-репо не собирался.

---

## 7. Счёт

**9.2 / 10, done: no.**

**+**
- Оба обязательных clippy-гейта на exact `3e2983c` — **0**; `type_complexity` закрыт
  `StoppedRecord` alias без `allow`.
- fmt — 0; layout 54, admin 368, server 150/3 ignored — всё зелёное.
- P2-1 по поведению исправлен: mode и PATCH идентичны на конфликтующих `preset+size`
  (200/25 и 409/13/0), тихого prune нет; единый `explicit_layout_capacity` используется
  обоими путями.
- P1-5 и все ранее закрытые P1 подтверждены 16 сценариями на реальных хендлерах, включая
  seeded legacy, attach 9/10, full-save echo и engine 9/9/9.
- §4.6 в остальном точна; §12/§13 не менялись и не переоценивают поведение.

**−**
- **P2-1:** committed-теста precedence **нет**; утверждение задания «test exists» ложно.
  Мутация возврата mode к size-first (и отдельно резолвера к size-first) оставляет
  **368/368** admin-тестов зелёными. Без такого теста регрессия P2-1 не ловится.
- **P3-1:** `size>MAX` — mode 409 vs PATCH 200 + silent prune; **P3-2:** size-only на
  preset-записи — 200/no-op; **P3-3:** pos0 → 409; **P3-4:** скобка §4.6:202 без
  precedence.

Для 9.5+ нужно: добавить committed-тесты
`mode_preset_growth_with_stale_size_is_200` и
`mode_size_beyond_preset_with_slots_is_409` (с проверкой неизменности записи на 409 и
PATCH-эквивалентом) и решить, валидировать ли `size>100` как 422 в обоих путях. T06-код
правки не требует.

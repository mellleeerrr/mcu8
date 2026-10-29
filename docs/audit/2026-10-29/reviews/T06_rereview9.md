# T06 — повторное ревью №9 (`737f6e1`, committed precedence-тест + мутационное доказательство)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, ветка `master`.
- **Проверяемый HEAD:** `737f6e1` «style(vks-common): drop stray blank line flagged by
  cargo fmt» — потомок `0cb6434` (committed-тест) и `3e2983c` (HEAD ревью №8).
- **История T06:** rr5 — 9.0/no (P1-5), `fff51a7`; rr6 — 5.0/no (build-break); rr7 — 8.5/no
  (clippy `type_complexity` + P2-1 precedence); rr8 — **9.2/no** (нет committed-теста
  precedence; mode/PATCH-поведение уже верное).
- **Что добавил `0cb6434` по T06:** только `crates/vks-admin/src/api_v3/resource_tests.rs`
  (+63 строки) — тест `mode_and_patch_agree_on_preset_with_stale_size`
  (`resource_tests.rs:2772-2833`). `shell.rs`/`resources.rs` в коммите не менялись
  (`git show --stat 0cb6434`); код T06-ёмкости с `3e2983c` **не менялся** (проверено
  `git diff 3e2983c..737f6e1` по обоим файлам — изменений capacity-функций нет,
  только T10-directory).
- **Полигон:** `git archive 737f6e1 | tar -x -C /dev/shm/t06rr9/src` (без `.git`/`target`),
  scratch-пробы `mod t06_rr9_probes` временно добавлены в `resource_tests.rs` и удалены,
  контрольная распаковка `/dev/shm/t06rr9/verify`; Rust **1.94.0** (`rust-toolchain.toml`),
  все сборки `--offline`. `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr9_target` (один
  уникальный кэш). `/tmp` не использовался.
- **Live-среда:** `vks_data.db` не открывалась (sha256 `d2ccc444b7d7…`, size `3334144`,
  mtime `16:31:10` — **до** начала ревью в ~16:48), сервер не запускался (порты `:8080`
  PID `1914707`, `:18082` PID `3253281` — чужие процессы, отличаются от rr8, не мои),
  `backups/` (mtime `2026-10-29 07:32:22`) не тронут, browser e2e не запускался.
  В live-репо записан только этот файл.

## Вердикт

**Verdict: 9.3 / 10 — done: no. 0 P0, 0 P1, 1 P2, 5 P3.**

Требование rr8 **(1)** выполнено и впервые **доказано мутационно**: committed-тест
`mode_and_patch_agree_on_preset_with_stale_size` падает при возврате резолвера к
size-first и при divergence любой из сторон, а остальные **374/375** admin-тестов
остаются зелёными; прогон 3×. Требование **(2)** — явное решение по `size > MAX`
(422 или документированный отказ) — **не выполнено**: с `3e2983c` §4.6 не менялся
(`+26` строк в `BACKEND_RULES.md` — только §5.4 T10), ни теста, ни кода по `size>100`
нет, и поведение по-прежнему расходится (mode 409 vs PATCH 200 + prune); это P2
приёмки → done: no.

| Категория | Кол-во |
|---|---:|
| **P0** | **0** |
| **P1** | **0** |
| **P2** | **1** (нет решения/документа/теста по `size > MAX`; mode/PATCH расходятся) |
| **P3** | **5** (size-only no-op, no committed PATCH-widen test, pos0→409, §4.6 precedence, 409-ассерт проверяет только size) |

---

## 1. Гейты exact `737f6e1` (Rust 1.94.0, `--offline`)

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all --check` | **0** | без вывода (2.7 s) |
| `cargo clippy -p vks-admin --all-targets --offline -- -D warnings` | **0** | `Finished` 1m02s, **0 warnings** |
| `cargo clippy --workspace --all-targets --offline -- -D warnings` | **0** | `Finished` 1m26s, 0 warnings |
| `cargo test -p vks-admin --lib` | **0** | **375 passed**, 0 failed, 0 ignored (35.29 s) |
| `cargo test -p vks-layout --lib` | **0** | **54 passed**, 0 failed |
| `cargo test --workspace --lib` | **0** | **16 suites: 1520 passed, 0 failed, 4 ignored** |
| новый тест, 3× (`mode_and_patch_agree_on_preset_with_stale_size`) | **0** | 3/3 ok (0.02–0.04 s) |
| T06 adjacent (`mode_*` 3, `t00_mode_*` 3, `t7_mode_only_*` 1), 3× | **0** | 3/3, 3/3 и 1/1 ok в каждом прогоне |
| мутационные прогоны, 3× `cargo test -p vks-admin --lib` | **101→fail** | каждая мутация: 1 failed / 374 passed (только новый тест) |

`vks-server` в workspace-прогоне: **154 passed, 3 ignored**; admin — 375; layout — 54.
Все прогоны — на pristine-дереве (`diff -r /dev/shm/t06rr9/src /dev/shm/t06rr9/verify`
после restore — **идентично**, sha256 трёх файлов совпадают с блобом `737f6e1`).

---

## 2. Требование (1): committed-тест + мутационное доказательство — ВЫПОЛНЕНО

### 2.1 Тест

`mode_and_patch_agree_on_preset_with_stale_size` (`resource_tests.rs:2776-2833`) на
свежей default-записи (capacity 13) и занятой сессии проверяет две стороны одного
резолвера на реальных route-хендлерах:

1. `{preset: preset_equal_13, size: 25, slots: 1..25(AUTO)}` → **409 и у mode, и у PATCH**;
   `capacity` записи не изменилась (`before`), т.е. отклонённый payload ничего не пишет;
2. `{preset: preset_equal_25, size: 13, slots: 1..25}` → **200 у обоих**, итоговая
   `capacity = 25` (устаревший `size 13` не сжимает пресет).

Коммит `0cb6434` содержит ровно этот тест; `shell.rs`/`resources.rs` не менялись, т.е.
тест закрепляет уже проверенное rr8 поведение. Прогон **3×** — ok.

### 2.2 Мутации (каждая — отдельно, полный `cargo test -p vks-admin --lib`)

Перед мутациями сохранены pristine-копии; после каждой — restore и сверка sha256 файла
с `git show 737f6e1`.

| # | Что откатано | Компиляция | Результат admin-сьюта | Падение |
|---|---|---|---|---|
| **A** | `explicit_layout_capacity` → size-first (`resources.rs:518-529`), т.е. резолвер как в `f602f72` | ok | **1 failed / 374 passed** | `resource_tests.rs:2823`: `mode: slot beyond preset capacity`, left 200, right 409 |
| **B** | mode-only divergence: `declared_from_request` считает `size` раньше пресета (`shell.rs:1107-1116`) | ok | **1 failed / 374 passed** | `:2823`: mode 200 ≠ 409 |
| **C** | PATCH-only divergence: `declared_capacity` в `update_resource` считает `size` раньше пресета (`resources.rs:1639`) | ok | **1 failed / 374 passed** | `:2825`: `PATCH: slot beyond preset capacity`, left 200, right 409 |

Во всех трёх случаях падает **только** новый тест; «rest of vks-admin stays green»
(374/375) выполнено, что и требовалось в rr8. После мутаций восстановлен
байт-идентичный `737f6e1` (`diff -r` пуст, sha256 `resources.rs`
`f17f92be…`, `shell.rs` `c91c022e…`, `resource_tests.rs` `558692e4…`).

### 2.3 Наблюдение (не дефект)

rr8 называл имена `mode_preset_growth_with_stale_size_is_200` и
`mode_size_beyond_preset_with_slots_is_409`; в коммите один тест
`mode_and_patch_agree_on_preset_with_stale_size` покрывает оба сценария и **обе** стороны
(mode и PATCH). По смыслу требование эквивалентно; это P3-2 ниже (слабость ассертов),
но не подмена требования.

---

## 3. Требование (2): решение по 422 для `size > MAX` — НЕ ВЫПОЛНЕНО (P2-1)

Проверено, что решения нет **нигде** в коммите:

- `git diff 3e2983c..737f6e1 -- docs/rules/BACKEND_RULES.md` — только §5.4 T10 (+26 строк,
  directory/single-writer); §4.6 (`:179-206`) **не тронут**;
- `docs/audit/2026-10-29/pitfalls/T06.md`, `docs/progress/MASTER_TASK_LIST*.md`,
  `docs/decisions/*` — упоминания `422` для `size>MAX` нет (`grep` по docs — 0);
- код не менялся: mode фильтрует `size` в `1..=100` и считает его отсутствующим
  (`shell.rs:1033-1035`), PATCH клампит `size:1000` до 100 в резолвере
  (`resources.rs:518-529`, вызов `:1639`);
- committed-теста на `size>100` нет (`tests.rs:1745` проверяет ровно `size=100`).

Исполнение на `737f6e1` (проба p1, `--nocapture`):

```text
P size1000:            mode=200 OK [size=13 slots=0]           | patch=200 OK [size=13 slots=0]
P size1000+slots25:    mode=409 Conflict [size=13 slots=0]     | patch=200 OK [size=13 slots=13]
P size100+slots25:     mode=200 OK [size=13 slots=13]          | patch=200 OK [size=13 slots=13]
```

То есть для невалидного входа mode и PATCH **по-прежнему расходятся**: mode 409, PATCH
200 с тихим prune окон 14..25. Это ровно rr8 P3-1, и обязательное «решить, валидировать
ли `size>100` как 422 в обоих путях» не сделано. Требование приёмки не выполнено →
**P2-1**, поэтому максимум < 9.5.

Минимально достаточно: либо 422 в обоих путях при `size > MAX_WINDOWS`, либо явно
записать в §4.6/ADR «`size>100` клампится, mode считает его отсутствующим» + committed
тест, фиксирующий одинаковый статус mode/PATCH.

---

## 4. Семантика ёмкости end-to-end (подтверждено исполнением)

Карта кода (exact `737f6e1`):

| Инвариант | Где | Проверка |
|---|---|---|
| Единый резолвер объявленной ёмкости: `grids → codian → preset_size → size>0`, clamp 100; **без** `slots.len` | `resources.rs:518-529` | код + probes |
| Резолвер вызывают **обе** стороны | PATCH `resources.rs:1639`; mode `shell.rs:1115` (через `declared_from_request`, `:1107-1116`) | `grep explicit_layout_capacity` — ровно 2 вызова |
| `validation_capacity = max(сохранённая, объявленная)` | PATCH `resources.rs:1655-1659`; mode `shell.rs:1160-1164` | stale-проба: stored 25/preset 13, слот 20 → 409 |
| Строгая проверка позиций **из запроса** (409), stored-пустые AUTO за ёмкостью пропускаются | `resources.rs:1572-1620` (`incoming`) | 409 на slots-only/слот-26; 200 на prune |
| Prune пустых AUTO за ёмкостью | `:535-544`; вызовы `normalize_loaded:1304`, mode `shell.rs:1175`, PATCH `resources.rs:1662` | prune-проба: grid4 + AUTO 1..5 → 200, size 4, slots 4 |
| `layout_size = max(derived, max(position занятых))`, занятость — один предикат | `:458-471`, `:482-510` | contact-only/grid4 тесты в 375 |
| `slots.len` не расширяет объявленную ёмкость | `resources.rs:518-529` | `slots25-only` → **409** у обоих |
| mode и PATCH согласованы | `shell.rs:1160-1174` vs `resources.rs:1655-1661` | матрица ниже |

Матрица прогонов (mode и PATCH на **отдельных** свежих записях; `--nocapture`):

```text
P preset-only-25:            mode=200 [size=25 slots=0]   | patch=200 [size=25 slots=0]
P preset-only-5:             mode=200 [size=5  slots=0]   | patch=200 [size=5  slots=0]
P size-only-25:              mode=200 [size=13 slots=0]   | patch=200 [size=13 slots=0]
P preset13+size25:           mode=200 [size=13 slots=0]   | patch=200 [size=13 slots=0]
P preset25+size13:           mode=200 [size=25 slots=0]   | patch=200 [size=25 slots=0]
P preset13+size25+slots25:   mode=409 [size=13 slots=0]   | patch=409 [size=13 slots=0]
P preset25+size13+slots25:   mode=200 [size=25 slots=25]  | patch=200 [size=25 slots=25]
P preset13+slots13:          mode=200 [size=13 slots=13]  | patch=200 [size=13 slots=13]
P preset13+slots26:          mode=409 [size=13 slots=0]   | patch=409 [size=13 slots=0]
P slots25-only:              mode=409 [size=13 slots=0]   | patch=409 [size=13 slots=0]
P dup-pos:                   mode=409 [size=13 slots=0]   | patch=409 [size=13 slots=0]
P pos0:                      mode=409 [size=13 slots=0]   | patch=409 [size=13 slots=0]
P size1000:                  mode=200 [size=13 slots=0]   | patch=200 [size=13 slots=0]
P size1000+slots25:          mode=409 [size=13 slots=0]   | patch=200 [size=13 slots=13]   <-- P2-1
P size100+slots25:           mode=200 [size=13 slots=13]  | patch=200 [size=13 slots=13]
```

Дополнительно (stale/prune/nested/sequential):

```text
P stale-start                    size=25 slots=0     (RAM-запись: preset13 + stale size25)
P stale mode slot20  -> 409 [size=25 slots=0]   | P stale patch slot20 -> 409 [size=25 slots=0]
P stale patch rename -> 200 [size=13 slots=0]   | P stale mode empty   -> 200 [size=13 slots=0]
P prune-start size=13 slots=5 → patch rename 200 [size=4 slots=4] | mode grid4 200 [size=4 slots=4]
P bare (create без preset/size) → size=13; mode/patch slots25 → 409/409
P seq: patch conflict 409 → mode conflict 409; patch grow 200 [25/25] → mode grow 200 [25/25]
P nested (UI payload{}) mode 409 / patch 409; grow 200 [25/25] у обоих
```

Итог: единственное расхождение mode/PATCH — `size > MAX_WINDOWS` (P2-1); во всех
остальных комбинациях (preset only, size only, конфликтующий preset+size, stale stored,
AUTO-слоты, duplicate positions, nested UI-payload, позиция 0) стороны совпадают,
`slots.len` ёмкость не расширяет, prune и строгая проверка запроса работают.

---

## 5. P-список (с файлами и строками)

| # | Sev | Проблема | Файл:строки |
|---|---|---|---|
| **P2-1** | **P2** | Нет решения/документа/теста по `size > MAX_WINDOWS`: mode молча игнорирует `size>100` (`filter(1..=100)`) и при слотах даёт 409, PATCH клампит до 100 и даёт 200 с тихим prune; §4.6 не менялся | `crates/vks-admin/src/api_v3/shell.rs:1033-1035`; `crates/vks-admin/src/api_v3/resources.rs:518-529,1639`; `docs/rules/BACKEND_RULES.md:179-206` |
| P3-1 | P3 | `size`-only рост на preset-записи — тихий no-op: 200, `size` остаётся пресетным, слоты за пресетом вырезаются (остаток rr7/rr8) | `resources.rs:1655-1663`; `shell.rs:1118-1145`; `docs/rules/BACKEND_RULES.md:202-203` |
| P3-2 | P3 | committed-тест на 409 проверяет неизменность только `payload.size` (`before`), не полный payload/slots; PATCH-сторона в тесте вызывается после mode, отдельного «mode не изменил запись» нет | `resource_tests.rs:2819-2826` |
| P3-3 | P3 | `position == 0` → **409 Conflict** вместо 422 Invalid (pre-existing; в pitfalls-плане T06 записан 422) | `resources.rs:1599-1601`; `docs/audit/2026-10-29/pitfalls/T06.md:290,316` |
| P3-4 | P3 | §4.6:202-203 «Mode-эндпоинт использует те же правила (`max(сохранённая, size/пресет)`)» — precedence «пресет раньше size» в скобке не назван, а для `size>MAX` «те же правила» неверны | `docs/rules/BACKEND_RULES.md:202-203`; `:180-182` |
| P3-5 | P3 | Нет committed-теста, что **PATCH** отклоняет расширение одними `slots` (409); committed `mode_capacity_is_declared_only_by_size_or_preset` покрывает только mode (поведение PATCH проверено пробой, но не закреплено) | `resource_tests.rs:2727-2770`; `resources.rs:1639` |

---

## 6. Документация

- **`BACKEND_RULES.md` §4.6** (`:179-206`): порядок резолвера (`grids → codian →
  preset_size → payload.size`), `effective_capacity = max(резолвер, занятые)`,
  `slot_holds_participant` (8 ключей), `validation_capacity = max(сохранённая,
  объявленная)`, «`slots.len` не входит», строгая проверка позиций запроса и prune —
  совпадают с кодом и прогонами §4. Остаются P2-1/P3-1/P3-4.
- **`LAYOUT_ENGINE.md` §12/§13**: `git diff 3e2983c..737f6e1` по файлу пуст; §12
  (S6 3×2, 83.28 %) закреплён `grid_five_slots_at_1280x720_fill_is_83_28_percent`
  (`vks-layout/src/grid.rs:186`, входит в 54 layout-теста); §13 (`preset_default_13` →
  400) не противоречит исполнению.

---

## 7. Гигиена

- Live-БД `vks_data.db`: sha256 `d2ccc444b7d7bc4ab75c5c077b7dce513c896622333732ebc3e1399f11531e0e`,
  size `3334144`, mtime `2026-10-29 16:31:10` — значение **до** начала ревью (16:48);
  за ревью не открывалась (admin/lib-тесты используют `MemoryStorage`).
- Порты `:8080` (PID `1914707`) и `:18082` (PID `3253281`) — не мои процессы, сервер не
  стартовал (PID 8080 отличается от rr8 — изменение среды не этим ревью).
- `backups/` (mtime `2026-10-29 07:32:22`) не тронут; browser e2e не запускался.
- В live-репо записан только этот файл; прочие изменения worktree — других задач.
- `/tmp` не использовался. Pristine `/dev/shm/t06rr9`, verify-копия, логи и target-кэш
  `/home/pro/.cache/t06_rr9_target` удалены после сдачи; live-репо не собирался.

---

## 8. Счёт

**9.3 / 10, done: no.**

**+**
- Обязательные гейты: fmt **0**, clippy admin **0 warnings**, clippy workspace **0**;
  admin **375**, layout **54**, workspace lib **1520 passed / 4 ignored / 0 failed**.
- Требование rr8 (1) выполнено: committed-тест `mode_and_patch_agree_on_preset_with_stale_size`
  есть, проходит **3×**, и **мутационно доказан** — откат резолвера к size-first (A),
  divergence mode (B) и divergence PATCH (C) дают падение **только** этого теста при
  374/375 зелёных; restore байт-идентичен `737f6e1`.
- Семантика ёмкости подтверждена 20+ прогонами на реальных хендлерах: preset-first,
  `max(stored, declared)`, `slots.len` не расширяет, prune пустых AUTO, строгий запрос,
  stale stored, nested UI-payload, duplicate/pos0; mode и PATCH совпадают везде, кроме
  `size>MAX`.
- Код T06 с rr8 не менялся — регрессий нет.

**−**
- **P2-1:** требование rr8 (2) не выполнено: решения по `size>MAX` (422 либо
  документированный кламп) нет ни в коде, ни в тестах, ни в docs; mode 409 vs PATCH 200
  с тихим prune сохраняется — единственный незакрытый пункт acceptance.
- P3-1…P3-5 (size-only no-op, слабость 409-ассерта, pos0→409, §4.6 precedence,
  нет committed-теста PATCH-widen) — не блокируют T06-логику, фиксируются честно.

Для 9.5+ достаточно: принять и зафиксировать решение по `size > MAX_WINDOWS`
(422 в обоих путях **или** документированный кламп с одинаковым статусом mode/PATCH) и
добавить committed-тест на оба пути; T06-код в остальном правок не требует.

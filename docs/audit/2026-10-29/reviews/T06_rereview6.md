# T06 — повторное ревью №6 (HEAD `fff51a7`, фикс P1-5 из rr5)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, ветка `master`.
- **Проверяемый HEAD:** `fff51a7` «fix(T06,T07): mode capacity declared only by size/preset;
  load-time prune; deeplink TTL poll; import signal restore» (родитель `42922b7`; более поздние
  `8e4197f`/`15561ff` — вне T06, но проверены на тот же дефект сборки).
- **Предыдущие ревью:** [`T06_rereview3.md`](T06_rereview3.md) — 6.5, done no (2 P1);
  [`T06_rereview4.md`](T06_rereview4.md) — 8.8, done no (P1-3);
  [`T06_rereview5.md`](T06_rereview5.md) — 9.0, done no (P1-5: mode-ёмкость из `slots.len`).
- **Полигон:** `git archive fff51a7 | tar -x -C /dev/shm/t06r6` (без `.git`/`target`), scratch-копия
  `/dev/shm/t06r6_scratch` для проб. Rust **1.94.0** (`rust-toolchain.toml`), `cargo --offline`;
  `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr6_target` (гейты) и
  `/home/pro/.cache/t06_rr6_scratch_target` (пробы); `/tmp` не использовался.
- **Live-среда:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на
  `:8080`/`:18082` не запускался/не опрашивался, browser e2e не запускался. В live-репо записан
  только этот файл.

## Вердикт

**Verdict: 5.0 / 10 — done: no. 1 P0, 0 P1, 2 P2, 5 P3.**

P1-5 из rr5 **закрыт и подтверждён исполнением** на реальных route-хендлерах: mode slots-only
(пустые и занятые) на ёмкости 13 → **409**, `preset_equal_25` без слотов → **200/size 25**,
`preset_equal_25` + слот 26 → **409 без записи слота**, PATCH-эквиваленты не изменились, mode-only
save и preset-only growth работают. Все ранее закрытые P1 (rr3/rr4) остались закрытыми, включая
13→43, `edit_linked`, detach, seeded legacy, attach 1..9/10-й и MCU 9/9/9.

Но **точный `fff51a7` не собирается**: коммит вместе с T06/T07 внёс несвязанный hunk для
codec-эндпоинтов (`shell.rs:565-617`), который вызывает `vks_common::codec_capabilities::
{audio,video}_unavailable_reason`, а этих функций нет ни в одном коммите истории — они лежат
только в dirty worktree как незакоммиченная часть другой задачи. Поэтому `cargo test/clippy`
на `fff51a7` падают с E0425, и все гейты задания на заявленном HEAD недостижимы. T06-код при этом
корректен: после отката **только** этого чужого hunk в scratch-копии clippy/tests зелёные
(см. §5), но коммит в текущем виде сдавать нельзя.

| Категория | Кол-во |
|---|---:|
| **P0** | **1** (сборка `fff51a7` сломана несвязанным codec-hunk) |
| **P1** | **0** (P1-5 закрыт; остаточная расходимость mode/PATCH — P2-1) |
| **P2** | **2** (precedence size/preset в mode; гигиена коммита) |
| **P3** | **5** |

---

## 1. P0 — `fff51a7` не компилируется (блокер, вне T06-кода)

### 1.1 Факт

На pristine `git archive fff51a7`:

```text
$ cargo test -p vks-admin --lib --offline
error[E0425]: cannot find function `audio_unavailable_reason` in module `vks_common::codec_capabilities`
   --> crates/vks-admin/src/api_v3/shell.rs:577:58
error[E0425]: cannot find function `video_unavailable_reason` in module `vks_common::codec_capabilities`
   --> crates/vks-admin/src/api_v3/shell.rs:605:59
error: could not compile `vks-admin` (lib) due to 2 previous errors
EXIT=101
```

То же для `cargo test -p vks-server --lib` (зависит от `vks-admin`) и для
`cargo clippy -p vks-layout -p vks-admin -p vks-server --all-targets -- -D warnings`
(EXIT=101), и для `cargo clippy --workspace --all-features --all-targets --locked`
(EXIT=101). `cargo test -p vks-layout` проходит (54), `cargo fmt --all --check` — 0: поломка
локализована в `vks-admin`/`vks-common`.

### 1.2 Причина

`fff51a7` содержит в `crates/vks-admin/src/api_v3/shell.rs` **два несвязанных с T06/T07 hunk'а**
(`settings_audio_codecs` `:565-587`, `settings_video_codecs` `:589-617`): добавлены поля
`"reason"`/`"video_codec_details"` и вызовы `audio_unavailable_reason`/`video_unavailable_reason`.
В сообщении коммита про codec/T22 нет ни слова, в `numstat` это часть 46 строк `shell.rs`.
Функций нет в `crates/vks-common/src/codec_capabilities.rs` ни на `fff51a7`, ни на
`25c9d99`/`42922b7`/`8e4197f`/`15561ff`:

```text
$ git grep -c audio_unavailable_reason 25c9d99 / 42922b7 / fff51a7 / 8e4197f / 15561ff
0 / 0 / 0 / 0 / 0
```

Они есть только в **dirty worktree** (`M crates/vks-common/src/codec_capabilities.rs`,
`M crates/vks-common/src/lib.rs`) как незавершённая часть другой задачи. То есть коммит собрали
из worktree, но вторую половину изменения (T22/T04b) не закоммитили.

### 1.3 Влияние и что делать

- Все гейты задания на заявленном HEAD (`clippy -p … -D warnings`, workspace clippy, admin/server
  тесты) невозможны; acceptance T06 на exact `fff51a7` тоже нельзя подтвердить без правки дерева.
- Ветка сломана не только на `fff51a7`: `8e4197f` и `15561ff` содержат тот же `shell.rs` и те же
  отсутствующие функции.
- **Fix:** либо убрать codec-hunk из T06/T07-коммита (вернуть `shell.rs` `:565-617` к `fff51a7^`),
  либо закоммитить парную часть `vks-common`/`codec_spec` отдельным коммитом и пересобрать
  `fff51a7`. После этого повторить гейты; T06-изменения правки не требуют.
- Для проверки T06 ниже использована scratch-копия, в которой откачены **только** эти два
  codec-hunk'а до родителя; mode/prune/tests/docs — ровно из `fff51a7` (`cargo check` зелёный).

## 2. P1-5 из rr5 — закрыт (исполнение на реальных хендлерах)

Пробы — scratch-тесты на `switch_video_layout_mode` / `edit_global` / `edit_linked` / `attach` /
`detach_layout_abonent`; запись создаётся с `preset_default_13`, size 13, привязана к сессии.
Фрагмент вывода (`--nocapture`):

```text
RR6 MODE slots-only empty 1..25      status=409 Conflict size=13 slots=0
RR6 MODE slots-only occupied 1..25   status=409 Conflict size=13 slots=0
RR6 MODE preset-only 25              status=200 OK       size=25 slots=0
RR6 MODE preset25 + explicit slot26  status=409 Conflict size=25 slots=0
RR6 MODE preset25 + slots 1..25      status=200 OK       size=25 slots=25
RR6 MODE mode-only save              status=200 OK       size=25 slots=25
```

| # | Сценарий (реальный mode-хендлер) | Факт | Статус |
|---|---|---|---|
| 1 | slots-only 1..25, пустые AUTO, база 13 | **409**, запись не изменена (size 13/slots 0) | **PASS** |
| 2 | slots-only 1..25, занятые `abonent_id` | **409**, size 13 | **PASS** |
| 3 | `preset_equal_25`, без slots | **200**, size 25 | **PASS** |
| 4 | `preset_equal_25` + слот 26 (пустой AUTO) | **409**, size 25, слот 26 **не записан** (нет silent prune) | **PASS** |
| 5 | `preset_equal_25` + slots 1..25 | **200**, size 25, slots 25 | **PASS** |
| 6 | mode-only (без preset/size/slots) | **200**, size/slots не изменились | **PASS** |
| 7 | commit-тест `mode_capacity_is_declared_only_by_size_or_preset` | проходит (в полном прогоне, §5) | **PASS** |

`declared_capacity` теперь берётся только из `size > 0`/пресета (`shell.rs:1010-1022`, `:1050`),
`slots.len` в объявленную ёмкость не входит; `normalize_loaded` действительно prune'ит пустые AUTO
за ёмкостью (`resources.rs:1221-1223`). P1-5 rr5 закрыт; regression на PATCH не найден.

## 3. Повторная проверка ранее закрытых P1 (rr3/rr4/rr5)

Все сценарии — scratch-пробы на real route handlers, кроме engine (GatewayManager + McuEngine).

| # | Сценарий | Факт (вывод проб) | Статус |
|---|---|---|---|
| 1 | PATCH `vks_grid_5` → `vks_grid_9`, 9 AUTO | **200**, size 9, slots 9 | **PASS** |
| 2 | PATCH `preset_default_13` → `vks_grid_43`, 43 AUTO | **200**, size 43, slots 43 | **PASS** |
| 3 | `edit_linked` 5→9 (scoped/COW) | **200**, size 9, slots 9 | **PASS** |
| 4 | contact-only legacy 9 (preset 9) → shrink `preset_equal_5` → rename | size **9**, slots 9; rename **200** | **PASS** |
| 5 | detach всех 9 → shrink на 5 → rename | detach_fail=0, size 9/slots 9; shrink **200** size 5/slots 5; rename **200** | **PASS** |
| 6 | detach одного p9 у legacy 9 (preset 5) → rename | **204**, size 8/slots 8; rename **200** | **PASS** |
| 7 | grid-4 + пустые AUTO 1..5 → rename | **200**, size 4, slots prune 5→4 | **PASS** |
| 8 | explicit junk: slots-only 1..9; preset9+pos10 (empty/occupied); duplicate pos9; pos0 | **409** (`outside layout`, `duplicate`, `must be >= 1`), запись не изменена | **PASS** |
| 9 | pos string / missing pos / non-object slot | **422** (`must be an integer`, `is required`, `items must be objects`) | **PASS** |
| 10 | seeded legacy `abonent_id` 9 (stale preset/size 5) через `put_configuration_document` + `sync_from_storage` | `size_after_sync=9`, slots 9; rename **200** | **PASS** |
| 11 | attach 1..9 / 10-й на seeded legacy 9 | `[200×9, 409]`, slots 9 | **PASS** |
| 12 | engine: 9 contact-only слотов + `preset_equal_9` | `assignments=9 geometry=9 slot_rects=9`, preset/mode корректны | **PASS** |
| 13 | full-save echo после load: grid-4 + AUTO 1..5 в storage → reload → PATCH эхом всей payload | после load size 4/slots 4; full-save **200**, size 4/slots 4 | **PASS** |

Committed-тесты `declared_preset_growth_is_accepted_and_validated`,
`legacy_empty_auto_slots_beyond_capacity_do_not_block_rename`,
`detached_empty_windows_do_not_poison_record`,
`contact_only_legacy_slots_keep_capacity_across_preset_shrink` и
`mode_capacity_is_declared_only_by_size_or_preset` входят в полный прогон и проходят.

## 4. Новые P2/P3 и проверка рисков

### 4.1 P2-1 — mode и PATCH по-разному выбирают приоритет `size`/`preset`

`declared_from_request` (`shell.rs:1010-1022`) сначала берёт `size`, потом пресет, а
`explicit_layout_capacity` (`resources.rs:436-448`), которым валидирует PATCH, — наоборот: сначала
пресет, потом `size`. При запросе с **обоими** полями поведение расходится (проверено):

```text
RR6 PRECEDENCE mode  preset25+size13+slots1..25  status=409 Conflict size=13 slots=0
RR6 PRECEDENCE patch preset25+size13+slots1..25  status=200 OK       size=25 slots=25
RR6 PRECEDENCE mode  preset13+size25+slots1..25  status=200 OK       size=13 slots=13
RR6 PRECEDENCE patch preset13+size25+slots1..25  status=409 Conflict size=13 slots=0
```

- `preset25 + size13 + slots1..25`: mode **отклоняет** легальный рост (409), PATCH принимает (200/25).
- `preset13 + size25 + slots1..25`: mode принимает **200** и молча вырезает слоты 14..25
  (`size=13 slots=13`), PATCH даёт **409**. Это тот же класс «200 + silent prune», что и P1-5,
  но требует одновременно `size > ёмкости пресета` и `preset` в одном запросе.

Достижимость UI: в вендорском бандле mode-payload собирается `Th`/`Fee`
(`client/pnm-frontend/deployed/pnm/chunks/index.bundle-2.2.1.66.js`): `manual` →
`{mode,preset,type,slots}` (без `size`), `auto` → `{mode,preset,type}`, `dynamic` →
`{…,size,autoscroll}` (без `slots`). То есть UI **не** присылает `size` вместе со `slots` в mode,
и оба расхождения из UI не воспроизводятся; это API-контракт/консистентность, поэтому **P2**, а не
P1. Но §4.6 (`docs/rules/BACKEND_RULES.md:202-203`) утверждает «Mode-эндпоинт использует те же
правила», что и PATCH, — это неверно при одновременно присланных `size`/`preset`.

**Fix:** переиспользовать `explicit_layout_capacity` (сделать `pub(super)`) на merged payload или
хотя бы поставить пресет перед `size` в `declared_from_request`; добавить committed-тесты
`mode_preset_growth_with_stale_size_is_200` и `mode_size_beyond_preset_with_slots_is_409`.

### 4.2 P2-2 — гигиена коммита: чужой функциональный hunk в T06/T07

Причина P0 — несвязанный codec-hunk (`shell.rs:565-617`), добавленный без упоминания в сообщении
и без тестов. Даже после починки сборки это отдельное функциональное изменение (T22), которое
не должно ехать в T06/T07-коммите: его нельзя ревьюить/откатывать независимо. Рекомендация —
вынести в отдельный коммит вместе с `vks-common`-частью.

### 4.3 Проверенные риски (без новых дефектов)

- **Load-time prune не теряет легальные окна.** Пустые AUTO за ёмкостью не рисуются движком
  (геометрия — только ячейки пресета) и по контракту §4.6 (`:190-191`, `:198-201`) являются
  мусором прежних пресетов/отвязки; `prune_empty_slots_beyond_capacity` удаляет их при загрузке и
  на запись. Легальных «окон впрок» за ёмкостью, которые движок бы использовал, нет. Остаётся
  информационный P3-5 (тихая потеря черновика), не блокер.
- **UI full-save echo теперь чист:** после reload легаси-записи с AUTO 1..5 за grid-4 в payload
  остаётся 4 слота, полный PATCH эхом — **200**, а не 409 (строка `RR6 ECHO full-save`).
- **§4.6 в остальном совпадает с кодом:** порядок резолвера (`grids → codian → preset_size →
  payload.size → slots.len`), `max(резолвер, max(position) занятых)`, 8 ключей
  `slot_holds_participant`, строгая проверка позиций из запроса и prune на load/write — сверены
  с `resources.rs:376-464`, `:1467-1476`, `:1536-1628`, `shell.rs:1010-1082`. Расходится только
  фраза о mode (P2-1).
- **Других регрессий admin-роутов нет:** полный `vks-admin --lib` на T06-реконструкции — 355
  committed-тестов, 0 failed (+9 проб = 364). Codec-роуты в pristine не компилируются (P0),
  поэтому их поведение в этом ревью не проверялось.

### 4.4 Остаточные P3

| # | Sev | Проблема | Файл:строки |
|---|---|---|---|
| P3-1 | P3 | `size`-only рост (без пресета) на preset-записи: 200, но size остаётся пресетным, лишние слоты вырезаются — тихий no-op (pre-existing rr5 P3-1, код не менялся) | `resources.rs:436-448`, `:1614-1627`; `shell.rs:1025-1050` |
| P3-2 | P3 | `position == 0` → 409 `Conflict` вместо 422 `Invalid` (pre-existing; подтверждено пробой) | `resources.rs:1563-1565` |
| P3-3 | P3 | `normalize_loaded` меняет только память; storage хранит stale `size`/пустые AUTO до первого commit (pre-existing, теперь включает и prune) | `resources.rs:1201-1224`; `state.rs:711`; `resources.rs:257-280` |
| P3-4 | P3 | Пробелы committed-тестов: mode-тест проверяет только статусы (нет проверки «запись не изменена», PATCH-эквивалентов и precedence); задание ожидает admin **356**, фактически committed **355** | `resource_tests.rs:2718-2764`; `:2600`, `:2629`, `:2664` |
| P3-5 | P3 | Load-time prune безвозвратно удаляет пустые AUTO за ёмкостью, если пользователь сохранял их как черновик; по документации это мусор, но миграционного предупреждения нет. Плюс двойной `recalc_layout_size` в `normalize_loaded` (стиль) | `resources.rs:453-464`, `:1221-1223` |

## 5. Гейты

### 5.1 Pristine `fff51a7` (как заявлено заданием)

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all --check` | **0** | без вывода |
| `cargo test -p vks-layout --lib --offline` | **0** | **54 passed**, 0 failed |
| `cargo clippy -p vks-layout -p vks-admin -p vks-server --all-targets --offline -- -D warnings` | **101** | **E0425** ×2 (`shell.rs:577`, `:605`) |
| `cargo clippy --workspace --all-features --all-targets --locked --offline -- -D warnings` | **101** | тот же E0425 в `vks-admin` |
| `cargo test -p vks-admin --lib --offline` | **101** | не компилируется (E0425) |
| `cargo test -p vks-server --lib --offline` | **101** | не компилируется (E0425 в зависимости `vks-admin`) |

### 5.2 T06-реконструкция: `fff51a7` + откат только codec-hunk'а

| Команда | Exit | Результат |
|---|---:|---|
| `cargo check -p vks-admin --lib --offline` | **0** | `Finished` (36.68 s) — других поломок в коммите нет |
| `cargo clippy -p vks-layout -p vks-admin -p vks-server --all-targets --offline -- -D warnings` | **0** | предупреждений нет (1 m 25 s) |
| `cargo clippy --workspace --all-features --all-targets --locked --offline -- -D warnings` | **0** | `Finished … in 58.44s` |
| `cargo test -p vks-admin --lib --offline` | **0** | **364 passed**, 0 failed (355 committed + 9 review-проб) |
| committed-тесты admin (`--list`, без проб) | 0 | **355** тестов, дублей имён нет; ожидание задания 356 — расхождение на 1 (P3-4) |
| 9 review-проб T06 | **0** | все 9 passed; выводы §2-4 |
| `cargo test -p vks-server --lib --offline -- --skip t06r6_engine` | **0** | **147 passed, 3 ignored**, 0 failed |
| engine-проба (scratch `vks-server`) | **0** | 1 passed; `assignments=9 geometry=9 slot_rects=9; preset=preset_equal_9 mode=manual` |
| `cargo fmt --all --check` (scratch) | 1 | diff только в добавленном review-коде проб; pristine `fff51a7` — **0** |

Примечание: первый полный прогон `vks-server` дал 146 passed / 1 failed —
`gateway_audio_bridges_g726_32_at_peer_dynamic_pt_with_8khz_clock` упал по таймауту
(`G.726 PCM must reach the 48 kHz MCU mix: Elapsed(())`). Изолированный повтор — ok, повторный
полный прогон — **147 passed, 3 ignored, 0 failed**. Это флейк таймингов аудио-теста, к T06 не
относится; в отчётности зафиксирован.

## 6. Гигиена

- Live-БД `vks_data.db`: sha256 `687b758f…caa81ce`, size 3248128, mtime 1793266711 — совпадает
  с базой rr5; за ревью не открывалась. Порты `:8080` (:3255206) и `:18082` (:3253281) — те же
  PIDs, сервер не стартовал. `backups/` (mtime 2026-10-29 07:32:22) не тронут.
- В live-репо мною записан только `docs/audit/2026-10-29/reviews/T06_rereview6.md`; прочие
  изменения worktree (`M crates/...`, `?? docs/...`, включая появившиеся во время ревью
  `directory_migrate*`) — других задач, не мои.
- `/tmp` не использовался. Pristine-копия `/dev/shm/t06r6`, scratch `/dev/shm/t06r6_scratch`,
  логи и `CARGO_TARGET_DIR` (`t06_rr6_target`, `t06_rr6_scratch_target`) удалены после сдачи;
  live-репо не собирался.

## 7. Счёт

**5.0 / 10, done: no.**

**+**
- P1-5 rr5 закрыт исполнением: slots-only 409 (пустые/занятые), preset-only growth 200/25,
  preset25 + слот 26 → 409 без записи слота; mode-only save не ломает запись.
- Все ранее закрытые P1 rr3/rr4 подтверждены на `fff51a7`-коде: 5→9, 13→43, `edit_linked`,
  contact-only, detach all/one + shrink + rename, grid-4+AUTO prune, seeded legacy, attach 9/10,
  MCU 9 assignments/9 geometry/9 slot_rects.
- §4.6 переписан и в основном точен; full-save echo легаси-записи теперь 200; load-time prune
  не теряет используемых окон; admin-регрессий нет (355 committed + 9 проб — 0 failed).
- На T06-реконструкции зелёные targeted и workspace clippy.

**−**
- **P0:** exact `fff51a7` не собирается из-за чужого codec-hunk'а; все гейты и acceptance на
  заявленном HEAD недостижимы. Это блокер сдачи, хотя и не дефект T06-логики.
- **P2-1:** mode при одновременно присланных `size`/`preset` расходится с PATCH (409 vs 200 и
  200+silent prune); §4.6 называет правила «теми же». UI это не присылает, но API-контракт
  остаётся неконсистентным.
- **P2-2:** несвязанное функциональное изменение в T06/T07-коммите без тестов (корень P0).
- **P3-4:** committed-тестов 355, а не 356; mode-тест не проверяет неизменность записи и
  PATCH-эквиваленты.

До done нужно: убрать/закоммитить codec-hunk и получить собираемый HEAD; закрыть P2-1
(единый `explicit_layout_capacity` для mode/PATCH + committed-тесты на stale size/precedence);
для 9.5+ — ещё P3-4 (неизменность записи и PATCH-эквиваленты в mode-тесте).

Ориентир: если устранить только P0 (сделать `fff51a7` собираемым, не меняя T06-логику), T06-часть
по совокупности evidence — около **9.0**; 9.5 блокируют P2-1 и P3-4. Текущие **5.0** — цена
несобираемого коммита, а не дефекта самой T06-логики.

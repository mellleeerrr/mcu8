# T06 — повторное ревью №7 (HEAD `f602f72`, проверка собираемости и P1-5/P2-1)

- **Репозиторий:** `/home/pro/Рабочий стол/mcu`, ветка `master`.
- **Проверяемый HEAD:** `f602f72` «fix(T09b): finalize on encoder group loss, replay finalize
  errors, surface tap writer errors» (родитель `a02daad` — T04b).
- **Предыдущие ревью:** [`T06_rereview5.md`](T06_rereview5.md) — 9.0, done no (P1-5: mode
  ёмкость из `slots.len`); [`T06_rereview6.md`](T06_rereview6.md) — 5.0, done no (P0: `fff51a7`
  не собирался из-за чужого codec-hunk'а; P2-1: precedence `size`/`preset` в mode).
- **Полигон:** `git archive f602f72 | tar -x -C /dev/shm/t06r7` (без `.git`/`target`), scratch-
  копия `/dev/shm/t06r7_scratch` для проб. Rust **1.94.0** (`rust-toolchain.toml`), все сборки
  `--offline`; `CARGO_TARGET_DIR=/home/pro/.cache/t06_rr7_target` (гейты) и
  `/home/pro/.cache/t06_rr7_scratch_target` (пробы); `/tmp` не использовался.
- **Live-среда:** `vks_data.db` не открывалась, SQLite-соединение не создавалось, сервер на
  `:8080`/`:18082` не запускался/не опрашивался, browser e2e не запускался. В live-репо записан
  только этот файл.

## Вердикт

**Verdict: 8.5 / 10 — done: no. 0 P0, 1 P1, 1 P2, 5 P3.**

Собираемость exact `f602f72` **подтверждена без шимов**: `cargo check -p vks-admin --lib`
= 0, admin lib = **357 passed**, server lib = **149 passed / 3 ignored**, layout = **54 passed**.
Все acceptance-сценарии T06 проходят **исполнением на реальных route-хендлерах** (15 scratch-
проб): P1-5 закрыт (slots-only 409, preset 200/25, preset+slot26 409 без записи), все ранее
закрытые P1 (rr3/rr4) остались закрытыми, full-save echo 200/slots 4, engine 9/9/9.

Но **обязательный clippy-гейт на exact `f602f72` падает** (P1, вне T06-логики): T09b-коммит
изменил тип `stopped_cache` на tuple и получил `clippy::type_complexity` в
`crates/vks-server/src/recorder.rs:136`. Targeted и workspace clippy — **exit 101**. Это блокер
сдачи HEAD в текущем виде. Плюс P2-1 из rr6 не исправлен: `f602f72` вообще не меняет T06-файлы
относительно `fff51a7`; mode по-прежнему берёт `size` раньше пресета, PATCH — пресет раньше
`size`, а §4.6 называет правила «теми же».

| Категория | Кол-во |
|---|---:|
| **P0** | **0** (buildability подтверждена) |
| **P1** | **1** (clippy-гейт на HEAD: T09b `recorder.rs:136`) |
| **P2** | **1** (precedence mode/PATCH + неточная фраза §4.6) |
| **P3** | **5** (pre-existing, включая два подтверждённых пробами) |

---

## 1. Собираемость `f602f72` — P0 rr6 закрыт

P0 из rr6 (E0425 `audio_unavailable_reason`/`video_unavailable_reason` в `shell.rs`) закрыт
коммитом `a02daad` (T04b), который добавил эти функции в `vks-common::codec_capabilities`.
Проверка **exact `f602f72`**, без откатов и без scratch-реконструкции:

```text
$ rustup run 1.94.0 cargo check -p vks-admin --lib --offline
    Checking vks-admin v0.1.0 (/dev/shm/t06r7/crates/vks-admin)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 47.57s
EXIT=0
```

`cargo test -p vks-admin --lib` и `cargo test -p vks-server --lib` на этом же дереве
компилируются и проходят (см. §6). Никаких шимов/подмен файлов не применялось; `/dev/shm/t06r7`
— чистый `git archive f602f72`.

## 2. P1 (новый, вне T06): clippy-гейт на exact HEAD не проходит

### 2.1 Факт

```text
$ rustup run 1.94.0 cargo clippy -p vks-layout -p vks-admin -p vks-server \
    --all-targets --offline -- -D warnings
error: very complex type used. Consider factoring parts into `type` definitions
   --> crates/vks-server/src/recorder.rs:136:20
    |
136 |     stopped_cache: Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>,
    |                    ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
    |
    = help: for further information visit https://rust-lang.github.io/rust-clippy/rust-1.94.0/index.html#type_complexity
    = note: `-D clippy::type-complexity` implied by `-D warnings`
error: could not compile `vks-server` (lib) due to 1 previous error
error: could not compile `vks-server` (lib test) due to 1 previous error
EXIT=101
```

Точно та же единственная ошибка в обязательном workspace-гейте:

```text
$ rustup run 1.94.0 cargo clippy --workspace --all-features --all-targets --locked \
    --offline -- -D warnings
...
error: very complex type used. Consider factoring parts into `type` definitions
   --> crates/vks-server/src/recorder.rs:136:20
...
EXIT=101
```

Других clippy-ошибок нет: `vks-layout` и `vks-admin` в targeted-прогоне компилируются чисто,
падение локализовано в `vks-server` (lib и lib test); workspace-прогон падает на том же месте.

### 2.2 Причина и атрибуция

Тип введён именно `f602f72` (T09b):

```text
$ git show f602f72 -- crates/vks-server/src/recorder.rs
-    stopped_cache: Arc<Mutex<HashMap<RoomId, RecordingMeta>>>,
+    /// Недавно остановленные записи: метаданные + ошибка finalize (если была)
+    /// для честного повторного DELETE (T09b review A P2-1).
+    stopped_cache: Arc<Mutex<HashMap<RoomId, (RecordingMeta, Option<String>)>>>,
```

До коммита тип был простым (`HashMap<RoomId, RecordingMeta>`) и `type_complexity` не
срабатывал. Это **не T06-дефект** (recorder/T09b), но это регрессия exact `f602f72` и
обязательный гейт задания, поэтому P1.

### 2.3 Влияние и фикс

- Acceptance T06 по поведению выполнено, но HEAD нельзя сдавать: гейт `-D warnings` красный.
- Фикс тривиален и локальный: `type StoppedRecording = (RecordingMeta, Option<String>);` и
  `Arc<Mutex<HashMap<RoomId, StoppedRecording>>>` (либо точечный
  `#[allow(clippy::type_complexity)]` на поле с комментарием). После фикса — повторить оба
  clippy-прогона; T06-код правки не требует.
- Если Lead готов принять T06-логику отдельно от T09b, это надо оформить явно: на exact
  `f602f72` гейт красный.

## 3. P1-5 и ранее закрытые P1 — подтверждены исполнением

Все пробы — scratch-тесты на реальных хендлерах `switch_video_layout_mode`, `edit_global`,
`edit_linked`, `attach_layout_abonent`, `detach_layout_abonent`; запись создаётся через
`POST /api/v3/video_layouts`, привязывается к сессии. 15 scratch-проб, **15 passed, 0 failed**;
фрагмент вывода (`--nocapture`):

```text
RR7 PRECEDENCE mode preset25+size13 -> 409 size=13 slots=0
RR7 PRECEDENCE patch preset25+size13 -> 200 size=25 slots=25
RR7 PRECEDENCE mode preset13+size25 -> 200 size=13 slots=13
RR7 PRECEDENCE patch preset13+size25 -> 409 size=13 slots=0
RR7 SIZE-ONLY status=200 size=13 slots=0
RR7 NORMALIZE memory_slots=4 stored_slots=5
RR7 ENGINE assignments=9 geometry=9 slot_rects=9 preset=preset_equal_9 mode=manual
```

### 3.1 P1-5 из rr5 (mode declared capacity)

| # | Сценарий (реальный mode-хендлер) | Факт | Статус |
|---|---|---|---|
| 1 | slots-only 1..25, пустые AUTO, база 13 | **409**, запись не изменена: size 13/slots 0 | **PASS** |
| 2 | slots-only 1..25, база 13 с 13 занятыми окнами | **409**, size 13/slots 13 | **PASS** |
| 3 | `preset_equal_25`, без slots | **200**, size 25 | **PASS** |
| 4 | `preset_equal_25` + слот 26 | **409**, size 25, слот 26 **не записан** (slots 0) | **PASS** |
| 5 | mode-only save (без preset/size/slots) | **200**, size/slots/mode не сломаны | **PASS** |
| 6 | PATCH-эквиваленты: slots-only 25 → **409**; preset 25 → **200/25**; preset25+slot26 → **409** без записи | как указано | **PASS** |

`declared_from_request` (`shell.rs:1010-1022`) действительно больше не использует `slots.len`;
`normalize_loaded` prune'ит пустые AUTO за ёмкостью (`resources.rs:1221-1223`). Regression на
PATCH нет.

### 3.2 Ранее закрытые P1 (rr3/rr4)

| # | Сценарий | Факт | Статус |
|---|---|---|---|
| 1 | PATCH `vks_grid_5` → `vks_grid_9`, 9 занятых | **200**, size 9, slots 9 | **PASS** |
| 2 | PATCH `preset_default_13` → `vks_grid_43`, 43 занятых | **200**, size 43, slots 43 | **PASS** |
| 3 | `edit_linked` 5→9 (scoped/COW) | **200**, size 9, slots 9 | **PASS** |
| 4 | contact-only legacy 9 → shrink `preset_equal_5` → rename | size **9**, slots 9; rename **200** | **PASS** |
| 5 | detach всех 9 → shrink на 5 → rename | detach 9×**204**; shrink **200** size 5/slots 5; rename **200** | **PASS** |
| 6 | detach одного p9 у legacy 9 (preset 5) → rename | **204**, size 8/slots 8; rename **200** | **PASS** |
| 7 | grid-4 + пустые AUTO 1..5 → rename | **200**, size 4, slots prune 5→4 | **PASS** |
| 8 | explicit junk: slots-only 1..9; preset9+pos10 (empty/occupied); duplicate pos9; pos0 | **409** (`outside layout`, `duplicate`, `must be >= 1`), запись не изменена | **PASS** |
| 9 | pos string / missing pos / non-object slot | **422** (`must be an integer`, `is required`, `items must be objects`), без мутации | **PASS** |
| 10 | seeded legacy `abonent_id` 9 (stale preset/size 5) через storage + `sync_from_storage` | после load size 9/slots 9; rename **200** | **PASS** |
| 11 | attach 1..9 / 10-й на seeded legacy 9 | `[200×9, 409]`, slots 9 | **PASS** |
| 12 | engine: `GatewayManager::apply_layout` (JSON) + `McuEngine`, 9 слотов + `preset_equal_9` | `assignments=9 geometry=9 slot_rects=9`, preset/mode корректны | **PASS** |
| 13 | full-save UI echo после load: grid-4 + AUTO 1..5 в storage → reload → PATCH эхом всей payload | после load size 4/slots 4; full-save **200**, size 4/slots 4 | **PASS** |

Committed-тесты `declared_preset_growth_is_accepted_and_validated`,
`legacy_empty_auto_slots_beyond_capacity_do_not_block_rename`,
`detached_empty_windows_do_not_poison_record`,
`contact_only_legacy_slots_keep_capacity_across_preset_shrink`,
`mode_capacity_is_declared_only_by_size_or_preset` входят в полный прогон и проходят (§6).

## 4. P2-1 — precedence `size`/`preset`: в `f602f72` выравнивания НЕТ

### 4.1 Коммит не содержит T06-изменений

```text
$ git diff --name-only fff51a7 f602f72 -- \
    crates/vks-admin/src/api_v3/shell.rs \
    crates/vks-admin/src/api_v3/resources.rs \
    crates/vks-admin/src/api_v3/resource_tests.rs \
    crates/vks-layout docs/rules/BACKEND_RULES.md docs/architecture/LAYOUT_ENGINE.md
(пусто)
```

`git show --name-only f602f72` — только `vks-recorder/src/container.rs`,
`vks-server/src/{recorder,recorder_tap}.rs`, `vks-server/tests/recorder_tap.rs` и четыре
review-документа. То есть Lead-план «выровнять mode по `explicit_layout_capacity`» **не
выполнен**: в `f602f72` остался тот же `declared_from_request`, который берёт `size` раньше
пресета (`shell.rs:1010-1022`), тогда как PATCH берёт пресет раньше `size`
(`explicit_layout_capacity`, `resources.rs:436-446`).

### 4.2 Фактическое поведение (исполнение)

| Payload с обоими полями | mode | PATCH |
|---|---|---|
| `preset_equal_25` + `size:13` + slots 1..25 | **409**, size 13/slots 0 | **200**, size 25/slots 25 |
| `preset_default_13` + `size:25` + slots 1..25 | **200**, size 13/slots 13 (тихий prune) | **409**, size 13/slots 0 |

- В первом случае mode **отклоняет легальный рост** (берёт stale `size=13`), PATCH принимает.
- Во втором mode **принимает 200 и молча вырезает** окна 14..25; PATCH даёт 409. Это тот же
  класс «200 + silent prune», что P1-5, но требует одновременно `size` и `preset`.
- Достижимость UI не перепроверялась в этом ревью; по анализу rr6 вендорский бандл шлёт
  `manual` с `slots`, `dynamic` с `size` и не шлёт `size`+`slots` одновременно, поэтому это
  API-контракт/консистентность — **P2**, не P1.

### 4.3 Committed-тесты не фиксируют выравнивание

`mode_capacity_is_declared_only_by_size_or_preset` (`resource_tests.rs:2721-2764`) проверяет
только статусы slots-only/preset/slot26; PATCH-эквивалентов, неизменности записи и
precedence-кейсов `size+preset` в нём нет. Ни один committed-тест не подаёт в mode оба поля.
Поэтому даже после предполагаемого выравнивания нужен новый тест; сейчас поведение
не зафиксировано.

### 4.4 Документация

`docs/rules/BACKEND_RULES.md:202-203` утверждает: «Mode-эндпоинт использует те же правила
(`max(сохранённая, size/пресет)`); `slots` без `size`/пресета ёмкость не расширяют». Вторая
часть верна (P1-5), первая — нет при одновременно присланных `size`/`preset`: mode использует
`size`-first, PATCH — `preset`-first, результаты расходятся (409 vs 200 и 200+prune vs 409).
Это часть P2-1; после выравнивания фразу можно оставить, до тех пор — она неточна.

**Fix (не блокирует T06-логику, но нужен для 9.5):** переиспользовать
`explicit_layout_capacity` в mode (сделать `pub(super)`) либо поменять порядок в
`declared_from_request` на preset-first; добавить committed-тесты
`mode_preset_growth_with_stale_size_is_200` и `mode_size_beyond_preset_with_slots_is_409`
(с проверкой неизменности записи на 409).

## 5. P3 (pre-existing/не блокирующие)

| # | Sev | Проблема | Файл:строки | Подтверждение |
|---|---|---|---|---|
| P3-1 | P3 | `size`-only рост (без пресета) на preset-записи: **200**, но `size` остаётся пресетным, лишние слоты вырезаются — тихий no-op | `resources.rs:436-446`, `:1614-1627`; `shell.rs:1025-1050` | проба: `RR7 SIZE-ONLY status=200 size=13 slots=0` |
| P3-2 | P3 | `position == 0` → **409 Conflict** вместо 422 Invalid | `resources.rs:1563-1565` | проба junk-кейса (409) |
| P3-3 | P3 | `normalize_loaded` чинит только память; storage хранит stale `size`/пустые AUTO до первого commit | `resources.rs:1201-1224`; `state.rs:711`; `resources.rs:257-280` | проба: memory 4, stored slots 5 |
| P3-4 | P3 | Пробелы committed-тестов: mode-тест без PATCH-эквивалентов, проверки неизменности записи и precedence; admin-тестов **357** (на 2 больше, чем 355 в rr6 — за счёт `a02daad`, не T06) | `resource_tests.rs:2721-2764` | тесты/прогон |
| P3-5 | P3 | Load-time prune безвозвратно удаляет пустые AUTO за ёмкостью (черновик), миграционного предупреждения нет; двойной `recalc_layout_size` в `normalize_loaded` | `resources.rs:453-464`, `:1221-1223` | код + full-save проба |

## 6. Гейты (exact `f602f72`, Rust 1.94.0, `--offline`)

| Команда | Exit | Результат |
|---|---:|---|
| `cargo fmt --all --check` | **0** | без вывода |
| `cargo check -p vks-admin --lib` | **0** | `Finished` (47.57 s) |
| `cargo test -p vks-layout --lib` | **0** | **54 passed**, 0 failed |
| `cargo test -p vks-admin --lib` | **0** | **357 passed**, 0 failed, 0 ignored (33.80 s) |
| admin + 15 review-проб (scratch) | **0** | **372 tests**: 357 committed + 15 probes, 0 failed |
| `cargo test -p vks-server --lib` | **0** | **149 passed, 3 ignored**, 0 failed (20.06 s) |
| engine-проба (scratch `vks-server/tests`) | **0** | 1 passed; `assignments=9 geometry=9 slot_rects=9` |
| `cargo clippy -p vks-layout -p vks-admin -p vks-server --all-targets -- -D warnings` | **101** | **P1**: `recorder.rs:136` `type_complexity` (единственная ошибка) |
| `cargo clippy --workspace --all-features --all-targets --locked -- -D warnings` | **101** | та же единственная ошибка `recorder.rs:136` |

Примечание: admin-тестов стало 357, а не 355/356 из rr6 — это следствие `a02daad` (T04b),
не T06; committed T06-тесты не менялись. Во всех прогонах `--offline`; `/tmp` не использовался.

## 7. Документация (truth check)

- **`BACKEND_RULES.md` §4.6** (`:179-206`): порядок резолвера
  `grids → codian → preset_size → payload.size → slots.len` совпадает с
  `derived_layout_size` (`resources.rs:467-476`); `max(резолвер, max(position) занятых)` и
  8 ключей `slot_holds_participant` — с `layout_size`/`slot_holds_participant`
  (`resources.rs:376-411`); строгая проверка позиций из запроса, prune на load/write — с
  `resources.rs:1536-1601`, `:449-464`, `:1221-1223`. **Расходится только фраза о mode**
  (`:202`): «те же правила» неверно при одновременных `size`/`preset` — часть P2-1.
- **`LAYOUT_ENGINE.md` §12/§13** (`:239-269`): `git diff fff51a7 f602f72` по файлу пуст —
  разделы не менялись; §12 (S6 3×2, 83.28 %) и §13 (`preset_default_13` → 400) не касаются
  T06-ёмкости и не переоценивают поведение. Overclaim не найден.
- Других утверждений, противоречащих исполнению, в §4.6/§12/§13 нет.

## 8. Гигиена

- Live-БД `vks_data.db`: sha256 `687b758f6e3c6dd31021109c05fa626f19dfb3f2e1fd6c69d3f3f1c90caa81ce`,
  size 3248128, mtime 1793266711 — совпадает с rr6; за ревью не открывалась.
- Порты `:8080` (PID 3255206) и `:18082` (PID 3253281) — те же PIDs, сервер не стартовал;
  browser e2e не запускался.
- `backups/` (mtime 2026-10-29 07:32:22) не тронут.
- В live-репо записан только `docs/audit/2026-10-29/reviews/T06_rereview7.md`; прочие
  изменения worktree (`M crates/...`, `?? docs/...`, включая `directory_migrate*`) — других
  задач, не мои.
- `/tmp` не использовался. Pristine `/dev/shm/t06r7`, scratch `/dev/shm/t06r7_scratch`, логи
  `/dev/shm/rr7_*.log` и `CARGO_TARGET_DIR` (`t06_rr7_target`, `t06_rr7_scratch_target`)
  удалены после сдачи; live-репо не собирался.

## 9. Счёт

**8.5 / 10, done: no.**

**+**
- Buildability exact `f602f72` подтверждена без шимов: check = 0, admin 357, server 149/3
  ignored, layout 54.
- P1-5 закрыт исполнением: mode slots-only 409 (пустые/занятые), preset-only 200/25,
  preset25+slot26 409 без записи, mode-only 200; PATCH-эквиваленты не изменились.
- Все ранее закрытые P1 rr3/rr4 подтверждены на exact HEAD: 5→9, 13→43, `edit_linked`,
  contact-only, detach all/one, grid-4 prune, junk 409/422 без мутации, seeded legacy,
  attach 9/10, engine 9/9/9.
- Full-save echo легаси-записи после load-time prune — 200/slots 4; load-time prune не теряет
  занятые окна; §4.6 в остальном точен.

**−**
- **P1:** clippy-гейт на exact `f602f72` красный из-за T09b-регрессии `recorder.rs:136`
  (`type_complexity`); targeted и workspace clippy = 101. Блокер сдачи HEAD, хотя и не
  T06-логики.
- **P2-1:** mode/PATCH по-прежнему расходятся при одновременных `size`/`preset`
  (409 vs 200; 200+silent prune vs 409); `f602f72` не содержит выравнивания, committed-тесты
  его не фиксируют, §4.6:202 называет правила «теми же». UI это не присылает — не P1, но
  API-контракт остаётся неконсистентным и блокирует 9.5.
- **P3-1/P3-2/P3-3/P3-4/P3-5:** pre-existing (size-only no-op, pos0 409, memory-only
  normalize, test gaps, prune draft/double recalc).

До done нужно: починить `stopped_cache` тип (type alias или allow) и получить зелёные оба
clippy-гейта. Для 9.5 — ещё выровнять mode по `explicit_layout_capacity` (preset-first) и
добавить committed-тесты precedence/неизменности записи; P3-4 закрывается теми же тестами.
Ориентир: по одной T06-логике это ~9.0; текущие **8.5** — цена красного gate в exact HEAD.

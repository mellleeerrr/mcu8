# T1 — Регрессионный анализ фикса «Общей» раскладки (session-scope default)

Дата: 2026-10-28. Ветка WIP от `543bc7b`. Фикс затрагивает три слоя:
backend (`crates/vks-admin/src/api_v3/resources.rs`, `shell.rs`), project-слой UI
(`client/pnm-frontend/deployed/pnm/codian-layouts.js`) и вендорский бандл
(`scripts/patch-pnm-layout-crash.py` + `MANIFEST.sha256`).

## Инвариант

Для любого непустого `*:video_layouts` scope в session-ответе UI есть **ровно одна**
раскладка с `payload.type == "default"`; `payload.type` существующей раскладки
иммутабелен; при COW «Общая» клонируется с `type=default`. Тогда
`sessionLayoutListByType.default` всегда определён и `hasIndividualLayout()` /
`SessionBroadcastPopup.created()` не падают.

## Поведение до/после — матрица

| Сценарий | До | После | Почему не регресс |
|---|---|---|---|
| PATCH «Общей» из редактора (UI шлёт `type=individual` из-за project-слоя) | COW-клон `type=individual`; session-scope без default → крэш | project-слой больше не форсит тип; backend сохраняет `type=default`, клон остаётся «Общей» | Редактор и раньше ожидал default (вендор шлёт `type=default`); теперь оба источника согласованы |
| `POST /api/v3/ui/video_layouts/mode` c `payload.type=individual` | писал `type` из body | `type` игнорируется, `mode/preset/size/slots` применяются | mode-эндпоинт меняет только режим/схему; `type` не входит в его контракт (см. `shell.rs` комментарий) |
| Создание layout без `type` (`POST /api/v3/ui/video_layouts`) | canonical payload → «Общая» с чужим preset | `payload.type=individual`, кроме точного canonical seed (`{}`) | UI всегда шлёт type; seed-путь (создание «Общей» перед attach) сохранён и по-прежнему один на документ |
| `payload.type` вне `{default,individual}` | принимался как есть | 422 `payload.type must be "default" or "individual"` | невалидных клиентов не было (кроме тестов); контракт ужесточён осознанно |
| Сессия с `[canonical default, individual-клон]` (старый inherit + COW) | после рестарта 2 «Общих» или 0 | при загрузке alias-доказательство перетипирует клон в default; лишние default-ссылки отвязываются, активная остаётся | ремонт идемпотентен и детерминирован: не зависит от порядка рестартов (session-first/server-first — один результат) |
| Повторный старт сессии после правки «Общей» | inherit добавлял pristine canonical рядом с клоном | clone-aware inherit: id, представленный COW-клоном, не наследуется повторно | конференция сохраняет pristine canonical для других сессий; в сессии одна «Общая» |
| «Общая», отредактированная пользователем (имя «Codian 07 — 13 окон») | после ремонта осталась бы individual-only | alias-доказательство возвращает `type=default`; имя в БД сохраняется, UI скрывает его computed'ом для default-строк | выбран вариант A (retype по provenance, см. 01_crash_forensics.md §разногласия); engine применяет ту же геометрию, визуально ничего не меняется |
| Пустой scope (0 раскладок) | `repair_scope_defaults` его не трогает | так же; крэш закрыт JS-guard'ом | пустой scope = комната без раскладок (stale-ссылки вычищаются `links.retain`), создавать записи при загрузке нельзя — иначе мусор на каждую мёртвую комнату |
| `list_linked` (скрытие canonical конференции из session-ответа) | как было | как было | не трогали: это вендорская семантика «сессия показывает свои раскладки»; после фикса скрывать больше нечего |
| UI-сохранение конструктора | `editValueField('type','individual')` в `restoreIdentity`/`onVendorModeClick` | удалено; store `type` — отдельный ref, инициализируется из payload | вендорский store сам пишет `type` из загруженного payload; override ломал его |

## Проверки (фактические, 18:00–18:12 MSK)

- `cargo fmt --all -- --check` — ok.
- `cargo clippy -p vks-admin --locked --all-targets -- -D warnings` — ok.
- `cargo test -p vks-admin --lib` — **304 passed, 0 failed** (в т.ч. 7 `t00_*`):
  `t00_layout_type_is_immutable_and_session_keeps_default_after_edit`,
  `t00_normalize_loaded_restores_default_from_alias_evidence`,
  `t00_normalize_loaded_dedupes_two_defaults_keeping_active`,
  `t00_normalize_loaded_prefers_alias_clone_over_pristine_default`,
  `t00_session_restart_does_not_add_second_default_after_edit`,
  `t00_layout_type_is_validated_on_create_and_repaired_on_load`,
  `t00_create_without_type_is_individual_except_canonical_seed`.
- `cargo build --release --locked -p vks-server` — ok; сервис перезапущен
  (PID 1814143, старт 18:00:39), в логе ровно один ремонт:
  `WARN resources: restored layout type=default from COW alias evidence
  scope=sessions:49b599d7…` — ожидаемо (второй битый scope `cac8ed13…` уже не существует).
- Живой API после рестарта:
  - `GET /api/v3/ui/sessions/49b599d7…/video_layouts` → `[2caea6d1:default]`
    (было `[2caea6d1:individual]`, count=1);
  - `GET /api/v3/ui/sessions/1e0337c6…/video_layouts` → `[29c4da67:default]`
    (было два default: `29c4da67`, `8a75f3e4`);
  - `PATCH …/video_layouts/<default>` c `{payload:{type:individual,mode:manual,preset:vks_pin_l_13}}`
    → 200, ответ `type=default`, scope остаётся `1 × default` (COW-клон default);
  - `POST /api/v3/ui/video_layouts/mode` c `payload.type=individual` → 200, `type=default`;
  - `POST /api/v3/ui/video_layouts` без type → 201 `individual`; с `payload:{}` → 201 `default`;
  - `payload.type=foo` → 422.
- Playwright (см. 04_e2e_repro.md): `--expect-clean` — 0 крэшей, immutable=true;
  `--expect-clean --guard-only` — 0 крэшей при «битом» списке; `--guard-only --legacy-bundle`
  — исходный `TypeError … reading 'some'` воспроизведён (тест чувствителен).
- проверка манифеста — 214/214 ok (патч-скрипт регенерирует его только при реальном
  изменении бандлов; повторный прогон `--check` — «БЕЗ ИЗМЕНЕНИЙ»). Команда запускается
  из каталога рантайма, записи манифеста относительны `deployed/pnm`:
  `( cd client/pnm-frontend/deployed/pnm && sha256sum -c ../../MANIFEST.sha256 )`.

## Остаточные риски / follow-up (не блокируют T1)

1. **Fork-on-type-change (P1).** Смена типа существующей раскладки через старый кэш
   (PATCH c `type=individual` по default) больше не портит данные, но и не создаёт
   отдельную individual-раскладку: пользователь, осознанно менявший тип, ожидал форк.
   Решение: `PATCH` default + `type=individual` → создать новый individual-клон и оставить
   «Общую» (≈8 строк) — отдельная задача после T1.
2. **Имя отредактированной «Общей».** При retype имя «Codian 07 — 13 окон» остаётся в
   `display_name`, но UI для default-строк его не показывает (вендорский computed). Это
   следствие варианта A; при желании — отдельное поле/бейдж, не критично.
3. **Пустой scope.** Ремонт не создаёт раскладку; крэш закрыт JS-guard'ом. Если появится
   write-путь, создающий пустой session-scope, нужен серверный инвариант «непустой при
   старте сессии» (сейчас `inherit_and_ensure_default_layout` это делает).
4. **`previews` 501.** `POST /sessions/{id}/previews/{pid}` — реальная вендорская фича,
   отвечает 501 (P2, не причина крэша).
5. **Firefox.** Отдельного прогона нет; guard и код одинаковы, риск низкий (см. 04).

---

# Часть B — полный регрессионный разбор `543bc7b..645120d` (агент 02)

**Снимок:** HEAD `645120d` (18:05:17 MSK). Ревьюируемые файлы после коммита не менялись:
`codian-layouts.js` `73676575…`, `resources.rs` `09e7698b…`, `grids.rs` `7811e25a…` (сверено 18:20).
Сервис перезапущен 18:00 (release, PID 1814143), т.е. живой API — уже пост-фикс. После коммита другие
агенты продолжили правки в `gateways.rs`/`lib.rs`/`network_integration.rs`/`repro_session_default_layout.js`
(в этот разбор не входят). Дельта коммита: 89 файлов, +26467/−975. Часть A (T1) отвечает за
инвариант «Общей»; часть B — регрессии остального коммита (фокус: цепочка UI↔calculate↔apply,
серверные инварианты, дубли, тесты). Все измерения — по коду/фикстурам; живой API только чтение.

## 1. TL;DR и оценка

Крэш и потеря «Общей» **закрыты по существу** (T1 подтвердил e2e: `--expect-clean`, `--guard-only`,
`--legacy-bundle`; `payload.type` иммутабелен, guard `(…||[])` в обоих бандлах). Но в этой же дельте
появились три системные проблемы, которых в `543bc7b` не было:

1. **P1. Клиент перестал спрашивать сервер про геометрию.** `codian-layouts.js` теперь сам синтезирует
   ответ `POST /video_layouts/calculate` для `vks_*`/`codian_*`. На срезе 543bc7b вендорский редактор и
   карточки конструктора получали ответ **сервера** (`loadConstructor` делал 6 calculate-запросов).
   Сейчас превью считает JS. Для 36 из 100 сеток JS и Rust дают разные раскладки; худший случай
   `vks_grid_50` — 50.0 % чёрного у сервера против 21.9 % у JS, `vks_grid_43` — 46.9 % против 12.5 %.
   Т.е. пользователь видит одно, а MCU применяет другое (и это не косметика: разница — число строк).
2. **P2. Новые таблицы `prominent_config`/`dual_config` нарушают 16:9, а тест, который это ловил, ослаблен.**
   Шапка `grids.rs:6–7` обещает «Все окна сохраняют пропорцию 16:9»; новые таблицы дают ячейки 1.42–1.62
   (pin 11–12: 768×540 → 20–24 % чёрного; `vks_dual_2`: 640×720 → 50 %), композитор их letterbox-ит
   (`compositor.rs:108`). Тест `pin_отдаёт_крупному_больше_половины_а_миниатюры_16_9`
   (`tests/grids.rs:114`) сохранил имя и doc-комментарий, но проверку `|ratio−16/9|<0.06` из него удалили.
3. **P2. `repair_scope_defaults` отключает собственный retype ровно в массовом сценарии.**
   `target_is_local` (`resources.rs:734`) ложен, когда COW-клон «Общей» виден и в конференции, и в сессии
   (а `cow_replace` mirror-ит сессию, `:648–660`). Тогда шаг 1 не перетипирует клон, шаг 2 добавляет
   pristine-канон и `keep` выбирает его, а не активный клон → после рестарта отредактированная «Общая»
   исчезает из scope. Комментарий `:728–730` при этом утверждает «это делаем всегда».

Плюс: `inherit_and_ensure_default_layout` (`:1222–1236`) не переносит смену active с конференции на
остановленную сессию; главный e2e не герметичен (падает на текущем стенде) и проверяет JS-геометрию
стора, а не отрендеренное превью; в коммит попали заведомо нерабочие challenger-тесты; манифест
не воспроизводится (`sorted(rglob)` вместо `sort -z`).

**Оценка дельты: 5/10.** Серверная половина (иммутабельность типа, `ensure_slot_roles`, ремонт, 5
`t00_*`-тестов и e2e-репро) — 7–8/10. UI-половина — 3–4/10: ~420 строк дублирующей геометрии,
расхождение с сервером и ослабленные тесты. Медиа/шлюзы (engine/farm/gateway_video/uas/rtcp) глубоко
не ревьюились — по ним только спот-чеки (§3).

## 2. Регрессии и баги

| # | Sev | Где | Доказательство | Минимальный фикс | LOC |
|---|-----|-----|----------------|------------------|-----|
| R-01 | **P1** | `codian-layouts.js:715`, `:1690–1730`, `:1759–1830` | `var cols = Math.ceil(Math.sqrt(count*(16/9)/(canvasW/canvasH)));` vs `grids.rs:271–279` `deviation=(aspect−16/9).abs()*100; waste=(cols*rows−count)*20` → 36/100 сеток расходятся: `43,44,45,50–54,57–60,65–70,73–77,82–88,91–96`. `vks_grid_43`: JS 7×7 (183×103), Rust 9×5 (142×144), чёрного 12.5 % vs 46.9 %; `vks_grid_50`: 8×7 (160×103) vs 10×5 (128×144), 21.9 % vs 50.0 %. На 543bc7b расхождения не было: вендор получал серверный ответ (`loadConstructor`, 6 запросов), перехвата не было. | Удалить синтез и перехват (вернуть `loadConstructor`), либо портировать `gallery_cost` в JS дословно **и** добавить parity-тест N=1..100×6 режимов. **Статус: закрыто в T2** — движок и оба перехвата удалены (−430 строк), клиент рисует иконки из `calculate`; `tests/e2e/layout_geometry_parity.js` + `tests/e2e/layout_constructor_ui.js` фиксируют серверный источник. | −420/+48 (сделано) |
| R-02 | P2 | `codian-layouts.js:623`, `:1450`, `:248`, `:1183`, `:1030`, `:1723` | `/^vks_[a-z0-9_]+_(\d+)$/` и захват `([a-z0-9_]+)` принимают `vks_grid_g12_m24_25`; `buildSymmetricLayout` не имеет ветки для `grid_g12_m24` → `var boxes` (hoisted) `undefined` → `computeSlots` → `undefined` → `raw.length` TypeError в `formatCalculatedSlots`; `setSlots(preset, undefined)` пишет undefined в стор. Сервер опции поддерживает (`grids.rs:9–11`). | Нормализовать опции: `arch = arch.replace(/_g\d+(_m\d+)?$/, '')` + переносить `g/m` в `GridOptions`; тест на `vks_grid_g12_m24_25`. **Уточнение симптома:** локальный `computeSlots` на опциях возвращал `[]` (не TypeError), отсюда пустой стор/вечный прелоадер; TypeError был в `formatCalculatedSlots` из-за `raw.length` при другом входе. **Статус: закрыто в T2** — локального парсера нет, канонические опции считает сервер, неканонические id UI не создаёт. | 10 (сделано) |
| R-03 | P2 | `resources.rs:734`, `:739`, `:746–756` | `let target_is_local = resources.links.iter().all(|(scope, ids)| scope == &key \|\| !ids.contains(target));` + `cow_replace` mirror (`:648–660`) → при живой сессии клон в обоих scope → retype пропущен → `defaults` = [pristine] → `ids.retain(... keep)` выкидывает клон, `keep` = pristine (active-клон не в defaults). Комментарий `:728–730`: «Это делаем всегда». **Статус: закрыто `f8f3f91`** — repair больше не смотрит на `target_is_local`, а ограничивает retype комнатой (`scope.split(':').nth(1) == room`): same-room mirror ретипается, cross-room чужой клон сознательно нет. Это не противоречит Part A (детерминизм порядка рестартов): Part A — про retype по alias-доказательству, R-03 — про то, какие клоны вообще считаются «своими» для комнаты. | Убрать `target_is_local` (или ограничить только случаем «target — default в другом scope»); тест: конференция+живая сессия, PATCH «Общей», reload → ровно один default и это клон. | −3 |
| R-04 | P2 | `resources.rs:1216–1236` | `if let Some(id) = session_default { ...; return; }` — возврат до учёта `conference_active`; вызов только при старте (`sessions.rs:448`), `stop_session` не чистит `active_layouts`/links. Смена раскладки на конференции при остановленной сессии не доедет до следующего старта. | Если сессия не была мостом/пользователь не менял active — `set_active(conference_active)`; зафиксировать политику и покрыть тестом «стоп→выбор на конф→старт». | 8 |
| R-05 | P2 | `tests/e2e/test_layout_constructor.js:77`, `:345–380`, `:377` (файл удалён в T2) | На текущем стенде падает до ассертов: `locator('.conference-layouts-item')` timeout 30 s (у первой конференции нет раскладок; `addLayoutBtn.count()==0`). Оракул — `layout-editor.modifiedValue.slots` (та же JS-геометрия), а не DOM/`calculate`; `:377` закрепляет `256×180` (1.42) как «миниатюры 16:9». | Своя комната как в `repro_session_default_layout.js`; проверять bounding-box отрендеренных слотов против ответа сервера. | 60 |
| R-06 | P2 | `tests/e2e/test_challenger*.js` (удалены в T2) | Скрипт падает `TypeError … reading '0'` (`function computeBalancedGrid` в JS нет); остальные ждут 15 карточек, в UI 6 (`CONSTRUCTOR_MODES`, e2e Step 2 ждёт 6). Файлы теперь в коммите, но ни один runner их не зовёт (03: H-08). | Удалить или переписать под 6 режимов и вписать в npm-скрипты. **Статус: сделано в T2** — 6 устаревших файлов удалены, вместо них `tests/e2e/layout_geometry_parity.js` (серверный контракт calculate для 6 режимов × 10 N + option-пресеты + статический guard «локального движка нет»). | −1200 |
| R-07 | P3 | `codian-layouts.js:494–495` + `:1371–1372` | `section.addEventListener('click', onActivate)` внутри `buildGroup`, а `ctorPanel.addEventListener('click', onSectionClick)` делегирует те же карточки → `select()` дважды за клик (и дважды за Enter: `onKeydown` + `onConstructorKeydown`). Сейчас идемпотентно, но `setSlots`/`syncCounter`/`ensureConstructor` вызываются дважды. | `event.stopPropagation()` в `onActivate`/`onKeydown` или не вешать их на секцию, когда она в `ctorPanel`. | 4 |
| R-08 | P3 | `codian-layouts.js:141`, `:574`, `:1734`, `:1746` | `constructorCount` — 1 вхождение; `__vksAborted` никогда не выставляется; `__vksSynthetic` никогда не читается; fallback `editValueField('slots', slots)` не только мёртв (в 2.2.1.66 есть `setSlots`), но и пишет массив в map `slots[preset]` → `T[preset]===undefined`. | Удалить. **Статус: частично в T2** — `constructorCount` удалён; `__vksAborted`/`__vksSynthetic` и fallback `editValueField('slots')` в `selectConstructor` удалены вместе с движком. | −10 (сделано) |
| R-09 | P3 | `crates/vks-admin/src/api_v3/shell.rs:769`, `:805` | `"role": ui_slot_role(slot.z_order)` в calculate — вендор выбрасывает role (`Bee` в `index.bundle-2.2.1.66.js` мапит только `position,x,y,width,height,zIndex`), а наш перехватчик роль считает сам (`uiSlotRole`). Поле не нужно ни одному потребителю. | Убрать из calculate (оставить в presets/GET, где его читает `mI`). | −6 |
| R-10 | P3 | `grids.rs:284–326`, `:328–358`; `tests/grids.rs:114–136` | Таблицы дают не-16:9 ячейки: 31/100 N на каждую pin-сторону (11–12: 768×540; 34–38: 366×240; 47–53; 74–82; 93–100) и 89/100 у dual (`vks_dual_2` 640×720 → 50 % чёрного, `3–6` 640×480 → 25–44 %). Композитор letterbox-ит (`compositor.rs:108–117`). Тест сохранил имя «…миниатюры 16:9» и doc-комментарий, но `ratio`-ассерт удалён (diff `@@ -112,22 +115,18 @@`), осталось `thumb ≤ main` и `main_area ≥ 3.5×thumb_area`. | Решить, что авторитетно: шапка `grids.rs:6–7` или letterbox-конвенция. Если 16:9 — править таблицы; если letterbox — переименовать тест/шапку и зафиксировать допустимый % чёрного. | 20 |
| R-11 | P3 | `codian-layouts.js:1697–1705`, `:454` | Превью сохранённой Codian-раскладки до загрузки каталога: `findEntry` = null → fallback `count=1`, один слот на весь холст; после `catalogState='ready'` никто не инвалидирует calculate (только `ensureGroup`). До перезагрузки страницы превью неверное. | На `ready` — повторный `schedule()`/trigger calculate; либо хранить `windows` в сохранённом слоте. **Статус: закрыто в T2** — превью запрашивает `calculate` по самому preset id (сервер знает `codian_*`), каталог больше не нужен для геометрии. | 6 (сделано) |
| R-12 | P3 | `client/pnm-frontend/MANIFEST.sha256` | Порядок строк = Python `sorted(rglob)` (проверено: `committed==python` True, `committed==sort` False), а `import-pnm-frontend.sh:289` пересобирает `find \| sort -z` → следующий импорт даёт дифф на ~108 строк при неизменных файлах. `sha256sum -c` при этом 214/214 OK. **Статус: исправлено в `22aac56`** (`LC_ALL=C sort -z` в импорте и патч-скрипте; пересобранный манифест побайтно совпадает с committed, `sha256sum -c` 214/214); строка таблицы — снимок на момент аудита. | `export LC_ALL=C` в импорте или сортировать тем же ключом, что патч-скрипт. | 1 (сделано) |

Закрыто и подтверждено (не находки): крэш `sessionLayoutListByType.default` (guard в обоих бандлах,
T1 e2e), потеря типа при PATCH/mode (иммутабельность, T1), `ensure_slot_roles` на GET (role реально
нужен `mI`), `scaled_slot_rects` без щелей (`x1−x0`, half-up) — проверено на 720p/480p.

## 3. Неочевидные места

1. **Геометрия в сторе вендора — мёртвый груз, и e2e проверяет именно её.** Превью рисуется из ответа
   `calculate` (`index6…js`, `1036931`: `{position,width,height,y,x,zIndex}` → `%` от `P.width/P.height`),
   а `modifiedValue.slots` — только позиции/роли/контакты; `mI` при сохранении геометрию выбрасывает, `kee`
   тоже. Поэтому `setSlots(preset, {x,y,width,height})` в `select/selectConstructor` не влияет ни на что,
   кроме `test_layout_constructor.js`: тест «зелёный» ровно потому, что проверяет данные, которые сам же
   синтез и создал. Это делает R-01 невидимым для основного e2e.
2. **Неизвестный `vks_*` в сторе — не только пустое превью.** В сторе вендора (`index6…js` ~980005)
   `x` при отсутствии `slots[preset]` делает `Array.from(new Array(ss(F)))`; для id с опциями `ss`
   вернёт 0/NaN → пустой список или RangeError. Поэтому R-02 опаснее, чем «превью без окон».
3. **Двойной `select()`** (R-07) сегодня идемпотентен, но он удваивает `watchSaves`-сброс
   `serverCountKey` и любые будущие side-effect'ы; ловится только кликом по Codian-карточке **внутри**
   `ctorPanel` — снаружи обработчик один.
4. **Скачок раскладки при авто-расширении.** `engine::regrid_for_video` (`engine.rs:708`) при нехватке
   окон пересобирает всё через `GridLayout::compute` (aspect-only, `grid.rs:15–40`), обнуляет `preset` и
   ставит `mode=auto`. Для `vks_grid_43` (9×5) 44-й участник даёт 7×7 — все окна меняются местами.
   Это новое поведение (в 543bc7b лишние просто отбрасывались), функционально лучше, но геометрически
   непоследовательно: у двух путей разные целевые функции (`gallery_cost` с waste vs aspect-only).
   H-20 в 03 прав, что файлы сливать не надо; но общую функцию «best_rows_cols» стоит вынести.
5. **`role` в calculate не нужен даже нашему перехватчику** — он считает роль сам (`uiSlotRole`), так что
   R-09 — чистое расширение API «на всякий случай». В GET раскладки role, наоборот, обязателен: `mI`
   подставляет `role || "AUTO"`, и без `ensure_slot_roles` занятые окна выглядели бы автозаполнением.
6. **`MEDIA` — не «неизвестная роль».** Вендорский стор специально сохраняет `MEDIA` (`f` в
   `index6…js`: `["PEER","MEDIA"].includes(D.role)`), а `ensure_slot_roles` конвертирует её в `PEER`
   (`resources.rs:1271–1289`), и тест `ensure_slot_roles_backfills_legacy_payloads` это закрепляет.
   Сервер сам `MEDIA` не порождает, поэтому риск низкий, но решение продуктовое — его надо принять
   явно (сохранить `MEDIA` как валидную роль дешевле, чем объяснять пропавшие медиа-окна).
7. **Каталог: failed — навсегда.** `catalogState` после ошибки не возвращается в `idle` (`:421–470`),
   ретрая нет; это pre-existing, но теперь от каталога зависит ещё и превью (R-11).
8. **H.323-тесты в этом окружении не запускаются** (`pyasn1`/`pycrate` отсутствуют) — 64 новые строки
   тестов не проверены; в CI они, судя по 03, тоже не бегут.

## 4. «Можно было написать меньше»

| Что | Строки | Экономия |
|---|---|---|
| Синтез геометрии + перехват: `getProminentConfig`/`getDualConfig` (`:653–688`), `getGridEdges` (`:690–694`), `buildSymmetricLayout` (`:696–1036`), `formatCalculatedSlots` (`:1690–1730`), `synthXhr` (`:1732–1757`), fetch/XHR-ветки (`:1759–1830`) | ≈ 511 добавленных (−48 удалённых) | **−≈420** с возвратом HEAD-`loadConstructor` (48) и сохранением сброса кэша `serverCountKey` (~35) |
| `uiSlotRole` + `normalizeSlots` (`:87–122`) — дубль серверного `ensure_slot_roles` (`resources.rs:1271`); каталог/calculate уже отдают role | 36 | −30 (оставить pass-through) |
| `select()` пишет в стор полную геометрию каталога (`:558–570`) — потребителей нет | 10 | −10 (оставить `{position, role}`) |
| Мёртвое: `constructorCount`, `__vksAborted`, `__vksSynthetic`, fallback `editValueField('slots')` | 10 | −10 |
| `role` в calculate + ассерты | 16 | −16 |
| 14 инлайновых `payload["type"]`-проверок (`sessions.rs:475,480`; `resources.rs:501,738,824,1051,1399,1939,2306`, …) при наличии `is_default_layout` (`:705`) | — | −30 и один идиом вместо трёх |
| `target_is_local` + шаг 1 `repair_scope_defaults` — либо guard, либо retype (R-03) | 20 | −20 (при выборе «retype всегда») |

Итого без потери поведения ≈ **−500 строк**, и R-01/R-02/R-07/R-09/R-11 уходят вместе с ними. Ключевой
аргумент: синтез не «оптимизация», а второй источник истины; если важна латентность — кэшировать
**серверные** ответы по `(preset,w,h)` (как HEAD `constructorCache`), это 10 строк вместо 420.

## 5. План тестов (что добавить, чтобы это не повторилось)

1. **Parity (закрывает R-01).** Либо (а) удалить синтез + Playwright: перехватить `POST /calculate` для
   `vks_grid_43`/`vks_grid_50`, сверить DOM-rect'ы с ответом (±1 px); либо (б) Rust-тест печатает
   геометрию N=1..100×6 в JSON-фикстуру, Node-тест сравнивает с `buildSymmetricLayout` — сейчас упадёт
   на 36 случаях (готовый дамп: `vks_grid_43` 9×5 vs 7×7).
2. **Опции (R-02).** `calculate`/`geometry` + UI: выбрать `vks_grid_g12_m24_25`, дернуть счётчик; ассерт —
   нет исключения и `slots.length===25`.
3. **COW+рестарт (R-03).** Конференция с «Общей», живая сессия (mirror), PATCH «Общей», reload документа:
   ровно один default в каждом scope и это **клон** с правками (сейчас — pristine).
4. **Active конференции (R-04).** Выбрать индивидуальную на конференции → stop session → start →
   `current_layout == conference active`; и обратный случай: пользователь выбрал в сессии — не перетирать.
5. **Порядок ремонта.** Два порядка рестартов (конференция/сессия) дают одинаковые links (01 §A5).
6. **16:9/чёрный (R-10).** Для N=1..100×6: `|ar−16/9| ≤ 2 %` **или** задокументированный порог
   `black% ≤ 10 %`; вернуть удалённый ассерт в `pin_…` и переименовать тест, если letterbox — норма.
7. **e2e-герметичность (R-05).** Своя комната, без `.conference-layouts-item`; 6 карточек, 5 в ряд;
   ассерт отрендеренного превью, а не `modifiedValue.slots`.
8. **Challenger-тесты (R-06).** Удалить 5 файлов или переписать под 6 режимов и вписать в `package.json`.
9. **Манифест (R-12).** CI: импорт → `git diff --exit-code client/pnm-frontend/MANIFEST.sha256`.
10. **Read-path роли.** GET раскладки с легаси-слотами (`role` пусто/`MEDIA`, `z_order>0`) → role в ответе;
    зафиксировать решение по `MEDIA`.

## 6. Вердикт по файлам

| Файл | Вердикт |
|---|---|
| `codian-layouts.js` | **−** Главный источник R-01/R-02/R-07/R-08/R-11. Плюсы: mode-tile/счётчик, роли каталога, header-guard. Убрать ~420 строк синтеза — файл станет меньше `543bc7b`. |
| `resources.rs` | **±** Иммутабельность типа, `ensure_slot_roles`, `is_default_layout`, ремонт — по делу. R-03 (guard), R-04 (early return), 14 инлайновых type-проверок. |
| `resource_tests.rs` | **+** 5 `t00_*` осмысленны (проверял 01: 5/5). Нет теста cross-scope retype, active-конференции, read-path ролей; `MEDIA→PEER` закреплён без решения. |
| `shell.rs` | **±** Каталог с геометрией/ролями на 1280×720 — хорошо; role в calculate — лишний (R-09). |
| `grids.rs` | **±** Схемы/опции/парсер аккуратны, golden обновлён. Таблицы нарушают 16:9 из собственной шапки (R-10); `gallery_cost` не менялся, но стал видимым из-за R-01. |
| `tests/grids.rs` | **−** Удалены ассерты `main > half` и `thumb 16:9`, имя и doc остались; «идеальные галереи» только для квадратов (4,9,16,25…) — 36 расхождений не ловятся. |
| `grids_geometry_1280x720.json` | **±** 174 записи, но plain-grid только `1..9,13,16,25,100` — 36 проблемных N не покрыты. |
| `engine.rs` | **+** Seat/regrid закрывает «20+ участников»; `scaled_slot_rects` корректен. Цена — скачок геометрии (§3.4). |
| `compositor.rs` | **+** Letterbox целочисленный, без искажений; `compose_sized` минимален. |
| `gateway_audio.rs` | **±** DTMF relay (+dedupe по `(event,timestamp)`) — по делу; DTMF-пакеты двигают `sequence` — проверить, что jitter/RTCP-счётчики это переживают. |
| `gateway_video.rs`/`gateways.rs`/`uas.rs`/`rtcp.rs`/`farm*.rs` | **не ревьюились глубоко** (по указанию). Спот: `TODO(keyframe)` ~`:875`, 4 из 6 функций `rtcp.rs` без потребителей (03 H-03), farm 1478 строк — вне фокуса. |
| `h323_listener.py` + тесты | **не проверено** (нет pyasn1 в окружении). |
| `.cargo/config.toml` | **−** `target-cpu=native` закоммичен; артефакты не переносимы, комментарий про воспроизводимость sha неверен (03 H-2). |
| `scripts/patch-pnm-layout-crash.py` | **+** Идемпотентен, fail-closed, `--check`; патчит оба бандла. Минус — порядок MANIFEST (R-12). |
| `tests/e2e/*` | **−** Главный тест не герметичен и проверяет не то (R-05); 5 challenger-файлов нерабочие (R-06); настоящий гейт — только `repro_session_default_layout.js` (T1). |

## 7. С чем не согласен

1. **С мотивом «синтез нужен для скорости».** Замеров нет; 6 calculate-запросов на localhost — доли
   миллисекунды. Если латентность реальна, кэшируйте ответы сервера, а не переписывайте алгоритм:
   сейчас цена — 420 строк, расхождение с MCU и «слепой» e2e.
2. **С формулировкой «превью и применение совпадают» (doc calculate).** После R-01 это не гарантия, а
   надежда: совпадение возможно только при едином источнике или parity-тесте на всех N.
3. **С 03/H-20 в части «разные контракты — не трогать».** Файлы сливать не надо, но две разные целевые
   функции на соседних путях дают скачок 9×5→7×7 при 44-м участнике; нужна общая `best_rows_cols`,
   а не два независимых эвристики.
4. **С 01 в пункте «рестарт чинит сессию».** После R-03 рестарт чинит только single-scope клон; в
   массовом случае (mirror в сессию) он **демотирует** отредактированную «Общую» в pristine. Политику
   retype надо либо принять и убрать guard, либо отвергнуть и убрать шаг 1 — сейчас код делает вид,
   что делает, но не делает.
5. **С «оставить challenger-тесты на будущее».** Коммитить заведомо падающие тесты хуже, чем не иметь
   их: они создают ложное чувство покрытия и шумят в `git status`. Либо переписать, либо удалить.
6. **Против удаления bundle-guard после серверного фикса.** Согласен с 01: старые кэши живут, guard
   дешёв. Но его надо защитить CI-джобой `git diff --exit-code -- chunks/`, иначе следующая правка
   вендора молча снесёт защиту.

## 8. Границы достоверности

- `cargo test` целиком не гонял (запрещено); точечно — `cargo test -p vks-layout --test grids --test
  grids_geometry_golden` (10+3 passed, 1 ignored) на снимке до коммита и дамп `vks-layout::grids::geometry`
  из отдельного `/tmp`-crate (без записи в репозиторий).
- Живой UI: headless-прогон `test_layout_constructor.js` упал на setup (R-05), поэтому визуально превью
  не сверял; выводы по превью — из кода вендора (`1036931`, `Bee`) и измеренной геометрии.
- Медиа-часть (farm, gateway_video, uas, rtcp) и H.323-тесты — вне достоверности этого отчёта.

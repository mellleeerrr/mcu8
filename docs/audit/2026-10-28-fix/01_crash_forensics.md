# 01. Crash forensics: `sessionLayoutListByType.default is undefined`

> **Документ — снимок форензики на 17:35–17:55 MSK (до фиксов).** Разделы 1–7 описывают состояние
> **до** T1/T2/M1: фразы «до сих пор пишет type», «политика не зафиксирована», рекомендация варианта B,
> «исход зависит от порядка рестартов» — **pre-fix** и после коммитов `f8f3f91`/`22aac56`/`7694a0c`/
> `26c300a`/`f3009ff` неактуальны. Актуальные статусы — `02_regressions.md` §2 и `00_plan_review.md` §8.
**Снимок:** 2026-10-28 17:35–17:55 MSK; последняя сверка кода — **17:52**.
**Режим:** read-only по коду и БД (`sqlite3 mode=ro`), сервис не перезапускался, данные пользователя
(`49b599d7…`) не мутировались. Записан только этот файл.
**Провенанс живого стенда:** процесс `PID 1418719` стартовал **11:44:12**; `target/release/vks-server`
пересобран в **17:48**, но процесс **не перезапущен** → в рантайме старый Rust-код. JS-файлы отдаются
с диска, т.е. после hard reload уже пропатчены.
**Дерево менялось параллельно ревью** (см. `00_plan_review.md`): `resources.rs` — 17:40, 17:46 и
**17:51:46**; `resource_tests.rs` — 17:52:38; `codian-layouts.js` — 17:43; оба вендорских чанка +
`MANIFEST.sha256` — 17:43–17:44; `scripts/patch-pnm-layout-crash.py` — 17:40. Между первым срезом
(17:47) и финальным (17:52) соседний агент **добавил durable-ремонт** (`repair_scope_defaults`),
**перевёл identity «Общей» на `payload.type`** и **убрал read-path fallback**; это учтено ниже.
**Хэши среза 17:52 (md5) — uncommitted WIP, невоспроизводимы из git:**
`resources.rs` `4176f96e…` (mtime 17:51:46), `resource_tests.rs` `be396a94…` (17:52:38),
`shell.rs` `354afa6e…` (11:29). Этот срез существовал только в рабочем дереве и после
коммитов `645120d`/`f8f3f91` перестал соответствовать истории; в git его нет.
**Воспроизводимые хэши (md5, `git show <commit>:<path>`):** на `645120d` —
`resources.rs` `36831347…`, `resource_tests.rs` `ac2e7359…`, `shell.rs`
(`crates/vks-admin/src/api_v3/shell.rs`) `c5406874…`, `codian-layouts.js` `c91bb08e…`;
на `22aac56` — `resources.rs` `a85c56d5…`, `resource_tests.rs` `8978dcc0…`;
на `26c300a` (cross-scope M1) — `resources.rs` `21530208…`, `resource_tests.rs` `0412cf63…`,
`codian-layouts.js` `03cfb280…`; на `f8589dd` (Rust после `f3009ff` M1-R2, JS после `951e68b`) —
`resources.rs` `a4a6dbe6…`, `resource_tests.rs` `253dbf7f…`, `codian-layouts.js` `e583a09c…`
(промежуточно на `d039104`: `resources.rs` `21530208…`, `resource_tests.rs` `0412cf63…`, JS `e583a09c…`).
Проверка: `git show <commit>:<path> | md5sum`;
вендорские бандлы `index6` `6aa06b9c…`, `index-legacy6` `9f4ef0f2…` совпадают во всех срезах.
**Тесты:** в 17:57 я прогнал `cargo test -p vks-admin --lib` — **302 passed, 0 failed**; отдельно
`t00_` — **5 passed**, включая retype, дедуп двух default и «рестарт не даёт вторую Общую».

---

## 1. TL;DR-вердикт

Гипотеза Lead'а **верна по существу, но неточна в причинности и неполна по путям потери default**:

1. Тип конвертирует **не бэкенд и не «нормализация»**, а **наш** `codian-layouts.js` (до 17:43 писал
   `type=individual` в editor store в `restoreIdentity` и `onVendorModeClick`). Вендор `type`
   существующих раскладок не переписывает — только `setupNewLayout` для **новой** раскладки.
2. Бэкенд `update_resource` слепо мержил `payload` (включая `type`) в запись; это вторая половина
   причины. `merge_json` не конвертирует, а **сохраняет то, что прислал клиент**.
3. COW в `edit_linked` вытесняет canonical default из session-scope. Но COW — **пять** write-путей,
   а не один; конференц-scope COW ещё и **mirror-ит** ссылку живой сессии (`cow_replace:648–660`) →
   ломает сессию из другого экрана.
4. `list_linked` **намеренно скрывает** conference-canonical из session-ответа, если alias ведёт в уже
   видимый клон (`list_linked:1605–1612`). Это механизм «default есть в БД, но не виден UI».
5. `inherit_and_ensure_default_layout` вызывается только при старте сессии (`sessions.rs:448`), поэтому
   живая сессия после PATCH остаётся без default. Рестарт сессию чинит; конференцию — нет.

**Точная цепочка инцидента:** constructor-tile на «Общей» → `type=individual` в store → вендорный
`Th()` отправляет `payload.type` как есть → PATCH `edit_linked` → `owners=2` (conf+session) →
`cow_replace` (новый id `2caea6d1`, alias `sessions|8a75f3e4→2caea6d1`) → `list_linked` скрывает
canonical конференции → в ответе ровно один `individual` → `sessionLayoutListByType.default === undefined`
→ `hasIndividualLayout()` / `SessionBroadcastPopup.created()` падают.

**Fingerprint клона (доказывает именно PATCH-COW, не create+attach):** `creator_id="system"` (наследован
от canonical; create дал бы admin), `display_name="Codian 07 — 13 окон"` (непустой — значит в момент
сериализации `type` уже был `individual`; при `default` `Th` шлёт `display_name:null`), слоты в форме
manual-маппера вендора (`position/role/contact_id/abonent_id/connection_id/call_string_id`, без
`display_name/contact_type`), `created_at=1793195529` (16:52:09), alias пишется **только** в
`cow_replace` (`resources.rs:641`).

**Что уже есть в дереве (WIP, не в живом бинаре):**
`update_resource` сохраняет `created_type` (`1320–1341`); whitelist `default|individual` (`1225–1247`);
identity «Общей» по `payload.type` (`is_default_layout:705`, `inherit:1213`); durable-ремонт при загрузке
документа `repair_scope_defaults` (`717–766`), который **перетипирует** alias-клон в `default` (`739`),
добирает canonical при отсутствии default и **схлопывает лишние default-ссылки** (`760–763`); легаси
`payload.type` нормализуется к `default` (`1043`); read-path fallback **удалён**; фронтовый форс типа
убран; вендорский guard `(…||[])` в modern+legacy; unit-тесты `t00_*`; патч-скрипт; MANIFEST; release
сборка 17:48; тесты `t00_*` прогнаны (302/302 lib, 5/5 t00).

**Что реально оставалось на момент снимка (позже закрыто/зафиксировано):** type immutable в
`switch_video_layout_mode` (`shell.rs:967` — тогда писал `type` из body; после фикса `shell.rs:962–966`
type не переносит); `create` без `type` рождает `default` с чужим preset (identity по `payload.type` +
валидация типа закрыты в `22aac56`); **политика ремонта** не была зафиксирована (сейчас принят вариант A:
retype alias-клона, identity по `payload.type`, детерминированно — `f8f3f91`/`7694a0c`); живой процесс
не перезапущен.

---

## 2. Доказательства по пунктам A

### A1. `sessionLayoutListByType` — computed без гарантии `default`

`chunks/index.bundle-2.2.1.66.js` (offset ≈ 1122372):

```js
sessionLayoutList(){return this.sessionLayoutsStore.layoutList.map(e=>
  (e.type==="default"&&(e.displayName=this.$pgettext(...,"General")),e))},
sessionLayoutListByType(){return this.sessionLayoutList.reduce((e,t)=>
  (e[t.type]||(e[t.type]=[]),e[t.type].push(t),e),{})},
```

- объект строится **только по фактически пришедшим** `layoutList`; ключа `default` нет, если нет ни
  одной раскладки с `payload.type=="default"` **или если список пуст**;
- `sessionLayoutList` для `type==="default"` **перезаписывает `displayName` на «Общая/General»** — это
  ключ к оценке durable-ремонта: имя Codian-раскладки после перетипа в UI исчезнет.

Крэш-сайт (на момент инцидента) `chunks/index6.bundle-2.2.1.66.js`, offset ≈ 720523:

```js
hasIndividualLayout(){return!this.sessionLayoutListByType.default.some(
  e=>e.layoutId===this.source.entity.layoutId)},
```

Сейчас пропатчено: `(this.sessionLayoutListByType.default||[]).some(…)` (offset 720524);
`SessionBroadcastPopup.created` — `(…default||[])[0]||(…individual||[])[0]` (offset 913248);
то же в `index-legacy6` (842451 / 1032809). Патч-скрипт: `scripts/patch-pnm-layout-crash.py`.

### A2. Живой API: в сессии ровно один `individual`, default нет

```
POST /api/v3/login {admin/admin_password} → token
GET /api/v3/ui/sessions/49b599d7…/video_layouts
→ count=1, [{id:2caea6d1…, payload.type:"individual", mode:"manual",
             preset:"vks_pin_l_13", size:13, slots:13, display_name:"Codian 07 — 13 окон"}]
GET /api/v3/ui/conferences/49b599d7…/video_layouts
→ count=1, [{id:8a75f3e4…, payload.type:"default", mode:"auto",
             preset:"preset_default_13", slots:0, display_name:""}]
```

### A3. Документ `pnm-definitions-v1` (read-only)

```
links:
  conferences:49b599d7…:video_layouts -> [8a75f3e4-dd62-4e83-ad45-cd92b7f22613]
  sessions:49b599d7…:video_layouts    -> [2caea6d1-6782-40fa-8d0a-c08d0790b091]
active_layouts:
  conferences:49b599d7… -> 8a75f3e4… ; sessions:49b599d7… -> 2caea6d1…
layout_aliases:
  sessions:49b599d7…|8a75f3e4… -> 2caea6d1…
payload 8a75f3e4 == canonical_default_payload() → true
   {mode:auto,preset:preset_default_13,type:default,slots:[],size:13,autoscroll:0}
payload 2caea6d1: {mode:manual,preset:vks_pin_l_13,type:individual,slots:13,size:13}
creator_id 2caea6d1 = "system"; created_at = 1793195529 = 2026-10-28 16:52:09 MSK
```

Сессия стартовала ~13:51 (лог: `13:51:17` gateway video profile для `49b599d7`), правка раскладки —
16:52. В БД есть **второй** COW-след (комната `1e0337c6`): alias `sessions|8a75f3e4→29c4da67`, где
`29c4da67` — `type=default`, но payload `mode=manual` (COW без смены типа). Это доказывает: COW
«Общей» — регулярный механизм, а не разовый глюк, и показывает «тип сохранили» (крэша нет, canonical
payload потерян). Сейчас в документе 5 раскладок: 4 `default` + 1 `individual`; в session-scope
`1e0337c6` лежат **две** default-раскладки (`29c4da67` + `8a75f3e4`) — материал для durable-дедупа.

### A4. Кто именно меняет `type`: фронт, а не бэкенд/нормализация

Вендорный сериализатор PATCH (`index.bundle`, offset ≈ 1110496):

```js
gy={mode:"mode",preset:"preset",type:"type"}                    // auto
kee={...gy,slots:(e)=>e.map(...)}                               // manual
function Th(e){return re(e,{display_name:({type:t,displayName:n})=>t==="default"?null:n,
                            payload:Fee[e.mode]})}
// updateSessionLayout(t,n,r){ PATCH /api/v3/ui/sessions/<t>/video_layouts/<n> , Th(r) }
```

`payload.type` уходит **всегда**, ровно тем значением, что лежит в `layoutEditorStore.modifiedValue`.
`applyChanges()` (`index6`, offset ≈ 1072878) шлёт именно `modifiedValue`. Вендор для **существующих**
раскладок `type` не пишет; `type:"individual"` есть только в `setupNewLayout`
(`index6` offsets 1060737/1071017) — это создание **новой** раскладки.

Наш `codian-layouts.js` на момент инцидента (снимок 11:38, до правки 17:43) писал:

```js
// restoreIdentity (~строка 408)
editor.editValueField('type', 'individual');
// onVendorModeClick (~строка 1636)
editor.editValueField('preset','preset_equal_25');
editor.editValueField('type','individual');
```

`editValueField` кладёт поле в `B.value` (черновик режима), а геттер `modifiedValue` разворачивает
`{displayName, mode, type:S.value, ...B.value}` — `B.type` **перекрывает** серверный `S.value`.
Поэтому после клика по constructor-tile в PATCH уходил `type=individual`.

Бэкенд (старый `update_resource`, до WIP) делал `merge_json(&mut record["payload"], payload)` —
`type` из body просто перезаписывал серверный. Никакой конвертации/нормализации типа не было.
Текущий WIP (`1320–1341`) сохраняет `created_type` и восстанавливает его после merge;
`validate_layout_payload` (`1225–1247`) принимает только `default|individual`.

**Альтернативы, которые я проверил и отклонил:**

- `POST /ui/video_layouts` + `POST …/video_layouts/{id}` (create+attach): не создаёт alias вообще,
  `creator_id` был бы admin, canonical остался бы в session-links. Не сходится с БД.
- `detach` default: 409 (`resources.rs:1910`). `DELETE` default: 409 при привязке
  (`resources.rs:1534`). Значит default не «удалили» — его **вытеснили** COW.
- конвертация на чтении (`resolve_read_layout_id`): не меняет payload, только id/поля ответа.

### A5. `ensure` только на старте: рестарт сессии и сервера давали **разный** результат
**(pre-fix снимок; после `f8f3f91` repair room-scoped, второй default отвязывается, видимый scope
дедуплицирован — см. 02 R-03 и `t00_normalize_loaded_retypes_mirrored_clone_in_both_scopes`)**

- `prepare_session_resources` (`sessions.rs:439`) → `inherit_and_ensure_default_layout`
  (`resources.rs:1175`) вызывается на `POST /sessions` (551) и `quick` (642); в `edit_linked`,
  `attach/detach`, `switch_video_layout_mode` — **не вызывается**.
- `normalize_loaded` (`resources.rs:1031`) вызывает `repair_scope_defaults` (`717`) **при загрузке
  документа**, т.е. только на старте процесса.
- `inherit` теперь ищет default по `payload.type` (`1204–1225`), а не по canonical payload — это
  исправляет прежний дубль после рестарта сессии (ниже).

Проверено на стенде (новые комнаты, старый бинарь):

| Сценарий | Результат |
|---|---|
| attach canonical в конф., start → session list | `1 [default]` |
| PATCH session default с `type=individual` (UI-body) | clone `individual`, session list `1 [individual]` — **точная копия инцидента** |
| PATCH session default **без** type (меняем preset) | clone `default` (preset `vks_pin_l_13`), session list `1 [default]` |
| stop/start после этого (старый код, canonical-identity) | session list `2 [default, default]` — дубль «Общей» |
| PATCH **conference** default с `type=individual` при живой сессии | conference `1 [individual]`, session list `1 [individual]` (mirror) — крэш-состояние из другого экрана |
| stop/start после conference-COW | session `2 [individual, default]` (починилась), conference `1 [individual]` — **не починилась** |

**Прогноз для нового кода (симуляция `repair_scope_defaults` на реальном документе):**

| Scope | До загрузки | После `repair_scope_defaults` |
|---|---|---|
| `sessions:49b599d7…` | `[2caea6d1 individual]` | `[2caea6d1 → **default**]` (retype по alias-evidence, `739`) |
| `sessions:1e0337c6…` | `[29c4da67 default, 8a75f3e4 default]` | `[29c4da67 default]` (лишний default отвязан, `760–763`) |
| `conferences:*` | `[8a75f3e4 default]` | без изменений |

**Подтверждено тестом:** `t00_normalize_loaded_restores_default_from_alias_evidence` фиксирует
retype-политику (клон `2caea6d1` → `default`), `t00_session_restart…` — что inherit больше не плодит
вторую «Общую». То есть retype — осознанное поведение кода, а не случайность; вопрос ниже — продуктовый.

**Критично (pre-fix снимок; после `f8f3f91` политика детерминирована —**
**room-фильтр `target_is_local` и дедуп видимого scope, см. 02 R-03):** исход зависел от
**порядка рестартов**:

- **Сначала рестарт сервера** → `repair_scope_defaults` перетипирует `2caea6d1` в `default`; в сессии
  одна «Общая» с геометрией Codian 07, **имя «Codian 07 — 13 окон» в UI исчезает** (computed
  перезаписывает `displayName`), `hasIndividualLayout` начинает считать её default.
- **Сначала stop/start сессии** (сервер жив) → `inherit` видит отсутствие default и добавляет
  canonical `8a75f3e4`; в сессии **две строки** — «Общая» + individual «Codian 07». После последующего
  рестарта сервера `repair_scope_defaults` уже видит default и **не перетипирует**, обе строки остаются.

Один и тот же документ в зависимости от порядка операций даёт либо «одна Общая», либо «Общая +
Codian 07». Это не ошибка реализации, а **незафиксированная политика ремонта** — см. §5/§7.

### A6. Воспроизведение (протокол, данные пользователя не тронуты)

```
POST /api/v3/ui/conferences {"display_name":"FORENSIC-…"}       → 41efde01-0966-4e38-a50e-353462f6a83e
POST /api/v3/conferences/<cid>/video_layouts/8a75f3e4…        → 204
POST /api/v3/sessions {"conference_id":"<cid>"}               → ACTIVE
GET  /api/v3/ui/sessions/<cid>/video_layouts                  → 1 [default]
PATCH /api/v3/ui/sessions/<cid>/video_layouts/8a75f3e4…
  {"display_name":"Codian 07 — 13 окон",
   "payload":{"mode":"manual","preset":"vks_pin_l_13","type":"individual",
              "slots":[{"position":1,"role":"PEER",…},{"position":2,"role":"AUTO"},…]}}
→ 200, clone 29e72f68-…, creator_id=system, type=individual
GET  → 1 [individual]; conference GET → 1 [default]
alias sessions:<cid>|8a75f3e4 → 29e72f68; session links [29e72f68]
DEL  /api/v3/ui/conferences/<cid>                             → 204 (purge)
```

Второй прогон (conference-scope COW + mirror) — комната `11a60e53-555d-46b4-855b-242a0e275caf`,
удалена `204`; третий (type-preserving COW + restart) — та же комната, удалена. Осиротевший клон
`29e72f68…` удалён `DELETE /api/v3/video_layouts/29e72f68… → 204`. Комната пользователя
`49b599d7…` после всех проверок: links/alias/active — без изменений.

### A7. Рядом: `POST /api/v3/sessions/<id>/previews/<pid>` → 501

- Вендор: `OQ()` (`index.bundle` offset ≈ 987308) делает
  `POST /api/v3/sessions/<sid>/previews/<streamId>` с `{preview_type,payload:{preview:{resolution:{480×270,fps_limit:1}}}}`
  и ждёт `{preview:{preview_stream_id, call_string_id}}`; вызывается из
  `AbonentInfoPopupParallelPreview.mounted()` (`index6` offset ≈ 552457), затем плеер открывает тот же
  URL (`index6` offset ≈ 552989). Это **реальная фича** «превью абонента» в попапе, не мусор.
- Бэкенд: есть только заглушка `GET /api/v3/abonents/previews → {"previews":[]}`
  (`sessions.rs:1862`, роут `mod.rs:447`); POST/DELETE `…/previews/{id}` не зарегистрированы →
  `catch_all` (`mod.rs:620–622`) отдаёт 501 + WARN. В логе: `49b599d7`, `13:53:28` и `13:53:42`
  (та же живая сессия), плюс `1e0337c6` от 25.10.
- Вердикт: **не мусор, но и не причина крэша**; UI ловит reject и показывает error-frame. Полная
  реализация требует per-abonent raw-frame/WS-пайплайна, которого сейчас нет. Минимум — явный роут
  `POST|DELETE /api/v3/sessions/{id}/previews/{pid}` → 501 JSON с внятным текстом (8 строк), чтобы
  убрать WARN и зафиксировать контракт. Приоритет P2, отдельным коммитом.

---

## 3. Полный список путей, которыми сессия/конференция теряет default

| # | Scope | Триггер | Код | Что происходит | Крэш? |
|---|---|---|---|---|---|
| 1 | session | PATCH раскладки (UI save) при `owners>1` (conf+session) — **инцидент** | `edit_linked:1691`, `cow_replace:622` | clone замещает canonical в session-links, alias; canonical скрыт в `list_linked` | **да**, если в PATCH `type=individual` |
| 2 | session | `POST /ui/video_layouts/mode` (вендорные тайлы режима) | `shell.rs:967` (type из body) + `1009` (COW) | COW + запись `type`; **не закрыто фиксом** | **да**, если клиент прислал `type=individual`; вендорный UI шлёт `originalValue.type` (`default`), старые кэши/API — могут `individual` |
| 3 | session | attach/detach абонента в «Общей» при `owners>1` | `attach_layout_abonent:852` (COW 942), `detach_layout_abonent:954` (COW 996) | COW, `type` сохраняется | нет (после фикса типа); canonical payload теряется |
| 4 | conference | PATCH «Общей» на экране конференции | `edit_linked:1691` + mirror `cow_replace:648–660` | clone в conference, mirror заменяет и session-link, alias в оба scope | **да** (воспроизведено): живая сессия теряет default |
| 5 | conference | attach/detach контактов раскладки | `attach_layout_contact:1732` (COW 1777), `detach_layout_contact:1794` (COW 1825) | COW «Общей» при `owners>1`, тип сохраняется | нет; canonical payload теряется |
| 6 | session/conf | `PATCH /api/v3/video_layouts/{id}` при единственной ссылке | `edit_global:1463` + `update_resource:1320` | правка in-place; тип теперь immutable; canonical payload может «уплыть» | нет (при невалидном legacy `type` — да, до ремонта) |
| 7 | session/conf | `POST /ui/video_layouts` с кастомным payload **без** `type` | `normalize_resource:414` + `create:1351` | стартует с canonical payload → `type=default` с чужим preset (в БД: `627bc70b`, `dcf7f40e` «T7 Dup Layout») | нет, но множит «Общие» |
| 8 | session/conf | create + attach второй «Общей» | `attach:1842` | добавляет вторую default-раскладку | нет |
| 9 | session | detach последней не-default | `detach:1885`, `repair_active_layout:769` | `ensure_canonical_default` возвращает canonical | нет |
| 10 | session/conf | detach/delete default | `detach:1910` 409, `remove_global:1534` 409 | защищено | нет |
| 11 | session | рестарт процесса/сессии | `inherit…:1175` / `repair_scope_defaults:717` | сессия наследует canonical **или** клон перетипируется; конференция чинится только durable-путём | нет после старта |
| 12 | любая | пустой scope (нет ни одной ссылки) | `list_linked:1571` | `count=0`, `sessionLayoutListByType={}` | **да** (ремонт не создаёт запись для пустого scope; спасает только JS-guard) |
| 13 | любая | legacy/битый `payload.type` (`"custom"`, отсутствует) | `normalize_loaded:1043` | при загрузке молча ставится `default` — крэша нет, но семантика записи искажена | после рестарта — нет; до — **да** |
| 14 | session | alias-цепочка/протухший alias | `resolve_layout_alias:554`, `list_linked:1605–1612` | alias скрывает canonical из выдачи | да, вместе с #1/#4 |
| 15 | любая | delete room / purge | `purge_room_resources:1145` | ссылки удаляются вместе с комнатой | нет |

**Вывод:** «default отсутствует в видимом наборе» — пересечение трёх инвариантов: (а) `type` не
редактируется; (б) в каждом видимом scope есть default; (в) alias/COW не скрывает default. WIP закрыл
(б) для непустых scope и (в) чтением/дедупом, но (а) на момент снимка оставалась дыра в mode-endpoint (закрыта: `shell.rs:962–966` type не переносит),
а политика ремонта (retype vs add) не была зафиксирована (сейчас принят retype — см. 02 R-03).

---

## 4. Сравнительная таблица исправлений

Объёмы — по текущему дереву/оценке; «строки» без тестов, если не указано.

| # | Исправление | Где / объём | Риск | Регрессии | Вердикт |
|---|---|---|---|---|---|
| 1 | `type` immutable в `update_resource` | `1320–1341`; **+9** (есть) | низкий | mode-endpoint обходит; старый кэш-UI «сохраняет» constructor в «Общую» (semantic drift); нужен п. 8 | **обязательно** (есть) |
| 2 | 422 при смене `type` в `edit_linked` | `1648+`; **+12** | средний | ломает 200-контракт и save-flow у клиентов со старым бандлом; вендорский `applyChanges` покажет ошибку | **не делать** |
| 3 | durable-ремонт `repair_scope_defaults` (retype alias-клона + дедуп) | `777–826`; **+~50** (есть) | **средний/высокий по данным** | перетипирует пользовательскую individual-раскладку в «Общую»; имя скрывается computed'ом; `hasIndividualLayout`/меню абонента меняют смысл; после `f8f3f91` исход детерминирован (A5 — pre-fix снимок) | **есть; политика принята** |
| 4 | `ensure` после каждого COW/detach | `edit_linked`/`mode`/`attach*/detach*`; **+15–25** | средний | наивная версия (canonical) даёт **две** «Общие»; нужно дедуплицировать | **не нужно**: identity по типу (п. 9) + durable (3) закрывают |
| 5 | frontend: не форсить `type` | `codian-layouts.js` **−2** (есть) | низкий | без п. 8 constructor-на-«Общей» делает «Общую» кастомной (имя скрыто) — теперь это осознанная политика | **обязательно как гигиена** (есть) |
| 6 | vendor guard `(…||[])` | `index6:720524,913248`; `index-legacy6:842451,1032809`; скрипт **+142 строки** (есть) | низкий runtime / высокий maintenance | апгрейд вендора теряет патч; нужен `--check` в CI + MANIFEST | **обязательно как страховка** (есть), P2 |
| 7 | one-time ремонт БД | скрипт **+30–50** | высокий (мутация user data) | не защищает будущее; необратим; дублирует durable | **не делать** |
| 8 | fork-on-type-change: PATCH default с `type=individual` → новая individual, «Общая» остаётся | `edit_linked` (+mode) **+25–30**, тесты | средний | UI после save рефетчит **старый id** → покажет «Общую» (нужен frontend-навигация, +10); список растёт | **рекомендуется, если constructor должен создавать отдельную раскладку**; иначе — п. 3 |
| 9 | identity «Общей» по `payload.type` (не по канону) | `is_default_layout:705`, `inherit:1213`; **+3** (есть) | низкий | меняет смысл «canonical»: payload-seed больше не эталон per-scope; исчезают дубли после рестарта | **обязательно** (есть) |
| 10 | альтернатива durable: **добавить** canonical рядом, не перетипируя клон | `repair_scope_defaults` **~8** вместо retype | низкий | в сессии 2 строки («Общая» + Codian), active = Codian; нужно решить, что считать «правильной» «Общей» | **отклонено**: принят retype (вариант A, `f8f3f91`; см. 02 R-03) |
| 11 | `create`: `type` отсутствует и payload ≠ canonical → `individual`; неизвестный → 422 | `normalize_resource:414`, `create:1351` **+4–5** | средний | может затронуть canonical-seed create; нужен тест на reuse | **should**, закрывает вторую дверь |
| 12 | previews: явный роут 501 JSON | `mod.rs` **+8** | нет | нет | **P2, отдельно** |
| 13 | лог/метрика при retype в durable-ремонте | `repair_scope_defaults` **+3** | нет | нет | **should**: сейчас перетип молчаливый |

---

## 5. Рекомендация и порядок

### P0 — до перезапуска/релиза (краш + предсказуемость данных)

1. **`type` immutable на всех write-путях.** `update_resource` уже; добавить в
   `switch_video_layout_mode` (`shell.rs:967`): не принимать `type` из body (или восстанавливать
   исходный после patch). ~6 строк. Без этого старый кэш/API снова создаст individual-only сессию.
2. **Зафиксировать политику durable-ремонта** (продуктовое решение, 1 из двух;
   **позже принят вариант A — retype, см. 02 R-03**):
   - **A. retype (сейчас в коде):** alias-клон «Общей» считается «Общей»; пользовательская раскладка
     `2caea6d1` станет `default`, имя «Codian 07 — 13 окон» в UI исчезнет, меню абонента изменит
     смысл. Плюс: одна строка, никакого «пустого General» рядом. Минус: скрытая реклассификация данных.
   - **B. append (рекомендую):** добавить canonical `8a75f3e4` в session-links, клон оставить
     `individual`; в списке две строки, active = Codian. Плюс: сохраняется пользовательская раскладка и
     её имя. Минус: две строки, нужен осознанный выбор active.
   Если выбирается A — добавить `tracing::warn!`/audit-строку со списком retype id (3 строки) и
   уведомить пользователя, что его раскладка стала «Общей». Если B — переписать ветку `evidence`
   (`739`) на добавление canonical (8 строк) и тест на фикстуре `49b599d7`.
3. **Тесты durable-ремонта** есть и **проходят** (`t00_normalize_loaded_restores_default_from_alias_evidence`,
   `t00_normalize_loaded_dedupes_two_defaults_keeping_active`,
   `t00_session_restart_does_not_add_second_default_after_edit`,
   `t00_layout_type_is_validated_on_create_and_repaired_on_load`; прогон 17:57 — 302/302 lib,
   5/5 t00). Не хватает кейсов: mode-endpoint type, `create` без type, «порядок рестартов»
   (session-first vs server-first) с фиксацией ожидаемого исхода.
4. **`create`:** missing type + непустой/кастомный payload → `individual`; неизвестный type → 422
   (`validate_layout_payload` уже есть, нужна нормализация в `normalize_resource`). 4–5 строк.
5. **JS:** удаление форса типа (есть), вендорский guard (есть), `scripts/patch-pnm-layout-crash.py`
   + `--check` в CI, MANIFEST пересчитан. Frontend действует после **hard reload**; старые вкладки
   живут со старым бандлом — серверный фикс всё равно обязателен.

### P1 — семантика «Общей» и старые кэши

6. Решить: **fork-on-type-change** (PATCH default с `type=individual` создаёт новую individual,
   «Общая» остаётся) **или** запретить constructor на `type=default` и требовать «Создать раскладку»
   (10 строк guard в `codian-layouts.js`). Рекомендую **fork** (совместим со старыми кэшами, где тип
   всё ещё шлётся), с frontend-навигацией на новый id (+10 строк).
7. durable-ремонт покрыть тестом на реальной фикстуре документа (сессия с alias, как в БД).

### P2 — гигиена

8. Явный роут previews → 501 JSON (8 строк) + метрика.
9. Разобрать остальные 501 (`/api/v3/health`, `/rooms`, `/stats`, …) — отдельным аудитом, к этому
   багу отношения не имеют.

### Чего НЕ делать

- **422 на смену типа** — ломает 200-контракт и UI-save; type-immutability достаточно.
- **`ensure` canonical после каждого COW** — даёт дубли «Общей»; корректнее identity по типу.
- **One-time repair БД** — не защищает будущее и необратим; durable-remenт идемпотентен.

### Порядок выката

1. JS (guard + без форса) — уже на диске; нужен cache-bust/hard reload → мгновенная страховка от
   крэша на текущих битых данных.
2. Решение A/B по п. 2 → правка (если B) → `cargo test -p vks-admin` → release-сборка → **согласованный**
   перезапуск сервиса (drop живых медиа-сессий!) → smoke:
   `GET /ui/sessions/49b599d7…/video_layouts`:
   - при A: `1 [default]` (id `2caea6d1`, displayName в UI «Общая»);
   - при B: `2 [default, individual]` (`8a75f3e4`, `2caea6d1`).
   Конференция — `1 [default]`.
3. Playwright на живом стенде: constructor-на-«Общей», refresh-циклы, попап трансляции, пустой список.

---

## 6. Edge cases и тест-матрица

### Rust (`crates/vks-admin/src/api_v3/resource_tests.rs`)

| # | Кейс | Ожидание |
|---|---|---|
| U1 | PATCH default с `type=individual` (UI-body) | 200; `payload.type=default`; геометрия применена; в scope ровно один `type=default` (есть `t00_…`) |
| U2 | `POST /ui/video_layouts/mode` c `type=individual` на default | 200; type не изменился (**сейчас баг**, `shell.rs:967`) |
| U3 | COW default через attach абонента | клон `type=default`; canonical-оригинал не изменён |
| U4 | Conference PATCH default + живая сессия (mirror) | оба scope содержат ровно один default; alias не скрывает его из session-list |
| U5 | Два старта сессии подряд после правки «Общей» | ровно один default (есть `t00_session_restart…`) |
| U6 | Durable-remenт на фикстуре `49b599d7` | по политике A: `2caea6d1` → default; по B: добавлен `8a75f3e4`, клон individual |
| U7 | Durable-remenт с двумя default (`1e0337c6`) | остаётся один default = active; лишний отвязан от scope (есть тест) |
| U8 | Нет default нигде (пустой/чужой scope) | count=0 (документирует, что нужен UI-guard; сервер не синтезирует id) |
| U9 | `create` c кастомным preset без `type` | `individual`, не «Общая» (после п. 4) |
| U10 | `create`/PATCH с `type:"foo"` | 422; легаси-запись при загрузке → `default` (есть тест) |
| U11 | Detach default → 409; Delete attached default → 409; detach последней не-default | canonical восстановлен |
| U12 | Двойной PATCH подряд (после COW) | второй уходит по alias в тот же клон, новых раскладок нет |
| U13 | Alias-цепочка `a→b→c` и протухший alias после detach | resolve ≤8, нет 404/дублей |
| U14 | Quick session (без конференции): PATCH default | после рестарта ровно один default |
| U15 | Порядок рестартов (session-first vs server-first) | одинаковый итог по выбранной политике (сейчас — разный) |
| U16 | Conference-scope COW до фикса типа (legacy) | durable-ремонт даёт default (retype или append); тип пользовательской раскладки по политике |

### Playwright (`tests/e2e`, node + `playwright`, без SIP)

| # | Сценарий | Ассерт |
|---|---|---|
| E1 | login → создать конф. → start → открыть «Общую» → constructor Codian 07 → save | нет `pageerror` с `sessionLayoutListByType/default`; таблица абонентов рендерится; в списке есть «Общая» |
| E2 | Подождать 2 интервала refresh | 0 pageerror за 30 c |
| E3 | Правка раскладки на экране конференции при живой сессии → вернуться в сессию | 0 pageerror; «Общая» видна |
| E4 | Открыть попап трансляции на individual-only состоянии (сид через API) | 0 pageerror (guard `(…||[])`) |
| E5 | Completely empty layout list | 0 pageerror (guard; сервер вернёт count=0) |
| E6 | «Создать раскладку» → constructor → save | новая individual с именем; «Общая» не изменилась |
| E7 | stop/start сессии → reload | ровно одна строка «Общая» (нет дубля) |
| E8 | Эмуляция старого кэша: PATCH с `type=individual` из `page.evaluate(fetch)` | 200; «Общая» осталась; (если fork — появилась новая individual) |
| E9 | Двойной клик Save | одна новая раскладка, нет 404 по alias |
| E10 | Контекстное меню абонента → «Настройки раскладки» на default-only и individual-only списках | меню открывается, нет TypeError |
| E11 | Пользователь открывает «Codian 07» после durable-ремонта | по политике A: показана как «Общая»; по B: имя сохранено |

---

## 7. С чем я не согласен с Lead'ом (и с соседним ревью)

1. **«Тип конвертирует бэкенд/нормализация» — нет.** Вендор не переписывает `type` существующих
   раскладок; бэкенд лишь сохранял присланное. Источник — наши строки 408/1636 в `codian-layouts.js`
   (на момент инцидента). Это меняет адресата фикса: сначала JS-гигиена, но **обязательно** серверный
   инвариант (старые кэши).
2. **«COW в `edit_linked:1600–1630`» — слишком узко.** COW есть в `edit_linked`, `mode`, attach/detach
   абонента и контактов; конференц-COW mirror-ит сессию. Инцидент — именно `edit_linked`, но
   «починить одну строку» недостаточно.
3. **«ensure только на старте ⇒ инвариант не поддерживается» — верно лишь для живой сессии.**
   Рестарт сессию **чинит** (наследование canonical), конференцию — нет. Более того, durable-ремонт
   при рестарте сервера решает иначе, чем рестарт сессии: **исход зависел от порядка** (§A5 —
   pre-fix снимок; после `f8f3f91` repair room-scoped и детерминирован), поэтому
   «рестарт» нельзя предлагать как ремонт без фиксации политики.
4. **Инвариант «ровно одна canonical default» в прежней формулировке противоречив.** «Общая» —
   редактируемая: её payload может уехать от канона. Держать identity по точному payload = дубли после
   рестарта. Правильная формулировка: **«в каждом непустом видимом scope ≥1 (и ровно 1) раскладка с
   `payload.type=="default"`; canonical payload — только глобальный seed/дедуп»**. WIP это уже сделал
   (`705`, `1213`) — фиксирую как согласие, а не претензию.
5. **Кандидат 2 (422)** — отклоняю: ломает 200-контракт, вендорский save-flow и старые кэши; ничего
   не добавляет к type-immutability.
6. **Кандидат 4 «ensure после COW/detach»** — отклоняю в наивном виде: canonical + type-default-клон
   дают две «Общие». Заменяется identity-by-type (п. 9) и durable-ремонтом (п. 3).
7. **Кандидат 7 (one-time ремонт БД)** — отклоняю: не защищает будущее, необратим, дублирует durable.
8. **Соседнее ревью (`00_plan_review.md` §3.3) и текущий код: «перетипировать alias-клон в default»** —
   не согласен как с дефолтной политикой (на момент снимка; **итоговое решение — вариант A**, см. 02 R-03
   и `t00_normalize_loaded_retypes_mirrored_clone_in_both_scopes`). `2caea6d1` — пользовательская individual-раскладка с именем
   «Codian 07 — 13 окон»; после retype UI показывает её как «Общую» (computed перезаписывает
   `displayName`), `hasIndividualLayout` начинает считать её default (меняется действие в меню
   абонента), а исход зависит от порядка рестартов. Правильнее **добавить canonical рядом**, сохранив
   individual. Признаю контраргумент: клон родился из «Общей», и если продукт считает редактирование
   «Общей» её изменением, retype логичен. Это **продуктовое решение**, а не техническое; в коде оно
   сейчас выбрано молча — его нужно зафиксировать и покрыть тестом.
9. **«Frontend-фикс не нужен»** — как единственный фикс не нужен; как гигиена (и чтобы старые кэши не
   продолжали слать `type=individual`) — нужен, уже сделан, стоит 2 строки.
10. **Вендорский guard — не P0.** Это страховка представления; P0 закрывается серверным инвариантом.
    Порядок: Rust → reload JS → e2e; но патч уже применён и вреда не несёт (кроме maintenance).

---

## 8. Что осталось непроверенным

- **Точный жест пользователя** (constructor-tile vs ручной tile vs preset-selector) в 16:52 не
  залогирован; восстановлен по payload/creator/alias. Аудит-лог PATCH не пишет.
- **Откуда имя** «Codian 07 — 13 окон»: наш JS его в `displayName` не пишет (только `title/aria-label`
  карточки); вероятно, ввод пользователя в поле имени или вендорский автонейминг. На причинность не
  влияет, но UX-вопрос открыт.
- **WIP-фиксы не проверены в рантайме:** живой процесс — старый бинарь (11:44); мои репро — на нём.
  Юнит-тесты я прогнал в 17:57: `cargo test -p vks-admin --lib` — **302 passed, 0 failed**,
  `t00_` — **5 passed**. Но рантайм (рестарт сервиса) не выполнялся; прогноз поведения
  `repair_scope_defaults` на живом документе — **симуляция**, а не прогон кода.
- **Durable-ремонт на живом документе** не выполнялся: сервер не перезапускался, поэтому
  `49b599d7` до сих пор `1 [individual]`, а `1e0337c6` — `2 [default, default]`.
- **Порядок рестартов** (A/B-исход) не проверен в рантайме; вывод — из кода.
- **Fork-on-type-change**: как вендорский UI отреагирует на новый id в ответе PATCH (рефетч старого id,
  навигация) — проверено только по коду `applyChanges`, не в браузере.
- **Кэширование вендорских чанков**: имена фиксированные (`…2.2.1.66.js`), `index.html` мог
  закэшироваться; гарантии, что у пользователя после reload новый guard, нет — нужен cache-bust.
- **Поведение вендора при двух `default`** (какую строку считает active/«Общей») — не проверял в
  браузере; в БД кейс есть (`1e0337c6`).
- **Preview-пайплайн**: есть ли в media-engine per-abonent raw frames/WS — не оценивал; вердикт «P2,
  заглушка» основан на отсутствии маршрута и плеера-эндпоинта.
- **Остальные 501** (`/health`, `/rooms`, `/stats`, `/monitoring/sessions`) — вне скоупа.
- **Параллельные правки**: `resources.rs` менялся в 17:40, 17:46 и 17:51; `cargo build` шёл
  17:44–17:48; финальные номера строк/поведение могут отличаться от среза. См. хэши в шапке.

---

### Приложение: быстрые команды проверки (после рестарта)

```bash
TOK=$(curl -s -X POST localhost:8080/api/v3/login -H 'Content-Type: application/json' \
  -d '{"login":"admin","password":"admin_password"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["token"])')
curl -s -H "Authorization: Bearer $TOK" \
  localhost:8080/api/v3/ui/sessions/49b599d7-b5cd-45a7-8999-47a28c453a92/video_layouts \
  | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d["count"],[(x["id"][:8],x["payload"]["type"],x["display_name"]) for x in d["video_layouts"]])'
# политика A: 1 [('2caea6d1','default','Codian 07 — 13 окон')]  (UI покажет «Общая»)
# политика B: 2 [('8a75f3e4','default',''), ('2caea6d1','individual','Codian 07 — 13 окон')]
python3 - <<'PY'
import sqlite3,json
r=json.loads(sqlite3.connect('file:vks_data.db?mode=ro',uri=True).execute(
 "select document_json from configuration_documents where document_key='pnm-definitions-v1'").fetchone()[0])['resources']
room='49b599d7-b5cd-45a7-8999-47a28c453a92'
print(r['links'][f'sessions:{room}:video_layouts'], r['layout_aliases'].get(f'sessions:{room}:video_layouts|8a75f3e4-dd62-4e83-ad45-cd92b7f22613'))
PY
```

# Ревью плана 2026-10-28-fix (v1) → выровненный план v2

**Снимок:** 2026-10-28 17:35–17:50 MSK. Дерево менялось **параллельно ревью**: `resources.rs` — 17:40,
`resource_tests.rs` — 17:42, `scripts/patch-pnm-layout-crash.py` — 17:40, оба вендорских чанка и
`MANIFEST.sha256` — 17:40…17:44. Всё «сейчас» ниже — на этот момент; если соседний агент продолжает
править T1, см. §2 (статусы) как срез, а не как приговор.

**Режим:** только чтение кода и БД; сервис 8080 не перезапускался; записан ровно один файл — этот отчёт.
Проверки, выполненные по ходу ревью:

| Проверка | Результат |
|---|---|
| `cargo check --workspace --locked` | зелёный, 6.52 s (инкрементально) |
| `cargo test -p vks-admin --locked` | **299 passed**, 0 failed (вкл. новые `t00_*`) |
| `cargo test -p vks-layout --locked` | **84 passed**, 0 failed, 1 ignored (`regenerate_golden`); золотые — 3/3 |
| `sha256sum`-сверка `MANIFEST.sha256` (python, 214 записей) | 214/214 OK, 0 bad |
| `python3 scripts/patch-pnm-layout-crash.py --check` | «уже применено» ×2 файла, `node --check` OK |
| `vks_data.db` (read-only, `mode=ro`) | 5 раскладок, 2 сессии с `sessions:*:video_layouts`, алиасы COW |
| Живой бинарь | `target/release/vks-server`, sha256 `94484d42…`, mtime **11:44**, PID 1418719 |

---

## 0. TL;DR — что не так с планом v1

1. **План отстал от кода:** T1(a) `payload.type` immutable + read-path ремонт, T1(c) вендорский
   guard-патч, T1(d) unit-тесты, `.gitignore target-*`, пересборка MANIFEST — **уже сделаны**
   (не закоммичены). План предлагает их «сделать» повторно. Осталось 2 реальные дырки и 1 косметика (§3.1–3.3).
2. **Тезис «HEAD 543bc7b не собирается без untracked» — неверен.** В HEAD нет ни одной ссылки на
   `farm`/`rtcp`/`farm_metrics`; HEAD собирается. Не собирается *рабочее дерево без untracked*.
   Настоящий блокер — провенанс: живой бинарь (11:44) старше правок (17:40), т.е. Rust-инвариант
   в рантайме **не активен**, а JS-патч уже действует после reload.
3. **Порядок перевёрнут:** P0 `-in` (участник не садится в окно) стоит 4-м, security — 8-м, а
   косметическая геометрия — 2–3-м. `-in` — одна функция, security — 4 эндпоинта, оба дешевле T2/T3.
4. **T2 недооценён:** удалять надо не 70–170 строк перехвата, а **+337 строк клиентского
   геометродвижка** (`buildSymmetricLayout`), который и есть второй источник истины. Перехват
   `calculate` — только верхушка.
5. **«0/174 golden не меняются» — миф по покрытию:** golden-набор — 13 значений N, и ни одно из
   **36** N, где rows×cols реально меняются при `WASTE_WEIGHT=0` (43, 44, 45, 50…), в него не входит.
6. **T3/T5/T8 частично уже в WIP** (grids.rs rewrite, DTMF clock, RTCP PLI, H.323 event_sink) — их надо
   переформулировать в «верифицировать + тесты», иначе план снова сделает работу дважды.
7. **T6/T8 — мега-задачи** (гигиена 7 подпунктов, шлюзы 4 фикса) — нарушают правило «коммит после
   каждой задачи» и смешивают риски. T2+T3a и вправду надо слить, но T3b — отдельно.

---

## 1. Оценка плана v1 (1–10)

| Критерий | Оценка | Обоснование (факты, не впечатления) |
|---|---|---|
| Полнота | **6.0** | Покрыты все крупные кластеры аудита, но пропущены: дубли «Общей» после рестарта сессии (`has_default`, resources.rs:1142), валидация `type` на create (resources.rs:1303–1345), alias-based ремонт данных, `defaults.deletable` у виртуальной строки, 337-строчный клиентский движок (T2), покрытие golden, провенанс/сплит WIP, обновление `network_integration.rs` при закрытии auth |
| Порядок/зависимости | **4.0** | T4(P0) после T2/T3; T7(P1, 4 эндпоинта) после T6; T9 зависит от T1(c) (MANIFEST) и T2/T3, но это не отражено; T8 требует изолированного контура/живых вызовов — контур заявлен в T0, но критерий готовности T0 этого не требует |
| Риск | **5.5** | Не оценены: незакоммиченное переписывание `grids.rs` (+650), `gateway_video.rs` (+1589), `engine.rs` (+704) — вне аудита; патч вендора без e2e-доказательства; restart-gate (пользователь запретил перезапуск); `target-cpu=native` |
| Элегантность / минимум кода | **4.5** | T2 даёт −70…−170 вместо −450; T3 дублирует уже сделанное; T1(b) — фикс, который не нужен (сервер уже авторитетен); T6 тащит удаление зависимостей (нужен nightly/`cargo-machete`, ценность ≈ 0, риск > 0) |
| Проверяемость | **6.5** | Тест-идеи хорошие, но критерии частично ложные («0/174 golden»), e2e с sipbot избыточен, parity-тест не определён, нет критерия «ровно один default», нет команд приёмки |
| **Итог** (0.25/0.2/0.2/0.2/0.15) | **5.3** | План годен как черновик, но в текущем виде принят быть не может (порог 9.5) |

---

## 2. Проверка по коду: что реально, что уже сделано, что устарело

| Задача v1 | Статус в дереве (17:50) | Доказательство / комментарий |
|---|---|---|
| T0 baseline | **частично** | `cargo check` зелёный (6.5 s); тесты 299 + 13; `.gitignore` для `target-impl/`, `target-proto/` уже добавлен; MANIFEST 214/214 OK. Нет: checkpoint-коммита, тега, фиксации sha/`rustc -vV`, копии БД |
| T0 «HEAD не собирается» | **неверно** | `git grep farm\|rtcp\|farm_metrics HEAD -- '*.rs'` — 0 совпадений; HEAD `lib.rs` без `pub mod farm`; дерево собирается только вместе с untracked `farm.rs`, `rtcp.rs`, `farm_metrics.rs` |
| T1(a) `type` immutable | **сделано** | resources.rs:1272–1289: `created_type` запоминается и восстанавливается после `merge_json`; тест `t00_layout_type_is_immutable_and_session_keeps_default_after_edit` — зелёный |
| T1(a) «гарантия canonical default» | **сделано частично и с дефектом** | `inherit_and_ensure_default_layout` (resources.rs:1123–1176) на старте сессии; `list_linked` read-path ремонт (resources.rs:1559–1576). **Дефект:** `has_default` = `is_canonical_default` (resources.rs:1142 + layout_defaults.rs:19–21) — точное равенство payload |
| T1(b) frontend | **не сделано (и не нужно как фикс)** | `codian-layouts.js:408` (`restoreIdentity`) и `:1636` (`onVendorModeClick`) форсят `type='individual'`; сервер теперь игнорирует — краша нет. Осталась ложь в черновике редактора |
| T1(c) vendor patch | **сделано** | `scripts/patch-pnm-layout-crash.py` (2 правила × 2 сборки), оба чанка изменены, `--check`/`node --check` OK, hook в `scripts/import-pnm-frontend.sh:272`, MANIFEST пересобран самим патчем (:126–140) |
| T1(d) тесты | **unit — да, e2e — нет** | 2 теста `t00_*` + `ensure_slot_roles_backfills_legacy_payloads`; Playwright-репро отсутствует |
| T1(e) WEBGL warning | не сделано | К крашу отношения не имеет; в T1 не место |
| T2 preview==apply | **не сделано** | Перехват жив: `codian-layouts.js:1690–1731` (`formatCalculatedSlots`), `:1759–1833` (fetch/XHR). **Плюс** `:696–1025` (`buildSymmetricLayout`, 337 строк), `:1027` (`computeSlots`), `:1447–1470` (`selectConstructor` → `editor.setSlots(...)`) |
| T3 геометрия | **частично, и уже иначе** | `grids.rs` переписан (dual, модульная матрица, 16:9), goldens пересозданы; `WASTE_WEIGHT=20` на месте (grids.rs:280); `grid.rs:75–83` — растяжение последней плитки не исправлено. Тест pin ослаблен: было «миниатюры 16:9 и крупный > половины», стало «area ≥ 3.5×» (tests/grids.rs diff) |
| T4 назначения | **не сделано** | `layout_identifier_has_uuid` = «последние 36 символов» (gateways.rs:1279–1289) → `sip-<uuid>-in@sip-gateway` и `h323-<uuid>-in@…` отбрасываются фильтром :1773. `attach>size → 409` уже есть (resources.rs:865–866). Уникальность `position` не проверяется (assignments — map, дубль позиции молча теряет участника) |
| T5 статус/события | **не сделано** | Нет `x-vks-layout-applied`/метрики; `push_layout_to_engine` тихий (sessions.rs:496–509); auto-expand применяет раскладку без `LayoutChanged` (engine.rs:1423–1431); `apply_layout` всегда `Ok` (gateways.rs:1895–1925) |
| T6 гигиена | **частично** | `.gitignore` — да; dead code — нет: `viewer.rs` (10.6 KB) + re-export `vks-layout/src/lib.rs:50`; `MockVideoDecoder/Encoder` + re-export `vks-media-mcu/src/lib.rs:61`; `constructorCount` (`codian-layouts.js:141`), `onConstructorActivate` (`:1418`) — 1 вхождение каждое; `openapi.json` — файл мёртв, маршрут живой (`handlers.rs:730`, тест `network_integration.rs:488`). `build_pli/build_fir` — **не мёртвые**: это тест-билдеры `rtcp.rs:185–233` (в план попали ошибочно). Каталоги: `target-impl` 5.9 G, `target-proto` 4.7 G, `target/debug` 46 G |
| T7 security | **не сделано** | `m5_api` вне auth-слоя: `/api/v3/gateways/status`, `/api/v3/mcu/farm`, `/api/v3/cluster/status` (vks-server/src/lib.rs:154–175), `/metrics` (:193). UI их не вызывает — 0 вхождений во всех 214 файлах рантайма (recursive grep); smoke-скрипты тоже |
| T8 шлюзы | **частично** | DTMF clock уже корректен (`uas.rs:236,309–310`); H.323 `event_sink` (`h323_listener.py:459,578–597`); `rtcp.rs` (PLI/FIR-парсер + `build_pli/fir` для тестов), PLI→`request_video_keyframe` только вне фермы, для фермы — честная метрика + TODO (`gateway_video.rs:872–940`). Не сделано: `source_latch` политика/reason-код, farm-битрейт = min(negotiated,max) |
| T9 финал | не сделано | Плюс отсутствует rollback-план и restart-gate |

---

## 3. Пропущенные подзадачи и подводные камни (конкретно)

### 3.1 Дубли «Общей» после рестарта сессии — **новый баг, созданный фиксом T1**

`has_default` ищет **канонический** payload (`resources.rs:1142` → `layout_defaults.rs:19`). После
фикса «тип неизменяем» COW-клон «Общей» сохраняет `type=default`, но его payload уже не равен канону
(пользователь выбрал Codian 07). На следующем `POST /api/v3/sessions` `has_default=false` →
`canonical_layout_id` → в scope добавляется **вторая** pristine «Общая». Итог: «Для всех участников (2)»,
две строки, неоднозначный `active`. Это не гипотеза: в БД уже есть кейс `1e0337c6` с **двумя**
default-раскладками (`29c4da67` payload-mode manual + `8a75f3e4` канон), который маскируется
alias-логикой только в ответе `list_linked`.

**Фикс:** identity для инварианта — `payload.type == "default"`, а не равенство payload. `is_canonical_default`
оставить только для seed/дедупликации. ~3 строки + тест «два старта подряд → ровно один default».

### 3.2 Виртуальная «Общая» выглядит удаляемой

Read-path ремонт (`resources.rs:1559–1576`) вставляет канон в **ответ**, не в `links`. У записи
`defaults.deletable=true` (`resources.rs:426`), поэтому UI рисует кнопку удаления; `DELETE
/api/v3/sessions/{id}/video_layouts/{canonical}` уходит в `detach` (`resources.rs:1855–1864`,
`direct=false`) → **404 «not directly attached»**, тост-ошибка. **Фикс:** в ответе ремонта ставить
`defaults.deletable=false` (2 строки).

### 3.3 Alias-based ремонт вместо «второй строки» (элегантность + данные)

В БД лежит доказательство происхождения клона:
`sessions:49b599d7…:video_layouts|8a75f3e4… → 2caea6d1…` (COW канонической «Общей»).
Сейчас `49b599d7` имеет единственную раскладку `2caea6d1` (`type=individual`, «Codian 07 — 13 окон»).
Правильный ремонт: при загрузке (`state.rs:667`, `normalize_loaded`) — если в session-scope нет
`type=default`, но алиас от канона ведёт на linked-раскладку, **выставить ей `type=default` durable**
(один раз, идемпотентно). Иначе read-path ремонт будет вечно показывать рядом с пользовательской
раскладкой пустую «Общую» с чужим id, а при следующем старте — ещё и дублировать (§3.1). Fallback —
существующий виртуальный ремонт. ~10 строк + тест на фикстуре из БД.

### 3.4 `create` не валидирует тип — вторая дверь в ту же дыру

`normalize_resource` стартует с канонического payload (`type=default`, resources.rs:414–417) и merge-ит
body: `POST` без `type` → `default`; `POST` с `type:"foo"` → запись, невидимая для UI (ни `default`,
ни `individual`) → та же TypeError-группа. `validate_layout_payload` (resources.rs:1177–1184) проверяет
только `slots`. **Фикс:** whitelist `default|individual|active_speaker`; пусто/missing → `individual`
(кроме канонического seed); неизвестное → `422 invalid.type`. Без этого неизменяемость update обходится
через create.

### 3.5 `type` — не только UI-группировка

`resolved_layout_abonents` (resources.rs:499–502) по `type=="individual"` решает, показывать всех
участников или только занятые слоты. Значит ремонт/флип типа меняет содержимое вкладки «Кто в
раскладке» — это нужно покрыть тестом, иначе «починили UI, сломали список участников».

### 3.6 Frontend: форс `type=individual` — не фикс, а гигиена

`restoreIdentity` (`codian-layouts.js:408`) и manual-ветка (`:1636`) пишут `individual` в черновик.
Сервер игнорирует — краша нет; но (а) черновик лжёт, (б) в PATCH уходит мусорное поле. Удалить строку
`:408` (или запоминать `identityType` рядом с `identityName`, 3 строки). **Отдельную задачу с тестами
из этого делать не нужно.**

### 3.7 Вендорский патч: что именно патчить и чем доказывать

- Незащищённых сайтов ровно **два на сборку**: `hasIndividualLayout` (`index6:720523`,
  `index-legacy6:842450`) и `SessionBroadcastPopup.created` (`index6:913241`, `index-legacy6:1032802`).
  В `index.bundle`/`index-legacy.bundle` — только *guarded* store-computed, патчить нечего (проверено
  recursive grep по всем 214 файлам рантайма).
- `node --check` для минифицированного бандла — слабая проверка; нужен e2e «пустой/без-default список →
  0 pageerror» (иначе патч недоказан).
- Кэш: `chunks/**` не хэшированы → `Cache-Control: no-cache` (frontend.rs:44–46,139,165). Cache-bust
  **не нужен** — браузер ревалидирует по `Last-Modified`. Это снимает один из страхов плана.
- Докстринг патча утверждает, что UI падает «на любом промежуточном состоянии, включая первые
  миллисекунды загрузки». Если бы это было так, падала бы **каждая** страница сессии; жалоба
  конкретна (нет default). Не тиражировать это утверждение: приёмочный тест должен сначала дождаться
  ответа `GET /api/v3/ui/sessions/{id}/video_layouts`, иначе результат нестабилен.

### 3.8 MANIFEST / import-скрипт

Патч сам пересобирает манифест (`patch-pnm-layout-crash.py:126–140`) — хорошо, формат совпадает с
`import-pnm-frontend.sh:288–290` (`sha256sum` + `./rel`). Нужно: (а) добавить `patch-pnm-layout-crash.py`
в CI-джобу `sidecar` (там уже гоняется `test_pnm_patch_lib.py`), (б) шаг `sha256sum -c` в CI, (в) при
следующем импорте нового PNM — патч обязан упасть (fail-closed уже есть).

### 3.9 Golden-набор

13 значений N × схемы × холсты ≈ 174 записи. При `WASTE_WEIGHT=0` rows×cols меняются у **36 из 100** N
(43,44,45,50,51,52,53,54,57,58,59,60,65,66,67,68,69,70,73,74,…), и **ни одно** из них в golden не входит.
Значит «0/174 не меняются» — артефакт покрытия, а не доказательство. Перед изменением веса — golden для
**всех N=1..100** (или хотя бы 36 затронутых + соседи), и осознанное обновление.

### 3.10 WIP-геометрия вне аудита

`grids.rs` переписан (+650), goldens пересозданы, тест pin ослаблен (16:9 → «area ≥ 3.5×»).
Аудит 2026-10-28 («0 пересечений/выходов, 174/174 golden, лучшая часть системы») описывает
**старую** геометрию. Новую нужно либо принять отдельным ADR (dual/модульная матрица — сознательный
дизайн), либо вернуть 16:9 для `pin`. Для релиза это отдельный коммит с обоснованием, а не «шум WIP».

### 3.11 T2: где на самом деле второй источник истины

`selectConstructor` (`codian-layouts.js:1447–1470`) кладёт в редактор слоты, посчитанные
`computeSlots` → `buildSymmetricLayout` (`:696–1025`). Именно эти слоты видит пользователь и сохраняет
PATCH. Применение (`gateways.rs:1880–1905`) берёт геометрию **из preset id**, клиентские x/y не
перекрывают. Т.е. даже после удаления перехвата `calculate` preview ≠ apply для `vks_*`. Полное
решение: `selectConstructor` → `POST /calculate` (кэш по `preset+size`) → `setSlots(ответ)`;
`buildSymmetricLayout`/`computeSlots`/`formatCalculatedSlots`/перехваты удаляются. Для 6 плиток
конструктора — либо тот же кэш, либо статические SVG. Промежуточный безопасный шаг — удалить только
перехват (−117), затем движок (−337).

### 3.12 `-in` ID: починка на 1 функцию

`parse_participant_id` уже сканирует скользящие окна и находит UUID в `sip-<uuid>-in@sip-gateway`
(`vks-server/src/lib.rs:385–410`). Режет только guard `layout_identifier_has_uuid` (`gateways.rs:1279–1289`,
suffix «последние 36») → фильтр `:1773`. Фикс: один хелпер «есть ли UUID внутри» (общий с
`parse_participant_id`), + тесты для `-in`/`-out`/`part-room-…`. Это P0: участник не садится в окно.

### 3.13 Security: детали, которые ломают наивный фикс

- `/metrics` и три API — вне auth (`vks-server/src/lib.rs:154–193`). UI их не вызывает (проверено).
- Тесты `network_integration.rs:2095–2146` дёргают `/metrics` и `/api/v3/mcu/farm` через `oneshot` **без**
  `Authorization` → после закрытия покраснеют; нужно добавить `Extension(ClientIp(127.0.0.1))` или токен.
- `main.rs:476` использует `into_make_service_with_connect_info::<SocketAddr>` → loopback-политика для
  `/metrics` реализуема. Политика: `/metrics` — loopback-only (Prometheus в репо не настроен) **или**
  `VKS_METRICS_TOKEN`; три `/api/v3/*` — `auth + moderator` (401/403).
- 200-контракт не менять: у `/mcu/farm` и `/cluster/status` форма ответа используется тестами.

### 3.14 Релиз и откат (в v1 отсутствует)

- JS-фикс действует **после reload** без рестарта (статика с диска); Rust-фикс требует
  rebuild + restart. Живой бинарь (11:44) старше правок (17:40) — сегодня инвариант в рантайме не активен.
- Рестарт 8080 — только по явному согласию пользователя (в ревью запрещён). Перед рестартом: копия
  `target/release/vks-server` → `…/vks-server.prev`, копия `vks_data.db`, `git status` чистый,
  `sha256sum -c MANIFEST`, `curl` smoke.
- `.cargo/config.toml:9–19` (WIP) фиксирует `-C target-cpu=native` и обещает «sha256 воспроизводим».
  На другом CPU sha другой; для прода — `x86-64-v3`. В DoD писать «sha + `rustc -vV` + CPU», а не
  «воспроизводимость».
- `target-impl`/`target-proto` не удалять, пока не скопирован `target-impl/release/vks-server`
  (в паспорте замеров он может быть нужен); `target/debug` (46 G) не трогать до релиза.

### 3.15 E2E без SIP

Для TypeError не нужен sipbot: достаточно, чтобы отрендерилась хотя бы одна строка абонента
(компонент с `hasIndividualLayout` — «column-4» в таблице участников) **или** открылся попап
трансляции (`SessionBroadcastPopup.created`). В БД нет контактов/call_strings для `49b599d7`, поэтому
репро надо строить так: (1) открыть страницу битой сессии → попап «Трансляция/Запись» → pageerror;
(2) если пусто — добавить SIP-адрес (строка появляется до ответа) или подключить WS-пир из
`tests/e2e/webrtc.js`. Живой SIP/H.323 для этого не нужен.

---

## 4. Сокращение и перегруппировка

| v1 | Решение | Причина |
|---|---|---|
| T0 | **оставить первым**, убрать «HEAD не собирается», добавить провенанс/тег/копию БД | блокер воспроизводимости |
| T1 | **сократить** до 3 правок (identity, create-type, deletable/alias) + e2e; (b) — 1 строка гигиены; (c) — отдельный коммит-страховка | 80 % уже сделано |
| T2 | **разделить**: T2a перехват (−117), T2b движок (−337) | разный риск/UX |
| T3 | **разделить**: T3a `WASTE_WEIGHT=0` + golden 1..100 (слить с T2a), T3b `grid.rs` центрирование (P2) | T2a/T3a — один parity-критерий; T3b — кадры MCU |
| T4 | **поднять** сразу после T1 (`-in` — P0), остальное (unique position, size) — туда же | функциональный P0 |
| T5 | **разделить**: T5a статус (header+метрика), T5b auto-expand event (только при наличии потребителя), T5c canvas race, T5d bitrate | разные риски |
| T6 | **разделить**: T6a WIP-commit/gitignore/провенанс (в S0), T6b dead code (2 коммита), T6c CI, T6d deps — **выкинуть** | deps = nightly+риск, ценность ≈ 0 |
| T7 | **поднять** перед T6/T8 | 30 строк, экспозиция живая |
| T8 | **переформулировать**: T8a `source_latch` (P1), T8b DTMF (проверить+тест, уже есть), T8c H.323 late video (вне CI), T8d RTCP (закрыть документацией/тестом) | часть уже в WIP; часть нельзя закрыть в CI |
| T9 | оставить, добавить restart-gate, rollback, MANIFEST, sha | — |

**T2+T3a одной задачей — да.** Оба про «один источник геометрии» и проверяются одним e2e-parity:
серверный `/calculate` == геометрия, применённая движком == то, что в `slots` редактора. Меньше кода:
удаляем перехват + `formatCalculatedSlots` + `buildSymmetricLayout`/`computeSlots` (−450), добавляем
один async-хелпер `serverGeometry(preset,w,h)` с кэшем (+30) и один Rust-тест parity. `WASTE_WEIGHT=0`
без golden 1..100 не принимать. **T3b (grid.rs) — отдельно**: это меняет кадры MCU, а не превью.

**Что выкинуть из плана совсем:** удаление неиспользуемых зависимостей (T6.4); удаление
`target-impl/proto` до релиза; WEBGL-warning патч (не про этот краш); `build_pli/build_fir`
(это тест-билдеры `rtcp.rs`); отдельную задачу «frontend type=individual» с тестами.

**Что сделать «сначала» (риск vs эффект):** S0 провенанс → S1 инвариант default (2 правки) → S2 `-in`
(P0) → S3 auth (P1) → S4 T2a (−117, видимый баг 1 px/выход за холст) → S6 T3a+golden → S5 T2b.

---

## 5. Выровненный план v2

Нумерация S* (stage), в скобках — соответствие v1. Общая оценка объёма прод-кода: **+120 / −500**.

### S0 (T0) — Провенанс и снимок (blocker, 0 прод-кода)

- **Файлы:** `.gitignore`, `.cargo/config.toml` (комментарий), этот отчёт; коммит/тег.
- **Шаги:** 1) `git switch -c fix/2026-10-28`; 2) checkpoint-коммит **всего** WIP, включая untracked
  (`farm.rs`, `rtcp.rs`, `farm_metrics.rs`, `patch-pnm-layout-crash.py`, `docs/audit/2026-10-*`,
  `scripts/bench`, challenger-e2e) + тег `audit-2026-10-28-base`; 3) `git status` пуст; 4) копия
  `vks_data.db` в `docs/audit/2026-10-28-fix/evidence/`; 5) записать sha256/`rustc -vV`/CPU/mtime
  живого бинаря; 6) `sha256sum -c MANIFEST` (214/214); 7) пересобрать `--release --locked`, сравнить
  поведение (sha не сравнивать — `native`); 8) в `.cargo/config.toml` исправить обещание
  «воспроизводимого sha» или перейти на `x86-64-v3`.
- **DoD:** тег есть, дерево чистое, отчёт с числами, БД-снимок, MANIFEST OK, `cargo check` зелёный.
- **Тесты:** нет (гейт для остальных).
- **Риск/откат:** нет; ничего не удалять, сервис не трогать. **Строки:** 0 (только `.gitignore` уже +2).
- **НЕ делать:** не удалять `target/debug`/`target-impl`, не перезапускать 8080, не «чинить» HEAD.

### S1 (T1, P0) — Инвариант «Общая»: доделать 3 правки

- **Файлы:** `crates/vks-admin/src/api_v3/resources.rs` (1142, 1303–1345, 1559–1576), `state.rs:667`,
  `client/pnm-frontend/deployed/pnm/codian-layouts.js:408`.
- **Шаги:** 1) `has_default`/ремонт — по `payload.type=="default"` (identity), `is_canonical_default`
  оставить только для seed/дедупа; 2) `create`: whitelist `default|individual|active_speaker`;
  пусто→`individual`; неизвестно→`422 invalid.type`; 3) ремонт: сначала alias→linked-клон
  (`type=default`, durable при загрузке), иначе виртуальный канон с `defaults.deletable=false`;
  4) `normalize_loaded`: чинить отсутствующий/неизвестный `type`; 5) удалить форс `type` в
  `restoreIdentity` (1 строка, сохранять `identityType`); 6) e2e-репро.
- **DoD:** (а) PATCH `{type:"individual"}` на default → ответ `default`; (б) `POST type:"foo"` → 422;
  (в) сессия без default в БД → `GET …/video_layouts` содержит ровно один default, строка не удаляема;
  (г) два `POST /sessions` подряд → ровно один default; (д) e2e: битая сессия + попап трансляции →
  0 pageerror; (е) битая сессия `49b599d7` рендерится, «Codian 07 — 13 окон» = «Общая».
- **Тесты:** `t00_*` (уже), + «no duplicate default on second start», + «create invalid type 422»,
  + «alias repair», + `resolved_layout_abonents` после флипа; e2e `test_layout_default_invariant.js`.
- **Риск/откат:** средний (данные). Откат — revert; durable-ремонт откатывается из копии БД S0.
- **Строки:** **+25 / −5** прод, +90 тестов, +70 e2e.
- **НЕ делать:** не менять 200/201-контракт; не возвращать 422 на PATCH `type` (вендор шлёт его всегда);
  не патчить все бандлы — только 2 сайта × 2 сборки (уже сделано).

### S2 (T4a, P0) — `-in` ID садится в окно

- **Файлы:** `crates/vks-server/src/gateways.rs` (1279–1289, 1773), `crates/vks-server/src/lib.rs`
  (рядом с `parse_participant_id:385`).
- **Шаги:** общий хелпер `has_embedded_uuid` (переиспользовать логику окон из `parse_participant_id`);
  `layout_identifier_has_uuid` → на него; тесты на 6 форм (`sip…`, `sip…-in@…`, `h323…-in@…`,
  `part-room-…`, `cs-1`, имя).
- **DoD:** входящий SIP/H.323 (`-in`) получает слот; тест на `assignments` из payload с `-in` id.
- **Тесты:** unit (gateways) + интеграционный на `apply_layout` с `-in`.
- **Риск/откат:** низкий; revert. **Строки:** **+12 / −4**, +30 тестов.
- **НЕ делать:** не менять `parse_participant_id` (он уже корректен), не трогать эвристику хеширования.

### S3 (T7, P1) — Auth на мониторинге и метриках

- **Файлы:** `crates/vks-server/src/lib.rs:154–193`, `crates/vks-server/tests/network_integration.rs:2095–2146`.
- **Шаги:** три `/api/v3/*` — под общий auth+mоderator (401/403); `/metrics` — loopback-only по
  `ConnectInfo` (есть, `main.rs:476`) или `VKS_METRICS_TOKEN`; тесты — с `ClientIp`/токеном; добавить
  тесты anon→401/403 и admin→200.
- **DoD:** anon → 401/403 на всех четырёх; UI/CI не сломан (UI их не зовёт).
- **Тесты:** `observability/tests` + network_integration (обновить существующие oneshot-вызовы).
- **Риск/откат:** низкий (UI не использует); revert. **Строки:** **+25 / −3**, тесты ±20.
- **НЕ делать:** не менять тела ответов; не закрывать `/healthz`,`/readyz`; не вводить RBAC сверх moderator.

### S4 (T2a+T3a, P1) — Один источник геометрии, шаг 1: сервер решает превью

- **Файлы:** `codian-layouts.js:1690–1833`, `crates/vks-layout/src/grids.rs:280`, golden fixture.
- **Шаги:** 1) удалить fetch/XHR-перехват `calculate` и `formatCalculatedSlots`, оставить только
  инвалидацию `serverCount` после save (5 строк) или убрать её вовсе; 2) `WASTE_WEIGHT=0` (+ тест
  «16:9-ячейки предпочтительнее»); 3) golden для **N=1..100** (обновить осознанно, с diff-ревью);
  4) parity-e2e: `slots`, которые показывает редактор после выбора схемы, == ответ `/calculate` ==
  `slot_rects` движка (допуск 0 px).
- **DoD:** в сетевом логе e2e **нет** запросов, синтезированных клиентом; 0 overlap/OOB на 6 режимах ×
  N=1..100; для 100 N rows×cols совпадают с новым golden.
- **Тесты:** Rust golden 1..100 + `grids.rs` инварианты; e2e parity; unit на `gallery_cost`.
- **Риск/откат:** средний (визуальные изменения для 36 N). Откат — `WASTE_WEIGHT=20` + старый golden.
- **Строки:** **+5 / −120** прод, fixture ±, +40 тестов.
- **НЕ делать:** не менять Codian/manual-пресеты (65+54 id), не трогать `grid.rs` здесь.

### S5 (T2b, P1) — Шаг 2: клиент больше не считает геометрию (−337)

- **Файлы:** `codian-layouts.js:696–1025, 1027, 1447–1470`.
- **Шаги:** async `serverGeometry(preset,count)` с кэшем (`Map`) и seq-guard от гонок; `selectConstructor`
  → `setSlots(ответ)`; 6 плиток режимов — из того же кэша (или статические SVG); удалить
  `buildSymmetricLayout`/`computeSlots`.
- **DoD:** `grep buildSymmetricLayout` — 0; выбор схемы даёт те же слоты, что и `/calculate`;
  сохранение работает при медленной сети (кнопка/секвенс не даёт записать пустые slots).
- **Тесты:** e2e по 6 режимам × N=1,7,13,43,100 (slots == сервер); unit на кэш/seq.
- **Риск/откат:** выше среднего (гонка/UX). Откат — вернуть функцию из git (1 файл, без рестарта).
- **Строки:** **+40 / −400**.
- **НЕ делать:** не менять вендорский store, не вводить второй кэш «на всякий случай», не оставлять
  фолбэк на локальный расчёт (иначе снова два источника).

### S6 (T3b, P2) — `grid.rs`: центрирование неполного ряда

- **Файлы:** `crates/vks-layout/src/grid.rs:48–89`, тесты `grid.rs`.
- **Шаги:** `base_w = w / cols` (а не `w/count`), `row_left = margin + (w - row_width)/2`; обновить
  инвариант «total area == canvas.area» и golden/юнит-тесты для N≡1 mod cols.
- **DoD:** N=43 → 7×7 с центрированной одиночной плиткой, без растяжения на всю ширину.
- **Тесты:** unit на N≡1 (mod cols) для 3 холстов; инварианты без пересечений.
- **Риск/откат:** низкий (косметика кадров); revert. **Строки:** **+10 / −5**.
- **НЕ делать:** не менять `grids.rs` (это превью), не трогать `preset_default_13`.

### S7 (T4b, P1) — Назначения: уникальность и размер

- **Файлы:** `resources.rs` (`attach_layout_abonent`), `gateways.rs` (`apply_layout`), общий
  `layout_size()` хелпер.
- **Шаги:** 409 при `position` вне `size`/дубле; один хелпер размера (`payload.size` ∨ `preset_*` ∨
  `slots.len()`), заменить 3 копии; проверка `attach > size` уже есть (resources.rs:865) — покрыть тестом.
- **DoD:** дубль позиции → 409 с reason; `layout_size` — единственная реализация.
- **Тесты:** unit/integration на 409/дубль/размер.
- **Риск/откат:** низкий; revert. **Строки:** **+20 / −15**.
- **НЕ делать:** не менять формат `position_base`, не вводить «умную» перенумерацию.

### S8 (T5a, P2) — Честный статус применения

- **Файлы:** `sessions.rs:496–509`, `gateways.rs:1895–1925`.
- **Шаги:** `push_layout_to_engine` возвращает результат; заголовок `x-vks-layout-applied:
  engine|pending|saved-only` в ответ PATCH/attach; метрика `vks_layout_apply_total{result}`; 200 не менять.
- **DoD:** UI может отличить «сохранено» от «применено»; метрика видна.
- **Тесты:** integration на 3 исхода; метрика в `/metrics`.
- **Риск/откат:** низкий. **Строки:** **+40 / −5**.
- **НЕ делать:** не делать 5xx на pending; не менять тела ответов.

### S9 (T6, гигиена) — 3 маленьких коммита

- **T6b dead code:** `vks-layout/src/viewer.rs` + `lib.rs:50`; `MockVideo*` + `vks-media-mcu/src/lib.rs:61`;
  `openapi.json` (файл); `codian-layouts.js:141,1418`. DoD: `cargo check`/тесты зелёные, grep — 0.
  Строки: **−250** (в основном удаление).
- **T6c CI:** джобы `manifest-check` (`sha256sum -c`), `pnm-patch-check` (все `patch-pnm-*.py --check`),
  `layout-golden`; e2e — не в CI (нет сети/живого контура), только локально.
- **T6d deps — выкинуть** из плана (нужен nightly/`cargo-machete`, риск для feature-gated сборок).
- **НЕ делать:** не удалять `build_pli/build_fir`, не удалять `target-impl/proto` до релиза,
  не «чистить» историю git.

### S10 (T8a/b, P1) — Шлюзы: звук и DTMF (после изолированного контура)

- **Файлы:** `gateways.rs:3667` (`source_latch`), `gateway_audio.rs:605–610`, `uas.rs:236,309–310`,
  `h323_listener.py` (UII).
- **Шаги:** 1) `source_latch` — политика + reason-код вместо «PT mismatch» (лог различим);
  2) DTMF clock — уже сделано, добавить тест `telephone-event/<clock>` для 8k/48k и H.323 UII;
  3) H.323 late video — только при живом пире, вне CI; 4) RTCP — честная метрика уже есть, закрыть
  документацией + тестом счётчика.
- **DoD:** 0 логов «PT mismatch» с `expected==received`; тон DTMF длится ~200 мс при Opus/48k.
- **Тесты:** unit (uas/rtcp) + ручной прогон SIP/H.323 на изолированном контуре (не 8080).
- **Риск/откат:** высокий (медиа). Откат — по конфигу `source_latch`.
- **Строки:** **+30 / −10** (+ тесты).
- **НЕ делать:** не менять SDP-матрицу кодеков, не включать `source_latch` по умолчанию без контура.

### S11 (T9) — Финал и релиз

- **Шаги:** `cargo fmt --check`, `clippy -D warnings`, `test --locked`, `deny`, `doc`, `build --release
  --locked`; `sha256sum -c MANIFEST` (214); полный Playwright-прогон (`live.js`, `webrtc.js`,
  `test_layout_constructor.js`, новый `test_layout_default_invariant.js`); `git status` пуст;
  бэкап бинаря+БД; **согласованный** рестарт; smoke (`/pnm/`, `/healthz`, сессия, раскладка).
- **DoD:** живой бинарь = собранный из тега, 0 pageerror на сессии, «Общая» одна, метрики закрыты.
- **Риск/откат:** откат — `vks-server.prev` + копия БД; JS — `git checkout` файла.
- **НЕ делать:** не рестартовать без подтверждения; не «дочинять» код в процессе приёмки.

---

## 6. T1 меньшим кодом: эталонная реализация

| Обязательно | Зачем | Лишнее / можно не делать | Почему |
|---|---|---|---|
| `type` immutable в `update_resource` | единственная гарантия на write-path; вендорский редактор всегда шлёт `type` | 422 на смену типа | сломает вендорский save-flow |
| identity «Общей» по `payload.type=="default"` | иначе дубли после рестарта (§3.1) | хранить «каноничность» отдельным флагом в payload | новый контракт, миграция, UI не знает |
| `create`: whitelist/нормализация типа | вторая дверь (§3.4) | менять формат `payload` | контракт 200 |
| Ремонт legacy: alias→clone→`default` (durable) + fallback virtual | данные уже битые (БД §3.3) | отдельный migration-скрипт/CLI | 10 строк в `normalize_loaded` дешевле |
| `defaults.deletable=false` у виртуальной строки | иначе 404-тост (§3.2) | скрывать кнопку в JS | правка вендора без нужды |
| Вендорский guard (2 сайта × 2 сборки) | чёрный ящик; защищает и от «пустого списка» | патчить `index.bundle`/`index-legacy` | там только guarded store-computed |
| Тесты: immutable, no-dup, create-422, alias-repair | критерии приёмки | тест на фронтовый форс типа | сервер авторитетен |
| e2e: битая сессия + попап → 0 pageerror | доказывает фикс | sipbot/живой SIP | строка абонента/попап достаточно |

**Эталонный объём:** Rust **+25/−5** (update уже есть), JS **−1/+3**, патч-скрипт +70 (уже есть),
тесты +90, e2e +70. Всё остальное (переписывание `payload.type` у всех записей, миграция БД,
фронтовый фикс как отдельная задача, WEBGL-патч, «422 на edit») — **лишнее**.

---

## 7. С чем не согласен (аргументы)

1. **«HEAD 543bc7b не собирается без untracked».** Неверно: `git grep farm|rtcp|farm_metrics HEAD -- '*.rs'`
   — 0; HEAD-`lib.rs` не содержит `pub mod farm`/`farm_metrics`. HEAD — валидная точка отката. Блокер —
   провенанс живого бинаря (11:44) относительно правок (17:40).
2. **«T1(a): гарантия canonical default в visible set».** Идентичность по точному payload — источник
   нового бага (дубли). Инвариант должен быть `≥1 layout с payload.type=="default"` на scope.
3. **«T1(b) frontend — сохранять исходный type».** Как фикс не нужен: сервер игнорирует смену типа.
   Как гигиена — 1 строка. Делать из этого подзадачу с тестами — трата.
4. **«T1(c) — часть P0».** Это страховка представления (P2), доказанная e2e; сам P0 закрывается
   серверным инвариантом. Порядок: после Rust-фикса, отдельным коммитом.
5. **«T2: −70…−170 строк».** Реальный дубль — 337-строчный `buildSymmetricLayout` + `selectConstructor`;
   без него preview≠apply остаётся. Оценка должна быть −450/+40, разбитая на два шага.
6. **«T3: 0/174 golden не меняются».** Правда только для 13 значений N; 36 из 100 N меняются, и их в
   golden нет. Критерий «0/174» вреден: он «докажет» безопасность непокрытого изменения.
7. **«T4 (P1) после T2/T3».** `-in` — P0 (участник не в окне), фикс — 1 функция. Должно идти сразу
   после T1.
8. **«T7 после T6».** Security — 4 эндпоинта, 30 строк, UI их не вызывает. Живая экспозиция в LAN
   важнее удаления `viewer.rs`.
9. **«T6: build_pli/build_fir — мёртвый код».** Нет: это тест-билдеры `rtcp.rs:185–233`; прод их не
   зовёт, но удаление обнулит roundtrip-тесты. Из плана убрать.
10. **«T8: реализовать DTMF clock / RTCP / H.323 events».** Уже в WIP (`uas.rs:236,309–310`, `rtcp.rs`,
    `h323_listener.py:459,578–597`); осталось верифицировать тестами и закрыть TODO по ферме.
11. **«T6.4 удалить неиспользуемые зависимости».** Ценность ≈ 0, нужен nightly/`cargo-machete`, риск
    для feature-gated крейтов. Выкинуть из плана.
12. **«T9: release-перезапуск».** Без явного согласия пользователя нельзя (в ревью запрещён). Плюс
    надо учитывать, что JS-фикс уже действует без рестарта, а Rust-инвариант — только после него.

---

## 8. Ход исполнения (обновление 2026-10-28, HEAD `0c83d6e` + рабочие правки)

| Stage | Статус | Коммиты / доказательства |
|---|---|---|
| S0 провенанс | **готово** | 01/02/04 обновлены: срез 17:52 помечен WIP, хэши привязаны к `645120d`/`22aac56`/`26c300a`/HEAD, интервалы записей переписаны; evidence-предупреждение в 04 |
| S1 инвариант «Общая» | **готово** | `22aac56` (identity по `payload.type`, type immutable, repair), `7694a0c` (M1-R1: один default на scope), `26c300a` (cross-scope: видимый scope union), `f3009ff` (M1-R2: идемпотентный re-attach в COW, alias-aware guard) |
| S2 `-in`/`part-room-` id | **готово** (в WIP-коммитах) | `embedded_uuid` + `parse_participant_id`, тесты `network_integration` |
| S3 auth `/metrics` + `/api/v3/*` | **готово** (в WIP-коммитах) | `metrics_guard` (loopback или токен; оба заголовка), auth+Moderator на служебных API, `metrics_tokens` unit-тест |
| S4 T2a перехват calculate | **готово** | `501354e` (удалены fetch/XHR-перехваты и `formatCalculatedSlots`, −117) |
| S5 T2b клиентский движок | **готово** | `501354e` (удалён `buildSymmetricLayout`/`computeSlots`, −337; серверный `serverGeometry` + parity/UI e2e) |
| S6 T3a `WASTE_WEIGHT` | **не начато** | решение отложено: меняет кадры MCU, нужен golden 1..100 до изменения; текущий серверный `grids.rs` — единственный источник геометрии |
| S7 назначения (unique/size) | **не начато** | план: 409 на дубль position и `layout_size()`-хелпер; текущий `attach_layout_abonent` уже проверяет `occupied >= limit` |
| S8 честный статус apply | **не начато** | план: `x-vks-layout-applied` + метрика `vks_layout_apply_total` |
| S9 гигиена | **частично** | `77e36a1` (viewer.rs, MockVideo*/трейты, openapi.json, `onConstructorActivate`), `951e68b` (stopPropagation, шапка, MANIFEST), `scripts/smoke_v3.sh` (individual вместо второго default); осталось: CI-джобы manifest/patch/golden, `.cargo/config.toml` target-cpu, 42/44 env-переменные |
| S10 шлюзы audio/DTMF | **не начато** | по плану — только после изолированного контура (живой 8080 не трогать) |
| S11 финал/релиз | **гейты, без рестарта** | `cargo test --workspace --locked` — 49/49 ok; network_integration 23/23; clippy/fmt clean; release собран 19:21; MANIFEST 214/214; Playwright: guard clean/legacy, parity, constructor 20/20. Живой бинарь (PID 1814143) **не перезапускался** — нужен согласованный restart-gate |

**Найденные по ходу ревью дефекты, закрытые вне исходного плана:** ложнозелёные e2e (pageerror-whitelist и provenance иконок — `0c83d6e`), двойной `select()` Codian (`951e68b`), JWT в argv bench (`951e68b`, `f3009ff`), метрики: неверный Bearer глушил корректный `X-Metrics-Token`, stale target dir с `CARGO_MANIFEST_DIR=/tmp/...` (чинится `cargo clean -p vks-gateway-h323`).

## Вердикт

**План v1 — не принят: 5.3/10** (полнота 6.0, порядок 4.0, риск 5.5, элегантность 4.5, проверяемость 6.5).
Он описывает в основном уже сделанную работу (T1), ставит P0-задачи после косметики и недооценивает
объём T2.

**План v2 (§5) — принят условно: 9.0/10 сейчас, 9.5/10 после шести неотложных правок:**
(1) identity «Общей» по `payload.type` вместо канона; (2) валидация типа на create;
(3) alias-based durable ремонт + `deletable=false` у виртуальной строки; (4) порядок S1→S2(`-in`)→S3(auth);
(5) T2 разбить на −117 и −337, T3a слить с T2a; (6) golden N=1..100 до `WASTE_WEIGHT=0`.

**Что изменил относительно v1:**
- переставил приоритеты: `-in` (P0) и auth — вперёд геометрии/гигиены;
- сократил T1 до 3 правок + e2e (80 % уже в дереве), убрал T1(b)(e) как самостоятельные задачи;
- разбил T2 на T2a/T2b и поднял оценку экономии с −70…−170 до −450/+40;
- разбил T3 на T3a (слито с T2a) и T3b (`grid.rs`), заменил ложный критерий «0/174» на golden 1..100;
- разбил T5/T6/T8 на коммиты с независимыми рисками; выкинул удаление зависимостей, WEBGL-патч,
  `build_pli/fir` и удаление `target-debug` из объёма итерации;
- добавил провенанс/тег/копию БД/rollback/restart-gate и обновление `network_integration.rs` при
  закрытии auth;
- зафиксировал точный ремонт данных через `layout_aliases` (в БД есть алиас COW «Общей»).

# 07. Серверная цепочка `role` для Codian-раскладок: где теряется, патч, геометрия

Агент `chain`, read-only по продакшн-коду (записан только этот файл). Метод: полное чтение `api_v3/shell.rs`,
`api_v3/resources.rs`, `api_v3/sessions.rs`, `vks-layout/{preset,codian,roles,grids,lib}.rs`, `gateways.rs`
(`apply_layout`), миграций `vks-storage`; live с токеном: `POST /api/v3/login` → `GET /ui/video_layouts/presets` →
`POST /ui/video_layouts/calculate` на `:8080`. Сервис **не перезапускался**, БД не менялась (чтение только через API).
Рабочее дерево (HEAD `543bc7b` + uncommitted) **уже содержит** патч роли (`shell.rs` +14, `resources.rs` +41,
`tests.rs` +14, `resource_tests.rs` +24, `codian-layouts.js` +728) — ниже сверка его полноты по цепочке, остаточные
дыры, готовый дифф и тесты. На момент аудита шёл чужой `cargo build --release -p vks-server` — тесты я не запускал.

## A. Точки, где слоты уходили в JSON без `role`, и что читает UI

| # | Путь | Код (HEAD, до патча) | Live/факт |
|---|---|---|---|
| 1 | Каталог `video_layout_presets` | `shell.rs:785-799`: `json!({position,x,y,width,height,z_order})` — поля `role` нет | live `GET /presets`: **119** пресетов, **54** Codian, **507** слотов, **0** с `role` |
| 2 | `video_layouts_calculate` | `shell.rs:755-762`: тот же набор без `role` | live 200 для `codian_*`/`vks_*` (не 404); ответ без `role` |
| 3 | CRUD create | `resources.rs:414-437` (`normalize_resource` → `merge_json`), валидация `:1171-1181` — только «слоты-объекты» | role не дефолтится и не проверяется |
| 4 | CRUD update | `resources.rs:1232-1266` (`update_resource`), `merge_layout_slots:1201-1231` — поля переносятся как есть | PATCH со слотом без `role` сохраняет payload без `role` |
| 5 | Read (global/linked) | `list_global:1299`, `get_global:1328`, `list_linked:1465-1508`, `get_linked:1511-1541` отдают `payload.slots` как сохранён | `abonents[].role ?? "AUTO"` (`:530`) — это **не** `payload.slots`; прозрачность он не лечит |
| 6 | Boot-гидратация | `normalize_loaded:980-1033` (вызов `state.rs:664`) фильтрует не-объекты в `payload.slots` | после рестарта роль не достраивалась; commit — `resources.rs:250-267` |
| 7 | Write (`attach`/`detach`) | attach пишет `"role":"PEER"` (`:854`, `:869`), detach — `"role":"AUTO"` (`:931`) | **асимметрия**: вендорский словарь на записи уже используется, обратной достройки нет |
| 8 | Другие сериализаторы слотов | только `preset.rs:1153-1168` (тестовый helper крейта `vks-layout`); прод-путей больше нет | — |

Почему отсутствие `role` = прозрачное окно (код вендора, `chunks/index6.bundle-2.2.1.66.js`):
`slotClass(){return{EMPTY:"layout-grid-slot--empty",ACTIVE:"layout-grid-slot--active-speaker",AUTO:"layout-grid-slot--autofill",
PEER:"layout-grid-slot--peer",ROLL:"layout-grid-slot--roll",PRESENTATION:"layout-grid-slot--content"}[slot.role]}`;
`slotIcon(){return role!=="EMPTY"?icons[role]:""}`; `icons={EMPTY,ACTIVE,AUTO,ROLL,PEER,PRESENTATION}` — ровно 6 ключей.
CSS (`chunks/index7.bundle-2.2.1.66.css`): `.layout-grid-slot{…}` **без** `background-color`; фон дают только
`--empty #333`, `--peer #5c5c5c`, `--autofill #715839`, `--active-speaker #6a403c`, `--roll #3c445f`,
`--content #2f5e40`. `role: undefined` → нет класса и иконки → окно прозрачное.

Слоты дальше по цепочке роль **не читают**: `gateways.rs:1721-1796` (`apply_layout`: только `position`−`position_base`,
ID (`abonent_id/connection_id/contact_id/call_string_id`), `x/y/width/height/z_order`); `LayoutSlot`
(`vks-core/src/ports/media_engine.rs:139-151`) поля `role` не имеет; геометрия применяется в пространстве
1920×1080 (`gateways.rs:1836-1837`, `:1868`), движок пересчитывает в холст комнаты (`engine.rs:188-224`).
`sessions.rs:458` (`current_layout_json`), `:491-507` (`push_layout_to_engine`) роль не трогают. Хранилище схемы
роли не знает: UI-layout'ы лежат JSON-документом `pnm-definitions-v1` (`resources.rs:241`) в `configuration_documents`
(`0005_configuration_documents.sql`, `core_adapter.rs:243-251`); таблицы `layouts` (`0001:26`) и `core_layouts`
(`0002:3`) — другие сущности. Вывод: лечить можно **только на выдаче/записи API**.

## B. Два пространства имён ролей — не смешивать

| | Вендорский UI (JSON `slot.role`) | `vks_layout::SlotRole` |
|---|---|---|
| Значения | `EMPTY ACTIVE AUTO PEER ROLL PRESENTATION` (UPPERCASE) | `default fixed active rolling` (`roles.rs:22-34`, `rename_all="snake_case"`) |
| Источник | `slotClass`/`icons`/`GridSlotRoleEditor` бандла | спецификация Codian 4200, `codian.rs:641-691` |
| Потребители | редактор раскладок в браузере | только тесты: `codian_layouts.rs:153`; `assign_by_role`/`rotation_state` в проде не зовутся (только реэкспорт `lib.rs:49`) |
| Совместимость | `FIXED` в `slotClass`/`icons` **отсутствует** | `active`/`rolling` — семантика MCU, не отображение |

Решение: JSON-ключ остаётся `role` (контракт вендора — переименование в проводе сломало бы UI), но **тип** на
сервере — отдельный `UiRole` в admin-слое, а не `SlotRole` (`serde_json::to_value(SlotRole)` не используется нигде).
Маппинг — в одном месте (`crates/vks-admin/src/api_v3/ui_role.rs`):

```rust
// UI-словарь вендора (slotClass/icons в chunks/index6...js). Ровно 6 значений.
pub(crate) const UI_ROLES: [&str; 6] = ["EMPTY", "ACTIVE", "AUTO", "PEER", "ROLL", "PRESENTATION"];
pub(crate) fn is_ui_role(value: &str) -> bool { UI_ROLES.contains(&value) }

/// Слот шаблона/сохранённой раскладки → вендорская UI-роль.
/// occupied — есть abonent_id/contact_id; spec — роль из `codian::roles()` (только custom_14/22).
pub(crate) fn ui_role(occupied: bool, z_order: i64, spec: Option<vks_layout::SlotRole>) -> &'static str {
    if z_order > 0 { return "PRESENTATION"; }                                // PiP/overlay
    if let Some(vks_layout::SlotRole::Active) = spec { return "ACTIVE"; }   // Default→AUTO, Rolling→ROLL,
    if let Some(vks_layout::SlotRole::Rolling) = spec { return "ROLL"; }    // Fixed→AUTO (в UI нет FIXED)
    if occupied { "PEER" } else { "AUTO" }
}
```

## C. Минимальный патч

### C1. Каталог: `AUTO` для пустых окон, не `EMPTY`

`setFreeUsers()` (бандл, `ManualLayoutConfigurator`) заполняет **только** `role==="AUTO"`
(`…if(t.role!=="AUTO"||!e.length) return t; … role:"PEER"`); `hasAutofillSlots`/`hasNoAutofillSlots` включают кнопки
«заполнить свободные»/«очистить». `EMPTY` — «явно пустое окно, не автозаполнять» (`clearSlots`→`setSlots(key,null)`),
у него нет иконки (`slotIcon`→`""`). Окно каталожного шаблона — место под участника, значит **`AUTO`**; это же
правило уже использует detach (`resources.rs:931`). Наложенные PiP (`z_order>0`) — `PRESENTATION` (у вендора это
«контент/оверлей»-роль; z_order всё равно несёт порядок отрисовки; строгая альтернатива — `AUTO`, одна строка в
`ui_role`, решение Lead — оставить `PRESENTATION`). Для `codian_custom_14/22` роли заданы спецификацией — их можно
отдать через `ui_role(.., spec)` (P2).

### C2–C4. `calculate`, read-path, update — что уже в дереве (unified diff)

```diff
--- a/crates/vks-admin/src/api_v3/shell.rs
+++ b/crates/vks-admin/src/api_v3/shell.rs
@@ -694,0 +695,11 @@
+/// UI-роль окна в словаре вендорского конструктора PNM (строго UPPERCASE …)
+fn ui_slot_role(z_order: u8) -> &'static str {
+    if z_order > 0 { "PRESENTATION" } else { "AUTO" }
+}
@@ -754,6 +766,7 @@ pub async fn video_layouts_calculate(…)
             json!({
                 "position": position + 1,
+                "role": ui_slot_role(slot.z_order),
                 "x": slot.rect.x,
@@ -789,6 +802,7 @@ pub async fn video_layout_presets()
                 json!({
                     "position": position + 1,
+                    "role": ui_slot_role(slot.z_order),
                     "x": slot.rect.x,
--- a/crates/vks-admin/src/api_v3/resources.rs
+++ b/crates/vks-admin/src/api_v3/resources.rs
@@ -1190,0 +1191,22 @@ fn ensure_slot_roles(record: &mut Value) {
+fn ensure_slot_roles(record: &mut Value) {
+    let Some(slots) = record["payload"]["slots"].as_array_mut() else { return };
+    for slot in slots.iter_mut().filter(|slot| slot.is_object()) {
+        if slot.get("role").and_then(Value::as_str).is_some_and(|role| !role.is_empty()) { continue; }
+        let occupied = slot.get("abonent_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty())
+            || slot.get("contact_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty());
+        slot["role"] = json!(if occupied { "PEER" }
+            else if slot.get("z_order").and_then(Value::as_i64).unwrap_or(0) > 0 { "PRESENTATION" }
+            else { "AUTO" });
+    }
+}
```

Вызовы (все в дереве): `normalize_resource:433` (create), `with_layout_abonents:538` (room-scoped list/get),
`normalize_loaded:995` (boot, `state.rs:664`), `update_resource:1276` (save), `list_global:1340`, `get_global:1373`.
Идемпотентно (известная роль не перезаписывается, прочие поля не трогаются); БД не мигрируется — boot-нормализация
живёт в памяти и попадёт в документ при ближайшем `commit` (`resources.rs:250-267`).

### Остаточные дыры (готовый дифф, ещё не в дереве)

```diff
@@ pub async fn templates  (resources.rs:2171-2233)
-    let templates = match layout {
-        Some(layout) if layout["payload"]["mode"].as_str() == Some("manual") => { vec![layout] }
+    let templates = match layout {
+        Some(mut layout) if layout["payload"]["mode"].as_str() == Some("manual") => {
+            ensure_slot_roles(&mut layout);   // ручная раскладка отдаётся в редактор, роль обязательна
+            vec![layout]
@@ fn ensure_slot_roles
-        if slot.get("role").and_then(Value::as_str).is_some_and(|role| !role.is_empty()) { continue; }
+        if slot.get("role").and_then(Value::as_str).is_some_and(is_ui_role) { continue; } // "auto"/"MEDIA" ≠ вендорская роль
```

Обоснование второй правки: сейчас «известной» считается любая непустая строка, поэтому сохранённое `"auto"`/`"MEDIA"`
(вендор сохраняет `["PEER","MEDIA"]` как есть) остаётся в проводе и даёт `slotClass[role] === undefined` → снова
прозрачное окно. Закрытый словарь `UI_ROLES` делает контракт проверяемым (тест D5).

## D. Тесты: «role присутствует и валиден» как контракт

Уже в дереве (зелёные у автора патча, я не перезапускал):
`crates/vks-admin/src/api_v3/tests.rs:2537` (`calculate` → `role=="AUTO"`, у всех слотов непустая роль),
`:2753` (каталог: роль ∈ 6 значений, `z_order>0 ⇔ PRESENTATION` — покрывает все 507 слотов 54 схем),
`resource_tests.rs:835` (`calculate` отдаёт роль), `:1814` (`ensure_slot_roles`: AUTO/PEER/contact_id/PRESENTATION/
ACTIVE не перезаписывается/пустая строка). Существующие инварианты каталога не тронуты: `:2728` — 119 = 65+54,
`windows` = числу слотов; `vks-layout/tests/codian_layouts.rs:80` — 54 схемы; `codian_geometry_golden.rs` — 507 окон.

Добавить:
1. `api_v3/tests.rs::legacy_layout_read_path_backfills_role` — запись в `resources.layouts` с
   `payload.slots=[{position:1},{position:2,abonent_id:"p"}]`, `mode:"manual"` → `GET /ui/video_layouts`,
   `GET /ui/video_layouts/{id}`, `GET /conferences/{id}/video_layouts` (linked): у всех трёх `payload.slots[*].role`,
   занятый = `PEER`, пустой = `AUTO`.
2. `api_v3/resource_tests.rs::patch_layout_slots_keeps_or_backfills_role` — PATCH со слотом без роли → в ответе роль
   есть; PATCH с `role:"ACTIVE"` → не перезаписана; повторный PATCH тем же телом → payload байт-в-байт стабилен.
3. `api_v3/tests.rs::templates_payload_has_roles` — `GET …/video_layouts/templates?video_layout_id=…` для manual:
   каждый слот `role ∈ UI_ROLES` (ловит дыру `templates`).
4. `api_v3/tests.rs` (продолжить каталог): `codian_51`/`codian_52`/`codian_33` — последние окна `PRESENTATION`
   (проверка z_order-ветки на реальных PiP-схемах, сейчас ветка проверяется только общим правилом).
5. `ui_role` unit-тест на закрытый словарь: `is_ui_role("auto")==false`, `is_ui_role("MEDIA")==false`,
   `is_ui_role("ROLL")==true`; после C-диффы: слот с `role:"auto"` на read-path → `AUTO`.
6. `api_v3/tests.rs` (если принят P2-маппинг spec-ролей): `codian_custom_22` → слот 0 `AUTO` (Fixed→AUTO),
   слот 1 `ACTIVE`, есть хотя бы один `ROLL`; `codian_custom_14` аналогично.
7. `vks-server/src/gateways.rs` (уже есть фикстуры с `role:"AUTO"`, `:4976/5008`): тест «роль в payload не влияет на
   посадку» — слоты с `role:"EMPTY"`/`"ACTIVE"` и `abonent_id` дают те же `assignments` по `position`, что и без роли
   (роль игнорируется — `LayoutSlot` без поля, тип-гарантия).
8. `tests/e2e/test_layout_constructor.js` — после клика `codian_07`: в store 13 слотов, у каждого `role` из 6 значений,
   в DOM у каждого `.layout-grid-slot` класс `layout-grid-slot--*` и непустой `background-color` (сейчас `role` — 0 вхождений).

## E. Геометрия: `calculate` ↔ клиент ↔ MCU

1. `calculate` **принимает** `width`/`height`: `shell.rs:745-752` (`>=1.0`, иначе `1280.0/720.0`); каталог считает
   геометрию **жёстко 1280×720** (`shell.rs:798`); Codian — `codian::geometry(width,height)` через `cell_rect`
   (`preset.rs:687-700`, `floor(coord·canvas/grid)` — та же формула, что у эталона), конструктор — `grids::geometry`
   (округление рёбер `get_grid_edges`, `grids.rs:234-238`).
2. Live-факты (токен, сервис без перезапуска): `codian_07` @1280×720 → крупное `(0,0,640,360)` + 12×(320×180);
   @1920×1080 → `(0,0,960,540)` + 12×(480×270) — пропорционально, без сдвигов. `vks_grid_9` @1280 → колонки
   427/426/427, @1920 → 640/640/640. Т.е. **серверная геометрия корректна**; «неверная геометрия» приходит не отсюда.
3. MCU: `apply_layout` строит ту же схему в пространстве 1920×1080 (`gateways.rs:1836-1837`, `:1868-1882`), x/y клиента
   для explicit-пресетов игнорируются (`:1886-1902`); `engine.rs:188-224` масштабирует обе границы half-up в холст
   комнаты с клампом — щелей/выходов нет. Два пространства (превью клиента / канон MCU) — by design.
4. Клиент **подменяет** ответ `calculate` для `vks_*`/`codian_*`: `codian-layouts.js:1682-1721`
   (`formatCalculatedSlots`: считает в 1280×720 и масштабирует `Math.round`), перехват fetch `:1751-1786`, XHR
   `:1799-1827`; серверный ответ не используется. Замеры агента `geometry` (`04_geometry.md` §1.5): `grid` до
   **1745 px** (другие rows/cols, напр. N=65), `pin/dual` ≤2 px при масштабе ≠1, Codian ≤1 px; для id с опциями
   (`vks_grid_g12_m24_9`, `vks_pin_l_g8_7`) функция возвращает `[]` — сервер эти id поддерживает.
5. **Можно ли удалить `formatCalculatedSlots` после фикса `role` — да.** Он существовал ради (a) роли в слотах,
   (b) Codian-слотов без похода в сервер, (c) масштаба под канвас; после C1–C3 всё это отдаёт сервер, причём без
   двойного округления. Удаление: `formatCalculatedSlots` (~40 стр.) + два перехватчика (~30 стр.) ≈ −70 строк;
   `computeSlots`/`buildSymmetricLayout` остаются (черновик редактора, превью, `setSlots` конструктора), `uiSlotRole`/
   `normalizeSlots` допустимо оставить как дешёвую защиту от старого сервера. Порядок: сначала выкатить C1–C3, затем
   снять перехват; без выравнивания `gallery_cost` (`WASTE_WEIGHT`, `04_geometry.md` §1.2) превью `grid` для
   36/100 N сменит сетку; id с опциями начнут работать; N>100 остаётся отсечён UI-счётчиком (`MAX_COUNT=100`).

Непроверенное: тесты не запускал (`target/` занят чужим release-build), патч в дереве не коммичен — «зелёность» заявлена
его автором; live `/presets` без `role` снят со старого бинаря (не перезапускался); в БД через API 6 раскладок
(5 пустых, 1 с 25×`AUTO`, Codian нет) — read-path-бэкфилл упреждает будущие записи, а не чинит текущие.

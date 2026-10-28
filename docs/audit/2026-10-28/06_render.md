# 06 — Почему окна конструктора PNM 2.2.1.66 прозрачные (render-аудит)
Дата: 2026-10-28, ~11:27–11:45 MSK. **Read-only**: прод-код я не менял, писал только этот файл;
`vks_data.db` не менялась, `login` писал audit (`login_ok`) — разрешено. Проверено: код клиента
(`codian-layouts.js`, вендорские чанки), CSS, live API 127.0.0.1:8080, headless Playwright.
**Состояние дерева:** другой агент в 11:29:03–11:29:27 MSK уже положил (незакоммичено)
`ui_slot_role`/`ensure_slot_roles`/`normalizeSlots`; бинарь `target/release/vks-server` — от
**2026-10-25 20:37**, поэтому live API этих правок **не содержит** (проверено curl). В §C — что
уже применено и 2 оставшихся пробела.

## A. Роли, которые ждёт вендорский рендерер (факты)
`GridSlot` (`chunks/index6.bundle-2.2.1.66.js`, offset 1017505–1017800):

```js
slotIcon(){ return this.layoutSlot.role!=="EMPTY" ? this.layoutLocalesStore.icons[this.layoutSlot.role] : "" }
slotClass(){ return {EMPTY:"layout-grid-slot--empty", ACTIVE:"layout-grid-slot--active-speaker",
  AUTO:"layout-grid-slot--autofill", PEER:"layout-grid-slot--peer", ROLL:"layout-grid-slot--roll",
  PRESENTATION:"layout-grid-slot--content"}[this.layoutSlot.role] }
```

* Полный перечень ровно **6** значений, строго **UPPERCASE**: `EMPTY`, `ACTIVE`, `AUTO`, `PEER`,
  `ROLL`, `PRESENTATION` (offset 1017629). Иконки для них: `c={EMPTY,ACTIVE,AUTO,ROLL,PEER,PRESENTATION}`
  (offset 994945; титулы/описания — 999116). Других сравнений `role==="…"` в чанке нет
  (перебор регуляркой: ровно эти 6).
* **`FIXED` в UI-словаре НЕТ.** `FIXED` встречается только как `mo.FIXED`/`Qc.FIXED` MDC-меню
  (`index.bundle` offset 218933/258877) — к ролям окон не относится. CSS-класс
  `.layout-grid-slot--fixed{background-color:#2b4245}` (`index7…css` offset 107785) **мёртвый**:
  в `slotClass` он не маппится, поэтому роль `FIXED` даёт то же, что и отсутствие роли.
* `AUTO` — это «autofill»: фон `#715839` (offset 109017), иконка автозаполнения.
  `EMPTY` — `#333` (108500), `PEER` — `#5c5c5c`, `ACTIVE` — `#6a403c`, `ROLL` — `#3c445f`,
  `PRESENTATION` — `#2f5e40`. `--overlap` лишь затемняет вариант (108571).
* **База `.layout-grid-slot` фона не имеет** (`index7…css` offset 107391: `border-radius:6px;cursor:pointer;
  display:flex;…;height:100%;overflow:hidden;position:relative;width:100%`) и `.layout-grid-slot-wrapper`
  тоже (107317). Фон/иконку даёт **только** role-класс.
* `role=undefined/null/неизвестный/в нижнем регистре/FIXED` → `slotClass === undefined`
  (класс не ставится), `slotIcon === undefined` (TcIcon рендерит пустоту). Проверено в живом DOM:
  слот без роли = `class="layout-grid-slot"`, `background-color: rgba(0,0,0,0)`,
  `icon.innerHTML === "<!----><!---->"`. Видимым остаётся только нумерованный чип
  `.layout-grid-slot__header` (`background:rgba(0,0,0,.38)`, CSS 109765) — «окно пустое/прозрачное».
* Нюанс: роль `MEDIA` есть в `titles`/`originalValue` (offset 979157), но отсутствует в `slotClass` и
  `icons` → медиа-окно тоже без фона (вендорский баг, не наш случай).

## B. Как вендор задаёт роль по умолчанию

* Каталог ручных схем в бандле — объект `mM` (offset 1021805): ключи `"N,N"` → `{showSize?, presets:[[preset_id…]]}`
  (напр. `"3,3":{presets:[["preset_equal_3"],["preset_selected_b_3",…]]}`). **Ролей и геометрии в каталоге нет**,
  только id. Карточки палитры — статические SVG (`getLayoutPresetIcon`), поэтому палитра вендора не прозрачная.
* Дефолты редактора (`On`, offset 978800): `auto:{preset:"preset_default_13"}`,
  `dynamic:{preset:"preset_equal",size:25}`, `manual:{preset:"preset_equal_25",slots:{}}`.
* Пустые окна получают `role:"AUTO"` из четырёх мест:
  1. fallback слота в store `layout-editor` (offset 979966):
     `||Array.from(new Array(ss(F)),(D,z)=>({role:"AUTO",position:z+1}))` — когда для preset нет
     сохранённых слотов/шаблона (`ss` — число из хвоста id; для `codian_52` дало бы 52);
  2. `editContactSlot(e,t=null,n="AUTO")` в `manual-layout-editor` (offset 1025525);
  3. маппер API `mI` — `role:t.role||"AUTO"` (`index.bundle` offset 1109616; так роль появляется
     у любой сохранённой раскладки, даже если сервер её не отдал);
  4. новый серверный `ui_slot_role` (`shell.rs:698`; в бинаре отсутствует).
* `setSlots` = `oe=(F,T)=>{I.value.manual.slots[V.value.preset]=T; r.value[F]=V.value.preset; …}`
  (offset 980811) — принимает `(preset, slots)` и **не трогает role**; роль приходит только из `slots`.
* `setFreeUsers` (offset 1041423) при клике «Заполнить видеоокна участниками»:
  `if(t.role!=="AUTO"||!e.length)return t; … role:"PEER"` — перезаписывает **только `AUTO`**.
  Кнопка disabled, если нет ни одного `AUTO` (`hasAutofillSlots`). Отсюда инвариант:
  `AUTO` — только окна, которые вендору разрешено автозаполнять; занятые — `PEER`, иначе Fill их затрёт.
* `addSlot`/`createSlot` в 2.2.1.66 **не существуют** (поиск пуст); слоты создаёт только fallback store и `editContactSlot`.
* Эмпирика: выбор вендорского `preset_equal_25` в ручном режиме → 25 слотов `role:"AUTO"`, DOM
  `layout-grid-slot--autofill`, фон `rgb(113,88,57)`. Эталон вендора «пустое окно = AUTO».

## C. Минимальная правка `codian-layouts.js` (и что уже применено)

Причина дыры: `select()` (baseline 1833 строки, прочитана в 11:27; файл позже перезаписан, нумерация
baseline дана по смыслу ≈) кладёт в `editor.setSlots` **сырые** `entry.slots` каталога — без `role`:
`if (entry.slots && entry.slots.length) slots = entry.slots;` (~517); fallback ставит `AUTO` только при
отсутствии слотов (~519). Live-бинарь роль в каталоге не отдаёт (см. §D), поэтому окна прозрачные.
**Дифф 1 — нормализация role на границе UI** (в начале IIFE, рядом с `var observer = null;`):
```js
+  /* Вендор красит окно/рисует иконку только по slot.role (UPPERCASE:
+   * EMPTY/ACTIVE/AUTO/PEER/ROLL/PRESENTATION); без роли окно прозрачное. */
+  function uiSlotRole(slot) {
+    var role = slot && typeof slot.role === 'string' ? slot.role : '';
+    if (role === 'EMPTY' || role === 'ACTIVE' || role === 'AUTO' || role === 'PEER' ||
+        role === 'ROLL' || role === 'PRESENTATION') return role;
+    if (slot && (slot.abonent_id || slot.contact_id || slot.abonentId || slot.contactId)) return 'PEER';
+    return Number(slot && slot.z_order) > 0 ? 'PRESENTATION' : 'AUTO';
+  }
+  function normalizeSlots(slots) {
+    var out = [];
+    for (var i = 0; i < (slots || []).length; i += 1) {
+      var slot = slots[i];
+      if (!slot || typeof slot !== 'object') continue;
+      var copy = {};
+      for (var key in slot) if (Object.prototype.hasOwnProperty.call(slot, key)) copy[key] = slot[key];
+      copy.role = uiSlotRole(slot);
+      out.push(copy);
+    }
+    return out;
+  }
```
**Дифф 2 — в `select()` перед `pendingAdopt = null;`** (сейчас стр. 552–556; baseline ≈521/522):
```js
       }
     }
+    // Каталог отдаёт геометрию без role — без роли редактор рисует окно прозрачным.
+    slots = normalizeSlots(slots);
     pendingAdopt = null;
```
**Дифф 3 — предпросмотр: убрать `Math.round` (дефект 0.05–0.1%) и добавить role**
(в текущем файле `formatCalculatedSlots` — стр. 1713–1720; baseline ≈1683–1690):
```js
       res.push({
         position: Number(s.position) || (j + 1),
-        x: Math.round((Number(s.x) || 0) * scaleX),
-        y: Math.round((Number(s.y) || 0) * scaleY),
-        width: Math.round((Number(s.width) || 0) * scaleX),
-        height: Math.round((Number(s.height) || 0) * scaleY),
+        role: uiSlotRole(s),            // опционально: Bee() всё равно берёт только геометрию
+        x: (Number(s.x) || 0) * scaleX,
+        y: (Number(s.y) || 0) * scaleY,
+        width: (Number(s.width) || 0) * scaleX,
+        height: (Number(s.height) || 0) * scaleY,
         z_order: Number(s.z_order) || 0
       });
```
Роли: пустое окно (нет `contactId/abonentId`, `z_order=0`) → `AUTO` (фон `#715839` + иконка, Fill
работает); занятое (`contactId/abonentId` — в store **camelCase**, в payload — snake_case) → `PEER`
(Fill не трогает, т.к. `t.role!=="AUTO"`); наложение `z_order>0` → `PRESENTATION` (зелёный, контент)
либо `AUTO` для паритета с вендорским fallback (у вендора PiP-слоты тоже `AUTO` + `--overlap`).
**В сохранении клиентский код менять не нужно**: ручной payload уже несёт `role`
(`index.bundle` offset 1110267: `kee={...gy,slots:…{position:t.position,role:t.role,contact_id:…}}`);
для старых записей серверный `ensure_slot_roles` (`resources.rs:1190`) и маппер `mI` (1109616)
достраивают `AUTO`.
**Уже применено в рабочем дереве (не мной, 11:29:27):** `uiSlotRole`/`normalizeSlots` — строки 87–110,
вызов в `select()` — 555, `role: uiSlotRole(s)` в `formatCalculatedSlots` — 1713. Пробелы:
(1) проверяются только `abonent_id/contact_id`, а store-слоты используют `contactId/abonentId` →
занятые окна получат `AUTO` и могут быть затёрты Fill; (2) `Math.round` не убран — дефект 0.05%
остался. Серверные правки (`shell.rs:769,805 role`; `resources.rs:1190 ensure_slot_roles`)
в рабочем дереве есть, но **бинарник не пересобран** → live API по-прежнему без role.

## D. Runtime-проверка (факты, сырой JSON)

1. `GET /pnm/` → UI 2.2.1.66; браузер грузит `/pnm/codian-layouts.js` (sha256 совпадает с файлом в дереве).
2. `POST /api/v3/login {"login":"admin","password":"admin_password"}` → 200, JWT (276 симв.), audit `login_ok`.
3. `GET /api/v3/ui/video_layouts/presets` (Bearer) → **200, 46 593 B, 119 пресетов: 65 vendor + 54 codian.
   Vendor: только `{id,source}`; codian: `{id,title,windows,source,slots}`, `role` в слотах ОТСУТСТВУЕТ
   у всех 54×N (перебор).** Сырые 2–3:
```json
{"id":"codian_01","slots":[{"height":720.0,"position":1,"width":1280.0,"x":0.0,"y":0.0,"z_order":0}],"source":"codian","title":"Полный экран","windows":1}
{"id":"codian_02","slots":[{"height":360.0,"position":1,"width":640.0,"x":0.0,"y":0.0,"z_order":0},{"height":360.0,"position":2,"width":640.0,"x":640.0,"y":0.0,"z_order":0},{"height":360.0,"position":3,"width":640.0,"x":0.0,"y":360.0,"z_order":0},{"height":360.0,"position":4,"width":640.0,"x":640.0,"y":360.0,"z_order":0}],"source":"codian","title":"Сетка 2×2","windows":4}
{"id":"codian_33","slots":[{"height":144.0,"position":2,"width":256.0,"x":51.0,"y":28.0,"z_order":1}],"source":"codian","title":"…","windows":20}
{"id":"preset_equal_1","source":"vendor"}
```
   PiP (`z_order=1`) есть у 5 пресетов: `codian_33/38/39/51/52`.
4. `POST /api/v3/ui/video_layouts/calculate {"preset":"codian_07","width":887,"height":499}` → 200,
   слот1 `width:443.0,height:249.0` (floor), без role; наш перехватчик отдаёт `444` — расхождение 1 px.
5. **Headless (Playwright, chromium):** открыл сохранённую раскладку
   `/pnm/monitoring/conferences/49b599d7…/layouts/7e6d4753…`, клик «Ручная настройка» →
   `.manual-layout-configurator` есть, `[data-vks-codian-group]` есть, **54 карточки**,
   превью-`<rect>` c `fill: rgba(0,0,0,.26)` (видны), 0 ошибок консоли и 0 ответов ≥400.
   Клик `codian_07` (после правки 11:29) → store `{mode:"manual",preset:"codian_07",slots:13, role:"AUTO"}`,
   DOM `layout-grid-slot--autofill`, фон `rgb(113,88,57)`, SVG-иконка на месте.
   Контрольный эксперимент: тот же store, слоты **без** `role` → DOM `class="layout-grid-slot"`,
   `background-color: rgba(0,0,0,0)`, `icon.innerHTML="<!----><!---->"`, чип-номер `rgba(0,0,0,.38)`
   поверх подложки предпросмотра `rgb(248,248,248)`. После возврата ролей — снова `--autofill`.
   Т.е. «прозрачные окна» воспроизводятся/лечатся детерминированно.
6. **Арифметика 0.05%** (дефект preview): вендор строит `width: H/P.width*100+"%"` (`index6` offset
   1036913; `P.width=.layout-grid-configurator.clientWidth`). В headless `clientWidth=887, clientHeight=499`; перехватчик отдал
   `round(640·887/1280)=round(443.5)=444` и `round(360·499/720)=round(249.5)=250`
   → `444/887·100 = 50.0564%`, `250/499·100 = 50.1002%`. Излишек = 0.5 px / размер контейнера
   (0.5/887=0.0564%, 0.5/499=0.1002%). Все стили сходятся: `25.0282%=222/887`, `74.9718%=665/887`,
   `25.0501%=125/499`. Это **округление синтетического calculate** на нечётном контейнере
   (887×499), а не деление «на другое число»; сервер отдаёт 443/249 (floor). Лечится Диффом 3.

## E. Что именно видит пользователь

* **Каталог виден**, но только в ручном режиме/«Конструкторе»: `.manual-layout-configurator`
  (а с ним наш `[data-vks-codian-group]`) монтируется лишь при `mode=manual`. В auto/dynamic
  карточек Codian в DOM **0** (проверено). Селекторы каталога: `.layout-scheme-selector`,
  `.layout-scheme-selector-item`, `__icon`, `__badge`, `--active`; классов `layout-card`/`layout-preview` нет.
* На канве — то, что описано в §A: прозрачные прямоугольники без иконок, видны только тёмные
  чипы с номерами поверх светло-серого `rgb(248,248,248)` предпросмотра. Это и есть «окна пустые».
* Отдельно: сохранённая «Codian 07 — 13 окон» (`7e6d4753…`, GET 200) имеет
  `payload:{mode:"auto",preset:"preset_default_13",slots:[]}` — при reopen открывается авто-режим,
  а в ручном выставляется `preset_equal_25` (25 окон), т.е. **выбранный Codian-макет после
  сохранения/reopen теряется** (вендорский save-маппер для auto/dynamic не шлёт `slots`:
  `gy={mode,preset,type}`, `Fee={auto:gy,dynamic:Nee,manual:kee}`, `index.bundle` 1110174/1110461).

## F. Пять самых вероятных причин «раскладки не отображаются» (ранг)

1. **0.95 — у слотов нет `slot.role`** → нет role-класса/фона/иконки (`.layout-grid-slot` без bg).
   Доказано live-каталогом (role нет), кодом вендора, CSS и headless-экспериментом. Лечится §C.
2. **0.70 — каталог Codian физически не смонтирован вне ручного режима** (auto/dynamic):
   `.manual-layout-configurator` отсутствует → «раскладок Codian нет вообще» в этих табах.
3. **0.45 — Codian-выбор не сохраняется**: сохранение в auto/dynamic уходит без `slots`/`preset`
   (пример выше), после reopen — не тот макет и не те роли.
4. **0.35 — кеш старого `codian-layouts.js`** у клиента/прокси: файл обновлён 28.10 11:29
   (`MANIFEST.sha256` тоже), без hard-refresh фикс не применяется; старый JS = причина №1.
5. **0.30 — preview ≠ apply**: подмена `calculate` (round) + другой алгоритм сетки для `vks_*`
   и 0.05%-смещение на нечётном контейнере → окна выглядят смещёнными/«не теми», хотя не прозрачны.

**Что перепроверить после пересборки бинаря (сейчас он 25.10):** live presets/calculate должны
начать отдавать `role` (`shell.rs:769,805`), сохранение — `ensure_slot_roles` (`resources.rs:1190`);
после этого клиентскую нормализацию можно оставить как страховку для старых payload'ов.

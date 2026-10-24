# Сквозная цепочка раскладок Codian (54) — аудит 2026-10-23

Задача: каталог → превью (`calculate`) → применение → `McuEngine.current_layout` →
`LayoutChanged` клиенту → композит. Где рвётся СЕЙЧАС.

Итог одной строкой: **в коде разрывов нет**; незакрыты две вещи, обе про задеплоенную
сборку — (1) живой `/calculate` отдаёт 400 для всех 54 codian, (2) живой `/presets` не
содержит поля `slots`, поэтому проектный слой рисует равную сетку вместо геометрии Codian.
Лечение: коммит рабочего дерева + пересборка + перезапуск (перезапуск запрещён задачей).

Состояние рабочего дерева на момент аудита (mtime): `shell.rs` 19:16:27,
`codian-layouts.js` 19:20:16, `resources.rs` 19:14:50.

## Цепочка (file:line)

| Шаг | Код |
|---|---|
| каталог (наше расширение) | `crates/vks-admin/src/api_v3/shell.rs:758-784` (`GET /api/v3/ui/video_layouts/presets`; для codian: `title`, `windows`, `slots` = геометрия 1280×720) |
| превью (эндпоинт) | `crates/vks-admin/src/api_v3/shell.rs:697-750` (`POST /api/v3/ui/video_layouts/calculate`) |
| превью (слой) | `client/pnm-frontend/deployed/pnm/codian-layouts.js`: `loadCatalog()` → `buildItem()` → `svgFromSlots()` (viewBox = bbox слотов) |
| сохранение схемы UI | `PATCH /api/v3/ui/conferences/{id}/video_layouts/{rid}` → `resources.rs:1570` (`edit_linked`) → `resources.rs:1230` (`update_resource`) |
| применение к конференции | `POST /api/v3/conferences/{id}/video_layouts/{rid}` → `resources.rs:1734` (`attach`) → `resources.rs:1772` → `sessions.rs:463` (`push_layout_to_engine`) |
| шлюз → движок | `crates/vks-server/src/gateways.rs:1476` (`apply_layout`), codian-ветка `1562-1595` |
| движок | `crates/vks-media-mcu/src/engine.rs:814` (`apply_layout`) → `143` (`layout_from_descriptor`, geometry-ветка `161-185`) → `874` (`current_layout`) |
| сигнал клиенту | `crates/vks-server/src/gateways.rs:711-724` (`broadcast_layout`) → `crates/vks-server/src/layout_signal.rs:68` (`LayoutChanged`) |
| композит | `crates/vks-media-mcu/src/compositor.rs` (`compose`: rect + z из слотов) |

## Где рвётся

### L-1. Каталог: серверного списка схем у вендора нет; решение — проектный слой

Список схем в редакторе UI — **статическая таблица в бандле**:
```
$ grep -c "video_layouts/presets" client/pnm-frontend/deployed/pnm/chunks/*.js | grep -v ':0'   # пусто
$ python3 -c "поиск 'mM={' в index6.bundle-2.2.1.66.js"
--- mM=\{ @ offset 1021805 (0xf976d) ---
mM={"1,1":{presets:[["preset_equal_1"]]},"2,2":{presets:[["preset_equal_2"]]},"3,3":{presets:[["preset_equal_3"],
["preset_selected_b_3","preset_selected_t_3","preset_selected_r_3","preset_selected_l_3"]]},...
уникальных preset_ id внутри mM: 65
```
`rangeMap: mM` @1023678 → `flatListPresets` = 65 вендорских id. Codian-схем в ней нет.
У вендора маршрута `/presets` нет вовсе:
```
$ curl -sk https://192.168.1.92/api/v3/ui/video_layouts/presets
HTTP/2 400 {"error":{"code":400,"description":"Bad Request","details":[{"reason":"invalid.video_layout_id",
"description":"Not a valid UUID.","position":"video_layout_id"}]}}
```
**Реализация (untracked `client/pnm-frontend/deployed/pnm/codian-layouts.js`, 16546 байт):**
токен `localStorage["TrueConfMCU_AccessToken"]` (ключ есть в `index.bundle`, 4 вхождения) →
`GET /api/v3/ui/video_layouts/presets` (один запрос, геометрия в поле `slots`) → инъекция группы
«Codian MCU 4200» в вендорский `.layout-scheme-selector`; выбор схемы — штатный путь
`editor.editValueField("preset", id)` + `editor.setSlots(id, slots)`.
Подключён: `index.html` → `<script src="/mcu/codian-layouts.js">`; живой сервер отдаёт:
```
$ curl -s -o /dev/null -w "%{http_code} %{size_download}\n" http://127.0.0.1:8080/pnm/codian-layouts.js
200 17226
```

### L-2. Живой `/calculate` → 400 для всех 54 codian (бинарь без рабоче-деревянной правки)

```
$ .../api/v3/ui/video_layouts/presets            -> 200, 119 пресетов, 54 codian, codian_07: windows=13
$ POST .../api/v3/ui/video_layouts/calculate {"preset":"codian_07","width":260,"height":180}
{"error":{"code":400,"description":"Bad Request"}}   # plain 400 (проверено 19:17)
$ POST ... codian_01 / codian_52 / codian_custom_14 / custom_22 -> тот же 400
$ POST ... {"preset":"preset_equal_4"}             -> {"slots":[...4 слота...]}   # вендорные работают
```
Механика: `preset.rs:681-683` (`is_known_preset` знает codian, закоммичен в `73c022b`) ⇒ `calculate`
не отдаёт vendor-envelope 400, а проваливается на `geometry_for_manual` → `None` →
`super::error_json(400,"Bad Request")` (`shell.rs:732-734`). Codian-ветка в `calculate` — только в
рабочем дереве: `git log -S "vks_layout::codian::geometry" -- crates/vks-admin/src/api_v3/shell.rs` → пусто.
На текущем HEAD: `cargo test -p vks-admin codian` → `codian_calculate_returns_slot_geometry_for_preview ok`.

### L-2b. Живой `/presets` без поля `slots` ⇒ превью = равная сетка

```
$ curl -s -H "Authorization: Bearer $T" .../api/v3/ui/video_layouts/presets | ...
   ключи элемента codian: ['id','source','title','windows']
   элементов с ключом slots: 0
```
Слой: `slots: Array.isArray(item.slots) ? item.slots : []` → пусто → иконка рисуется
`svgFallback(entry.windows)` (равная сетка по числу окон), т.е. **в палитре превью Codian не
соответствует геометрии**. Комментарий слоя (строка 178) это фиксирует: «заглушка, если каталог
пришёл без `slots` (старый сервер)». В рабочем дереве `shell.rs:765-784` поле `slots` уже есть
(`codian::geometry(id, 1280.0, 720.0)`) ⇒ после пересборки превью будет настоящим.

### L-3. Тихая подмена пресета в `POST /api/v3/ui/video_layouts/mode` — ИСПРАВЛЕНО в рабочем дереве

Было: `shell.rs:897-902` фильтровал `field("preset")` по `resources::preset_size(p).is_some()`, а
`preset_size` (`resources.rs:378-382`) требует префикс `preset_` ⇒ любой `codian_*` отброшен, в
payload писался `preset_default_13` / `preset_equal_{inferred_size}` ⇒ «применилось, но не то».
Стало: `preset_field` принимает и `vks_layout::codian::is_codian_preset(p)`, размер берётся из
`codian::slot_count`.

### L-4. `mode=="auto"` ⇒ геометрия пресета не берётся — ИСПРАВЛЕНО в рабочем дереве

Было: `gateways.rs:1557` `if mode != "auto"`, дефолт `mode=auto`
(`vks-common/src/layout_defaults.rs:14`, `resources.rs:415`) ⇒ раскладка с пресетом, созданная без
явного `mode:"manual"`, доезжала до MCU без геометрии.
Стало: `let explicit_codian = !stored_preset.is_empty() && vks_layout::codian::is_codian_preset(stored_preset);
if mode != "auto" || explicit_codian` — поведение «Общей»/auto не меняется.

### L-6. `LayoutChanged` не несёт id схемы — ИСПРАВЛЕНО (T10a, a753799, ADR-0032)

Стало: `Layout.preset=4`/`Layout.mode=5` — справочная проекция админ-состояния
(raw preset, нормализованный mode); `layout_id` сознательно не выводится
(COW-нестабильный UUID), `LayoutModeChanged` отклонён.

`crates/vks-server/src/layout_signal.rs:32-64` → `Layout { version, slots, kind }`; для codian-пути
`engine.rs:162` строит `Layout::new(LayoutKind::Custom)` ⇒ клиент получает `kind="custom"` и
нормализованную геометрию 0..1, но **не знает, что это `codian_07`**. Мелочь, не блокер.

### L-7. Иконки 54 схем Codian будут сплошным прямоугольником (разделение ячеек невидимо)

Разделение ячеек в иконке держится на единственном механизме — зазоре `PREVIEW_INSET = 3`
канвас-пикселя. Каталог отдаёт геометрию в px 1280×720, бокс иконки вендора —
`.layout-scheme-selector-item{height:36px;width:52px;padding:4px}` ⇒ контент 44×28 px ⇒
масштаб `min(44/1280, 28/720) = 0.0344`:

```
inset  3 канвас-px -> зазор между соседними ячейками 0.206 px; rx=2 -> 0.069 px   # невидимо
inset 15 канвас-px -> зазор 1.031 px
inset 25 канвас-px -> зазор 1.719 px
```

Обводки у наших `<rect>` нет: в `chunks/index7.bundle-2.2.1.66.css` для схем-селектора есть
только заливки и ОДНО stroke-правило — `.layout-scheme-selector-item__icon .tc-icon__icon--loading
svg{stroke:rgba(0,0,0,.26)}` (состояние загрузки); `stroke-width` в файле не встречается ни разу.
Вендорские иконки разделены реальными зазорами в путях SVG — значит inset должен играть ту же роль.

Фикс (контракт не меняется, ≈12-15 строк в `svgFromSlots`):
1) inset пропорциональный bbox: `inset = round(maxX * 0.012..0.02)` ⇒ зазор 1.0-1.7 px.
   Сейчас inset применяется в той же петле, что считает bbox (`codian-layouts.js:146-170`:
   `maxX = Math.max(maxX, ix + iw)` — bbox выводится уже из сжатых прямоугольников) ⇒ нужен
   пред-проход по слотам для bbox, затем рисование;
2) сжатие на четыре стороны уводит и КРАЙНИЕ ячейки от границы макета ⇒ щель-рамка по периметру;
   отступ применяется только к тем сторонам, что НЕ лежат на границе макета:
   ```js
   var ix = x, iw = width;
   if (x > 0)             { ix = x + h; iw -= h; }   // левый край не на границе ⇒ отступ
   if (x + width < maxX)  { iw -= h; }               // правый край не на границе ⇒ отступ
   // (аналогично по y); если iw/ih <= 0 — рисуем без зазора
   ```
   `h = gap/2`, `gap = round(maxX * 0.02)` ⇒ зазор между соседями = 2h = gap, внешняя граница
   макета остаётся flush (проверено на сетке 4×4: ячейки [0..308], [332..628], [652..948],
   [972..1280] — зазор 24 px, края 0 и 1280 не сдвинуты; одиночная ячейка во весь канвас
   (`codian_01`) остаётся [0..1280] без отступов).
   Внимание: кламп вида `ix = Math.max(0, x - h); iw = Math.min(maxX, x + width + h) - ix`
   РАСШИРЯЕТ ячейки и даёт перекрытие (проверено: [0..332] и [308..652] ⇒ наложение 24 px
   вместо зазора) — использовать только формулу выше.

Слой исправлен в 19:26:43: `half = Math.max(1, Math.round(maxX * 0.01))` ⇒ зазор 26 канвас-пикселя
(0.89 px в иконке 44px), `rx = half`, откат к «без зазора» при `iw/ih <= 0`, сортировка по
`(z_order, position)` сохранена — всё как согласовано.

Остаточная мелочь в реализации (L-7b): предикаты валидности в двух проходах записаны по-разному,
и для `width/height = Infinity` (JSON `1e400` → `Infinity`) они расходятся:
  bbox-проход: `!isFinite(boxX) || !isFinite(boxY) || !(boxWidth > 0) || !(boxHeight > 0)` — принимает +Infinity
  рисование:    `!isFinite(x) || !isFinite(y) || !isFinite(width) || !isFinite(height)` — отбрасывает
Для конечных значений предикаты эквивалентны (перебор в node: -5, 0, NaN, null, норма — совпадают).

Симуляция текущей функции (node, 2 нормальных слота + один с width=Infinity):
```
<svg viewBox="0 0 Infinity 720" ...>
  <rect x="0" y="0" width="640" height="360" rx="Infinity"></rect>
  <rect x="640" y="0" width="640" height="360" rx="Infinity"></rect></svg>
```
* координаты/размеры rect'ов КОРРЕКТНЫ — срабатывает guard `if (iw <= 0 || ih <= 0) { ix=x; iy=y; iw=width; ih=height; }`
  (при `half = Infinity` отступы дают `iw = -Infinity`, guard откатывает rect к исходной геометрии);
* ломаются `viewBox` (браузер игнорирует «0 0 Infinity …» ⇒ содержимое в пользовательских единицах
  в окне 300×150 ⇒ иконка = обрезанный кусок) и `rx="Infinity"` (невалидное значение атрибута);
* радиус поражения — ОДНА иконка: `buildItem` рисует каждую из СВОИХ слотов
  (`svgFromSlots(entry.slots)`), набор слотов у каждой схемы свой; все 54 пострадают только при
  системном «битом» значении от сервера;
* практическая экспозиция нулевая: сервер не сериализует не-конечные f64, геометрия Codian конечна.

Фикс (≈5 строк, общий предикат на оба прохода):
```js
function boxOf(s) {
  var x = Number(s && s.x), y = Number(s && s.y), w = Number(s && s.width), h = Number(s && s.height);
  if (!isFinite(x) || !isFinite(y) || !isFinite(w) || !isFinite(h) || !(w > 0) || !(h > 0)) return null;
  return { x: x, y: y, w: w, h: h, s: s };
}
var boxes = copy.map(boxOf).filter(Boolean);
var maxX = 0, maxY = 0;
boxes.forEach(function (b) { maxX = Math.max(maxX, b.x + b.w); maxY = Math.max(maxY, b.y + b.h); });
if (!(maxX > 0) || !(maxY > 0)) return '';   // и рисование по boxes
```
Минимальный вариант (добавить `isFinite` по width/height в bbox-проход) тоже корректен, но оставляет
два текстуально разных предиката. Опционально: `viewBox` собирать только из конечных значений.

**ИСПРАВЛЕНО в слое (19:29:57)**: единый предикат, общий массив `valid`, bbox и рисование по нему.
Проверено запуском самой функции из файла (node, извлечение `svgFromSlots` из `codian-layouts.js`):
```
=== норма 2×2 ===
<svg ... viewBox="0 0 1280 720" ...><rect x="0" y="0" width="627" height="347" rx="13"></rect>
<rect x="653" y="0" width="627" height="347" rx="13"></rect><rect x="0" y="373" ...></rect><rect x="653" y="373" ...></rect></svg>
=== + слот width=Infinity ===
<svg ... viewBox="0 0 1280 720" ...>…те же 4 rect'а, без битого…</svg>          # битый слот отброшен предикатом
=== только битый слот ===
""                                                                                  # пустая иконка, а не невалидный SVG
```
Зазор между соседями 26 канвас-пикселя (0.89 px в иконке 44px), края макета flush, `rx=13`,
`viewBox`/`rx` всегда конечные ⇒ L-7b закрыт.

## Проверено и СОДЕРЖИТЕЛЬНО НЕ ЛОМАЕТСЯ

* **Формат слотов вендорного UI — snake_case, привязка не теряется.** `index.bundle @ 1110265`:
  `kee={...gy, slots:({slots:e})=>e==null?void 0:e.map(t=>({position:t.position,role:t.role,contact_id:t.contactId,abonent_id:t.abonentId,connection_id:t.connectionId,call_string_id:t.addressId}))}`,
  `Fee={auto:gy,dynamic:Nee,manual:kee}`, `Th=re(e,{display_name:...,payload:Fee[e.mode]})` — те же ключи,
  что ищут `resources.rs:1206-1210` и `gateways.rs:1518`. (Ранняя гипотеза о потере привязки — ОТОЗВАНА.)
* **Вендорный UI зовёт `calculate`** (`index.bundle @ ~1110600`: `koe` → `J.post("/api/v3/ui/video_layouts/calculate",Uee(e))`,
  `Uee=re(e,{preset,width,height})`); эндпоинт уважает запрошенный размер (живая проверка:
  `preset_equal_4` 260×180 → ячейка 130×90; 1280×720 → 640×360).
* **CSS вендора красит inline-SVG слоя** (дефекта «невидимая иконка» нет) —
  `chunks/index7.bundle-2.2.1.66.css`: `.layout-scheme-selector-item__icon svg *{fill:rgba(0,0,0,.4)}`
  (+ `fill:#fff` для `--active`, `.overlap{fill:rgba(0,153,166,.8)}`),
  `.layout-scheme-selector-item__icon,.layout-scheme-selector-item__icon .tc-icon__icon{font-size:100%;height:100%;width:100%}`.
  Но обводки нет ⇒ разделение ячеек держится только на inset-зазоре (см. L-7).
* **`windows` из каталога == число слотов геометрии**: сверка всех 52 обычных id с
  `docs/progress/evidence/codian-layouts/codian_geometry.json` → расхождений 0
  (`codian_custom_22` → 22, `codian_custom_14` → 14) ⇒ слоты, которые слой кладёт в редактор,
  совпадают с настоящей геометрией.
* **viewBox слоя = bbox слотов** (`codian-layouts.js:164-175`) ⇒ каталог 1280×720 масштабируется
  целиком (`preserveAspectRatio="xMidYMid meet"`), а не показывает левый верхний угол.
* **`resolve()` не ломает codian-id:** `preset.rs:149` короткозамыкается на `has_explicit_size("codian_43")=true`
  ⇒ id доезжает до `apply_layout` неизменным.
* **Применение доходит до движка:** `resources.rs:1772` → `sessions.rs:463` → `gateways.rs:1476` →
  `engine.rs:814` → `current_layout` → `broadcast_layout` → `LayoutChanged`.

## Эталон

```
$ diff <(grep -o 'preset_[a-z_0-9()]*' vendor.js|sort -u) <(grep -o 'preset_[a-z_0-9()]*' ours.js|sort -u)
→ идентичны (92 id); таблица схем бандлов совпадает, mM = 65 id у обоих
$ curl -sk https://192.168.1.92/api/v3/ui/video_layouts/calculate -d '{"preset":"preset_equal_4"}'  -> 401
```
Вендор: `/calculate` есть (401 = требует авторизации), `/presets` нет ⇒ наш `/presets` — расширение,
вендорский UI его не читает; список схем вендор строит сам из статической `mM`.

## Живое состояние (не проверялось вживую и почему)

```
$ .../api/v3/ui/sessions?page=1&page_size=20   -> все 20 сессий state=STOPPED
$ .../api/v3/ui/conferences/<id>/video_layouts  -> mode=auto preset=preset_default_13 size=13 slots=[]
$ grep -o 'codian_[a-z0-9_]*' по всем раскладкам -> пусто (codian ни разу не применялся)
```
* `apply → current_layout → LayoutChanged → композит` — **вживую не проверено**: MCU-комнаты нет
  (все сессии STOPPED), а старт сессии изменил бы состояние сервера (запрещено); доказано кодом.
* `PATCH`/`mode` на живом сервере — **не проверено**: они мутируют payload раскладки.
* WS-кадринг `LayoutChanged` — **не проверено**: нужна живая сессия.
* DOM-инъекция слоя в браузере — **не проверено**: нужен GUI-прогон, а не API; и проверять надо
  **после пересборки**, иначе увидится `svgFallback`, а не слой.

## L-7/L-7b: иконки Codian в палитре (закрыто)

Геометрия каталога приходит в px канваса (1280×720), бокс вендорской иконки —
44×28 px содержимого (`.layout-scheme-selector-item{height:36px;width:52px;padding:4px}`),
поэтому зазор между ячейками должен быть долей макета, а не фиксированным числом
канвас-пикселей. Вендорский CSS не даёт разделения: единственная stroke-строка в
`index7.bundle-2.2.1.66.css` — для состояния `--loading`, обычные иконки разделены
только заливкой/зазорами в путях SVG.

Что реализовано в `client/pnm-frontend/deployed/pnm/codian-layouts.js` (`svgFromSlots`):

1. один предикат валидности на оба прохода: `valid` собирается из конечных x/y/width/height
   с `width>0 && height>0`; bbox и рисование идут по одному и тому же массиву
   (расхождение предикатов невозможно by construction);
2. bbox по сырым слотам ⇒ `viewBox="0 0 maxX maxY"` (без этого был виден только
   левый верхний угол макета: раньше `viewBox="0 0 260 180"` при координатах 1280×720);
3. `half = max(1, round(maxX*0.01))` ⇒ зазор между соседями `2*half = 26` канвас-px
   = 0.89 px в иконке 44 px (при прежних `PREVIEW_INSET=3` зазор был 0.206 px — невидим);
4. отступ только по сторонам, не лежащим на границе макета, ⇒ внешняя граница flush,
   по периметру нет лишней рамки; `rx = half`; при `iw/ih <= 0` откат «без зазора»;
5. порядок отрисовки — по `(z_order, position)` (PIP сверху); класс `.overlap` не
   используется: в `--active` он красится в teal на teal-подложке и PIP исчезает.

Проверка (запуск самой функции из файла в node и headless-стенд): сетка 2×2 1280×720 ⇒
`viewBox="0 0 1280 720"`, ячейки `x=0 w=307`, `x=333 w=294`, `x=653 w=294`, `x=973 w=307`
(зазор 26 на всех стыках, правый край 1280 flush); слот `width=Infinity` отбрасывается,
иконка показывает остальные окна вместо невалидного SVG; одиночная ячейка во весь
канвас (`codian_01`) остаётся `0..1280` без отступов.

## Стенд слоя (frontend-harness)

`frontend-harness.html` — детерминированный прогон (синхронные `requestAnimationFrame`
и `fetch`, мок pinia-стора `layout-editor`, реальный каталог с сервера внутри страницы).
Запуск: см. шапку `frontend-harness-result.md`; слой кладётся рядом как
`codian-layouts.js` (копия из `client/pnm-frontend/deployed/pnm/`).

Итог прогона: 54 карточки, 54 SVG, заголовок группы есть, группа вставлена в
`.base-layout-configurator-side-panel`, `viewBox` 1280×720 у `codian_01` и `codian_04`,
зазоры 26 канвас-px, подписи «1 окно / 4 окна / 9 окон», клик по `codian_07` вызывает
`editValueField('preset','codian_07')` + `setSlots('codian_07', 13)`, активный класс
`layout-scheme-selector-item--active` выставлен, ошибок консоли нет.

Точка вставки — панель схем (`.base-layout-configurator-side-panel`, `width:300px`,
`padding:16px`), сразу под вендорской палитрой; группа — `.main-form-section-16` с
`.main-form__subheader-text` и `.layout-scheme-selector__group` по 4 карточки
(4×52 + 3×12 = 244 ≤ 268 контента панели), без нового CSS.

## Живая проверка после деплоя (Playwright, реальный 8080)

Сервер пересобран и перезапущен (см. план §7.6), конференция для проверки —
из URL владельца: `49b599d7-b5cd-45a7-8999-47a28c453a92` (в её БД — раскладка
`codian_07`, 13 окон). Скрипты: `live-probe.js`, `live-probe-save.js`.

```
URL   /pnm/monitoring/conferences/49b599d7-…/layouts/7e6d4753-…
items = 54, unique = 54, rows = 7, perRow = 8
sectionParent = manual-layout-configurator   (секция на полную ширину)
viewBox всех проверенных карточек = "0 0 1280 720"   (геометрия из каталога, не fallback)
rects codian_04: [x=0 w=307], [x=333 w=294], [x=653 w=294], [x=973 w=307]
                 ⇒ зазор 26 канвас-px, правый край 1280 flush
клик по codian_07 → карточка получила --active
перехват сети: POST /api/v3/ui/video_layouts/calculate {"preset":"codian_07","width":887,"height":499}
               → 200 (раньше живой вендорский UI получал 400)
errors консоли: []
```

Скриншоты: `live-manual-codian-07.png` (вся страница: шапка «Codian 07 — 13 окон»,
ручная настройка, сетка 13 окон, группа Codian MCU 4200) и `live-codian-group.png`
(крупно: 54 карточки 8×7, вендорские классы, зазоры видны, активная карточка — teal-рамка).

Пустая светлая колонка слева от сетки — это `base-layout-configurator-side-panel`
с вендорской палитрой, которая для пресета вне `rangeMap` пуста; это поведение
вендора, слой его не маскирует (иначе ломается 16:9 предпросмотра — см. замеры
в отчёте интегратора).

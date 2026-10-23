/*
 * Codian MCU 4200: 54 схемы в редакторе раскладок PNM 2.2.1.66.
 *
 * Вендорский UI 2.2.1.66 рисует палитру manual-схем из захардкоженной карты
 * `index6.bundle-2.2.1.66.js` (объект `mM`, смещение ~1021805: ключи "8,8",
 * "10,24", …, списки `preset_*`). Наших 54 id (`codian_01`…`codian_52`,
 * `codian_custom_22`, `codian_custom_14`) в этой карте нет и быть не может,
 * поэтому проектный слой добавляет отдельную группу «Codian MCU 4200» в тот
 * же контейнер редактора ручной раскладки, теми же классами:
 *
 *   .manual-layout-configurator
 *     .base-layout-configurator            (вендорский предпросмотр + палитра)
 *     .main-form-section-16                (наша группа, без нового CSS)
 *       .main-form__subheader > .main-form__subheader-text
 *       .layout-scheme-selector
 *         .layout-scheme-selector__group   (по 8 карточек в ряд)
 *           .layout-scheme-selector-item[data-source=codian][data-codian-id]
 *             .tc-icon-container.layout-scheme-selector-item__icon
 *               i.tc-icon__icon > svg
 *
 * Почему так, а не патч бандла:
 *  - `deployed/pnm` — ребрендованное вендорское дерево, правки бандлов
 *    откатываются импортом (`scripts/import-pnm-frontend.sh`); проектный слой
 *    (api-errors.js, perf-overrides.js, этот файл + блок
 *    `prostor:project-layer:start/end` в index.html) импорт сохраняет;
 *  - CSP (`script-src 'self'`, без unsafe-inline) требует внешний файл: ни
 *    инлайновых скриптов, ни `<style>` здесь нет.
 *
 * Контракт с вендорским UI:
 *  - каталог: GET /api/v3/ui/video_layouts/presets (source="codian", поля
 *    id/title/windows/slots — геометрия уже в ответе, один запрос на всё);
 *    токен UI — localStorage["TrueConfMCU_AccessToken"], заголовок
 *    `Authorization: Bearer …` (тот же, что у axios-интерсептора в бандле);
 *  - превью: SVG-иконка карточки рисуется из `slots` каталога (позиции
 *    1-based, z_order — порядок отрисовки), цвета и размеры — вендорскими
 *    CSS-правилами `.layout-scheme-selector-item__icon`;
 *  - размещение: отдельной секцией под `.base-layout-configurator` внутри
 *    `.manual-layout-configurator`. В левую панель схем (300 px) группа не
 *    влезает: 54 карточки делают её вдвое выше предпросмотра, а
 *    `.layout-grid-content` держит 16:9 по высоте конфигуратора — панель
 *    сжимается до min-content (замерено: 144 px), предпросмотр растягивается
 *    до ~1513 px и вылезает за карточку. Полноширинная секция ниже сохраняет
 *    вендорскую геометрию страницы: палитра 300 px, предпросмотр 908 px;
 *    секция использует только вендорские классы (`.main-form-section-16`,
 *    `.main-form__subheader`, `.layout-scheme-selector`), без нового CSS;
 *  - выбор схемы: тот же путь, что у вендорской карточки — пишем preset в
 *    pinia-store `layout-editor` (`editValueField("preset", id)`), а не
 *    выдумываем свой запрос; сохранение/применение делает штатная кнопка
 *    «Создать»/«Сохранить»/«Применить», которая отправляет payload.preset и
 *    доходит до `gateways.apply_layout` → `vks_layout::codian::geometry`.
 *  - слоты: вендорский фолбэк числа окон — хвост id (`ss(preset)`), у
 *    `codian_52` это 52 окна вместо реальных. Поэтому вместе с preset кладём
 *    массив слотов ровно по `windows` из каталога: иначе сетка предпросмотра
 *    нарисует лишние окна.
 *
 * Деградация: нет `.manual-layout-configurator` (другая страница/режим
 * auto|dynamic), нет токена, 401/404/400 каталога — молча ничего не
 * вставляем; вендорская страница работает как раньше. Повторный запуск
 * идемпотентен (флаг + data-атрибут группы), перерисовки Vue ловятся
 * MutationObserver'ом + $subscribe на сторе.
 */
(function () {
  'use strict';
  if (window.__vksCodianLayoutsInstalled) return;
  window.__vksCodianLayoutsInstalled = true;

  // Заголовок группы владелец удалил; константа осталась только как
  // доступное имя группы (aria-label), в DOM название не выводится.
  var GROUP_TITLE = 'Раскладки Codian';
  var CATALOG_URL = '/api/v3/ui/video_layouts/presets';
  var TOKEN_KEY = 'TrueConfMCU_AccessToken';
  var HOST_SELECTOR = '.manual-layout-configurator';
  var GROUP_ATTR = 'data-vks-codian-group';
  var ITEM_ATTR = 'data-codian-id';
  var PREVIEW_WIDTH = 320;
  var PREVIEW_HEIGHT = 180;
  var PREVIEW_INSET = 3;
  var ITEMS_PER_LINE = 8;

  // Конструктор «Схема расположения участников»: параметрические id
  // (`vks_grid_9`, `vks_pin_r_8`) — геометрию из id считает сервер, поэтому
  // превью (calculate) и применение (apply_layout) всегда совпадают.
  var CONSTRUCTOR_ATTR = 'data-vks-constructor';
  // Пункт «Конструктор» в вендорском селекторе режимов раскладки: та же
  // разметка/классы, что у auto/dynamic/manual, вставляется после «Ручная
  // настройка». Клик переводит редактор в ручной режим и раскрывает секцию
  // конструктора (схемы vks_grid_* / vks_pin_*).
  var MODE_SELECTOR = '.layout-mode-selector';
  var MODE_TILE_ATTR = 'data-vks-constructor-tile';
  var modeTileEngaged = false;
  var CONSTRUCTOR_MODE_ATTR = 'data-vks-constructor-mode';
  var CONSTRUCTOR_MODES = [
    { key: 'grid', title: 'Равномерная сетка', id: function (count) { return 'vks_grid_' + count; } },
    { key: 'pin_l', title: 'Крупный слева, лента справа', id: function (count) { return 'vks_pin_l_' + count; } },
    { key: 'pin_r', title: 'Крупный справа, лента слева', id: function (count) { return 'vks_pin_r_' + count; } },
    { key: 'pin_t', title: 'Крупный сверху, лента снизу', id: function (count) { return 'vks_pin_t_' + count; } },
    { key: 'pin_b', title: 'Крупный снизу, лента сверху', id: function (count) { return 'vks_pin_b_' + count; } }
  ];

  var constructorCount = 0;
  var constructorSide = 'grid'; // grid | l | r | t | b (последняя выбранная схема)
  var adoptPending = '';
  // Целевой preset конструктора: вендор после mode='manual' асинхронно
  // сбрасывает preset на preset_equal_25, поэтому цель переутверждаем
  // в schedule(), пока пользователь не выберет другое число окон.
  var constructorTarget = '';
  var constructorState = 'idle'; // idle | loading | ready | failed
  var constructorSlots = Object.create(null);

  var catalog = [];
  var catalogState = 'idle'; // idle | loading | ready | failed
  var serverCount = 0; // авторитетное N сохранённой раскладки с сервера
  var serverCountKey = '';
  var subscribedStore = null;
  var scheduled = false;
  var observer = null;

  function readToken() {
    try {
      return localStorage.getItem(TOKEN_KEY) || '';
    } catch (e) {
      return '';
    }
  }

  function request(path, options) {
    if (typeof window.fetch !== 'function') return null;
    var token = readToken();
    if (!token) return null;
    var init = options || {};
    init.headers = { Authorization: 'Bearer ' + token, Accept: 'application/json' };
    if (init.body) init.headers['Content-Type'] = 'application/json';
    init.credentials = 'same-origin';
    return window.fetch(path, init).then(function (response) {
      if (!response.ok) throw new Error('HTTP ' + response.status);
      return response.json();
    });
  }

  /* N из сохранённой раскладки: вендорский store после fallback size/25 не
   * отражает исходное число окон, поэтому один раз читаем серверный payload. */
  function loadLayoutCount() {
    var match = /\/monitoring\/conferences\/([^/]+)\/layouts\/([^/]+)/.exec(window.location.pathname);
    if (!match) return;
    var key = match[1] + '/' + match[2];
    if (key === serverCountKey) return;
    serverCountKey = key;
    var result = request('/api/v3/ui/conferences/' + match[1] + '/video_layouts/' + match[2]);
    if (!result) return;
    result.then(function (data) {
      var payload = (data && (data.video_layout || data.payload)) || data || {};
      var count = Array.isArray(payload.slots) ? payload.slots.length : 0;
      if (!(count > 0)) count = Number(payload.size) || 0;
      if (count > 0) serverCount = count;
    }).catch(function () { serverCountKey = ''; });
  }

  function pinia() {
    var root = document.getElementById('root') || document.getElementById('app');
    var app = root && root.__vue_app__;
    var globals = app && app.config && app.config.globalProperties;
    return (globals && globals.$pinia) || null;
  }

  function store(name) {
    var instance = pinia();
    if (!instance || !instance._s || typeof instance._s.get !== 'function') return null;
    return instance._s.get(name) || null;
  }

  function layoutEditor() {
    return store('layout-editor');
  }

  function subscribe(editor) {
    if (!editor || editor === subscribedStore || typeof editor.$subscribe !== 'function') return;
    subscribedStore = editor;
    try {
      editor.$subscribe(function () { schedule(); reAdopt(); }, { detached: true });
    } catch (e) {
      /* наблюдатель остаётся единственным источником обновлений */
    }
  }

  /* Явный выбор числа окон в вендорском счётчике отменяет цель конструктора:
   * дальше reAdopt подхватывает новое N из слотов. */
  function onChipPointer(event) {
    var target = event.target;
    if (!target || typeof target.closest !== 'function') return;
    if (!target.closest('.layout-size-range-selector-item')) return;
    constructorTarget = '';
  }

  /* Вендорские чипы «Количество видеоокон»/пресеты ставят свой preset поверх
   * нашего. Пока «Конструктор» активен, возвращаем vks_<side>_<N> с новым N:
   * иначе сохранится вендорский preset, а карточки останутся без active. */
  function reAdopt() {
    if (!modeTileEngaged) return;
    var editor = layoutEditor();
    if (!editor) return;
    var value = editor.modifiedValue || {};
    if (value.mode !== 'manual') return;
    var preset = typeof value.preset === 'string' ? value.preset : '';
    if (preset.indexOf('vks_') === 0) return;
    // Вендор после mode='manual' ставит свой дефолт preset_equal_25 уже после
    // нашего выбора — возвращаем цель (исходное N из auto/ручной раскладки).
    if (constructorTarget && preset === 'preset_equal_25') {
      selectConstructor(constructorTarget);
      return;
    }
    var count = currentCount();
    if (!(count > 0)) return;
    var key = preset + ':' + count;
    if (key === adoptPending) return;
    adoptPending = key;
    selectConstructor('vks_' + (constructorSide === 'grid' ? 'grid' : 'pin_' + constructorSide) + '_' + count);
  }

  function round(value) {
    return Math.round(value * 100) / 100;
  }

  function svgFromSlots(slots) {
    // Один предикат валидности на оба прохода: иначе bbox может учесть слот,
    // который не нарисуется (например, width=Infinity из испорченного JSON),
    // и viewBox станет невалидным.
    var valid = [];
    for (var i = 0; i < slots.length; i += 1) {
      var slot = slots[i] || {};
      var x = Number(slot.x);
      var y = Number(slot.y);
      var width = Number(slot.width);
      var height = Number(slot.height);
      if (!isFinite(x) || !isFinite(y) || !isFinite(width) || !isFinite(height)) continue;
      if (width <= 0 || height <= 0) continue;
      valid.push({
        x: x,
        y: y,
        width: width,
        height: height,
        z: Number(slot.z_order) || 0,
        position: Number(slot.position) || 0,
      });
    }
    if (!valid.length) return '';
    valid.sort(function (a, b) {
      return a.z !== b.z ? a.z - b.z : a.position - b.position;
    });
    // Геометрия каталога — px канваса (1280×720), иконка ~44×28 px, поэтому
    // зазор должен быть долей макета, а не фиксированным числом пикселей.
    var maxX = 0;
    var maxY = 0;
    for (var b = 0; b < valid.length; b += 1) {
      maxX = Math.max(maxX, valid[b].x + valid[b].width);
      maxY = Math.max(maxY, valid[b].y + valid[b].height);
    }
    // Полузазор сжимает КАЖДУЮ ячейку со всех четырёх сторон: соседние дают
    // 2h между собой, по периметру остаётся поле h. Так одиночные ячейки
    // одинаковы независимо от края (раньше крайние были шире внутренних на h).
    // Полузазоры по осям — одинаковая ДОЛЯ канваса (2%), поэтому соседние
    // ячейки дают 2h между собой, по периметру остаётся поле h, а 16:9
    // ячейки остаются 16:9 (единый half по обеим осям искажал аспект ~10%).
    var halfX = Math.max(1, Math.round(maxX * 0.02));
    var halfY = Math.max(1, Math.round(maxY * 0.02));
    var radius = Math.min(halfX, halfY);
    var rects = [];
    for (var r = 0; r < valid.length; r += 1) {
      var cell = valid[r];
      var ix = cell.x + halfX;
      var iy = cell.y + halfY;
      var iw = cell.width - 2 * halfX;
      var ih = cell.height - 2 * halfY;
      if (iw <= 0 || ih <= 0) {
        ix = cell.x;
        iy = cell.y;
        iw = cell.width;
        ih = cell.height;
      }
      rects.push(
        '<rect x="' + round(ix) + '" y="' + round(iy) + '" width="' + round(iw) +
        '" height="' + round(ih) + '" rx="' + round(radius) + '"></rect>'
      );
    }
    return '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 ' + round(maxX) + ' ' + round(maxY) +
      '" fill="none" preserveAspectRatio="xMidYMid meet">' + rects.join('') + '</svg>';
  }

  /* Заглушка, если каталог пришёл без `slots` (старый сервер): равная сетка. */
  function svgFallback(windows) {
    var count = windows > 0 ? windows : 1;
    var cols = Math.ceil(Math.sqrt(count));
    var rows = Math.ceil(count / cols);
    var cellWidth = PREVIEW_WIDTH / cols;
    var cellHeight = PREVIEW_HEIGHT / rows;
    var slots = [];
    for (var i = 0; i < count; i += 1) {
      slots.push({
        position: i + 1,
        x: (i % cols) * cellWidth,
        y: Math.floor(i / cols) * cellHeight,
        width: cellWidth,
        height: cellHeight,
        z_order: 0,
      });
    }
    return svgFromSlots(slots);
  }

  /* «1 окно / 2 окна / 5 окон» — подпись карточки по-русски. */
  function windowsWord(count) {
    var tail = count % 100;
    if (tail >= 11 && tail <= 14) return 'окон';
    var last = count % 10;
    if (last === 1) return 'окно';
    if (last >= 2 && last <= 4) return 'окна';
    return 'окон';
  }

  /* Число окон карточки: data-windows + вендорский бейдж
   * (.layout-scheme-selector-item__badge, absolute top-right). Бейдж
   * декоративный (aria-hidden): число уже в aria-label карточки. */
  function setCardCount(item, count) {
    item.setAttribute('data-windows', String(count));
    if (!(count > 0)) {
      var old = item.querySelector('.layout-scheme-selector-item__badge');
      if (old) old.remove();
      return;
    }
    var badge = item.querySelector('.layout-scheme-selector-item__badge');
    if (!badge) {
      badge = document.createElement('div');
      badge.className = 'layout-scheme-selector-item__badge';
      badge.setAttribute('aria-hidden', 'true');
      item.appendChild(badge);
    }
    badge.textContent = String(count);
  }

  function findEntry(id) {
    for (var i = 0; i < catalog.length; i += 1) {
      if (catalog[i].id === id) return catalog[i];
    }
    return null;
  }

  function loadCatalog() {
    if (catalogState !== 'idle') return;
    catalogState = 'loading';
    var pending = request(CATALOG_URL);
    if (!pending) {
      catalogState = 'failed';
      return;
    }
    pending.then(
      function (data) {
        var list = data && data.presets;
        if (!Array.isArray(list)) {
          catalogState = 'failed';
          return;
        }
        var codian = [];
        for (var i = 0; i < list.length; i += 1) {
          var item = list[i];
          if (!item || item.source !== 'codian') continue;
          if (typeof item.id !== 'string' || !/^codian_[a-z0-9_]+$/.test(item.id)) continue;
          var windows = Number(item.windows);
          codian.push({
            id: item.id,
            title: typeof item.title === 'string' && item.title ? item.title : item.id,
            windows: isFinite(windows) && windows > 0 ? Math.floor(windows) : 0,
            slots: Array.isArray(item.slots) ? item.slots : [],
          });
        }
        if (!codian.length) {
          catalogState = 'failed';
          return;
        }
        catalog = codian;
        catalogState = 'ready';
        ensureGroup();
      },
      function () {
        catalogState = 'failed';
      }
    );
  }

  function buildItem(entry) {
    var item = document.createElement('div');
    item.className = 'layout-scheme-selector-item';
    item.setAttribute(ITEM_ATTR, entry.id);
    item.setAttribute('data-source', 'codian');
    item.setAttribute('role', 'button');
    item.setAttribute('tabindex', '0');
    item.setAttribute('aria-pressed', 'false');
    // Одно доступное имя для title/aria: число окон — в нём, а бейдж
    // помечен aria-hidden (декоративный дубль).
    var label = entry.windows > 0 ? entry.title + ' — ' + entry.windows + ' ' + windowsWord(entry.windows) : entry.title;
    item.setAttribute('title', label);
    item.setAttribute('aria-label', label);

    var container = document.createElement('span');
    container.className = 'tc-icon-container layout-scheme-selector-item__icon';
    var icon = document.createElement('i');
    icon.className = 'material-icons tc-icon__icon notranslate';
    icon.innerHTML = entry.slots.length ? svgFromSlots(entry.slots) : svgFallback(entry.windows);
    container.appendChild(icon);
    item.appendChild(container);
    setCardCount(item, entry.windows);
    return item;
  }

  function buildGroup() {
    var section = document.createElement('div');
    section.className = 'main-form-section-16';
    section.setAttribute(GROUP_ATTR, '1');

    var selector = document.createElement('div');
    selector.className = 'layout-scheme-selector';
    selector.setAttribute('role', 'group');
    selector.setAttribute('aria-label', GROUP_TITLE);
    for (var i = 0; i < catalog.length; i += ITEMS_PER_LINE) {
      var group = document.createElement('div');
      group.className = 'layout-scheme-selector__group';
      for (var j = i; j < Math.min(i + ITEMS_PER_LINE, catalog.length); j += 1) {
        group.appendChild(buildItem(catalog[j]));
      }
      selector.appendChild(group);
    }
    section.appendChild(selector);

    section.addEventListener('click', onActivate);
    section.addEventListener('keydown', onKeydown);
    return section;
  }

  function closestItem(node) {
    var element = node;
    while (element && element !== document) {
      if (element.getAttribute && element.getAttribute(ITEM_ATTR)) return element;
      element = element.parentNode;
    }
    return null;
  }

  function onActivate(event) {
    var item = closestItem(event.target);
    if (!item) return;
    event.preventDefault();
    select(item.getAttribute(ITEM_ATTR));
  }

  function onKeydown(event) {
    if (event.key !== 'Enter' && event.key !== ' ' && event.key !== 'Spacebar') return;
    var item = closestItem(event.target);
    if (!item) return;
    event.preventDefault();
    select(item.getAttribute(ITEM_ATTR));
  }

  /* Тот же путь, что у вендорской карточки: preset в store редактора.
   * setSlots обязателен: число окон вендор выводит из хвоста id, а у
   * codian_XX хвост — номер макета, не размер. */
  function select(id) {
    var entry = findEntry(id);
    var editor = layoutEditor();
    if (!entry || !editor) return;
    var count = entry.windows > 0 ? entry.windows : 1;
    var value = editor.modifiedValue || {};
    var existing = Array.isArray(value.slots) && value.preset === id ? value.slots : null;
    var slots = Array.isArray(existing) && existing.length === count ? existing : [];
    if (!slots.length) {
      for (var i = 1; i <= count; i += 1) slots.push({ position: i, role: 'AUTO' });
    }
    try {
      editor.editValueField('preset', id);
      editor.setSlots(id, slots);
    } catch (e) {
      console.warn('codian-layouts: не удалось выбрать схему ' + id, e);
      return;
    }
    syncActive();
    subscribe(editor);
  }

  function syncActive() {
    var editor = layoutEditor();
    var preset = editor && editor.modifiedValue ? editor.modifiedValue.preset : null;
    var items = document.querySelectorAll('[' + ITEM_ATTR + ']');
    for (var i = 0; i < items.length; i += 1) {
      var item = items[i];
      var active = preset !== null && item.getAttribute(ITEM_ATTR) === preset;
      if (active) {
        if (!item.classList.contains('layout-scheme-selector-item--active')) {
          item.classList.add('layout-scheme-selector-item--active');
        }
        item.setAttribute('aria-pressed', 'true');
      } else {
        if (item.classList.contains('layout-scheme-selector-item--active')) {
          item.classList.remove('layout-scheme-selector-item--active');
        }
        item.setAttribute('aria-pressed', 'false');
      }
    }
    if (editor) subscribe(editor);
  }


  /* Число окон текущей раскладки в редакторе: слоты выбранного пресета (их
   * заполняет вендорский calculate), затем хвост id, затем size. */
  function currentCount() {
    var editor = layoutEditor();
    var value = editor && editor.modifiedValue;
    if (!value) return 0;
    var preset = typeof value.preset === 'string' ? value.preset : '';
    var slots = value.slots;
    if (Array.isArray(slots) && slots.length > 0 && slots.length <= 64) return slots.length;
    var entry = findEntry(preset);
    if (entry && entry.windows > 0) return entry.windows;
    var tail = /_(\d+)$/.exec(preset);
    if (tail) {
      var fromId = Number(tail[1]);
      if (fromId >= 1 && fromId <= 64) return fromId;
    }
    var size = Number(value.size);
    if (isFinite(size) && size >= 1 && size <= 64) return Math.floor(size);
    return 0;
  }

  /* Превью всех вариантов конструктора: один calculate на вариант (как у
   * вендора), ответ кэшируется до смены числа окон. */
  function loadConstructor(count) {
    if (constructorState === 'loading' && constructorCount === count) return;
    constructorCount = count;
    constructorState = 'loading';
    constructorSlots = Object.create(null);
    var results = Object.create(null);
    var requests = [];
    CONSTRUCTOR_MODES.forEach(function (mode) {
      requests.push(
        request('/api/v3/ui/video_layouts/calculate', {
          method: 'POST',
          body: JSON.stringify({ preset: mode.id(count), width: PREVIEW_WIDTH, height: PREVIEW_HEIGHT })
        }).then(
          function (data) { results[mode.key] = data && Array.isArray(data.slots) ? data.slots : null; },
          function () { results[mode.key] = null; }
        )
      );
    });
    Promise.all(requests).then(function () {
      if (constructorCount !== count) return;
      var ready = 0;
      for (var key in results) {
        if (Object.prototype.hasOwnProperty.call(results, key) && results[key]) ready += 1;
      }
      constructorSlots = results;
      constructorState = ready > 0 ? 'ready' : 'failed';
      ensureConstructor();
    });
  }

  function buildConstructorItem(mode, slots, count) {
    var preset = mode.id(count);
    var item = document.createElement('div');
    item.className = 'layout-scheme-selector-item';
    item.setAttribute(CONSTRUCTOR_MODE_ATTR, mode.key);
    item.setAttribute('data-preset', preset);
    item.setAttribute('role', 'button');
    item.setAttribute('tabindex', '0');
    item.setAttribute('aria-pressed', 'false');
    item.setAttribute('aria-label', mode.title + ' — ' + count + ' ' + windowsWord(count));
    item.setAttribute('title', mode.title + ' — ' + count + ' ' + windowsWord(count));
    var container = document.createElement('span');
    container.className = 'tc-icon-container layout-scheme-selector-item__icon';
    var icon = document.createElement('i');
    icon.className = 'material-icons tc-icon__icon notranslate';
    icon.innerHTML = slots && slots.length ? svgFromSlots(slots) : svgFallback(count);
    container.appendChild(icon);
    item.appendChild(container);
    setCardCount(item, count);
    return item;
  }

  function buildConstructorSelector(count) {
    var selector = document.createElement('div');
    selector.className = 'layout-scheme-selector';
    selector.setAttribute('role', 'group');
    selector.setAttribute('aria-label', 'Схема расположения участников');
    var group = document.createElement('div');
    group.className = 'layout-scheme-selector__group';
    for (var i = 0; i < CONSTRUCTOR_MODES.length; i += 1) {
      var mode = CONSTRUCTOR_MODES[i];
      group.appendChild(buildConstructorItem(mode, constructorSlots[mode.key], count));
    }
    selector.appendChild(group);
    return selector;
  }

  function syncConstructorActive() {
    var editor = layoutEditor();
    var preset = editor && editor.modifiedValue ? editor.modifiedValue.preset : null;
    var items = document.querySelectorAll('[' + CONSTRUCTOR_MODE_ATTR + ']');
    for (var i = 0; i < items.length; i += 1) {
      var item = items[i];
      var active = preset !== null && item.getAttribute('data-preset') === preset;
      if (active) item.classList.add('layout-scheme-selector-item--active');
      else item.classList.remove('layout-scheme-selector-item--active');
      item.setAttribute('aria-pressed', active ? 'true' : 'false');
    }
  }

  function ensureConstructor() {
    var host = document.querySelector(HOST_SELECTOR);
    if (!host) return;
    var count = currentCount();
    if (count < 1) return;
    if (constructorCount !== count) {
      loadConstructor(count);
      return;
    }
    if (constructorState !== 'ready') return;
    var section = host.querySelector('[' + CONSTRUCTOR_ATTR + ']');
    var firstGroup = host.querySelector('[' + GROUP_ATTR + ']');
    if (!section) {
      section = document.createElement('div');
      section.className = 'main-form-section-16';
      section.setAttribute(CONSTRUCTOR_ATTR, '1');
      section.addEventListener('click', onConstructorActivate);
      section.addEventListener('keydown', onConstructorKeydown);
      // Секция конструктора ставится перед группой Codian: при переходе на
      // пункт «Конструктор» она видна сразу, без прокрутки 54 карточек.
      if (firstGroup && firstGroup.parentNode === host) host.insertBefore(section, firstGroup);
      else host.appendChild(section);
    }
    // Перерисовываем только при смене числа окон: MutationObserver ловит наши
    // же вставки, поэтому безусловный innerHTML дал бы бесконечный цикл.
    var rendered = section.getAttribute('data-count');
    var items = section.querySelectorAll('[' + CONSTRUCTOR_MODE_ATTR + ']');
    if (rendered === String(count) && items.length === CONSTRUCTOR_MODES.length) {
      syncConstructorActive();
      return;
    }
    section.setAttribute('data-count', String(count));
    section.innerHTML = '';
    section.appendChild(buildConstructorSelector(count));
    syncConstructorActive();
  }

  function closestConstructorItem(node) {
    var element = node;
    while (element && element !== document) {
      if (element.getAttribute && element.getAttribute(CONSTRUCTOR_MODE_ATTR)) return element;
      element = element.parentNode;
    }
    return null;
  }

  function onConstructorActivate(event) {
    var item = closestConstructorItem(event.target);
    if (!item) return;
    event.preventDefault();
    selectConstructor(item.getAttribute('data-preset'));
  }

  function onConstructorKeydown(event) {
    if (event.key !== 'Enter' && event.key !== ' ' && event.key !== 'Spacebar') return;
    var item = closestConstructorItem(event.target);
    if (!item) return;
    event.preventDefault();
    selectConstructor(item.getAttribute('data-preset'));
  }

  /* Наш id кодирует число окон: вендорский геттер сам синтезирует `ss(id)`
   * AUTO-слотов из хвоста, поэтому достаточно записать preset. */
  function selectConstructor(preset) {
    var editor = layoutEditor();
    if (!editor || !preset) return;
    var side = /^vks_pin_([lrtb])_/.exec(preset);
    constructorSide = side ? side[1] : 'grid';
    try {
      editor.editValueField('preset', preset);
    } catch (e) {
      console.warn('codian-layouts: не удалось выбрать схему конструктора ' + preset, e);
      return;
    }
    syncActive();
    syncConstructorActive();
    subscribe(editor);
  }


  function buildModeTile() {
    var tile = document.createElement('div');
    tile.className = 'layout-mode-selector-item';
    tile.setAttribute(MODE_TILE_ATTR, '1');
    tile.setAttribute('data-qa', 'layout-mode-selector-constructor');
    tile.setAttribute('role', 'button');
    tile.setAttribute('tabindex', '0');
    var text = document.createElement('p');
    text.className = 'layout-mode-selector-item__text';
    text.textContent = 'Конструктор';
    var description = document.createElement('p');
    description.className = 'layout-mode-selector-item__description';
    description.textContent = 'Схема расположения участников: сетка или крупный участник с лентой';
    tile.appendChild(text);
    tile.appendChild(description);
    tile.addEventListener('click', onModeTileActivate);
    tile.addEventListener('keydown', function (event) {
      if (event.key !== 'Enter' && event.key !== ' ' && event.key !== 'Spacebar') return;
      event.preventDefault();
      onModeTileActivate(event);
    });
    return tile;
  }

  function onModeTileActivate(event) {
    event.preventDefault();
    var editor = layoutEditor();
    if (!editor) return;
    modeTileEngaged = true;
    // N/имя фиксируем ДО mode='manual': после переключения вендор подставляет
    // свой fallback (size=25), и currentCount() уже не равен исходному N.
    var startValue = editor.modifiedValue || {};
    var startPreset = typeof startValue.preset === 'string' ? startValue.preset : '';
    // В manual слоты — источник истины; в auto/dynamic вендорский store уже
    // мог получить fallback size, поэтому берём серверный payload раскладки.
    var startCount = startValue.mode === 'manual' ? currentCount() : 0;
    if (!(startCount > 0)) startCount = serverCount;
    if (!(startCount > 0)) startCount = currentCount();
    if (!(startCount > 0)) {
      // Хвост нашего/вендорского preset'а — надёжный размер (codian_* исключён).
      var tail = /^(?:vks_|preset_)/.test(startPreset) ? /_(\d+)$/.exec(startPreset) : null;
      if (tail) startCount = Number(tail[1]);
    }
    if (!(startCount > 0)) {
      var chip = document.querySelector('.layout-size-range-selector-item--active');
      if (chip) startCount = parseInt(chip.textContent, 10) || 0;
    }
    if (!(startCount > 0)) startCount = Number(startValue.size) || 0;
    var startName = startValue.displayName || '';
    var startSide = constructorSide;
    // Вендорский `mode` — свойство редактора (reactivity store), не поле формы:
    // `editValueField('mode')` пишет в черновик раскладки и портит payload.
    try {
      editor.mode = 'manual';
    } catch (e) {
      console.warn('codian-layouts: не удалось переключить режим на manual', e);
    }
    syncModeTile();
    schedule();
    // Вкладка «Конструктор» — наш конструктор: после перерисовки ручного
    // режима (слоты появляются не сразу) один раз подставляем vks_grid_<N>.
    // Тихий fallback на 1 окно запрещён: при N=0 остаёмся на вендорском preset.
    var attempts = 0;
    var keepName = startName;
    function family() {
      return 'vks_' + (startSide === 'grid' ? 'grid' : 'pin_' + startSide) + '_';
    }
    if (startCount > 0) {
      try {
        if (keepName) editor.editValueField('displayName', keepName);
        editor.editValueField('type', 'individual');
      } catch (e) { /* поля недоступны — останутся вендорские */ }
      constructorTarget = family() + startCount;
      selectConstructor(constructorTarget);
      return;
    }
    function adopt() {
      if (!modeTileEngaged) return;
      var value = editor.modifiedValue || {};
      var preset = typeof value.preset === 'string' ? value.preset : '';
      if (preset.indexOf('vks_') === 0) return;
      var count = currentCount();
      if (count > 0) {
        // Свежий auto→manual: вендорский watch мог сбросить имя/тип — возвращаем,
        // иначе save уходит с display_name:null, type:default (P0 T2).
        try {
          if (keepName) editor.editValueField('displayName', keepName);
          editor.editValueField('type', 'individual');
        } catch (e) { /* поля недоступны — сохранение увидит вендорские значения */ }
        constructorTarget = 'vks_grid_' + count;
        selectConstructor(constructorTarget);
        return;
      }
      if (attempts++ < 40) setTimeout(adopt, 50);
    }
    adopt();
    var section = document.querySelector('[' + CONSTRUCTOR_ATTR + ']');
    if (section && typeof section.scrollIntoView === 'function') {
      section.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
    }
  }

  function syncModeTile() {
    var tile = document.querySelector('[' + MODE_TILE_ATTR + ']');
    if (!tile) return;
    var editor = layoutEditor();
    var value = editor && editor.modifiedValue ? editor.modifiedValue : null;
    var mode = value ? value.mode : null;
    var preset = value ? value.preset : null;
    if (mode !== 'manual') modeTileEngaged = false;
    var active = modeTileEngaged || (typeof preset === 'string' && preset.indexOf('vks_') === 0);
    if (active) {
      tile.classList.add('layout-mode-selector-item--active');
      var actives = document.querySelectorAll('.layout-mode-selector-item--active');
      for (var i = 0; i < actives.length; i += 1) {
        if (actives[i] !== tile) {
          actives[i].classList.remove('layout-mode-selector-item--active');
          actives[i].setAttribute('aria-pressed', 'false');
        }
      }
    } else {
      tile.classList.remove('layout-mode-selector-item--active');
    }
    tile.setAttribute('aria-pressed', active ? 'true' : 'false');
  }

  function ensureModeTile() {
    var selector = document.querySelector(MODE_SELECTOR);
    if (!selector) return;
    var tile = selector.querySelector('[' + MODE_TILE_ATTR + ']');
    if (!tile) {
      tile = buildModeTile();
      var manual = selector.querySelector('[data-qa="layout-mode-selector-manual"]');
      if (manual && manual.parentNode) manual.parentNode.insertBefore(tile, manual.nextSibling);
      else selector.appendChild(tile);
    }
    syncModeTile();
  }

  function ensureGroup() {
    if (catalogState !== 'ready') return;
    var host = document.querySelector(HOST_SELECTOR);
    if (!host) return; // ручной режим не открыт (auto/dynamic или другая страница)
    if (host.querySelector('[' + GROUP_ATTR + ']')) {
      syncActive();
      return;
    }
    host.appendChild(buildGroup());
    syncActive();
  }

  function schedule() {
    if (scheduled) return;
    scheduled = true;
    var run = function () {
      scheduled = false;
      try {
        // Пункт «Конструктор» живёт в вендорском селекторе режимов и должен
        // быть виден на всех режимах, поэтому вставляется до проверки host.
        ensureModeTile();
        loadLayoutCount();
        var host = document.querySelector(HOST_SELECTOR);
        if (!host) return;
        if (catalogState === 'idle') loadCatalog();
        if (catalogState === 'ready') ensureGroup();
        if (catalogState === 'ready') ensureConstructor();
        reAdopt();
      } catch (e) {
        /* вендорская страница важнее нашей группы, но поломка не должна быть невидимой */
        console.warn('codian-layouts: группа Codian не вставлена', e);
      }
    };
    if (typeof window.requestAnimationFrame === 'function') {
      window.requestAnimationFrame(run);
    } else {
      window.setTimeout(run, 16);
    }
  }

  function start() {
    if (observer) return;
    observer = new MutationObserver(function () { schedule(); });
    observer.observe(document.documentElement, { childList: true, subtree: true });
    schedule();
  }

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', start);
  } else {
    start();
  }
})();

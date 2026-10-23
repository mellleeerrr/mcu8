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
  var ITEMS_PER_LINE = 8;
  // Счётчик «Количество видеокон» конструктора: диапазон и вендорская
  // разметка `.dynamic-layout-configurator-counter` (1:1 с 2.2.1.66).
  var MAX_COUNT = 100;

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
  // Однократный отложенный выбор после переключения в ручной режим: пока он
  // не null, reAdopt молчит (иначе принял бы вендорский preset_equal_25 и
  // поставил 25 вместо исходного N). Выполняется из reAdopt, когда вендор
  // выставил свой preset_*, или из страховочного таймера (медленный вендор).
  var pendingAdopt = null;
  var identityName = ''; // displayName серверной раскладки: вендор может обнулить
  var constructorState = 'idle'; // idle | loading | ready | failed
  var constructorSlots = Object.create(null);
  var counterValue = 0;

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
      // GET отдаёт { video_layout: { payload: {...} } }; старый разбор читал
      // video_layout как payload и всегда получал 0.
      var layout = (data && data.video_layout) || data || {};
      var payload = layout.payload || layout;
      var count = Array.isArray(payload.slots) ? payload.slots.length : 0;
      if (!(count > 0)) count = Number(payload.size) || 0;
      if (count > 0) serverCount = count;
      if (payload.display_name) identityName = String(payload.display_name);
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

  /* Вендорские чипы «Количество видеоокон»/пресеты ставят свой preset поверх
   * нашего. Пока «Конструктор» активен, возвращаем vks_<side>_<N> с новым N:
   * иначе сохранится вендорский preset, а карточки останутся без active. */
  function reAdopt() {
    var editor = layoutEditor();
    if (!editor) return;
    var value = editor.modifiedValue || {};
    rememberIdentity(value);
    if (value.mode !== 'manual') return;
    var preset = typeof value.preset === 'string' ? value.preset : '';
    // Сохранённая vks_* (reopen) — это уже конструктор: запоминаем сторону и
    // считаем тайл активным, иначе первый же вендорский чип сломает раскладку.
    if (preset.indexOf('vks_') === 0) {
      var side = /^vks_pin_([lrtb])_/.exec(preset);
      constructorSide = side ? side[1] : 'grid';
      modeTileEngaged = true;
      return;
    }
    // Пока идёт первичный выбор — ждём, пока вендор выставит свой preset_*.
    // Этот же флаг (pendingAdopt !== null) — единственный источник состояния
    // «adopt ещё не выполнен» (отдельного initialAdopt нет).
    if (pendingAdopt) {
      if (preset.indexOf('preset_') === 0) {
        var runAdopt = pendingAdopt;
        pendingAdopt = null;
        runAdopt();
      }
      return;
    }
    if (!modeTileEngaged) return;
    // Только вендорские preset_* — Codian-карточки не перехватываем.
    if (preset.indexOf('preset_') !== 0) return;
    var count = currentCount();
    if (!(count > 0)) return;
    // Без дедупликации: повторный клик того же чипа тоже обязан вернуть vks_*
    // (после adopt preset становится vks_*, и следующий subscribe выходит выше).
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

  /* Вендор обнуляет displayName/type при переходе в manual (канонический auto
   * имеет type:"default"). Держим серверное имя и возвращаем его перед выбором
   * схемы; type конструктора всегда individual. */
  function rememberIdentity(value) {
    if (value && typeof value.displayName === 'string' && value.displayName) identityName = value.displayName;
  }

  function restoreIdentity(editor) {
    try {
      if (identityName) editor.editValueField('displayName', identityName);
      editor.editValueField('type', 'individual');
    } catch (e) { /* поля недоступны — останутся вендорские */ }
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
    // Явный выбор Codian-схемы выводит из конструктора: иначе reAdopt вернул бы
    // наш vks_* и карточка каталога осталась бы без active и без сохранения.
    modeTileEngaged = false;
    pendingAdopt = null;
    syncModeTile();
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
    if (Array.isArray(slots) && slots.length > 0 && slots.length <= MAX_COUNT) return slots.length;
    var entry = findEntry(preset);
    if (entry && entry.windows > 0) return entry.windows;
    var tail = /_(\d+)$/.exec(preset);
    if (tail) {
      var fromId = Number(tail[1]);
      if (fromId >= 1 && fromId <= MAX_COUNT) return fromId;
    }
    var size = Number(value.size);
    if (isFinite(size) && size >= 1 && size <= MAX_COUNT) return Math.floor(size);
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

  /* Вендорский CounterComponent (2.2.1.66): `.counter` = «мин.» + центр
   * (−, number-input max=100, +) + «макс.»; кнопки — TcButton (mdc-button). */
  function buildCounterButton(action, label, icon, ariaLabel) {
    // Разметка 1:1 вендорского TcButton (mode="base", type="text"):
    //   div.tc-button-container.tc-button-container--base > button.tc-button.mdc-button--text
    //     > span.mdc-button__ripple + div.tc-button__content > i.tc-button__icon
    //     / span.mdc-button__label.tc-button__text.
    var container = document.createElement('div');
    container.className = 'tc-button-container tc-button-container--base counter__button';
    container.setAttribute('data-counter-action', action);
    if (ariaLabel) {
      container.setAttribute('aria-label', ariaLabel);
      container.setAttribute('title', ariaLabel);
    }
    var button = document.createElement('button');
    button.type = 'button';
    button.className = 'tc-button mdc-button mdc-button--text' + (icon ? ' tc-button--icon' : '');
    var ripple = document.createElement('span');
    ripple.className = 'mdc-button__ripple';
    button.appendChild(ripple);
    var content = document.createElement('div');
    content.className = 'tc-button__content';
    if (icon) {
      var glyph = document.createElement('i');
      glyph.className = 'material-icons tc-button__icon mdc-button__icon notranslate';
      glyph.textContent = icon;
      content.appendChild(glyph);
    } else {
      var text = document.createElement('span');
      text.className = 'mdc-button__label tc-button__text';
      text.textContent = label;
      content.appendChild(text);
    }
    button.appendChild(content);
    container.appendChild(button);
    return container;
  }

  function buildCounter(count) {
    var wrapper = document.createElement('div');
    wrapper.className = 'dynamic-layout-configurator-counter';
    wrapper.setAttribute('data-vks-counter', '1');
    var caption = document.createElement('p');
    caption.className = 'dynamic-layout-configurator-counter__text';
    caption.textContent = 'Количество видеокон';
    wrapper.appendChild(caption);

    var counter = document.createElement('div');
    counter.className = 'counter';
    counter.appendChild(buildCounterButton('min', 'мин.', null, 'Минимальное количество видеокон (1)'));
    var center = document.createElement('div');
    center.className = 'counter-center-segment';
    center.appendChild(buildCounterButton('dec', null, 'remove', 'Уменьшить количество видеокон'));
    var field = document.createElement('div');
    field.className = 'counter__text-field tc-text-field-container';
    var textField = document.createElement('div');
    textField.className =
      'tc-text-field mdc-text-field mdc-text-field--outlined tc-text-field--no-label mdc-text-field--no-label';
    var input = document.createElement('input');
    input.type = 'number';
    input.className = 'tc-text-field__input mdc-text-field__input';
    input.min = '1';
    input.max = String(MAX_COUNT);
    input.step = '1';
    input.value = String(count);
    input.setAttribute('data-counter-input', '1');
    input.setAttribute('aria-label', 'Количество видеокон');
    textField.appendChild(input);
    var outline = document.createElement('span');
    outline.className = 'tc-notched-outline mdc-notched-outline mdc-notched-outline--upgraded';
    var leading = document.createElement('span');
    leading.className = 'mdc-notched-outline__leading';
    var notch = document.createElement('span');
    notch.className = 'mdc-notched-outline__notch';
    var trailing = document.createElement('span');
    trailing.className = 'mdc-notched-outline__trailing';
    outline.appendChild(leading);
    outline.appendChild(notch);
    outline.appendChild(trailing);
    textField.appendChild(outline);
    field.appendChild(textField);
    center.appendChild(field);
    center.appendChild(buildCounterButton('inc', null, 'add', 'Увеличить количество видеокон'));
    counter.appendChild(center);
    counter.appendChild(buildCounterButton('max', 'макс.', null, 'Максимальное количество видеокон (100)'));
    wrapper.appendChild(counter);
    return wrapper;
  }

  function countForCounter() {
    var count = currentCount();
    if (count < 1) count = counterValue;
    return Math.max(1, Math.min(MAX_COUNT, count || 1));
  }

  function syncCounter(count) {
    var section = document.querySelector('[' + CONSTRUCTOR_ATTR + ']');
    if (!section) return;
    var value = Math.max(1, Math.min(MAX_COUNT, count));
    var input = section.querySelector('[data-counter-input]');
    if (input) input.value = String(value);
    var buttons = section.querySelectorAll('[data-counter-action]');
    for (var i = 0; i < buttons.length; i += 1) {
      var action = buttons[i].getAttribute('data-counter-action');
      var disabled = (action === 'min' || action === 'dec') ? value <= 1 : value >= MAX_COUNT;
      var inner = buttons[i].querySelector('button');
      if (inner) inner.disabled = disabled;
      if (disabled) buttons[i].setAttribute('aria-disabled', 'true');
      else buttons[i].removeAttribute('aria-disabled');
    }
    counterValue = value;
  }

  /* Смена N счётчиком: включить ручной режим (если нужно), перестроить ВСЕ
   * карточки конструктора под новое N и сохранить выбранную сторону. */
  function applyCount(next) {
    var count = Math.max(1, Math.min(MAX_COUNT, Math.floor(Number(next)) || 1));
    var editor = layoutEditor();
    if (!editor) return;
    var value = editor.modifiedValue || {};
    var preset = typeof value.preset === 'string' ? value.preset : '';
    var savedSide = /^vks_pin_([lrtb])_/.exec(preset);
    var side = savedSide ? savedSide[1] : constructorSide;
    modeTileEngaged = true;
    function apply() {
      if (!modeTileEngaged) return;
      var current = editor.modifiedValue || {};
      var currentPreset = typeof current.preset === 'string' ? current.preset : '';
      // В ручном режиме (в т.ч. codian_*/preset_equal_*) вендорский сброс уже
      // не придёт — ставим нашу схему сразу; в остальных режимах ждём
      // вендорский preset_* после переключения.
      if (current.mode !== 'manual' && currentPreset.indexOf('preset_') !== 0 && currentPreset.indexOf('vks_') !== 0) return;
      pendingAdopt = null;
      rememberIdentity(current);
      restoreIdentity(editor);
      selectConstructor('vks_' + (side === 'grid' ? 'grid' : 'pin_' + side) + '_' + count);
    }
    if (value.mode !== 'manual') {
      // Вендор после mode='manual' асинхронно ставит preset_equal_25; пока он
      // не сделал этого, reAdopt молчит (pendingAdopt), иначе примет 25.
      pendingAdopt = apply;
      try {
        editor.mode = 'manual';
      } catch (e) {
        pendingAdopt = null;
        console.warn('codian-layouts: не удалось переключить режим на manual', e);
        return;
      }
      syncModeTile();
      schedule();
      setTimeout(function () {
        var run = pendingAdopt;
        if (!run) return;
        pendingAdopt = null;
        run();
      }, 400);
      return;
    }
    apply();
  }

  function counterTarget(node) {
    var element = node;
    while (element && element !== document) {
      if (element.getAttribute && element.getAttribute('data-counter-action')) return element.getAttribute('data-counter-action');
      element = element.parentNode;
    }
    return '';
  }

  function onSectionClick(event) {
    var action = counterTarget(event.target);
    if (action) {
      event.preventDefault();
      var current = countForCounter();
      if (action === 'min') applyCount(1);
      else if (action === 'max') applyCount(MAX_COUNT);
      else if (action === 'dec') applyCount(current - 1);
      else if (action === 'inc') applyCount(current + 1);
      return;
    }
    onConstructorActivate(event);
  }

  // Ввод с клавиатуры: не пересоздаём input на каждую цифру (фокус терялся),
  // а применяем N с коротким дебаунсом; Enter/потеря фокуса применяют сразу.
  var counterInputTimer = null;
  function onCounterInput(event) {
    var target = event.target;
    if (!target || !target.getAttribute || !target.getAttribute('data-counter-input')) return;
    var count = Math.floor(Number(target.value));
    // Недобор (пусто/0) или перебор не применяем на каждую цифру: даём
    // дописать число, клампим в change/blur.
    if (!(count >= 1 && count <= MAX_COUNT)) return;
    if (counterInputTimer) window.clearTimeout(counterInputTimer);
    counterInputTimer = window.setTimeout(function () {
      counterInputTimer = null;
      applyCount(count);
    }, 300);
  }

  function onCounterChange(event) {
    var target = event.target;
    if (!target || !target.getAttribute || !target.getAttribute('data-counter-input')) return;
    if (counterInputTimer) {
      window.clearTimeout(counterInputTimer);
      counterInputTimer = null;
    }
    var count = Math.floor(Number(target.value));
    var next = count >= 1 && count <= MAX_COUNT ? count : count > MAX_COUNT ? MAX_COUNT : counterValue || 1;
    applyCount(next);
    syncCounter(next);
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
      section.addEventListener('click', onSectionClick);
      section.addEventListener('keydown', onConstructorKeydown);
      section.addEventListener('input', onCounterInput);
      section.addEventListener('change', onCounterChange);
      // Секция конструктора ставится перед группой Codian: при переходе на
      // пункт «Конструктор» она видна сразу, без прокрутки 54 карточек.
      if (firstGroup && firstGroup.parentNode === host) host.insertBefore(section, firstGroup);
      else host.appendChild(section);
    }
    // Перерисовываем только при смене числа окон: MutationObserver ловит наши
    // же вставки, поэтому безусловный innerHTML дал бы бесконечный цикл.
    var rendered = section.getAttribute('data-count');
    var items = section.querySelectorAll('[' + CONSTRUCTOR_MODE_ATTR + ']');
    if (
      rendered === String(count) &&
      items.length === CONSTRUCTOR_MODES.length &&
      section.querySelector('[data-vks-counter]')
    ) {
      syncConstructorActive();
      syncCounter(count);
      return;
    }
    section.setAttribute('data-count', String(count));
    var counter = section.querySelector('[data-vks-counter]');
    if (!counter) {
      counter = buildCounter(count);
      section.insertBefore(counter, section.firstChild);
    }
    var selector = section.querySelector('.layout-scheme-selector');
    if (selector && selector.parentNode) selector.parentNode.removeChild(selector);
    section.appendChild(buildConstructorSelector(count));
    syncConstructorActive();
    syncCounter(count);
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
    if (event.target && event.target.getAttribute && event.target.getAttribute('data-counter-input')) {
      if (event.key === 'Enter') onCounterChange(event);
      return;
    }
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
    // Сохранённая pin-раскладка задаёт сторону конструктора после reopen.
    var savedSide = /^vks_pin_([lrtb])_/.exec(startPreset);
    var startSide = savedSide ? savedSide[1] : constructorSide;
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
    var keepName = startName;
    function family() {
      return 'vks_' + (startSide === 'grid' ? 'grid' : 'pin_' + startSide) + '_';
    }
    function adoptNow() {
      pendingAdopt = null;
      // N=0 (медленный/неудачный GET и нет числового хвоста) — остаёмся на
      // вендорском preset, тихий vks_*_0 запрещён.
      if (!(startCount > 0)) return;
      rememberIdentity(startValue);
      if (keepName) identityName = keepName;
      restoreIdentity(editor);
      selectConstructor(family() + startCount);
    }
    // Уже ручной режим: вендорский сброс preset_equal_25 не произойдёт, а
    // текущий preset может быть codian_*/preset_equal_* — ставим сразу.
    if (startValue.mode === 'manual') {
      adoptNow();
      var manualSection = document.querySelector('[' + CONSTRUCTOR_ATTR + ']');
      if (manualSection && typeof manualSection.scrollIntoView === 'function') {
        manualSection.scrollIntoView({ block: 'nearest', behavior: 'smooth' });
      }
      return;
    }
    if (!(startCount > 0)) return;
    // Вендор после mode='manual' асинхронно ставит preset_equal_25 (дефолт на
    // 25 слотов). Ждём preset_* в reAdopt (его вызывает и подписка store, и
    // schedule() по MutationObserver после вендорских перерисовок) — таймер
    // только страховка для нестандартного пути; target-N уже зафиксирован.
    pendingAdopt = adoptNow;
    setTimeout(function () {
      // Вендор не успел за 400 мс: выбираем наше N сами (startCount уже
      // зафиксирован в adoptNow). Поздний сброс вендора после этого — вне
      // защиты: live он не воспроизводится (сброс ~100–350 мс).
      var run = pendingAdopt;
      if (!run) return;
      pendingAdopt = null;
      run();
    }, 400);
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
      // Ушли из конструктора в ручную/Codian-схему — вендорский тайл «Ручная
      // настройка» обязан остаться active (иначе ни один режим не подсвечен).
      if (mode === 'manual' && !document.querySelector('.layout-mode-selector-item--active')) {
        var manual = document.querySelector('[data-qa="layout-mode-selector-manual"]');
        if (manual) {
          manual.classList.add('layout-mode-selector-item--active');
          manual.setAttribute('aria-pressed', 'true');
        }
      }
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

  /* После сохранения раскладки серверный payload меняется: сбрасываем кэш
   * числа окон, иначе «Конструктор» после save+auto возьмёт старое N.
   * Вендор сохраняет и через fetch, и через XHR (axios) — перехватываем оба. */
  function watchSaves() {
    if (typeof window.fetch === 'function' && !window.fetch.__vksCodianWrapped) {
      var nativeFetch = window.fetch;
      var wrapped = function (input, init) {
        var result = nativeFetch.apply(this, arguments);
        var url = typeof input === 'string' ? input : (input && input.url) || '';
        var method = String((init && init.method) || 'GET').toUpperCase();
        if (method !== 'GET' && /\/video_layouts\//.test(url) && !/calculate/.test(url) && result && result.then) {
          result.then(function (response) {
            if (response && response.ok) {
              serverCountKey = '';
              schedule();
            }
          }, function () { /* ошибку покажет вендорский UI */ });
        }
        return result;
      };
      wrapped.__vksCodianWrapped = true;
      window.fetch = wrapped;
    }
    var xhrProto = window.XMLHttpRequest && window.XMLHttpRequest.prototype;
    if (!xhrProto || xhrProto.__vksCodianWrapped) return;
    var nativeOpen = xhrProto.open;
    var nativeSend = xhrProto.send;
    xhrProto.open = function (method, url) {
      this.__vksCodianMethod = String(method || 'GET').toUpperCase();
      this.__vksCodianUrl = String(url || '');
      return nativeOpen.apply(this, arguments);
    };
    xhrProto.send = function () {
      var xhr = this;
      var method = xhr.__vksCodianMethod;
      var url = xhr.__vksCodianUrl || '';
      if (method !== 'GET' && /\/video_layouts\//.test(url) && !/calculate/.test(url)) {
        xhr.addEventListener('load', function () {
          if (xhr.status >= 200 && xhr.status < 300) {
            serverCountKey = '';
            schedule();
          }
        });
      }
      return nativeSend.apply(this, arguments);
    };
    xhrProto.__vksCodianWrapped = true;
  }

  /* Вендорский транспорт сохранения может быть не fetch/XHR-обёрткой (axios
   * со своим адаптером, service-worker): дополнительно ловим сам клик по
   * «Сохранить» и через паузу перечитываем N с сервера. */
  function watchSaveButton() {
    document.addEventListener(
      'click',
      function (event) {
        var target = event.target;
        var button = target && target.closest ? target.closest('button') : null;
        if (!button || !/Сохранить/.test(button.textContent || '')) return;
        var retry = function () {
          serverCountKey = '';
          loadLayoutCount();
        };
        window.setTimeout(retry, 1500);
        window.setTimeout(retry, 3000);
      },
      true,
    );
  }

  function start() {
    if (observer) return;
    watchSaves();
    watchSaveButton();
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

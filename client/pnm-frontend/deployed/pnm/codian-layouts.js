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

  var GROUP_TITLE = 'Codian MCU 4200';
  var CATALOG_URL = '/api/v3/ui/video_layouts/presets';
  var TOKEN_KEY = 'TrueConfMCU_AccessToken';
  var HOST_SELECTOR = '.manual-layout-configurator';
  var GROUP_ATTR = 'data-vks-codian-group';
  var ITEM_ATTR = 'data-codian-id';
  var PREVIEW_WIDTH = 260;
  var PREVIEW_HEIGHT = 180;
  var PREVIEW_INSET = 3;
  var PREVIEW_CONCURRENCY = 4;
  var ITEMS_PER_LINE = 8;

  var catalog = [];
  var catalogState = 'idle'; // idle | loading | ready | failed
  var previews = Object.create(null);
  var icons = Object.create(null);
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
      editor.$subscribe(function () { schedule(); }, { detached: true });
    } catch (e) {
      /* наблюдатель остаётся единственным источником обновлений */
    }
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
    // Полузазор: соседние ячейки дают 2h между собой, а края макета остаются
    // вплотную к viewBox (иначе по периметру появляется лишняя рамка).
    var half = Math.max(1, Math.round(maxX * 0.01));
    var rects = [];
    for (var r = 0; r < valid.length; r += 1) {
      var cell = valid[r];
      var ix = cell.x;
      var iw = cell.width;
      var iy = cell.y;
      var ih = cell.height;
      if (cell.x > 0) {
        ix = cell.x + half;
        iw -= half;
      }
      if (cell.x + cell.width < maxX) iw -= half;
      if (cell.y > 0) {
        iy = cell.y + half;
        ih -= half;
      }
      if (cell.y + cell.height < maxY) ih -= half;
      if (iw <= 0 || ih <= 0) {
        ix = cell.x;
        iy = cell.y;
        iw = cell.width;
        ih = cell.height;
      }
      rects.push(
        '<rect x="' + round(ix) + '" y="' + round(iy) + '" width="' + round(iw) +
        '" height="' + round(ih) + '" rx="' + round(half) + '"></rect>'
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
    item.setAttribute('data-windows', String(entry.windows));
    item.setAttribute('role', 'button');
    item.setAttribute('tabindex', '0');
    item.setAttribute('aria-pressed', 'false');
    item.setAttribute('title', entry.windows > 0 ? entry.title + ' — ' + entry.windows + ' ' + windowsWord(entry.windows) : entry.title);
    item.setAttribute('aria-label', entry.title);

    var container = document.createElement('span');
    container.className = 'tc-icon-container layout-scheme-selector-item__icon';
    var icon = document.createElement('i');
    icon.className = 'material-icons tc-icon__icon notranslate';
    icon.innerHTML = entry.slots.length ? svgFromSlots(entry.slots) : svgFallback(entry.windows);
    container.appendChild(icon);
    item.appendChild(container);
    return item;
  }

  function buildGroup() {
    var section = document.createElement('div');
    section.className = 'main-form-section-16';
    section.setAttribute(GROUP_ATTR, '1');

    var header = document.createElement('div');
    header.className = 'main-form__subheader';
    var text = document.createElement('p');
    text.className = 'main-form__subheader-text';
    text.textContent = GROUP_TITLE;
    header.appendChild(text);
    section.appendChild(header);

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
    for (var id in icons) {
      if (Object.prototype.hasOwnProperty.call(icons, id)) paintIcon(id);
    }
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
    var existing = editor.modifiedValue && editor.modifiedValue.slots && editor.modifiedValue.slots[id];
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
        var host = document.querySelector(HOST_SELECTOR);
        if (!host) return;
        if (catalogState === 'idle') loadCatalog();
        if (catalogState === 'ready') ensureGroup();
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

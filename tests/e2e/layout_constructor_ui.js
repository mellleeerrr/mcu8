#!/usr/bin/env node
/*
 * UI-регрессия конструктора раскладок (проектный слой `codian-layouts.js`).
 *
 * Проверяет то, что не видит серверный parity-скрипт:
 *  - режим «Конструктор» открывается, 6 карточек и счётчик на месте;
 *  - иконки карточек рисуются из ОТВЕТА сервера `calculate` (не локальным
 *    движком): в сети видны POST'ы с пресетами `vks_*_<N>`;
 *  - клик по карточке меняет пресет синхронно (карточка/счётчик), без
 *    pageerror и crash-сигнатур;
 *  - store редактора после выбора содержит ровно N слотов (вендорский
 *    fallback по хвосту id — геометрию в store мы больше не пишем).
 *
 * Своя комната: создаётся и удаляется скриптом; чужие раскладки не читаются и
 * не мутируются. Запуск: `node tests/e2e/layout_constructor_ui.js`.
 */
'use strict';

const { chromium } = require('playwright');

const BASE = (process.env.VKS_BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const RUN = `E2E Ctor ${Date.now()}`;

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok) });
  console.log(`[ctor-ui] ${ok ? 'OK  ' : 'FAIL'} ${name}${detail ? ' — ' + detail : ''}`);
  return Boolean(ok);
}

async function api(token, method, path, body) {
  const response = await fetch(BASE + path, {
    method,
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let doc = {};
  try { doc = text ? JSON.parse(text) : {}; } catch (e) { doc = { raw: text }; }
  return { status: response.status, doc };
}

(async () => {
  let token = '';
  let conferenceId = '';
  let sessionId = '';
  let browser = null;
  let failure = '';
  let vendor400Count = 0;
  const geometry = new Map();
  try {
    const login = await api('', 'POST', '/api/v3/login', { login: LOGIN, password: PASSWORD });
    token = login.doc.token || login.doc.access_token || '';
    if (!check('login', login.status === 200 && Boolean(token), `${login.status}`)) return 1;

    const conf = await api(token, 'POST', '/api/v3/conferences', { display_name: RUN });
    conferenceId = (conf.doc.conference && conf.doc.conference.id) || '';
    if (!check('conference создана', Boolean(conferenceId), `${conf.status}`)) return 1;
    const sess = await api(token, 'POST', '/api/v3/sessions', { conference_id: conferenceId });
    sessionId = (sess.doc.session && sess.doc.session.id) || sess.doc.id || '';
    if (!check('сессия запущена', Boolean(sessionId), `${sess.status}`)) return 1;
    const layouts = await api(token, 'GET', `/api/v3/sessions/${sessionId}/video_layouts?page=1&page_size=100`);
    const list = layouts.doc.video_layouts || layouts.doc.layouts || [];
    if (!check('у сессии появилась «Общая»', list.some((layout) => layout.payload && layout.payload.type === 'default'),
      list.map((layout) => layout.id).join(','))) return 1;

    browser = await chromium.launch({ headless: true });
    const context = await browser.newContext({ viewport: { width: 1600, height: 1000 } });
    const page = await context.newPage();
    const pageErrors = [];
    const crashes = [];
    const calculate = [];
    page.on('pageerror', (error) => {
      const detail = {
        message: error && error.message ? String(error.message) : '',
        name: error && error.name ? String(error.name) : '',
        url: (error && error.config && error.config.url) || '',
        status: (error && error.response && error.response.status) || 0,
        stack: error && error.stack ? String(error.stack).slice(0, 300) : '',
      };
      pageErrors.push(JSON.stringify(detail));
    });
    page.on('console', (message) => {
      if (message.type() !== 'error') return;
      const text = message.text();
      if (/hasIndividualLayout|sessionLayoutListByType/.test(text)) crashes.push(text);
    });
    // Вызовы reportError аккумулируются в Node в момент вызова: счётчики
    // живут не в странице/фрейме, а в процессе теста, поэтому удаление или
    // навигация iframe не теряет данные.
    const reported = { expected: 0, unexpected: 0 };
    await context.exposeBinding('__vksReport', (_source, payload) => {
      const kind = payload && payload.kind === 'expected' ? 'expected' : 'unexpected';
      reported[kind] += 1;
    });
    // Вендорский auto-превью шлёт calculate с `preset_default_13` (400 по
    // контракту) и сообщает об ошибке через window.reportError. Перехватываем
    // ЛЮБОЙ вызов reportError и проверяем, что это настоящая axios-ошибка этого
    // запроса (status 400, config.url, preset в config.data, request). Любой
    // другой вызов (в т.ч. crafted-объект) — fail; allowlist по форме ошибки или
    // окну времени не используется.
    await context.addInitScript(() => {
      const wrapper = (error) => {
        let expected = false;
        try {
          const config = error && error.config;
          expected = Boolean(config)
            && /\/video_layouts\/calculate/.test(String(config.url || ''))
            && String(config.data || '').includes('preset_default_13')
            // axios-маркеры настоящей HTTP-ошибки, а не произвольного объекта.
            && error.status === 400
            && Boolean(error.request);
        } catch (e) { expected = false; }
        try { window.__vksReport({ kind: expected ? 'expected' : 'unexpected' }); } catch (e) {}
      };
      // Обёртка неизменяема: код страницы не может переопределить reportError
      // и спрятать/подделать вызов.
      try {
        Object.defineProperty(window, 'reportError', { value: wrapper, writable: false, configurable: false });
      } catch (e) {
        window.reportError = wrapper;
      }
    });
    const calculateResponses = [];
    page.on('request', (request) => {
      if (request.method() === 'POST' && /\/video_layouts\/calculate/.test(request.url())) {
        try { calculate.push(JSON.parse(request.postData() || '{}').preset || ''); } catch (e) { calculate.push(''); }
      }
    });
    page.on('response', async (response) => {
      const request = response.request();
      if (request.method() !== 'POST' || !/\/video_layouts\/calculate/.test(response.url())) return;
      let preset = '';
      try { preset = JSON.parse(request.postData() || '{}').preset || ''; } catch (e) { preset = ''; }
      const status = response.status();
      if (status === 400 && preset === 'preset_default_13') {
        vendor400Count += 1;
      }
      const entry = { preset, status, slots: null };
      if (status === 200) {
        try {
          const doc = JSON.parse(await response.text());
          entry.slots = Array.isArray(doc.slots) ? doc.slots : [];
          geometry.set(preset, entry.slots);
        } catch (e) { entry.slots = null; }
      }
      calculateResponses.push(entry);
    });

    await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'domcontentloaded' });
    await page.fill('input[autocomplete="username"], input[type="text"]', LOGIN);
    await page.fill('input[type="password"]', PASSWORD);
    await page.click('button[type="submit"], button:has-text("Авторизоваться")');
    await page.waitForTimeout(2000);

    await page.goto(`${BASE}/pnm/monitoring/sessions/${sessionId}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2000);
    const tab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: 'Раскладки' }).first();
    if (await tab.count()) await tab.click();
    await page.waitForTimeout(1200);
    const row = page.locator('.session-layouts-item').first();
    if (!check('строка раскладки в списке', (await row.count()) > 0)) return 1;
    await row.click();
    await page.waitForTimeout(1500);
    const ctorTab = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]').first();
    await ctorTab.waitFor({ state: 'attached', timeout: 15000 });
    await ctorTab.click();
    await page.waitForTimeout(1500);
    // Переход в manual перерисовывает вендорский LayoutGridConfigurator;
    // его auto-пресет preset_default_13 даёт 400 и перехваченный reportError.

    const state = await page.evaluate(() => {
      const panel = document.querySelector('[data-vks-constructor="1"]');
      const cards = panel ? Array.from(panel.querySelectorAll('[data-vks-constructor-mode]')) : [];
      const counter = panel ? panel.querySelector('[data-counter-input]') : null;
      return {
        panel: Boolean(panel),
        cards: cards.length,
        modes: cards.map((card) => card.getAttribute('data-vks-constructor-mode')),
        counter: counter ? counter.value : null,
        icons: panel ? panel.querySelectorAll('[data-vks-constructor-mode] svg').length : 0,
      };
    });
    check('панель конструктора видна', state.panel);
    check('ровно 6 карточек режимов', state.cards === 6, `${state.cards}`);
    check('счётчик окон на месте', state.counter !== null, `${state.counter}`);

    const count = Number(state.counter) || 25;
    await page.waitForFunction(
      () => document.querySelectorAll('[data-vks-constructor-mode] svg').length >= 6,
      null,
      { timeout: 8000 },
    ).catch(() => {});
    check('иконки карточек нарисованы', await page.evaluate(
      () => document.querySelectorAll('[data-vks-constructor-mode] svg').length >= 6), `${state.icons}`);
    const expectedPresets = ['grid', 'pin_l', 'pin_r', 'pin_t', 'pin_b', 'dual'].map((mode) => `vks_${mode}_${count}`);
    check('иконки пришли с сервера (calculate по всем 6 режимам)',
      expectedPresets.every((preset) => calculate.includes(preset)),
      JSON.stringify(calculate.slice(0, 12)));

    // Смена N: счётчик → карточки пересобираются, новые пресеты уходят на сервер.
    const counterInput = page.locator('[data-vks-constructor="1"] [data-counter-input]').first();
    await counterInput.fill('12');
    await counterInput.press('Enter');
    await page.waitForTimeout(1500);
    const n12 = await page.evaluate(() => {
      const root = document.getElementById('root') || document.getElementById('app');
      const store = root && root.__vue_app__
        && root.__vue_app__.config.globalProperties.$pinia._s.get('layout-editor');
      const slots = store && store.modifiedValue ? store.modifiedValue.slots : [];
      return { count: Array.isArray(slots) ? slots.length : -1, positions: slots.map((slot) => slot.position) };
    });
    check('N=12: store содержит ровно 12 слотов (fallback по пресету)', n12.count === 12, `${n12.count}`);
    check('N=12: позиции 1..12 без пропусков',
      n12.positions.length === 12 && n12.positions.every((position, index) => position === index + 1),
      n12.positions.join(','));

    // Строгая проверка «иконки из ответа сервера»: 200, ровно N слотов,
    // rect-count и viewBox совпадают с геометрией ответа (fallback-svg имеет
    // другой viewBox 320x180 и не пройдёт).
    const modes = ['grid', 'pin_l', 'pin_r', 'pin_t', 'pin_b', 'dual'];
    const presets12 = modes.map((mode) => `vks_${mode}_12`);
    const deadline = Date.now() + 8000;
    while (Date.now() < deadline
      && !presets12.every((preset) => calculateResponses.some((entry) => entry.preset === preset && entry.status === 200
        && Array.isArray(entry.slots) && entry.slots.length === 12))) {
      await page.waitForTimeout(200);
    }
    const rounded = (value) => Math.round(Number(value) * 100) / 100;
    const serverIcons = presets12.map((preset) => {
      const entry = calculateResponses.filter((item) => item.preset === preset && item.status === 200).pop();
      const slots = entry && Array.isArray(entry.slots) ? entry.slots : [];
      const valid = slots.filter((slot) => {
        const values = [slot.x, slot.y, slot.width, slot.height].map(Number);
        return values.every((value) => Number.isFinite(value)) && values[2] > 0 && values[3] > 0;
      });
      const maxX = valid.reduce((max, slot) => Math.max(max, Number(slot.x) + Number(slot.width)), 0);
      const maxY = valid.reduce((max, slot) => Math.max(max, Number(slot.y) + Number(slot.height)), 0);
      const halfX = Math.max(1, Math.round(maxX * 0.02));
      const halfY = Math.max(1, Math.round(maxY * 0.02));
      // Тот же расчёт, что svgFromSlots: полузазор 2% (иначе iw/ih <= 0 — без зазора).
      const rects = valid.map((slot) => {
        let x = Number(slot.x) + halfX;
        let y = Number(slot.y) + halfY;
        let width = Number(slot.width) - 2 * halfX;
        let height = Number(slot.height) - 2 * halfY;
        if (width <= 0 || height <= 0) {
          x = Number(slot.x); y = Number(slot.y);
          width = Number(slot.width); height = Number(slot.height);
        }
        return [rounded(x), rounded(y), rounded(width), rounded(height)].join(',');
      }).sort();
      return {
        mode: preset.replace(/^vks_|_12$/g, ''), count: slots.length, maxX, maxY, rects,
        status: entry ? entry.status : 0,
      };
    });
    check('N=12: все 6 calculate вернули 200 и ровно 12 слотов',
      serverIcons.every((icon) => icon.status === 200 && icon.count === 12),
      JSON.stringify(serverIcons.map((icon) => [icon.mode, icon.status, icon.count])));
    const rendered = await page.evaluate((expected) => {
      const out = {};
      for (const icon of expected) {
        const card = document.querySelector(`[data-vks-constructor="1"] [data-vks-constructor-mode="${icon.mode}"]`);
        const svg = card ? card.querySelector('svg') : null;
        const viewBox = svg ? String(svg.getAttribute('viewBox') || '').trim().split(/\s+/).map(Number) : [];
        const boxes = svg
          ? Array.from(svg.querySelectorAll('rect')).map((rect) => [
            rect.getAttribute('x'), rect.getAttribute('y'), rect.getAttribute('width'), rect.getAttribute('height'),
          ].map(Number).map((value) => Math.round(value * 100) / 100).join(',')).sort()
          : [];
        out[icon.mode] = { rects: boxes.length, viewBox, boxes };
      }
      return out;
    }, serverIcons);
    const iconMismatch = serverIcons.filter((icon) => {
      const got = rendered[icon.mode] || { rects: 0, viewBox: [] };
      return got.rects !== icon.count
        || Math.abs((got.viewBox[0] || 0)) > 0.01
        || Math.abs((got.viewBox[1] || 0)) > 0.01
        || Math.abs((got.viewBox[2] || 0) - icon.maxX) > 0.01
        || Math.abs((got.viewBox[3] || 0) - icon.maxY) > 0.01
        || JSON.stringify(got.boxes) !== JSON.stringify(icon.rects);
    });
    check('N=12: иконки отрисованы из ответа сервера (rect=N, viewBox и координаты = геометрия ответа)',
      serverIcons.every((icon) => icon.count === 12) && iconMismatch.length === 0,
      JSON.stringify(iconMismatch));

    const pinL = page.locator('[data-vks-constructor="1"] [data-vks-constructor-mode="pin_l"]').first();
    await pinL.click();
    await page.waitForTimeout(1500);
    check('клик по карточке ушёл на сервер (calculate vks_pin_l_12)', calculate.includes('vks_pin_l_12'));
    const active = await page.evaluate(() => {
      const card = document.querySelector('[data-vks-constructor="1"] [data-vks-constructor-mode="pin_l"]');
      const counter = document.querySelector('[data-vks-constructor="1"] [data-counter-input]');
      return {
        pressed: card ? card.getAttribute('aria-pressed') : null,
        active: card ? card.classList.contains('layout-scheme-selector-item--active') : false,
        counter: counter ? counter.value : null,
      };
    });
    check('карточка pin_l активна', active.pressed === 'true' || active.active, JSON.stringify(active));
    check('счётчик остался 12 после клика', active.counter === '12', `${active.counter}`);
    check('нет crash-сигнатур в консоли', crashes.length === 0, crashes.slice(0, 2).join(' | '));
    // Ни один pageerror не допустим: вендорский шум перехвачен на
    // window.reportError и посчитан отдельно; любой неперехваченный бросок
    // (в т.ч. crafted-объект) обязан провалить тест.
    // `preset_default_13` calculate -> 400 (`invalid.preset`) is expected: the
    // vendor UI reports it through window.reportError. Assert the counts are
    // consistent instead of pinning a brittle exact count (0/1/2 depending on
    // how many times the vendor re-renders the editor).
    check('нет pageerror; reportError только от axios preset_default_13',
      pageErrors.length === 0 && reported.unexpected === 0 && reported.expected === vendor400Count && vendor400Count >= 1,
      `pageerrors: ${pageErrors.slice(0, 2).join(' | ')}; 400: ${vendor400Count}; reportError: ${JSON.stringify(reported)}`);
    check('превью и иконки идут только серверным calculate', calculate.length >= 7);
  } catch (error) {
    failure = String(error && error.stack ? error.stack : error);
    console.error('[ctor-ui] ERROR', failure);
  } finally {
    if (browser) await browser.close().catch(() => {});
    if (token && sessionId) await api(token, 'DELETE', `/api/v3/sessions/${sessionId}`).catch(() => {});
    if (token && conferenceId) await api(token, 'DELETE', `/api/v3/conferences/${conferenceId}`).catch(() => {});
  }

  const failed = checks.filter((entry) => !entry.ok);
  const ok = failed.length === 0 && !failure;
  console.log(`[ctor-ui] итог: ${ok ? 'PASS' : 'FAIL'} (${checks.length - failed.length}/${checks.length})`);
  return ok ? 0 : 1;
})().then((code) => process.exit(code)).catch((error) => {
  console.error('[ctor-ui] FATAL', error);
  process.exit(1);
});

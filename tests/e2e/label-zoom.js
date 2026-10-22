#!/usr/bin/env node
/*
 * Живой E2E-измеритель подписи участника в раскладке (PNM 2.2.1.66 + CSS-оверрайд
 * `prostor-pnm-overrides`) и сверка счётчиков списка мониторинга с API.
 *
 * (A) Логин → страница раскладок конференции → «Ручная настройка» →
 *     для каждого размера раскладки (1, 2, 3, 4, 5+, 8, 9, 10+, 25, 34+)
 *     заново сажаем контакт в ячейку 1 и измеряем подпись
 *     (`.layout-grid-slot-dn__text`) относительно плитки (`.layout-grid-slot-wrapper`).
 *     Ключевой регрессионный признак: подпись должна быть вдвое крупнее
 *     плитки (правило `.layout-grid-slot .layout-grid-slot-dn__text{font-size:2em}`),
 *     центрирована и целиком внутри плитки.
 *
 * (B) Счётчики в списке мониторинга (/pnm/monitoring/conferences) против
 *     /api/v3/ui/conferences, против /api/v3/conferences/{id}/contacts
 *     и против счётчиков в шапке карточки комнаты.
 *
 * Запуск: NODE_PATH=<node_modules с playwright> node tests/e2e/label-zoom.js
 * Артефакты: artifacts/e2e-label-zoom/{result.json,size-*.png,monitoring-list.png}
 *
 * Скрипт НИЧЕГО не пишет в живую БД: раскладка не сохраняется («Создать» не
 * нажимается), посадка участника и добавление в «Получатели» — только
 * клиентский черновик (единственный не-GET запрос — POST .../video_layouts/calculate).
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const BASE = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const CONFERENCE_ID = process.env.VKS_CONFERENCE_ID || '49b599d7-b5cd-45a7-8999-47a28c453a92';
const PARTICIPANT = process.env.VKS_PARTICIPANT || 'T12b Label Probe';
const ART = path.resolve(process.env.VKS_ARTIFACTS || 'artifacts/e2e-label-zoom');
const CHROMIUM = process.env.VKS_CHROMIUM
  || '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
// Строгий режим (исторический): после фикса счётчиков (e81811f) проверка
// «контакты комнаты vs счётчики» fatal всегда, флаг оставлен для совместимости.
const STRICT_COUNTS = process.env.VKS_STRICT_COUNTS === '1';

// Размеры раскладки: id — для JSON, label — точный текст кнопки в
// `.layout-size-range-selector` (последняя, icon-only «overlap», НЕ трогаем).
const SIZES = [
  { size: 1, label: '1' },
  { size: 2, label: '2' },
  { size: 3, label: '3' },
  { size: 4, label: '4' },
  { size: 5, label: '5 +', plus: true },
  { size: 8, label: '8' },
  { size: 9, label: '9' },
  { size: 10, label: '10 +', plus: true },
  { size: 25, label: '25' },
  { size: 34, label: '34 +', plus: true },
];

// Критерии центровки (из tests/e2e/live.js:442-455).
const CENTER_DX = 1.5;
const CENTER_DY = 3;
// Ожидаемое отношение font-size подписи к font-size плитки (CSS-фикс 2em).
const LABEL_FONT_RATIO = 2;
const FONT_RATIO_TOLERANCE = 0.15;

fs.mkdirSync(ART, { recursive: true });

const checks = [];
const pageErrors = [];
const consoleErrors = [];
const nonGetRequests = [];
const startedAt = Date.now();
let failure = null;

/**
 * Регистрирует проверку. НЕ бросает: отсутствие подписи не должно ронять прогон.
 * severity: 'fatal' (по умолчанию) | 'known-issue' (не влияет на код выхода,
 * пока не включён VKS_STRICT_COUNTS) | 'info' (справочно).
 */
function check(name, ok, detail, severity = 'fatal') {
  const entry = { name, ok: Boolean(ok), severity, detail: detail === undefined ? null : detail };
  checks.push(entry);
  const mark = entry.ok ? 'ok  ' : (severity === 'fatal' ? 'FAIL' : `WARN(${severity})`);
  console.log(`${mark} ${name}${entry.detail ? ` — ${entry.detail}` : ''}`);
  return entry.ok;
}

async function api(token, method, urlPath, body) {
  const response = await fetch(`${BASE}${urlPath}`, {
    method,
    headers: { authorization: `Bearer ${token}`, 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* not json */ }
  return { status: response.status, json, text };
}

/** Имя файла скриншота: «5 +» → «05-5-plus». */
const slug = (label, index) => {
  const base = String(label).trim().toLowerCase()
    .replace(/\s*\+\s*$/, '-plus')
    .replace(/\s+/g, '-')
    .replace(/-$/, '');
  return `${String(index).padStart(2, '0')}-${base}`;
};

/**
 * Замер подписи в указанной плитке (1-based). Всё считается в браузере,
 * чтобы не тащить геометрию через JSON.
 */
async function measureTile(page, position) {
  const tile = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await tile.count())) return { labelFound: false, error: `tile ${position} not rendered` };
  return tile.evaluate(element => {
    const box = el => {
      const b = el.getBoundingClientRect();
      return {
        left: b.left, top: b.top, right: b.right, bottom: b.bottom,
        width: b.width, height: b.height,
      };
    };
    const text = element.querySelector('.layout-grid-slot-dn__text');
    const avatar = element.querySelector('.layout-grid-slot__avatar');
    const tileBox = box(element);
    if (!text) {
      return {
        labelFound: false,
        tileText: (element.innerText || '').trim(),
        tileW: tileBox.width, tileH: tileBox.height,
        slotFontSizePx: parseFloat(getComputedStyle(element).fontSize),
        hasAvatar: Boolean(avatar),
      };
    }
    const labelBox = box(text);
    const labelStyle = getComputedStyle(text);
    const slotStyle = getComputedStyle(element);
    const avatarBox = avatar ? box(avatar) : null;
    return {
      labelFound: true,
      labelText: (text.innerText || '').trim(),
      labelFontSizePx: parseFloat(labelStyle.fontSize),
      labelLineHeight: labelStyle.lineHeight,
      labelTextOverflow: labelStyle.textOverflow,
      slotFontSizePx: parseFloat(slotStyle.fontSize),
      tileW: tileBox.width, tileH: tileBox.height,
      labelW: labelBox.width, labelH: labelBox.height,
      dx: (labelBox.left + labelBox.width / 2) - (tileBox.left + tileBox.width / 2),
      dy: (labelBox.top + labelBox.height / 2) - (tileBox.top + tileBox.height / 2),
      labelTopVsTileTop: labelBox.top - tileBox.top,
      labelBottomVsTileBottom: labelBox.bottom - tileBox.bottom,
      labelLeftVsTileLeft: labelBox.left - tileBox.left,
      labelRightVsTileRight: labelBox.right - tileBox.right,
      overflowX: text.scrollWidth > text.clientWidth + 1,
      overflowY: text.scrollHeight > text.clientHeight + 1,
      scrollWidth: text.scrollWidth, clientWidth: text.clientWidth,
      scrollHeight: text.scrollHeight, clientHeight: text.clientHeight,
      labelInsideTile: labelBox.top >= tileBox.top - 0.5 && labelBox.bottom <= tileBox.bottom + 0.5,
      labelInsideTileX: labelBox.left >= tileBox.left - 0.5 && labelBox.right <= tileBox.right + 0.5,
      hasAvatar: Boolean(avatar),
      avatarW: avatarBox ? avatarBox.width : null,
      avatarH: avatarBox ? avatarBox.height : null,
    };
  });
}

/** Закрыть выехавшую панель видеослота (иначе она сужает сетку). */
async function closeSlotPanel(page) {
  const close = page.locator('.grid-slot-side-bar button').filter({ hasText: 'close' }).first();
  if (await close.count()) {
    await close.click({ timeout: 5000 }).catch(() => {});
    await page.waitForTimeout(400);
  }
  const stillOpen = await page.locator('.grid-slot-side-bar').isVisible().catch(() => false);
  if (stillOpen) {
    await page.keyboard.press('Escape').catch(() => {});
    await page.waitForTimeout(300);
  }
  return !(await page.locator('.grid-slot-side-bar').isVisible().catch(() => false));
}

/** Посадить контакт в ячейку `position` (1-based) через панель видеослота. */
async function placeParticipant(page, position) {
  const cell = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
  if (!(await cell.count())) return { placed: false, error: `tile ${position} not rendered` };
  await cell.scrollIntoViewIfNeeded().catch(() => {});
  await cell.click({ timeout: 10000 });
  await page.waitForTimeout(700);
  // Список контактов комнаты в панели видеослота.
  const scoped = page.locator('.grid-slot-contact-item').getByText(PARTICIPANT, { exact: false });
  let target = scoped.last();
  if (!(await scoped.count())) target = page.getByText(PARTICIPANT, { exact: false }).last();
  if (!(await target.count())) {
    return { placed: false, error: `contact «${PARTICIPANT}» not listed in the slot panel` };
  }
  await target.click({ timeout: 10000 });
  await page.waitForTimeout(700);
  const labeled = await page.locator('.layout-grid-slot-wrapper .layout-grid-slot-dn__text').count();
  return { placed: labeled > 0, error: labeled > 0 ? null : 'slot panel click did not produce a label' };
}

/** Число из контейнера со счётчиком (`.monitoring-*-container` / `__additional-container`). */
const readCounter = element => {
  const texts = [...element.querySelectorAll('span')]
    .map(span => (span.innerText || '').trim())
    .filter(text => /^\d+$/.test(text));
  return texts.length ? Number(texts[texts.length - 1]) : null;
};

(async () => {
  let browser;
  let context;
  let token = '';
  const sizes = [];
  let counters = null;
  try {
    browser = await chromium.launch({ headless: true, executablePath: CHROMIUM, args: ['--no-sandbox'] });
    context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
    const page = await context.newPage();
    page.on('console', message => {
      if (message.type() === 'error' && consoleErrors.length < 40) consoleErrors.push(message.text());
    });
    page.on('pageerror', error => {
      if (pageErrors.length < 20) pageErrors.push(String((error && error.stack) || error));
    });
    page.on('request', request => {
      if (request.method() !== 'GET') {
        const url = request.url();
        // Тело логина содержит пароль — в артефакты не пишем.
        const body = url.includes('/api/v3/login') ? '<redacted>' : (request.postData() || '').slice(0, 300);
        nonGetRequests.push({ method: request.method(), url, body });
      }
    });

    // ------------------------------------------------------------------ логин
    await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'networkidle' });
    await page.fill('input[autocomplete="username"], input[type="text"]', LOGIN);
    await page.fill('input[type="password"]', PASSWORD);
    await page.click('button[type="submit"], button:has-text("Авторизоваться")');
    await page.waitForTimeout(2500);
    token = await page.evaluate(() => localStorage.getItem('TrueConfMCU_AccessToken'));
    check('login: token in localStorage', Boolean(token));
    if (!token) throw new Error('login failed: no access token in localStorage');
    check('login: token is JWT', typeof token === 'string' && token.split('.').length === 3);

    // ------------------------------------------------- (A) измерения подписи
    await page.goto(`${BASE}/pnm/monitoring/conferences/${CONFERENCE_ID}/layouts`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    await page.getByText('Ручная настройка', { exact: true }).click();
    await page.waitForTimeout(1000);
    const rangeButtons = await page.locator('.layout-size-range-selector button').evaluateAll(els => els.map(el => (el.innerText || '').trim()));
    check('layout: size range selector visible', rangeButtons.length > 0, JSON.stringify(rangeButtons));

    // Добавить контакт в «Получатели» раскладки (клиентский черновик, без записи в БД):
    // так сценарий совпадает с рабочим вендорным сценарием.
    try {
      await page.getByText(/Получатели/).first().click();
      await page.waitForTimeout(800);
      const addButton = page.getByRole('button', { name: /Добавить/ }).last();
      if (await addButton.count()) {
        await addButton.click();
        await page.waitForTimeout(800);
        const row = page.locator('.popup').last().getByText(PARTICIPANT, { exact: false }).first();
        if (await row.count()) {
          await row.click();
          await page.waitForTimeout(600);
          await page.getByRole('button', { name: /Добавить/ }).last().click();
          await page.waitForTimeout(800);
          check('layout: contact added to layout recipients', true, PARTICIPANT);
        } else {
          check('layout: contact added to layout recipients', false, `«${PARTICIPANT}» not in recipients popup`);
        }
      }
      await page.getByText('Основное', { exact: true }).first().click();
      await page.waitForTimeout(800);
    } catch (error) {
      check('layout: contact added to layout recipients', false, String(error).slice(0, 200));
      await page.getByText('Основное', { exact: true }).first().click().catch(() => {});
      await page.waitForTimeout(500);
    }

    for (let index = 0; index < SIZES.length; index += 1) {
      const spec = SIZES[index];
      const name = `${spec.size}${spec.plus ? '+' : ''}`;
      const file = slug(spec.label, index + 1);
      console.log(`\n--- layout size ${name} ---`);

      // 1. Выбор размера (кнопка с точным текстом; icon-only «overlap» пропускаем).
      const sizeButton = page.locator('.layout-size-range-selector button')
        .filter({ hasText: new RegExp(`^${spec.label.replace(/[+]/g, '\\+')}$`) }).first();
      const sizeButtonFound = await sizeButton.count();
      check(`size ${name}: range button present`, sizeButtonFound > 0, spec.label);
      if (!sizeButtonFound) {
        sizes.push({ size: spec.size, plus: Boolean(spec.plus), label: spec.label, error: 'size button not found' });
        continue;
      }
      await sizeButton.click({ timeout: 10000 });
      await page.waitForTimeout(900);

      const tiles = await page.locator('.layout-grid-slot-wrapper').count();
      check(`size ${name}: tiles rendered`, spec.plus ? tiles >= spec.size : tiles === spec.size, `tiles=${tiles}`);

      // 2. Посадка участника в ячейку 1 (при смене размера посадка сбрасывается).
      const placement = await placeParticipant(page, 1);
      check(`size ${name}: participant placed in cell 1`, placement.placed, placement.error || PARTICIPANT);

      // 3. Замер с открытой панелью видеслота (строгая геометрия) и в покое (панель закрыта).
      const withPanel = await measureTile(page, 1);
      await page.screenshot({ path: path.join(ART, `size-${file}-editor.png`), fullPage: true });
      const panelClosed = await closeSlotPanel(page);
      const resting = await measureTile(page, 1);
      await page.screenshot({ path: path.join(ART, `size-${file}.png`), fullPage: true });

      const measurement = resting.labelFound ? resting : withPanel;
      const record = {
        size: spec.size,
        plus: Boolean(spec.plus),
        label: spec.label,
        tiles,
        panelClosed,
        placement: placement.placed ? 'placed' : `failed: ${placement.error}`,
        ...measurement,
        withPanel,
        resting,
      };
      sizes.push(record);

      if (!measurement.labelFound) {
        check(`size ${name}: participant label found`, false, 'label (.layout-grid-slot-dn__text) not rendered');
        continue;
      }
      check(`size ${name}: participant label found`, true, measurement.labelText);
      check(`size ${name}: label font-size is ${LABEL_FONT_RATIO}x slot font-size (CSS 2em fix)`,
        Math.abs(measurement.labelFontSizePx / measurement.slotFontSizePx - LABEL_FONT_RATIO) <= FONT_RATIO_TOLERANCE,
        `label=${measurement.labelFontSizePx}px slot=${measurement.slotFontSizePx}px ratio=${(measurement.labelFontSizePx / measurement.slotFontSizePx).toFixed(2)}`);
      check(`size ${name}: label centered in tile`,
        Math.abs(measurement.dx) <= CENTER_DX && Math.abs(measurement.dy) <= CENTER_DY,
        `dx=${measurement.dx.toFixed(2)} dy=${measurement.dy.toFixed(2)}`);
      check(`size ${name}: label fully inside tile`, measurement.labelInsideTile,
        `top+=${measurement.labelTopVsTileTop.toFixed(1)} bottom+=${measurement.labelBottomVsTileBottom.toFixed(1)}`);
      check(`size ${name}: label does not stick out horizontally`, measurement.labelInsideTileX,
        `left+=${measurement.labelLeftVsTileLeft.toFixed(1)} right+=${measurement.labelRightVsTileRight.toFixed(1)}`);
      check(`size ${name}: avatar rendered`, measurement.hasAvatar,
        `avatar=${measurement.avatarW}x${measurement.avatarH}`);
      console.log(`     tile=${measurement.tileW.toFixed(1)}x${measurement.tileH.toFixed(1)} ` +
        `label=${measurement.labelW.toFixed(1)}x${measurement.labelH.toFixed(1)} ` +
        `font=${measurement.labelFontSizePx}px/${measurement.slotFontSizePx}px ` +
        `dx=${measurement.dx.toFixed(2)} dy=${measurement.dy.toFixed(2)} ` +
        `inside=${measurement.labelInsideTile} overflowX=${measurement.overflowX}`);
    }

    // ------------------------------------------ (B) счётчики мониторинга vs API
    const conferencesResponse = await api(token, 'GET', '/api/v3/ui/conferences?page=1&page_size=100');
    const conference = (conferencesResponse.json && conferencesResponse.json.conferences || [])
      .find(item => item.id === CONFERENCE_ID);
    check('counters: conference present in /api/v3/ui/conferences', Boolean(conference),
      `status=${conferencesResponse.status}`);
    const rawContacts = (conference && conference.contacts) || {};
    const apiCounts = {
      participants: Number(rawContacts.users || 0) + Number(rawContacts.h323 || 0) + Number(rawContacts.sip || 0),
      devices: Number(rawContacts.rtsp || 0) + Number(rawContacts.vnc || 0) + Number(rawContacts.media || 0) + Number(rawContacts.file || 0),
      raw: rawContacts,
    };

    const contactsResponse = await api(token, 'GET', `/api/v3/conferences/${CONFERENCE_ID}/contacts`);
    const contactList = (contactsResponse.json && contactsResponse.json.contacts) || [];
    const byType = {};
    const byProtocol = {};
    for (const contact of contactList) {
      const type = contact.contact_type || contact.contactType || 'unknown';
      byType[type] = (byType[type] || 0) + 1;
      const scheme = String(contact.connection_url || contact.call_string_id || '').split(':')[0].toLowerCase();
      byProtocol[scheme] = (byProtocol[scheme] || 0) + 1;
    }
    const contactsEndpoint = {
      count: Number(contactsResponse.json && contactsResponse.json.count !== undefined
        ? contactsResponse.json.count
        : contactList.length),
      byType,
      byProtocol,
      names: contactList.map(contact => contact.display_name),
    };

    // Счётчики в списке мониторинга.
    await page.goto(`${BASE}/pnm/monitoring/conferences`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    await page.screenshot({ path: path.join(ART, 'monitoring-list.png'), fullPage: true });
    const conferenceName = (conference && conference.display_name) || '';
    const listRows = await page.locator('.monitoring-list-item').evaluateAll(rows => rows.map(row => {
      const title = row.querySelector('.main-list-item__text--primary');
      const additional = [...row.querySelectorAll('.monitoring-list-item__additional-container')].map(box => ({
        title: box.getAttribute('title') || '',
        texts: [...box.querySelectorAll('span')].map(span => (span.innerText || '').trim()).filter(Boolean),
      }));
      return { name: (title && title.innerText || '').trim(), additional };
    }));
    const row = listRows.find(item => item.name === conferenceName)
      || listRows.find(item => (conferenceName && item.name.includes(conferenceName)));
    const listValue = (titlePart) => {
      const box = row && row.additional.find(item => item.title.includes(titlePart));
      if (!box) return null;
      const numeric = box.texts.filter(text => /^\d+$/.test(text));
      return numeric.length ? Number(numeric[numeric.length - 1]) : null;
    };
    const uiCounts = {
      participants: listValue('Участники'),
      devices: listValue('Устройства'),
    };
    check('counters: conference row found in monitoring list', Boolean(row), conferenceName);

    // Счётчики в шапке карточки комнаты.
    await page.goto(`${BASE}/pnm/monitoring/conferences/${CONFERENCE_ID}`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(1500);
    await page.screenshot({ path: path.join(ART, 'conference-card.png'), fullPage: true });
    const headerBoxes = await page.locator('.monitoring-conference__participants-container').evaluateAll(boxes => boxes.map(box => ({
      title: box.getAttribute('title') || '',
      texts: [...box.querySelectorAll('span')].map(span => (span.innerText || '').trim()).filter(Boolean),
    })));
    const headerValue = (titlePart) => {
      const box = headerBoxes.find(item => item.title.includes(titlePart));
      if (!box) return null;
      const numeric = box.texts.filter(text => /^\d+$/.test(text));
      return numeric.length ? Number(numeric[numeric.length - 1]) : null;
    };
    const headerCounts = {
      participants: headerValue('Участники'),
      devices: headerValue('Устройства'),
    };
    check('counters: conference card header counters found',
      headerCounts.participants !== null && headerCounts.devices !== null, JSON.stringify(headerBoxes));

    const uiVsApi = uiCounts.participants === apiCounts.participants && uiCounts.devices === apiCounts.devices;
    const uiVsHeader = uiCounts.participants === headerCounts.participants && uiCounts.devices === headerCounts.devices;
    const uiVsContacts = uiCounts.participants === contactsEndpoint.count;
    counters = {
      uiCounts,
      apiCounts,
      contactsEndpoint,
      headerCounts,
      match: uiVsApi && uiVsHeader && uiVsContacts,
      matchDefinition: 'uiCounts == apiCounts == headerCounts && uiCounts.participants == contactsEndpoint.count',
      matchDetail: { uiVsApi, uiVsHeader, uiVsContacts },
    };
    check('counters: UI list == API /ui/conferences', uiVsApi,
      `ui=${JSON.stringify(uiCounts)} api=${JSON.stringify({ participants: apiCounts.participants, devices: apiCounts.devices })}`);
    check('counters: UI list == conference card header', uiVsHeader,
      `ui=${JSON.stringify(uiCounts)} header=${JSON.stringify(headerCounts)}`);
    check('counters: UI participants == contacts endpoint count', uiVsContacts,
      `ui=${uiCounts.participants} contacts=${contactsEndpoint.count} (${JSON.stringify(contactsEndpoint.names)})`);

    check('ui: no pageerror', pageErrors.length === 0, JSON.stringify(pageErrors).slice(0, 500));
    const writes = nonGetRequests.filter(request => !request.url.includes('/video_layouts/calculate')
      && !request.url.includes('/api/v3/login'));
    check('db: no writes to the live stand (draft only)', writes.length === 0, JSON.stringify(writes).slice(0, 500));
  } catch (error) {
    failure = error;
    console.error(error && error.stack || error);
  } finally {
    if (browser) await browser.close().catch(() => {});
    // Код выхода: 0 — все fatal-проверки прошли; 1 — есть проваленная
    // fatal-проверка; 2 — прогон упал целиком (логин/страница).
    const fatalChecks = checks.filter(entry => entry.severity === 'fatal');
    const failedFatal = fatalChecks.filter(entry => !entry.ok);
    const labelChecks = checks.filter(entry => entry.name.startsWith('size ') || entry.name.startsWith('layout:'));
    const counterChecks = checks.filter(entry => entry.name.startsWith('counters:'));
    const labelOk = labelChecks.length > 0 && labelChecks.every(entry => entry.ok);
    const counterOk = counterChecks.every(entry => entry.ok);
    const contactsOk = counterChecks
      .filter(entry => entry.name === 'counters: UI participants == contacts endpoint count')
      .every(entry => entry.ok);
    const ok = !failure && failedFatal.length === 0;
    const result = {
      ok,
      base: BASE,
      conference_id: CONFERENCE_ID,
      participant: PARTICIPANT,
      duration_ms: Date.now() - startedAt,
      criteria: {
        centerDx: CENTER_DX,
        centerDy: CENTER_DY,
        labelFontRatio: LABEL_FONT_RATIO,
        fontRatioTolerance: FONT_RATIO_TOLERANCE,
        strictCounts: STRICT_COUNTS,
      },
      summary: {
        sizesMeasured: sizes.length,
        sizesWithLabel: sizes.filter(item => item.labelFound).length,
        labelChecksOk: labelOk,
        counterChecksOk: counterOk,
        countersMatchContactsEndpoint: contactsOk,
        failedFatalChecks: failedFatal.map(entry => entry.name),
        knownIssues: checks.filter(entry => entry.severity === 'known-issue' && !entry.ok).map(entry => entry.name),
        fontRatios: sizes.map(item => ({
          size: `${item.size}${item.plus ? '+' : ''}`,
          slotFontSizePx: item.slotFontSizePx,
          labelFontSizePx: item.labelFontSizePx,
          ratio: item.slotFontSizePx ? Number((item.labelFontSizePx / item.slotFontSizePx).toFixed(3)) : null,
        })),
      },
      sizes,
      counters,
      checks,
      page_errors: pageErrors,
      console_errors: consoleErrors,
      non_get_requests: nonGetRequests,
      failure: failure ? String((failure && failure.stack) || failure) : null,
    };
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify(result, null, 2));
    console.log('\n===== SUMMARY =====');
    console.log(JSON.stringify({
      ok: result.ok,
      sizesMeasured: result.summary.sizesMeasured,
      sizesWithLabel: result.summary.sizesWithLabel,
      labelChecksOk: result.summary.labelChecksOk,
      counterChecksOk: result.summary.counterChecksOk,
      countersMatchContactsEndpoint: result.summary.countersMatchContactsEndpoint,
      failedFatalChecks: result.summary.failedFatalChecks,
      knownIssues: result.summary.knownIssues,
      uiCounts: counters && counters.uiCounts,
      apiCounts: counters && { participants: counters.apiCounts.participants, devices: counters.apiCounts.devices },
      headerCounts: counters && counters.headerCounts,
      contactsEndpoint: counters && { count: counters.contactsEndpoint.count, byType: counters.contactsEndpoint.byType },
    }, null, 2));
    console.log(`artifacts: ${ART}`);
    process.exit(ok ? 0 : (failure ? 2 : 1));
  }
})();

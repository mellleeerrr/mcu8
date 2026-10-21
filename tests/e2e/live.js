#!/usr/bin/env node
/*
 * Живой E2E TrueConf-совместимого MCU (PROSTOR/PNM 2.2.1.66).
 *
 * Проверяет именно бизнес-логику UI, а не только API:
 *  login → создание комнаты → старт сессии → добавление SIP-участника
 *  по адресу → ручная раскладка 4 → назначение участника в ячейку 1 →
 *  сохранение/применение → «Очистить все видеоокна» → Применить.
 *
 * Запуск: NODE_PATH=<node_modules с playwright> node tests/e2e/live.js
 * Артефакты: artifacts/e2e/{network.json,*.png,trace.zip,result.json}
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const { chromium } = require('playwright');

const BASE = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const SIP_URI = process.env.VKS_SIP_URI || 'sip:probe-e2e@192.168.1.132';
const SIP_NAME = process.env.VKS_SIP_NAME || 'E2E SIP Probe';
const ART = path.resolve(process.env.VKS_ARTIFACTS || 'artifacts/e2e');
const CHROMIUM = process.env.VKS_CHROMIUM
  || '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';

fs.mkdirSync(ART, { recursive: true });
const network = [];
const consoleErrors = [];
const pageErrors = [];
const checks = [];
const startedAt = Date.now();
let failure = null;

function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail });
  if (!ok) throw new Error(`CHECK FAILED: ${name}${detail ? ` — ${detail}` : ''}`);
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

(async () => {
  let browser;
  let context;
  let token = '';
  let conferenceId = '';
  let sessionId = '';
  let layoutId = '';
  let participantName = SIP_NAME;
  try {
    browser = await chromium.launch({ headless: true, executablePath: CHROMIUM, args: ['--no-sandbox'] });
    context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
    await context.tracing.start({ screenshots: true, snapshots: true, sources: true });
    const page = await context.newPage();
    page.on('console', message => { if (message.type() === 'error' && consoleErrors.length < 40) consoleErrors.push(message.text()); });
    page.on('pageerror', error => { if (pageErrors.length < 20) pageErrors.push(String(error && error.stack || error.message || error)); });
    page.on('request', request => {
      if (!request.url().includes('/api/v3/')) return;
      network.push({ kind: 'request', method: request.method(), url: request.url(), body: request.postData() || '' });
    });
    page.on('response', async response => {
      if (!response.url().includes('/api/v3/')) return;
      let body = '';
      try { body = (await response.text()).slice(0, 20000); } catch { /* stream */ }
      network.push({ kind: 'response', status: response.status(), url: response.url(), body });
    });

    // 1. Логин через реальный UI.
    await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'networkidle' });
    await page.fill('input[autocomplete="username"], input[type="text"]', LOGIN);
    await page.fill('input[type="password"]', PASSWORD);
    await page.click('button[type="submit"], button:has-text("Авторизоваться")');
    await page.waitForTimeout(2500);
    check('login: token in localStorage', await page.evaluate(() => Boolean(localStorage.getItem('TrueConfMCU_AccessToken'))));
    token = await page.evaluate(() => localStorage.getItem('TrueConfMCU_AccessToken'));
    check('login: token is JWT', typeof token === 'string' && token.split('.').length === 3);

    // 2. Список сессий/конференций открывается без pageerror.
    const sessionsResponse = page.waitForResponse(r => r.url().includes('/api/v3/ui/sessions?') && r.status() === 200, { timeout: 20000 });
    await page.goto(`${BASE}/pnm/monitoring/sessions`, { waitUntil: 'networkidle' });
    await sessionsResponse;
    await page.waitForTimeout(800);
    await page.screenshot({ path: path.join(ART, '01-sessions.png') });

    // 3. Создание комнаты кнопкой «add» — UI сразу POST-ит конференцию.
    await page.goto(`${BASE}/pnm/monitoring/conferences`, { waitUntil: 'networkidle' });
    await page.waitForTimeout(800);
    const createResponsePromise = page.waitForResponse(
      r => /\/api\/v3\/(ui\/)?conferences\/?$/.test(new URL(r.url()).pathname) && r.request().method() === 'POST',
      { timeout: 20000 },
    );
    await page.locator('button').filter({ hasText: 'add' }).first().click();
    const createResponse = await createResponsePromise;
    const createBody = await createResponse.json();
    conferenceId = createBody.conference && createBody.conference.id;
    check('conference: POST returned id', Boolean(conferenceId), JSON.stringify(createBody).slice(0, 300));
    const createdAudio = createBody.conference && createBody.conference.payload
      && createBody.conference.payload.audio_codecs;
    check('conference: audio codec matrix sent', Array.isArray(createdAudio) && createdAudio.length >= 16,
      `send ${JSON.stringify(createdAudio)}`);
    check('conference: G722.1C 48 in payload', !createdAudio || createdAudio.some(c => c.name === 'G7221C' && c.bitrate === 48),
      JSON.stringify(createdAudio));
    await page.waitForTimeout(1200);
    await page.screenshot({ path: path.join(ART, '02-conference-created.png') });

    // Закрыть модалку редактирования (создание уже произошло).
    const saveModal = page.getByRole('button', { name: 'Сохранить' });
    if (await saveModal.count()) await saveModal.last().click().catch(() => {});
    await page.waitForTimeout(600);

    // 4. Старт сессии.
    const startResponsePromise = page.waitForResponse(
      r => /\/api\/v3\/(ui\/)?sessions\/?$/.test(new URL(r.url()).pathname) && r.request().method() === 'POST',
      { timeout: 20000 },
    );
    await page.getByRole('button', { name: /Начать конференцию/ }).first().click();
    const startResponse = await startResponsePromise;
    const startBody = await startResponse.json();
    sessionId = startBody.session && startBody.session.id;
    check('session: started with id', Boolean(sessionId), JSON.stringify(startBody).slice(0, 300));
    await page.waitForTimeout(2000);
    if (!page.url().includes('/monitoring/sessions/')) {
      await page.goto(`${BASE}/pnm/monitoring/sessions/${sessionId}`, { waitUntil: 'networkidle' });
    }
    await page.waitForTimeout(1500);
    await page.screenshot({ path: path.join(ART, '03-session.png') });

    // 5. Добавление участника по адресу (SIP).
    let addAddress = page.locator('#abonent-blank-add-address-button, #abonent-table-add-address-button');
    if (!(await addAddress.count())) {
      addAddress = page.getByRole('button', { name: /Добавить по адресу|Добавить участника по адресу/ });
    }
    check('participant: add-address button present', (await addAddress.count()) > 0);
    await addAddress.first().click();
    await page.waitForTimeout(600);
    check('participant: address modal opened', (await page.locator('.address-manager:visible').count()) > 0);
    // Протокол SIP (в модалке уже выбран по умолчанию; меню открываем только
    // если текущее значение иное).
    const protocolButton = page.locator('.address-manager button').filter({ hasText: /^(SIP|H\.323|Авто)$/ }).first();
    if (await protocolButton.count()) {
      const protocolText = (await protocolButton.innerText()).trim();
      if (protocolText !== 'SIP') {
        await protocolButton.click();
        await page.waitForTimeout(400);
        const sipOption = page.locator('.tc-context-menu:visible, .mdc-menu-surface:visible').getByText('SIP', { exact: true }).first();
        if (await sipOption.count()) await sipOption.click();
      }
    }
    const visibleFields = page.locator('.address-manager input.tc-text-field__input:visible');
    check('participant: address+name fields visible', (await visibleFields.count()) >= 2, `fields=${await visibleFields.count()}`);
    await visibleFields.nth(0).fill(SIP_URI);
    await visibleFields.nth(1).fill(SIP_NAME);
    const callResponsePromise = page.waitForResponse(
      r => r.url().includes('/call_strings') && r.request().method() === 'POST',
      { timeout: 20000 },
    );
    const submitCall = page.locator('.address-manager').getByRole('button', { name: /Добавить|Позвонить|Вызвать/ }).last();
    await submitCall.click();
    const callResponse = await callResponsePromise;
    const callBody = await callResponse.json();
    check('sip: call_strings POST status', callResponse.status() === 200 || callResponse.status() === 201, String(callResponse.status()));
    const sipPayload = JSON.stringify(callBody);
    check('sip: payload carries full audio matrix', ['AAC', 'G711', 'G719', 'G7221C', 'G726', 'OPUS']
      .every(name => sipPayload.includes(`"${name}"`)), sipPayload.slice(0, 500));
    check('sip: payload carries required video matrix', ['H261', 'H263', 'H264', 'H264-HIGH', 'H265']
      .every(name => sipPayload.includes(`"${name}"`)), sipPayload.slice(0, 800));
    await page.waitForTimeout(4000);
    const participantVisible = await page.getByText(SIP_NAME, { exact: false }).count();
    check('sip: participant visible in conference UI', participantVisible > 0, `matches=${participantVisible}`);
    if (participantVisible === 0) {
      // UI могла назвать абонента по URI; проверяем по адресу.
      check('sip: participant visible by uri', (await page.getByText('192.168.1.132', { exact: false }).count()) > 0);
    }
    await page.screenshot({ path: path.join(ART, '04-sip-participant.png') });

    // 6. Ручная раскладка 4, участник в ячейку 1.
    await page.getByText('Раскладки', { exact: true }).first().click();
    await page.waitForTimeout(1000);
    await page.getByRole('button', { name: 'Создать' }).first().click();
    await page.waitForTimeout(800);
    await page.getByText('Ручная настройка', { exact: true }).click();
    await page.waitForTimeout(500);
    await page.locator('button').filter({ hasText: /^4$/ }).first().click();
    await page.waitForTimeout(700);
    const schemeCount = await page.locator('.layout-scheme-selector-item').count();
    check('layout: 4 → 7 scheme variants', schemeCount === 7, `schemes=${schemeCount}`);
    const layoutName = `E2E Manual 4 ${Date.now() % 100000}`;
    const nameField = page.locator('input.tc-text-field__input:visible').first();
    await nameField.fill(layoutName);
    await page.getByText(/Получатели/).first().click();
    await page.waitForTimeout(600);
    await page.getByRole('button', { name: /Добавить/ }).last().click();
    await page.waitForTimeout(800);
    const recipientRow = page.locator('.popup').last().getByText(SIP_NAME, { exact: false }).first();
    check('layout: recipient listed', (await recipientRow.count()) > 0);
    await recipientRow.click();
    await page.getByRole('button', { name: 'Добавить' }).last().click();
    await page.waitForTimeout(800);
    await page.getByText('Основное', { exact: true }).first().click();
    await page.waitForTimeout(800);
    await page.locator('.layout-grid-slot-wrapper').first().click();
    await page.waitForTimeout(700);
    // Слот-панель в vendor-UI не всегда имеет класс `.popup` (в 2.2.1.66
    // это выезжающая панель). Участник в ней — единственный видимый элемент
    // с именем в текущей вкладке «Основное».
    const slotPeer = page.getByText(SIP_NAME, { exact: false }).last();
    if (!(await slotPeer.count())) {
      await page.screenshot({ path: path.join(ART, '99-slot-popup-failure.png'), fullPage: true });
      console.error('SLOT POPUPS', await page.locator('.popup').evaluateAll(es => es.map(e => (e.innerText || '').slice(0, 500))));
      console.error('BODY TAIL', (await page.locator('body').innerText()).slice(-1200));
    }
    check('layout: slot 1 fixed-participant list', (await slotPeer.count()) > 0);
    await slotPeer.click();
    await page.waitForTimeout(600);
    const layoutCreatePromise = page.waitForResponse(
      r => r.url().includes('/api/v3/ui/video_layouts') && r.request().method() === 'POST' && !r.url().endsWith('/calculate'),
      { timeout: 20000 },
    );
    await page.getByRole('button', { name: /Создать|Сохранить/ }).last().click();
    const layoutCreateResponse = await layoutCreatePromise;
    const layoutCreateBody = await layoutCreateResponse.json();
    layoutId = layoutCreateBody.video_layout && layoutCreateBody.video_layout.id;
    check('layout: created', Boolean(layoutId), JSON.stringify(layoutCreateBody).slice(0, 300));
    check('layout: participant in position 1', JSON.stringify(layoutCreateBody.video_layout.payload.slots || [])
      .includes('"position":1') && JSON.stringify(layoutCreateBody.video_layout.payload.slots || []).includes('PEER'),
      JSON.stringify(layoutCreateBody.video_layout.payload.slots));
    await page.waitForTimeout(1200);
    await page.screenshot({ path: path.join(ART, '05-manual-layout-created.png') });

    // 7. Открыть сохранённую раскладку и проверить геометрию ячеек.
    const listLayout = page.getByText(layoutName, { exact: true }).first();
    check('layout: listed in session', (await listLayout.count()) > 0);
    await listLayout.click();
    await page.waitForTimeout(1500);
    const calcResponse = network.filter(x => x.kind === 'response' && x.url.includes('/calculate')).pop();
    if (calcResponse) {
      const calc = JSON.parse(calcResponse.body);
      check('layout: calculate positions are 1-based', calc.slots && calc.slots[0].position === 1,
        JSON.stringify(calc.slots && calc.slots.slice(0, 2)));
    }
    const tileBoxes = await page.locator('.layout-grid-slot-wrapper').evaluateAll(elements => elements.map(element => {
      const rect = element.getBoundingClientRect();
      return { text: (element.innerText || '').trim(), left: rect.left, top: rect.top, width: rect.width, height: rect.height };
    }));
    check('layout: 4 tiles rendered', tileBoxes.length === 4, JSON.stringify(tileBoxes));
    const widths = tileBoxes.map(box => box.width);
    const minWidth = Math.min(...widths);
    const maxWidth = Math.max(...widths);
    check('layout: equal cells (no off-by-one tiny tile)', maxWidth - minWidth < Math.max(8, maxWidth * 0.05),
      JSON.stringify(tileBoxes));
    const first = tileBoxes[0];
    const minLeft = Math.min(...tileBoxes.map(box => box.left));
    const minTop = Math.min(...tileBoxes.map(box => box.top));
    check('layout: participant cell is top-left tile 1', Math.abs(first.left - minLeft) < 2 && Math.abs(first.top - minTop) < 2,
      JSON.stringify(tileBoxes));
    check('layout: participant rendered in tile 1', first.text.includes(SIP_NAME), first.text);
    await page.screenshot({ path: path.join(ART, '06-layout-applied.png') });

    // 7b. T04a: 2×2 → все 7 подвариантов дают 4 ячейки (bbox + текст уже
    // проверены для первого варианта; здесь — полнота каталога и calculate).
    const schemeItems = page.locator('.layout-scheme-selector-item');
    const schemeTotal = await schemeItems.count();
    check('layout: 2x2 scheme catalog has 7 variants', schemeTotal === 7, `schemes=${schemeTotal}`);
    const variantPresets = [];
    for (let index = 0; index < schemeTotal; index += 1) {
      await schemeItems.nth(index).click();
      await page.waitForTimeout(350);
      const boxes = await page.locator('.layout-grid-slot-wrapper').evaluateAll(elements => elements.map(element => {
        const rect = element.getBoundingClientRect();
        return { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
      }));
      check(`layout: 2x2 variant ${index + 1} has 4 tiles`, boxes.length === 4, JSON.stringify(boxes));
      const calc = network.filter(x => x.kind === 'request' && x.url.includes('/calculate')).pop();
      if (calc && calc.body) {
        try { variantPresets.push(JSON.parse(calc.body).preset); } catch { /* ignore */ }
      }
    }
    check('layout: all 7 variants exercised', new Set(variantPresets).size === 7,
      JSON.stringify(variantPresets));
    check('layout: variant presets are distinct 4-up schemes',
      variantPresets.every(preset => typeof preset === 'string' && preset.includes('4')), JSON.stringify(variantPresets));
    await schemeItems.nth(0).click();
    await page.waitForTimeout(400);

    // 7c. T04a: UI-посадка участника в ячейки 1, N/2, N для 4/8/9/25.
    // Меняем размер и целевую ячейку в редакторе, не сохраняя черновик;
    // проверяем bbox и текст именно в целевой ячейке, ровно один PEER.
    const readTiles = () => page.locator('.layout-grid-slot-wrapper').evaluateAll(elements => elements.map(element => {
      const rect = element.getBoundingClientRect();
      return { text: (element.innerText || '').trim(), left: rect.left, top: rect.top, width: rect.width, height: rect.height };
    }));
    const placeParticipant = async (size, position) => {
      await page.locator('button').filter({ hasText: new RegExp(`^${size}$`) }).first().click();
      await page.waitForTimeout(650);
      const cell = page.locator('.layout-grid-slot-wrapper').nth(position - 1);
      await cell.click();
      await page.waitForTimeout(500);
      const peer = page.getByText(SIP_NAME, { exact: false }).last();
      if (await peer.count()) {
        await peer.click();
        await page.waitForTimeout(450);
      }
      return readTiles();
    };
    for (const size of [4, 8, 9, 25]) {
      for (const position of [1, Math.ceil(size / 2), size]) {
        const tiles = await placeParticipant(size, position);
        const target = tiles[position - 1];
        check(`layout: UI ${size} cell ${position} rendered`, Boolean(target && target.width > 20 && target.height > 20),
          JSON.stringify(target));
        check(`layout: UI ${size} cell ${position} holds SIP participant`, Boolean(target && target.text.includes(SIP_NAME)),
          JSON.stringify(tiles.map(tile => tile.text)));
        check(`layout: UI ${size} keeps ${size} cells when participant is in ${position}`,
          tiles.length === size, JSON.stringify(tiles.map(tile => tile.text)));
      }
    }
    // Возвращаем ручную 2×2 с участником в ячейке 1 для clear-all/detach.
    await placeParticipant(4, 1);

    // 7d. T04a: calculate для 4/8/9/25 — ровно N ячеек и 1-based позиции.
    for (const [preset, expected] of [['preset_equal_4', 4], ['preset_equal_8', 8], ['preset_equal_9', 9], ['preset_equal_25', 25]]) {
      const calc = await api(token, 'POST', '/api/v3/ui/video_layouts/calculate', { preset, width: 1280, height: 720 });
      const slots = calc.json && calc.json.slots || [];
      check(`layout: ${preset} calculates ${expected} slots`, calc.status === 200 && slots.length === expected,
        `status=${calc.status} slots=${slots.length}`);
      check(`layout: ${preset} positions are 1..${expected}`,
        slots.length === expected && slots[0].position === 1 && slots[expected - 1].position === expected,
        JSON.stringify(slots.slice(0, 2).concat(slots.slice(-2))));
    }

    // 8. «Очистить все видеоокна» → Применить: все слоты AUTO.
    // TcButton рисует title не всегда на <button>: ищем любой элемент с
    // подсказкой «Очистить все видеоокна» (в en-бандле Clear all video windows).
    let clearButton = page.getByTitle(/Очистить все видеоокна|Clear all video windows/).first();
    if (!(await clearButton.count())) clearButton = page.locator('button[title*="Очистить"], button[title*="Clear"]').first();
    if (!(await clearButton.count())) {
      console.error('TITLED ELEMENTS', await page.locator('[title]').evaluateAll(es => es.map(e => ({ tag: e.tagName, title: e.title, cls: String(e.className).slice(0, 80) })).filter(x => /Очист|Clear/.test(x.title))));
    }
    check('layout: clear-all button present', (await clearButton.count()) > 0);
    await clearButton.click();
    await page.waitForTimeout(500);
    const patchPromise = page.waitForResponse(
      r => r.url().includes(`/video_layouts/${layoutId}`) && ['PATCH', 'POST'].includes(r.request().method()),
      { timeout: 20000 },
    );
    await page.getByRole('button', { name: /Применить|Сохранить/ }).last().click();
    const patchResponse = await patchPromise;
    const patchBody = await patchResponse.json();
    const patchedSlots = patchBody.video_layout && patchBody.video_layout.payload.slots || patchBody.payload && patchBody.payload.slots || [];
    check('layout: clear-all PATCH slots present', Array.isArray(patchedSlots) && patchedSlots.length === 4,
      JSON.stringify(patchedSlots));
    check('layout: clear-all sets all AUTO', patchedSlots.every(slot => slot.role === 'AUTO'), JSON.stringify(patchedSlots));
    await page.waitForTimeout(800);
    await page.screenshot({ path: path.join(ART, '07-clear-all-applied.png') });

    // 8b. T04a: detach ручной раскладки → в сессии остаётся «Общая»,
    // ручной id из списка исчезает, default не удалён.
    const detach = await api(token, 'DELETE', `/api/v3/sessions/${sessionId}/video_layouts/${layoutId}`);
    check('layout: detach manual returns 204', detach.status === 204, `status=${detach.status} ${detach.text.slice(0, 200)}`);
    const layoutsAfterDetach = await api(token, 'GET',
      `/api/v3/ui/sessions/${sessionId}/video_layouts?page=1&page_size=100&sort_field=display_name&sort_order=0`);
    const afterList = layoutsAfterDetach.json && (layoutsAfterDetach.json.video_layouts || layoutsAfterDetach.json.layouts) || [];
    check('layout: detached manual is gone', !afterList.some(item => item.id === layoutId), JSON.stringify(afterList.map(item => item.id)));
    check('layout: canonical default remains after detach',
      afterList.some(item => item.payload && item.payload.type === 'default'), JSON.stringify(afterList.map(item => item.payload && item.payload.type)));
    // Визуально: после detach сессия показывает «Общую», ручной раскладки нет.
    await page.reload();
    await page.waitForTimeout(1500);
    const layoutsPanel = page.getByText('Раскладки', { exact: true }).first();
    if (await layoutsPanel.count()) {
      await layoutsPanel.click();
      await page.waitForTimeout(700);
    }
    const bodyAfterDetach = await page.locator('body').innerText();
    check('layout: default «Общая» visible after detach', /Общая/.test(bodyAfterDetach), bodyAfterDetach.slice(0, 500));
    check('layout: manual name gone from session UI', !bodyAfterDetach.includes(layoutName), bodyAfterDetach.slice(0, 500));
    await page.screenshot({ path: path.join(ART, '08-detach-default.png') });

    // 9. Кодек-эндпоинты имеют эталонную форму.
    const audio = await api(token, 'GET', '/api/v3/settings/audio_codecs');
    check('api: audio_codecs shape {name,bitrate}', audio.status === 200 && Array.isArray(audio.json.audio_codecs)
      && audio.json.audio_codecs[0].name && Object.prototype.hasOwnProperty.call(audio.json.audio_codecs[0], 'bitrate'),
      JSON.stringify(audio.json));
    const video = await api(token, 'GET', '/api/v3/settings/video_codecs');
    check('api: video_codecs shape [string]', video.status === 200 && Array.isArray(video.json.video_codecs)
      && video.json.video_codecs.every(codec => typeof codec === 'string'), JSON.stringify(video.json));

    // 10. Нет необъяснённых 501/5xx в сценарии.
    const bad = network.filter(x => x.kind === 'response' && x.status >= 500);
    check('network: no 5xx', bad.length === 0, JSON.stringify(bad.map(x => `${x.status} ${x.url}`)));
    check('network: no 501', !network.some(x => x.kind === 'response' && x.status === 501),
      JSON.stringify(network.filter(x => x.kind === 'response' && x.status === 501).map(x => x.url)));
    check('ui: no pageerror', pageErrors.length === 0, JSON.stringify(pageErrors));
    // Vendor bundle 2.2.1.66 кидает эти TypeError при открытии/закрытии
    // модалки конференции; на эталоне 192.168.1.92 те же 6 ошибок (проверено
    // отдельным прогоном). Это не наши регрессии, но фиксируем их отдельно.
    const vendorNoise = /Cannot read properties of (null|undefined) \(reading '(route|displayName|mode|connectionsLimit|length)'\)/;
    // Ожидаемые 404 при detach фиксируются отдельным final-ассертом
    // (console404.length <= expectedDetach404.length), иначе stale-запросы UI
    // после отвязки раскладки дают ложный красный.
    const expected404Noise = /Failed to load resource: the server responded with a status of 404/;
    const unexpectedConsole = consoleErrors.filter(message => !vendorNoise.test(message) && !expected404Noise.test(message));
    check('ui: no unexpected console errors', unexpectedConsole.length === 0, JSON.stringify(unexpectedConsole));
  } catch (error) {
    failure = error;
    console.error(error && error.stack || error);
  } finally {
    // Сначала закрываем браузер: вендорский бандл 2.2.1.66 кидает TypeError,
    // если комната удаляется при открытой модалке; pageerror-проверка уже
    // выполнена, а cleanup-API не зависит от страницы.
    if (context) {
      try { await context.tracing.stop({ path: path.join(ART, 'trace.zip') }); } catch { /* ignore */ }
      await context.close().catch(() => {});
      context = null;
    }
    // Останавливаем только созданную этим прогоном сессию, не чужие.
    if (sessionId && token) {
      try {
        const stop = await api(token, 'DELETE', `/api/v3/sessions/${sessionId}`);
        checks.push({ name: 'cleanup: session stopped', ok: stop.status < 300, detail: String(stop.status) });
      } catch (error) {
        checks.push({ name: 'cleanup: session stopped', ok: false, detail: String(error) });
      }
    }
    // T04a: detach оставляет layout глобальным — удаляем именно созданный id,
    // не трогая «Общую» и чужие раскладки.
    if (layoutId && token) {
      try {
        const removedLayout = await api(token, 'DELETE', `/api/v3/video_layouts/${layoutId}`);
        checks.push({ name: 'cleanup: created layout removed', ok: [204, 404].includes(removedLayout.status), detail: String(removedLayout.status) });
      } catch (error) {
        checks.push({ name: 'cleanup: created layout removed', ok: false, detail: String(error) });
      }
    }
    // T04a: удаляем созданную комнату — purge снимет session/conference links
    // и оставшиеся раскладки, иначе прогоны копят комнаты и layout-мусор.
    if (conferenceId && token) {
      try {
        const removed = await api(token, 'DELETE', `/api/v3/conferences/${conferenceId}`);
        checks.push({ name: 'cleanup: conference removed', ok: removed.status < 300, detail: String(removed.status) });
      } catch (error) {
        checks.push({ name: 'cleanup: conference removed', ok: false, detail: String(error) });
      }
    }
    if (browser) await browser.close().catch(() => {});
    // T04a: финальные ассерты после закрытия страницы и cleanup — вендорский
    // UI не должен добавлять pageerror/console 404 вне ожидаемого detach.
    const expectedDetach404 = network.filter(x => x.kind === 'response' && x.status === 404
      && x.url.includes('/video_layouts/') && layoutId && x.url.includes(layoutId));
    const unexpected4xx = network.filter(x => x.kind === 'response' && x.status >= 400 && x.status < 500
      && !(expectedDetach404.includes(x)));
    checks.push({ name: 'final: no unexpected 4xx', ok: unexpected4xx.length === 0,
      detail: JSON.stringify(unexpected4xx.map(x => `${x.status} ${x.url}`)) });
    checks.push({ name: 'final: no pageerror after cleanup', ok: pageErrors.length === 0,
      detail: JSON.stringify(pageErrors) });
    const console404 = consoleErrors.filter(message => /status of 404/.test(message));
    checks.push({ name: 'final: console 404 only expected detach', ok: console404.length <= expectedDetach404.length,
      detail: JSON.stringify(console404) });
    const result = {
      ok: !failure && checks.every(check => check.ok),
      base: BASE,
      conference_id: conferenceId,
      session_id: sessionId,
      layout_id: layoutId,
      sip_uri: SIP_URI,
      duration_ms: Date.now() - startedAt,
      checks,
      page_errors: pageErrors,
      console_errors: consoleErrors,
      failure: failure ? String(failure && failure.stack || failure) : null,
    };
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify(result, null, 2));
    fs.writeFileSync(path.join(ART, 'network.json'), JSON.stringify(network, null, 2));
    console.log(JSON.stringify(result, null, 2));
    process.exit(result.ok ? 0 : 1);
  }
})();

#!/usr/bin/env node
/*
 * Детерминированный E2E-репро `TypeError: … "some", this.sessionLayoutListByType.default is undefined`.
 *
 * Что ломается в UI (вендорный бандл 2.2.1.66, `AbonentTableItemConnected`):
 *
 *   hasIndividualLayout(){ return !this.sessionLayoutListByType.default.some(…) }
 *   sessionLayoutListByType(){ return this.sessionLayoutList.reduce((m,l)=>(m[l.type]||…,m),{}) }
 *
 * `sessionLayoutList` — session-scope раскладки из
 * `GET /api/v3/ui/sessions/<id>/video_layouts`, поле `type` = `payload.type`.
 * Если в session-scope нет ни одной раскладки с `payload.type === "default"`
 * (а есть только `individual`), то `sessionLayoutListByType.default` === undefined
 * и любой connected-абонент в таблице участников валит render-функцию
 * `TypeError: Cannot read properties of undefined (reading 'some')`
 * (в Firefox это ровно «can't access property "some"»).
 *
 * Сценарий (идемпотентный, трогает только свои объекты с префиксом «E2E Crash»):
 *   1. UI-логин admin → токен из localStorage['TrueConfMCU_AccessToken'].
 *   2. API: POST /api/v3/conferences {display_name:"E2E Crash <ts>"} → POST /api/v3/sessions.
 *   3. Проверяем, что session-scope содержит «Общую» (payload.type=default).
 *   4. scripts/bench/sipbot.py → SIP UAC подключается в комнату (connected-абонент).
 *   5. Открываем /pnm/monitoring/sessions/<id> — БЕЗ pageerror (базовая линия).
 *   6. Best-effort: открываем редактор «Общей» в UI и сохраняем («Применить»),
 *      фиксируя payload PATCH/POST /video_layouts — есть ли там type=individual.
 *   7. Через тот же UI-эндпоинт PATCH /api/v3/ui/sessions/<id>/video_layouts/<default_id>
 *      отправляем {"payload":{"type":"individual"}} — сервер принимает `type` из payload
 *      без валидации и через COW оставляет в session-scope только individual
 *      (у пользовательской сессии 49b599d7 ровно такое состояние: count=1, type=individual).
 *   8. Перезагружаем страницу сессии → Vue рендерит таблицу с connected-абонентом →
 *      ловим TypeError из hasIndividualLayout.
 *
 * Запуск:
 *   cd tests/e2e && node repro_session_default_layout.js                # ждём крэш (до фикса)
 *   cd tests/e2e && node repro_session_default_layout.js --expect-clean # ждём ОТСУТСТВИЕ крэша (после фикса)
 *   node repro_session_default_layout.js --legacy-bundle                # тот же прогон, но клиент
 *       принудительно откатывается к pre-fix коду: Playwright подменяет в ОТВЕТЕ бандла
 *       `(this.sessionLayoutListByType.default||[])` → `this.sessionLayoutListByType.default`.
 *       Нужен, когда фикс уже задеплоен: доказывает, что тест ловит именно этот дефект.
 *       Файлы на стенде не меняются — подмена живёт только в браузере прогона.
 *   node repro_session_default_layout.js --expect-clean --guard-only     # пост-фикс регресс:
 *       бэкенд не мутируем вообще, а браузеру отдаём список session-раскладок БЕЗ default
 *       (route.fulfill вырезает строки type=default). С патчем `(default||[])` страница
 *       обязана устоять; без патча (--legacy-bundle) тот же прогон падает.
 *       Это самый честный «после фикса»-режим: не зависит от того, запретил ли бэкенд
 *       менять payload.type.
 *
 * Важно: если бэкенд уже иммутабелен (PATCH payload.type не меняет), боевой flip
 * битый scope не создаёт. Тогда скрипт сам инжектит то же клиентское состояние на
 * границе API (route вырезает type=default из ответа списка) и помечает
 * `flip.forced = "api-route-strips-default"` в result.json. Это не меняет стенд —
 * только ответы, видимые браузеру прогона.
 *
 * Коды возврата:
 *   без флага       — 0 если крэш воспроизведён, 1 если нет (и при любой ошибке сценария);
 *   --expect-clean  — 0 если крэш НЕ воспроизведён и сценарий реально отработал, иначе 1.
 *
 * Артефакты: artifactsDir('repro-crash') — result.json, network.json, pageerrors.json,
 * *.png. Скрипт не меняет код приложения/бандлы/бэкенд и не перезапускает сервис.
 *
 * Оговорка про «pageerror»: в Chromium у Vue 3 нет error boundary, поэтому ошибка
 * render-функции перехватывается самим Vue и выводится через console.error
 * (`runtime.handleError` → `logError`), а не как uncaught exception window.onerror.
 * Скрипт поэтому считает крэшем и `pageerror`, и console-error с сигнатурой
 * `hasIndividualLayout` / `sessionLayoutListByType.default` — семантика та же
 * (render-компонент падает, страница остаётся полу-отрисованной).
 */
'use strict';

const fs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const { spawn, execFile } = require('node:child_process');
const { chromium } = require('playwright');
const { artifactsDir } = require('./paths');

const BASE = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const REPO = path.resolve(__dirname, '..', '..');
const SIPBOT = process.env.VKS_SIPBOT || path.join(REPO, 'scripts', 'bench', 'sipbot.py');
const PYTHON = process.env.VKS_PYTHON || 'python3';
const CHROMIUM = process.env.VKS_CHROMIUM
  || path.join(os.homedir(), '.cache', 'ms-playwright', 'chromium-1243', 'chrome-linux64', 'chrome');
const ART = artifactsDir('repro-crash');
const EXPECT_CLEAN = process.argv.includes('--expect-clean');
/** Откат одной строки фикса в ответе бандла (только в браузере прогона). */
const LEGACY_BUNDLE = process.argv.includes('--legacy-bundle');
/** Пост-фикс: проверка JS-guard при намеренно битом ответе списка раскладок. */
const GUARD_ONLY = process.argv.includes('--guard-only');
const KEEP = process.argv.includes('--keep');
const SIP_DURATION = Number(process.env.VKS_SIPBOT_DURATION || 150);
const RUN_NAME = `E2E Crash ${Date.now()}`;
/** Сессия пользователя из баг-репорта: только чтение, для сверки «битого» инварианта. */
const KNOWN_BROKEN_SESSION = '49b599d7-b5cd-45a7-8999-47a28c453a92';
/**
 * «Общая» (default) раскладка, чья правка через session-эндпоинт ломает инвариант.
 * Именно этот запрос делает UI-редактор (Th(): {display_name, payload:{mode,preset,type,…}}),
 * только редактор всегда шлёт type:"default".
 */
const FLIP_PAYLOAD = { payload: { type: 'individual' } };

fs.mkdirSync(ART, { recursive: true });
const startedAt = Date.now();

const network = [];
const pageErrors = [];
const consoleErrors = [];
const layoutSaves = [];
const checks = [];
const cleanup = [];
const crashRecords = [];
let crashArmed = false;
let failure = null;
/** Патч фикса в живом бандле: `(this.sessionLayoutListByType.default||[])`; pre-fix — без `||[]`. */
const FIXED_GUARD = '(this.sessionLayoutListByType.default||[])';
const LEGACY_GUARD = 'this.sessionLayoutListByType.default';
const bundleState = {
  live_has_guard: null,
  live_has_unguarded: null,
  legacy_requested: LEGACY_BUNDLE,
  injection: null,
  injections: {},
  error: null,
};

const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));

function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok), detail: detail === undefined ? '' : String(detail) });
  return Boolean(ok);
}

function log(...args) {
  console.log('[repro]', ...args);
}

/** Сигнатура бага: render упал на hasIndividualLayout / отсутствующем default. */
function isCrashSignature(text) {
  const value = String(text || '');
  if (/hasIndividualLayout/.test(value)) return true;
  if (/sessionLayoutListByType\.default/.test(value)) return true;
  // Точный Chromium-текст TypeError ловим только после взведения сценария:
  // до поломки стейта он означал бы чужую ошибку (ложное срабатывание).
  if (crashArmed && /Cannot read propert(y|ies) of undefined \(reading 'some'\)/.test(value)) return true;
  if (crashArmed && /can't access property ["']some["']/.test(value)) return true;
  return false;
}

function recordCrash(kind, text, extra) {
  const entry = {
    kind,
    matched: isCrashSignature(text),
    text: String(text || ''),
    stack: String((extra && extra.stack) || ''),
    url: String((extra && extra.url) || ''),
    at: new Date().toISOString(),
  };
  if (!entry.matched) return null;
  crashRecords.push(entry);
  log(`CRASH (${kind}) ${entry.text.split('\n')[0]}`);
  return entry;
}

function localIPv4() {
  if (process.env.VKS_SIP_LOCAL_IP) return process.env.VKS_SIP_LOCAL_IP;
  const candidates = [];
  for (const [name, list] of Object.entries(os.networkInterfaces())) {
    for (const address of list || []) {
      if (address.family !== 'IPv4' || address.internal) continue;
      // Бенч-прогоны MCU используют «внешний» интерфейс стенда (192.168.x);
      // veth-адреса 10.x не должны попасть в SDP как candidate.
      candidates.push({ name, address: address.address, rank: /^192\.168\./.test(address.address) ? 0 : 1 });
    }
  }
  candidates.sort((a, b) => a.rank - b.rank);
  return candidates.length ? candidates[0].address : '127.0.0.1';
}

/** Что реально отдаёт стенд: есть ли в бандле защита `(…default||[])`. */
async function probeLiveBundle() {
  try {
    const response = await fetch(`${BASE}/mcu/chunks/index6.bundle-2.2.1.66.js`);
    const body = await response.text();
    bundleState.live_has_guard = body.includes(FIXED_GUARD);
    bundleState.live_has_unguarded = body.includes(`${LEGACY_GUARD}.some(`);
  } catch (error) {
    bundleState.error = String(error && error.message || error);
  }
}

/**
 * Pre-fix симуляция: в ответе бандла возвращаем ровно ту строку, что была в HEAD
 * (`return !this.sessionLayoutListByType.default.some(...)`). Диск не трогаем.
 */
async function installLegacyBundleRoute(context) {
  await context.route('**/chunks/*bundle-*.js*', async route => {
    try {
      const response = await route.fetch();
      const url = response.url();
      const file = url.split('/').pop().split('?')[0];
      let body = await response.text();
      let result;
      if (body.includes(FIXED_GUARD)) {
        body = body.replace(FIXED_GUARD, LEGACY_GUARD);
        result = 'guard-removed';
      } else if (body.includes(`${LEGACY_GUARD}.some(`)) {
        result = 'already-legacy';
      } else {
        result = 'pattern-not-found';
      }
      bundleState.injections[file] = result;
      // Вердикт — только по основному чанку, который грузит компонент:
      // остальные чанки паттерна не содержат и не должны перетирать статус.
      if (file.startsWith('index6.bundle')) bundleState.injection = result;
      await route.fulfill({ response, body });
    } catch (error) {
      bundleState.error = String(error && error.message || error);
      await route.continue();
    }
  });
}

/**
 * `--guard-only`: бэкенд здоров, но браузеру отдаётся список раскладок сессии
 * БЕЗ строк type=default — ровно то состояние, на котором падал вендор.
 * С патчем `(default||[])` страница обязана устоять; с откатанным guard'ом
 * (--legacy-bundle) падение здесь и есть доказательство чувствительности теста.
 */
async function installBrokenLayoutsRoute(context) {
  bundleState.layout_route_installed = true;
  await context.route('**/api/v3/ui/sessions/*/video_layouts*', async route => {
    const request = route.request();
    if (request.method() !== 'GET') return route.continue();
    try {
      const response = await route.fetch();
      const body = await response.text();
      let json = null;
      try { json = JSON.parse(body); } catch { return route.fulfill({ response, body }); }
      const list = json.video_layouts || json.layouts;
      if (!Array.isArray(list)) return route.fulfill({ response, body });
      const filtered = list.filter(layout => !layout || !layout.payload || layout.payload.type !== 'default');
      if (json.video_layouts) json.video_layouts = filtered; else json.layouts = filtered;
      json.count = filtered.length;
      bundleState.layout_injection = `stripped ${list.length - filtered.length} default(s)`;
      return route.fulfill({
        response,
        body: JSON.stringify(json),
        headers: { ...response.headers(), 'content-type': 'application/json' },
      });
    } catch (error) {
      bundleState.error = String(error && error.message || error);
      return route.continue();
    }
  });
}

async function api(token, method, urlPath, body) {
  const response = await fetch(`${BASE}${urlPath}`, {
    method,
    headers: {
      authorization: `Bearer ${token}`,
      'content-type': 'application/json',
    },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let json = null;
  try { json = text ? JSON.parse(text) : null; } catch { /* not json */ }
  return { status: response.status, json, text };
}

function sessionLayouts(token, sessionId) {
  return api(token, 'GET',
    `/api/v3/ui/sessions/${encodeURIComponent(sessionId)}/video_layouts?page=1&page_size=100&sort_field=display_name&sort_order=0`);
}

function layoutsOf(response) {
  const json = response.json || {};
  return json.video_layouts || json.layouts || [];
}

function typeOf(layout) {
  return layout && layout.payload ? layout.payload.type : undefined;
}

function hasDefault(layouts) {
  return layouts.some(layout => typeOf(layout) === 'default');
}

/** Ждём connected-абонента: id гейтвея `…@sip-gateway` + живая медиа-статистика. */
async function waitForConnectedAbonent(token, sessionId, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    const response = await api(token, 'GET', `/api/v3/sessions/${encodeURIComponent(sessionId)}/abonents`);
    const abonents = (response.json && response.json.abonents) || [];
    last = abonents;
    const connected = abonents.find(abonent => {
      const id = String(abonent.abonent_id || abonent.connection_id || '');
      const media = abonent.media || {};
      return id.includes('@sip-gateway')
        && (Number(media.audio_rx_packets || 0) > 0 || Boolean(abonent.connected_at));
    });
    if (connected) return { abonent: connected, abonents };
    await sleep(1000);
  }
  return { abonent: null, abonents: last };
}

function spawnSipbot(sessionId, localIp) {
  const jsonOut = path.join(ART, 'sipbot.json');
  const args = [
    SIPBOT,
    '--room', sessionId,
    '--host', process.env.VKS_SIP_HOST || localIp,
    '--local-ip', localIp,
    '--audio-codec', 'PCMU',
    '--video-profile', '360p15',
    '--duration', String(SIP_DURATION),
    '--json-out', jsonOut,
    '--no-mcu-cpu',
    // Свой слот SIP/RTP-портов (5080+2i / 20000+4i): параллельный прогон
    // того же скрипта не должен конфликтовать по портам.
    '--index', String(process.pid % 40),
  ];
  const child = spawn(PYTHON, args, { cwd: REPO, stdio: ['ignore', 'pipe', 'pipe'] });
  const bot = { child, args, stdout: '', stderr: '', jsonOut, exitCode: null, signal: null };
  child.stdout.on('data', chunk => { bot.stdout += chunk.toString(); });
  child.stderr.on('data', chunk => { bot.stderr += chunk.toString(); });
  child.on('exit', (code, signal) => { bot.exitCode = code; bot.signal = signal; });
  return bot;
}

async function stopSipbot(bot) {
  if (!bot || !bot.child || bot.child.exitCode !== null) return;
  try { bot.child.kill('SIGTERM'); } catch { /* already gone */ }
  for (let attempt = 0; attempt < 20 && bot.child.exitCode === null; attempt += 1) await sleep(500);
  if (bot.child.exitCode === null) {
    try { bot.child.kill('SIGKILL'); } catch { /* ignore */ }
  }
}

(async () => {
  let browser = null;
  let context = null;
  let page = null;
  let token = '';
  let conferenceId = '';
  let sessionId = '';
  let sessionName = RUN_NAME;
  let sipbot = null;
  let knownBroken = null;
  let constructorCapture = null;
  let flip = null;
  let baselineClean = false;

  try {
    check('chromium: binary exists', fs.existsSync(CHROMIUM), CHROMIUM);
    check('sipbot: script exists', fs.existsSync(SIPBOT), SIPBOT);

    await probeLiveBundle();
    log('live bundle:', JSON.stringify(bundleState));

    browser = await chromium.launch({
      headless: true,
      executablePath: CHROMIUM,
      args: ['--no-sandbox'],
    });
    context = await browser.newContext({ viewport: { width: 1600, height: 1000 }, ignoreHTTPSErrors: true });
    if (LEGACY_BUNDLE) await installLegacyBundleRoute(context);
    if (GUARD_ONLY) await installBrokenLayoutsRoute(context);
    page = await context.newPage();

    page.on('pageerror', error => {
      const text = String((error && (error.stack || error.message)) || error);
      pageErrors.push({ text, at: new Date().toISOString() });
      recordCrash('pageerror', text, { stack: text, url: page.url() });
    });
    page.on('console', message => {
      if (message.type() !== 'error') return;
      const text = message.text();
      const location = message.location ? message.location() : {};
      consoleErrors.push({ text, location, at: new Date().toISOString() });
      recordCrash('console', text, {
        stack: text,
        url: location.url || page.url(),
      });
    });
    page.on('request', request => {
      if (!request.url().includes('/api/v3/')) return;
      const entry = {
        kind: 'request',
        method: request.method(),
        url: request.url(),
        body: (request.postData() || '').slice(0, 8000),
        at: new Date().toISOString(),
      };
      network.push(entry);
      if (/video_layouts/.test(entry.url) && ['PATCH', 'POST', 'PUT', 'DELETE'].includes(entry.method)) {
        layoutSaves.push(entry);
      }
    });
    page.on('response', async response => {
      if (!response.url().includes('/api/v3/')) return;
      let body = '';
      try { body = (await response.text()).slice(0, 8000); } catch { /* stream */ }
      network.push({
        kind: 'response',
        status: response.status(),
        method: response.request().method(),
        url: response.url(),
        body,
        at: new Date().toISOString(),
      });
    });

    // ── 1. UI-логин (реальный маршрут, токен из localStorage) ───────────────
    await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'domcontentloaded' });
    await page.fill('input[autocomplete="username"], input[type="text"]', LOGIN);
    await page.fill('input[type="password"]', PASSWORD);
    await page.click('button[type="submit"], button:has-text("Авторизоваться")');
    await page.waitForTimeout(2500);
    token = await page.evaluate(() => localStorage.getItem('TrueConfMCU_AccessToken') || '');
    check('login: token in localStorage', Boolean(token));

    check('bundle: live guard state captured', bundleState.live_has_guard !== null, JSON.stringify(bundleState));
    if (LEGACY_BUNDLE) {
      check('bundle: pre-fix injection applied in browser response',
        ['guard-removed', 'already-legacy'].includes(bundleState.injection), JSON.stringify(bundleState));
    }

    // ── 2. Read-only сверка с сессией из баг-репорта ───────────────────────
    const brokenResponse = await sessionLayouts(token, KNOWN_BROKEN_SESSION);
    const brokenLayouts = layoutsOf(brokenResponse);
    knownBroken = {
      session_id: KNOWN_BROKEN_SESSION,
      status: brokenResponse.status,
      count: brokenLayouts.length,
      types: brokenLayouts.map(typeOf),
      has_default: hasDefault(brokenLayouts),
      note: 'Только чтение: пользовательскую сессию не трогаем.',
    };
    log('known broken session:', JSON.stringify(knownBroken));
    // Диагностика, а не предусловие: сессия пользователя только читается, её ремонт —
    // отдельная задача бэкенда. Фиксируем факт (нет default / уже отремонтирована),
    // чтобы отчёт не путал «починили UI» и «починили данные».
    const knownRepaired = knownBroken.has_default
      && knownBroken.types.filter(type => type === 'default').length === 1;
    knownBroken.repaired = knownRepaired;
    knownBroken.note = knownRepaired
      ? 'Прочитана без изменений: бэкенд уже починил данные (default в scope есть).'
      : 'Прочитана без изменений: default в scope нет — ремонт данных ещё не задеплоен.';
    check('diagnostic: legacy session 49b599d7 read-only state recorded',
      brokenResponse.status === 200,
      JSON.stringify({ types: knownBroken.types, repaired: knownRepaired }));

    // ── 3. Своя конференция и сессия ────────────────────────────────────────
    const created = await api(token, 'POST', '/api/v3/conferences', { display_name: RUN_NAME });
    conferenceId = (created.json && created.json.conference && created.json.conference.id) || '';
    check('conference: created', created.status === 200 || created.status === 201, `${created.status} id=${conferenceId}`);
    if (!conferenceId) throw new Error(`conference create failed: ${created.status} ${created.text.slice(0, 300)}`);

    const started = await api(token, 'POST', '/api/v3/sessions', { conference_id: conferenceId });
    sessionId = (started.json && started.json.session && started.json.session.id) || conferenceId;
    sessionName = (started.json && started.json.session && started.json.session.display_name) || RUN_NAME;
    check('session: started', started.status === 200 || started.status === 201, `${started.status} id=${sessionId}`);
    if (!sessionId) throw new Error(`session start failed: ${started.status} ${started.text.slice(0, 300)}`);

    const initialLayoutsResponse = await sessionLayouts(token, sessionId);
    let layouts = layoutsOf(initialLayoutsResponse);
    const initialDefault = layouts.find(layout => typeOf(layout) === 'default');
    check('session: canonical default attached at start', Boolean(initialDefault),
      JSON.stringify(layouts.map(layout => `${layout.id}:${typeOf(layout)}`)));
    if (!initialDefault) throw new Error('session has no default layout right after start — серверный инвариант нарушен');

    // ── 4. SIP-участник через scripts/bench/sipbot.py ──────────────────────
    const localIp = localIPv4();
    log('sipbot: local ip', localIp, 'room', sessionId);
    sipbot = spawnSipbot(sessionId, localIp);
    let connected = await waitForConnectedAbonent(token, sessionId, 45000);
    check('sip: participant connected (media flows)', Boolean(connected.abonent),
      connected.abonent
        ? `${connected.abonent.display_name || ''} ${connected.abonent.abonent_id}`
        : `sipbot stderr: ${sipbot.stderr.slice(-500)}`);
    if (!connected.abonent) {
      // Одна повторная попытка: первый INVITE мог совпасть с прогревом гейтвея.
      await stopSipbot(sipbot);
      sipbot = spawnSipbot(sessionId, localIp);
      connected = await waitForConnectedAbonent(token, sessionId, 30000);
      check('sip: participant connected after retry', Boolean(connected.abonent), sipbot.stderr.slice(-500));
    }
    if (!connected.abonent) throw new Error(`sipbot did not connect: ${sipbot.stderr.slice(-800)}`);

    // ── 5. Базовая линия: страница сессии с connected-абонентом НЕ падает ──
    if (GUARD_ONLY) {
      // Список раскладок уже битый (route вырезал default) — проверяем сам guard.
      crashArmed = true;
      await page.goto(`${BASE}/pnm/monitoring/sessions/${sessionId}`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(12000);
      baselineClean = crashRecords.length === 0;
      check(EXPECT_CLEAN
        ? 'guard-only: broken layout list did not crash the session page (JS guard active)'
        : 'guard-only: broken layout list crashed the session page (pre-fix behavior)',
      EXPECT_CLEAN ? baselineClean : !baselineClean,
      JSON.stringify(crashRecords.map(record => record.text.split('\n')[0])));
      await page.screenshot({ path: path.join(ART, '01-session-baseline.png'), fullPage: true });
      log('guard-only scenario done, reproduced =', crashRecords.length > 0);
      return { ok: true, reproduced: crashRecords.length > 0 };
    }
    await page.goto(`${BASE}/pnm/monitoring/sessions/${sessionId}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(9000);
    baselineClean = crashRecords.length === 0;
    check('baseline: session page with connected abonent renders without crash', baselineClean,
      JSON.stringify(crashRecords.map(record => record.text.split('\n')[0])));
    await page.screenshot({ path: path.join(ART, '01-session-baseline.png'), fullPage: true });
    if (!baselineClean) {
      // Битый стейт не наш — фиксируем и выходим, иначе тест «после фикса» станет ложным.
      throw new Error(`baseline already crashes before layout mutation: ${crashRecords[0].text.split('\n')[0]}`);
    }

    // ── 6. Best-effort: payload сохранения «Общей» в редакторе раскладок ───
    // Нужен для root cause: шлёт ли UI type="individual" при правке default.
    try {
      const savesBefore = layoutSaves.length;
      await page.getByText('Раскладки', { exact: true }).first().click({ timeout: 10000 });
      await page.waitForTimeout(2000);
      const commonRow = page.getByText('Общая', { exact: true });
      const commonCount = await commonRow.count();
      if (!commonCount) throw new Error('«Общая» не найдена в списке раскладок сессии');
      await commonRow.first().click({ timeout: 8000 });
      await page.waitForTimeout(2500);
      const dynamicMode = page.getByText('Динамическая раскладка', { exact: false });
      if (await dynamicMode.count()) {
        await dynamicMode.first().click({ timeout: 6000 }).catch(() => {});
        await page.waitForTimeout(1200);
      }
      const applyButton = page.getByRole('button', { name: 'Применить' });
      if (!(await applyButton.count())) throw new Error('кнопка «Применить» не найдена');
      await applyButton.last().click({ timeout: 8000 }).catch(() => {});
      await page.waitForTimeout(3500);
      const saves = layoutSaves.slice(savesBefore);
      constructorCapture = {
        attempted: true,
        ok: saves.length > 0,
        editor_url: page.url(),
        saves,
        reason: saves.length ? '' : 'ни одного PATCH/POST /video_layouts не ушло',
      };
    } catch (error) {
      constructorCapture = { attempted: true, ok: false, saves: layoutSaves, reason: String(error && error.message || error) };
    }
    check('constructor: save request captured', Boolean(constructorCapture && constructorCapture.ok),
      JSON.stringify((constructorCapture && constructorCapture.saves || []).map(entry => entry.body)));

    // После сохранения UI default мог переехать в COW-клон — перечитываем.
    let afterUiLayouts = layoutsOf(await sessionLayouts(token, sessionId));
    let defaultLayout = afterUiLayouts.find(layout => typeOf(layout) === 'default');
    check('session: default still present after editor save', Boolean(defaultLayout),
      JSON.stringify(afterUiLayouts.map(layout => `${layout.id}:${typeOf(layout)}`)));

    // ── 7. Пытаемся сменить тип «Общей» тем же UI-эндпоинтом ────────────────
    if (!defaultLayout) throw new Error('cannot arm: default layout missing before flip');
    const flipResponse = await api(token, 'PATCH',
      `/api/v3/ui/sessions/${encodeURIComponent(sessionId)}/video_layouts/${encodeURIComponent(defaultLayout.id)}`,
      FLIP_PAYLOAD);
    flip = {
      method: 'PATCH',
      path: `/api/v3/ui/sessions/${sessionId}/video_layouts/${defaultLayout.id}`,
      request: FLIP_PAYLOAD,
      status: flipResponse.status,
      response_layout: flipResponse.json && flipResponse.json.video_layout
        ? { id: flipResponse.json.video_layout.id, type: typeOf(flipResponse.json.video_layout) }
        : null,
    };
    check('flip: PATCH executed', [200, 204, 409, 422].includes(flipResponse.status),
      `${flipResponse.status} ${flipResponse.text.slice(0, 300)}`);

    layouts = layoutsOf(await sessionLayouts(token, sessionId));
    const brokenScope = !hasDefault(layouts) && layouts.some(layout => typeOf(layout) === 'individual');
    flip.immutable = hasDefault(layouts);
    flip.scope_after = layouts.map(layout => `${layout.id}:${typeOf(layout)}`);
    // Иммутабельность payload.type — диагностика состояния бэкенда, а не
    // предусловие UI-теста: до фикса flip проходил, после — type не меняется.
    check('diagnostic: flip outcome (default retained = backend immutable, absent = mutable)',
      hasDefault(layouts) || brokenScope,
      JSON.stringify({ types: flip.scope_after, immutable: flip.immutable }));

    // Если бэкенд уже иммутабелен (payload.type не меняется), боевой PATCH битый
    // scope не создаёт. Тогда нужное клиенту состояние (список session-раскладок
    // без type=default) инжектится на границе API — тот же дефект, тот же рендер.
    if (!brokenScope) {
      await installBrokenLayoutsRoute(context);
      flip.forced = 'api-route-strips-default';
    } else {
      flip.forced = null;
    }
    const armed = brokenScope || Boolean(bundleState.layout_route_installed);
    check('crash-arm: session scope broken (API flip or client-side list injection)', armed,
      JSON.stringify({
        types: flip.scope_after,
        immutable: flip.immutable,
        forced: flip.forced,
        layout_injection: bundleState.layout_injection || null,
      }));

    // ── 8. Перезагрузка страницы: connected-строка + битый default ─────────
    crashArmed = true;
    await page.goto(`${BASE}/pnm/monitoring/sessions/${sessionId}`, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(12000);
    await page.screenshot({ path: path.join(ART, '02-session-after-flip.png'), fullPage: true });
    const participantRowVisible = (await page.locator('.abonent-item-connected-wrapper, .abonent-item-connected').count()) > 0;
    if (!crashRecords.length) {
      // Второй заход: свежий mount (SPA + первый fetch списка раскладок).
      await page.reload({ waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(12000);
    }
    if (!crashRecords.length) {
      // Третий заход: список сессий → страница сессии, плюс цикл refreshSession
      // (интервал 5-10 c) на уже отрисованной connected-строке.
      await page.goto(`${BASE}/pnm/monitoring/sessions`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(2500);
      await page.goto(`${BASE}/pnm/monitoring/sessions/${sessionId}`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(12000);
    }

    const crash = crashRecords[0] || null;
    check(EXPECT_CLEAN
      ? 'crash: no TypeError from hasIndividualLayout after fix'
      : 'crash: TypeError from hasIndividualLayout captured',
    EXPECT_CLEAN ? !crash : Boolean(crash),
    crash ? crash.text.split('\n').slice(0, 6).join('\n') : 'no crash captured');
    check('ui: connected participant row present in DOM (scenario precondition)', participantRowVisible);

    if (crash) {
      await page.screenshot({ path: path.join(ART, '03-crash.png'), fullPage: true }).catch(() => {});
    } else {
      await page.screenshot({ path: path.join(ART, '03-no-crash.png'), fullPage: true }).catch(() => {});
    }

    // ── 9. Итог режима считается в finally (после cleanup) ─────────────────
    return { ok: true, reproduced: Boolean(crash) };
  } catch (error) {
    failure = error;
    console.error(error && error.stack || error);
    return { ok: false, reproduced: crashRecords.length > 0, scenarioOk: false };
  } finally {
    if (!KEEP) {
      if (sipbot) {
        await stopSipbot(sipbot);
        cleanup.push({ action: 'sipbot: stopped', ok: sipbot.exitCode === 0 || sipbot.exitCode === null, detail: `code=${sipbot.exitCode} signal=${sipbot.signal}` });
      }
      if (token && sessionId) {
        const stopped = await api(token, 'DELETE', `/api/v3/sessions/${encodeURIComponent(sessionId)}`).catch(error => ({ status: 0, text: String(error) }));
        cleanup.push({ action: 'session: stopped', ok: stopped.status < 300 || stopped.status === 404, detail: String(stopped.status) });
      }
      if (token && conferenceId) {
        const removed = await api(token, 'DELETE', `/api/v3/conferences/${encodeURIComponent(conferenceId)}`).catch(error => ({ status: 0, text: String(error) }));
        cleanup.push({
          action: 'conference: removed (purge room-scoped layouts)',
          ok: removed.status < 300 || removed.status === 404,
          detail: String(removed.status),
        });
      }
    } else {
      cleanup.push({ action: 'cleanup skipped (--keep)', ok: true, detail: '' });
    }

    if (context) await context.close().catch(() => {});
    if (browser) await browser.close().catch(() => {});

    const crash = crashRecords[0] || null;
    const reproduced = Boolean(crash);
    // "constructor:" — best-effort сбор payload'а (root cause), не влияет на вердикт;
    // "crash:"/"bundle:"/"mode:"/"diagnostic:" — вердикт/диагностика стенда,
    // а не предусловия сценария.
    const scenarioOk = checks
      .filter(item => !/^(constructor:|crash:|bundle:|mode:|diagnostic:)/.test(item.name))
      .every(item => item.ok);
    const ok = EXPECT_CLEAN ? (scenarioOk && !reproduced) : reproduced;
    checks.push({ name: `mode: ${EXPECT_CLEAN ? 'expect-clean' : 'expect-crash'} satisfied`, ok, detail: `reproduced=${reproduced} scenario_ok=${scenarioOk}` });

    const result = {
      ok,
      mode: GUARD_ONLY
        ? (EXPECT_CLEAN ? 'guard-only-expect-clean' : 'guard-only-expect-crash')
        : (EXPECT_CLEAN ? 'expect-clean' : 'expect-crash'),
      guard_only: GUARD_ONLY,
      legacy_bundle: LEGACY_BUNDLE,
      reproduced,
      scenario_ok: scenarioOk,
      generated_at: new Date().toISOString(),
      duration_ms: Date.now() - startedAt,
      base: BASE,
      run_name: RUN_NAME,
      conference_id: conferenceId,
      session_id: sessionId,
      session_name: sessionName,
      known_broken_session: knownBroken,
      bundle: bundleState,
      crash: crash
        ? {
          reproduced: true,
          kind: crash.kind,
          message: crash.text.split('\n')[0],
          stack: crash.text,
          url: crash.url,
          at: crash.at,
          note: 'Vue 3 перехватывает render-ошибку и логирует её через console.error; window.onerror (pageerror) не срабатывает.',
        }
        : { reproduced: false },
      flip,
      constructor_save: constructorCapture,
      layout_saves: layoutSaves,
      page_errors: pageErrors,
      console_errors: consoleErrors,
      crash_records: crashRecords,
      checks,
      cleanup,
      failure: failure ? String(failure && failure.stack || failure) : null,
      artifacts: {
        dir: ART,
        result: path.join(ART, 'result.json'),
        network: path.join(ART, 'network.json'),
        pageerrors: path.join(ART, 'pageerrors.json'),
        baseline: path.join(ART, '01-session-baseline.png'),
        after_flip: path.join(ART, '02-session-after-flip.png'),
        crash_screenshot: path.join(ART, reproduced ? '03-crash.png' : '03-no-crash.png'),
      },
    };

    fs.writeFileSync(path.join(ART, 'network.json'), JSON.stringify(network, null, 2));
    fs.writeFileSync(path.join(ART, 'pageerrors.json'), JSON.stringify({
      pageerrors: pageErrors,
      console_errors: consoleErrors,
      crash_records: crashRecords,
      signature: 'hasIndividualLayout / sessionLayoutListByType.default',
      note: result.crash.note || '',
    }, null, 2));
    fs.writeFileSync(path.join(ART, 'result.json'), JSON.stringify(result, null, 2));

    console.log('\n===== RESULT =====');
    console.log(JSON.stringify({
      ok: result.ok,
      mode: result.mode,
      reproduced: result.reproduced,
      crash_message: result.crash.message || null,
      stack: (result.crash.stack || '').split('\n').slice(0, 8),
      bundle: result.bundle,
      flip: result.flip,
      constructor_save: (result.layout_saves || []).map(entry => ({ method: entry.method, url: entry.url, body: entry.body })),
      checks: result.checks,
      cleanup: result.cleanup,
      artifacts: result.artifacts,
    }, null, 2));
    process.exit(result.ok ? 0 : 1);
  }
})();

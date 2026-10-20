#!/usr/bin/env node
// T2.2 acceptance evidence: история конференций PNM (call_history list/detail,
// ui/session_history, ui/call_history, export ZIP, clean) через vendor-UI
// 2.2.1.66 на нашем `/api/v3`.
//
// Запуск:
//   PLAYWRIGHT_MODULE=/path/to/playwright \
//   BASE_URL=http://127.0.0.1:8080 \
//   node scripts/evidence/t22-evidence.js
//
// Скрипт создаёт уникальную конференцию, запускает/останавливает сессию с
// реальной попыткой вызова, проверяет API+UI, экспорт/очистку и делает PNG.
// Любой провал acceptance-гейта → `ok:false` и exit 1.

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const OUT_PRELIGHT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t22';
let chromium;
try {
  chromium = require(process.env.PLAYWRIGHT_MODULE || 'playwright').chromium;
} catch (e) {
  fs.mkdirSync(OUT_PRELIGHT, { recursive: true });
  fs.writeFileSync(
    path.join(OUT_PRELIGHT, 'summary.json'),
    JSON.stringify({ ok: false, phase: 'playwright-require', error: String(e) }, null, 1) + '\n',
  );
  console.error(e);
  process.exit(1);
}

const BASE = (process.env.BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const OUT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t22';
const ADMIN_LOGIN = process.env.ADMIN_LOGIN || 'admin';
const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD || 'admin_password';
fs.mkdirSync(OUT, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sh = (cmd) => { try { return execSync(cmd, { encoding: 'utf8' }).trim(); } catch { return ''; } };

// Провенанс фиксируется ДО первой записи в evidence-каталог.
const build = {
  git_head: sh('git rev-parse HEAD'),
  git_head_ct: Number(sh('git log -1 --format=%ct HEAD')) || 0,
  git_dirty: sh('git status --porcelain') !== '',
  binary_sha256: null,
  binary_mtime: null,
  binary_path: null,
  server_pid: null,
};
{
  const pids = sh('pgrep -f "target/release/vks-server"').split('\n').filter(Boolean);
  for (const pid of pids) {
    const exe = `/proc/${pid}/exe`;
    try {
      build.binary_sha256 = crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex');
      build.binary_mtime = Math.floor(fs.statSync(exe).mtimeMs / 1000);
      build.binary_path = exe;
      build.server_pid = Number(pid);
      break;
    } catch { /* пробуем следующий процесс */ }
  }
}

const stamp = String(Date.now()).slice(-6);
const CONF_NAME = `T22History_${stamp}`;
const CALL_NAME = `T22 Polycom ${stamp}`;
const CALL_URI = 'sip:127.0.0.1:5999';
const steps = [];
const screenshots = [];
const httpErrors = [];
const consoleErrors = [];
const vendorErrors = [];

const summary = {
  task: 'T2.2',
  ok: false,
  started_at: new Date().toISOString(),
  build,
  acceptance: {},
  totals: {},
  error: null,
};

(async () => {
  const browser = await chromium.launch({
    args: ['--no-sandbox', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--ignore-certificate-errors'],
  });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1600, height: 1000 } });
  const page = await context.newPage();
  page.on('response', (response) => {
    if (response.status() >= 400 && response.url().includes('/api/v3/')) {
      httpErrors.push(`${response.status()} ${response.url()}`);
    }
  });
  page.on('console', (message) => {
    if (message.type() !== 'error') return;
    const text = message.text();
    // Известная ошибка vendor-оболочки 2.2.1.66 (не наш ответ и не влияет на UI).
    if (text.includes('validationHandler') && text.includes('catch is not a function')) vendorErrors.push(text);
    else consoleErrors.push(text);
  });
  page.on('pageerror', (error) => consoleErrors.push(String(error)));

  const record = async (name, fn) => {
    const value = await fn();
    steps.push(name);
    return value;
  };
  const shot = async (name) => {
    const file = path.join(OUT, name);
    await page.screenshot({ path: file, fullPage: true });
    screenshots.push(name);
    return file;
  };
  const api = async (pathname, options = {}) => {
    const response = await page.request.fetch(BASE + pathname, {
      method: options.method || 'GET',
      headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
      data: options.data,
      failOnStatusCode: false,
    });
    const text = await response.text();
    let json = null;
    try { json = JSON.parse(text); } catch { /* не-JSON (ZIP) */ }
    return { status: response.status(), json, text, response };
  };

  let token = null;
  try {
    // login через UI, затем токен того же сеанса
    await page.goto(BASE + '/pnm/', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2500);
    if (await page.locator('input[type=password]').count()) {
      await page.fill('input[type=text], input[autocomplete=username]', ADMIN_LOGIN).catch(() => {});
      await page.fill('input[type=password]', ADMIN_PASSWORD);
      await page.keyboard.press('Enter');
      await page.waitForTimeout(5000);
    }
    token = await page.evaluate(() => {
      for (const key of Object.keys(localStorage)) {
        const value = localStorage.getItem(key);
        if (value && value.includes('eyJ')) {
          try { const parsed = JSON.parse(value); if (parsed.token) return parsed.token; }
          catch { if (value.startsWith('eyJ')) return value; }
        }
      }
      return null;
    });
    if (!token) {
      token = (await (await page.request.post(BASE + '/api/v3/login', { data: { login: ADMIN_LOGIN, password: ADMIN_PASSWORD } })).json()).token;
    }
    if (!token) throw new Error('нет токена после UI-логина');

    // Детерминированный старт: чистим историю от предыдущих прогонов/проб.
    await api('/api/v3/call_history/clean', { method: 'POST' });

    // 1. конференция + сессия + попытка вызова
    const created = await record('conference-create', async () => {
      const response = await page.request.post(BASE + '/api/v3/conferences', {
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        data: { display_name: CONF_NAME },
      });
      if (response.status() !== 200) throw new Error(`conference create ${response.status()}: ${await response.text()}`);
      return (await response.json()).conference;
    });
    const conferenceId = created.id;
    const dtmf = created.primary_dtmf?.name;
    summary.acceptance.conference_dtmf = typeof dtmf === 'string' && /^\d{9}$/.test(dtmf);
    summary.acceptance.conference_route = created.primary_route?.name === CONF_NAME;
    const started = await record('session-start', async () => {
      const response = await page.request.post(BASE + '/api/v3/sessions', {
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        data: { conference_id: conferenceId },
      });
      if (response.status() !== 200) throw new Error(`session start ${response.status()}: ${await response.text()}`);
      return (await response.json()).session;
    });
    await record('call-string-create', async () => {
      const response = await page.request.post(BASE + `/api/v3/ui/sessions/${conferenceId}/call_strings`, {
        headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
        data: { uri: CALL_URI, display_name: CALL_NAME },
      });
      if (response.status() !== 201) throw new Error(`call string ${response.status()}: ${await response.text()}`);
      return (await response.json()).call_string;
    });
    await page.waitForTimeout(7000);
    await record('session-stop', async () => {
      const response = await page.request.delete(BASE + `/api/v3/sessions/${conferenceId}`, {
        headers: { Authorization: `Bearer ${token}` },
      });
      if (response.status() !== 204) throw new Error(`session stop ${response.status()}: ${await response.text()}`);
      return true;
    });

    // 2. API-контракт списка
    let historyId = null;
    await record('history-list-api', async () => {
      const list = await api(`/api/v3/call_history?display_name=${encodeURIComponent(CONF_NAME)}&page_size=10`);
      if (list.status !== 200) throw new Error(`call_history ${list.status}`);
      if (list.json.count < 1) throw new Error('история пуста после остановки');
      const session = list.json.sessions.find((row) => row.display_name === CONF_NAME);
      if (!session) throw new Error('нет записи созданной конференции');
      historyId = session.id;
      summary.acceptance.history_list_schema =
        typeof session.display_name === 'string' &&
        typeof session.started_at === 'number' &&
        typeof session.stopped_at === 'number' &&
        typeof session.abonents === 'number' &&
        session.stop_reason === 'Manual' &&
        session.creator && typeof session.creator.display_name === 'string' &&
        session.stopper && typeof session.stopper.display_name === 'string' &&
        session.payload && typeof session.payload.bitrate === 'number';
      summary.acceptance.history_dtmf_route = session.dtmf === dtmf && session.route === CONF_NAME;
      return session;
    });

    // 3. detail + ui-эндпоинты
    await record('history-detail-api', async () => {
      const detail = await api(`/api/v3/call_history/${historyId}`);
      if (detail.status !== 200) throw new Error(`detail ${detail.status}`);
      const calls = detail.json.session.call_strings || [];
      const call = calls.find((row) => row.connection_url === CALL_URI);
      summary.acceptance.history_detail_calls =
        !!call &&
        call.display_name === CALL_NAME &&
        Array.isArray(call.invitations) &&
        call.invitations.length >= 1 &&
        typeof call.invitations[0].finish_reason === 'string';
      const ui = await api(`/api/v3/ui/session_history/${historyId}`);
      if (ui.status !== 200) throw new Error(`ui/session_history ${ui.status}`);
      summary.acceptance.ui_session_history =
        ui.json.session.display_name === CONF_NAME &&
        ui.json.session.started_by && typeof ui.json.session.started_by.display_name === 'string' &&
        ui.json.session.dtmf === dtmf;
      const uiCalls = await api(`/api/v3/ui/call_history/${historyId}`);
      if (uiCalls.status !== 200) throw new Error(`ui/call_history ${uiCalls.status}`);
      summary.acceptance.ui_call_history = uiCalls.json.count >= 1 && Array.isArray(uiCalls.json.call_strings);
    });

    // 4. export ZIP (+ CSV-заголовок) и пагинация
    await record('history-export-api', async () => {
      const exported = await api('/api/v3/call_history/export?page_size=100');
      if (exported.status !== 200) throw new Error(`export ${exported.status}`);
      const filename = exported.json.filename;
      if (!filename || !filename.endsWith('.zip')) throw new Error(`export filename: ${filename}`);
      summary.export = { filename };
      const download = await page.request.get(BASE + `/static/call_history/${filename}`);
      if (download.status() !== 200) throw new Error(`static export ${download.status()}`);
      const bytes = await download.body();
      const csvHeader = Buffer.from('Название', 'utf8');
      summary.acceptance.export_zip = bytes.length > 100 && bytes.subarray(0, 4).equals(Buffer.from('PK\x03\x04'));
      summary.acceptance.export_csv_header = bytes.includes(csvHeader);
      const pageSize = await api('/api/v3/call_history?page_size=1');
      summary.acceptance.history_pagination =
        pageSize.status === 200 && Array.isArray(pageSize.json.sessions) && pageSize.json.sessions.length <= 1 && pageSize.json.count >= 1;
    });

    // 5. UI: список истории (строка с DTMF/маршрутом)
    await record('history-list-ui', async () => {
      await page.goto(BASE + '/pnm/history', { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(4500);
      const listText = await page.locator('body').innerText();
      if (!listText.includes(CONF_NAME)) throw new Error('UI-список не содержит созданную конференцию');
      if (!listText.includes(`ID: ${dtmf}`)) throw new Error(`UI-список не содержит ID: ${dtmf}`);
      if (!listText.includes(CONF_NAME)) throw new Error('UI-список не содержит маршрут');
      summary.acceptance.history_list_ui = true;
      await shot('01-history-list.png');
    });

    // 6. UI: карточка истории (вызов + приглашение)
    await record('history-detail-ui', async () => {
      await page.locator('.history-table-item').filter({ hasText: CONF_NAME }).first().locator('strong').first().click();
      await page.waitForTimeout(4000);
      if (!/\/pnm\/history\/[\w-]+/.test(page.url())) throw new Error(`UI не открыл detail: ${page.url()}`);
      const detailText = await page.locator('body').innerText();
      if (!detailText.includes(`ID: ${dtmf}`)) throw new Error('карточка истории без DTMF');
      if (!detailText.includes(CONF_NAME)) throw new Error('карточка истории без маршрута');
      if (!detailText.includes(CALL_NAME)) throw new Error('карточка истории без вызова');
      if (!detailText.includes(CALL_URI)) throw new Error('карточка истории без URI вызова');
      summary.acceptance.history_detail_ui = true;
      await shot('02-history-detail.png');
    });

    // 7. export через UI: кнопка «Действия» в строке истории скачивает ZIP
    await record('history-export-ui', async () => {
      await page.goto(BASE + '/pnm/history', { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(4000);
      const row = page.locator('.history-table-item').filter({ hasText: CONF_NAME }).first();
      await row.waitFor({ timeout: 10000 });
      const downloadPromise = page.waitForEvent('download', { timeout: 15000 });
      await row.locator('button').last().click();
      const download = await downloadPromise;
      const filename = download.suggestedFilename();
      if (!filename.endsWith('.zip')) throw new Error(`UI export filename: ${filename}`);
      summary.export = { ...(summary.export || {}), ui_filename: filename };
      summary.acceptance.history_export_ui = true;
    });

    // 8. clean через API и пустой список в UI
    let backup = null;
    await record('history-clean-api', async () => {
      const cleaned = await api('/api/v3/call_history/clean', { method: 'POST' });
      if (cleaned.status !== 200) throw new Error(`clean ${cleaned.status}`);
      backup = cleaned.json.filename;
      if (!backup || !backup.includes('backup')) throw new Error(`clean backup filename: ${backup}`);
      const check = await api('/api/v3/call_history');
      summary.acceptance.history_clean = check.status === 200 && check.json.count === 0;
      return cleaned.json;
    });
    await record('history-empty-ui', async () => {
      await page.goto(BASE + '/pnm/history', { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(4000);
      const text = await page.locator('body').innerText();
      if (text.includes(CONF_NAME)) throw new Error('после clean UI всё ещё показывает конференцию');
      summary.acceptance.history_clean_ui = true;
      await shot('03-history-empty.png');
    });

    const gates = Object.values(summary.acceptance);
    summary.ok = gates.length > 0 && gates.every(Boolean) && consoleErrors.length === 0 && httpErrors.length === 0;
    if (!summary.ok) {
      summary.error =
        `провалены гейты: ${Object.entries(summary.acceptance).filter(([, ok]) => !ok).map(([name]) => name).join(', ') || '-'};` +
        ` consoleErrors=${consoleErrors.length} httpErrors=${httpErrors.length}`;
    }
  } catch (error) {
    summary.error = String(error && error.stack ? error.stack : error);
    try { await shot('99-failure.png'); } catch { /* скриншот не обязателен */ }
  } finally {
    summary.finished_at = new Date().toISOString();
    summary.totals = {
      steps: steps.length,
      screenshots: screenshots.length,
      http_errors: httpErrors.length,
      console_errors: consoleErrors.length,
      vendor_console_errors: vendorErrors.length,
    };
    summary.steps = steps;
    summary.http_error_samples = httpErrors.slice(0, 10);
    summary.console_error_samples = consoleErrors.slice(0, 10);
    summary.vendor_error_samples = vendorErrors.slice(0, 10);
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    await browser.close().catch(() => {});
  }
  console.log(JSON.stringify({ ok: summary.ok, acceptance: summary.acceptance, totals: summary.totals, error: summary.error }, null, 1));
  process.exit(summary.ok ? 0 : 1);
})();

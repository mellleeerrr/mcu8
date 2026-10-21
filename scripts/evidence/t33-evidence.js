#!/usr/bin/env node
// T3.3: финальный обход страниц PNM + таблица паритета ключевых API.
//
// Только чтение: сценарий не создаёт/не удаляет конференции, не меняет
// настройки. Пишет summary.json + PNG; при 4xx/5xx или console error на
// страницах — ok:false, exit 1.
//
//   PLAYWRIGHT_MODULE=/path/to/playwright BASE_URL=http://127.0.0.1:8080 \
//   node scripts/evidence/t33-evidence.js
'use strict';
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
let chromium;
try {
  chromium = require(process.env.PLAYWRIGHT_MODULE || 'playwright').chromium;
} catch (error) {
  console.error(error);
  process.exit(1);
}

const BASE = (process.env.BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const OUT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t33';
const LOGIN = process.env.ADMIN_LOGIN || 'admin';
const PASSWORD = process.env.ADMIN_PASSWORD || 'admin_password';
fs.mkdirSync(OUT, { recursive: true });
const sh = (cmd) => { try { return execSync(cmd, { encoding: 'utf8' }).trim(); } catch { return ''; } };

const build = {
  git_head: sh('git rev-parse HEAD'),
  git_head_ct: Number(sh('git log -1 --format=%ct HEAD')) || 0,
  git_dirty: sh('git status --porcelain') !== '',
  binary_sha256: null,
  binary_mtime: null,
  server_pid: null,
};
for (const pid of sh('pgrep -f "target/release/vks-server"').split('\n').filter(Boolean)) {
  try {
    const exe = `/proc/${pid}/exe`;
    build.binary_sha256 = crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex');
    build.binary_mtime = Math.floor(fs.statSync(exe).mtimeMs / 1000);
    build.server_pid = Number(pid);
    break;
  } catch { /* следующий */ }
}

const summary = { task: 'T3.3', ok: false, build, acceptance: {}, pages: [], totals: {}, error: null };
const httpErrors = [];
const consoleErrors = [];
const vendorErrors = [];
const isVendorError = (text) => /Invalid scale configuration for scale: (yAxes|xAxes|responsive|maintainAspectRatio)/.test(text);
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

(async () => {
  const browser = await chromium.launch();
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 } });
  const page = await context.newPage();
  page.on('response', (response) => {
    if (response.url().includes('/api/') && response.status() >= 400) httpErrors.push(`${response.status()} ${response.url()}`);
  });
  page.on('console', (message) => {
    if (message.type() !== 'error') return;
    const text = message.text();
    if (isVendorError(text)) vendorErrors.push(text);
    else consoleErrors.push(text);
  });
  const shot = async (name) => {
    const file = path.join(OUT, name);
    await page.screenshot({ path: file, fullPage: false });
    summary.pages.push(name);
  };

  try {
    await page.goto(BASE + '/pnm/', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2500);
    if (await page.locator('input[type=password]').count()) {
      await page.fill('input[type=text], input[autocomplete=username]', LOGIN).catch(() => {});
      await page.fill('input[type=password]', PASSWORD);
      await page.keyboard.press('Enter');
      await page.waitForTimeout(5000);
    }
    let token = await page.evaluate(() => {
      for (const storage of [localStorage, sessionStorage]) {
        for (const key of Object.keys(storage)) {
          try {
            const parsed = JSON.parse(storage.getItem(key));
            if (parsed && parsed.token) return parsed.token;
          } catch { /* не JSON */ }
          const raw = storage.getItem(key);
          if (raw && raw.startsWith('eyJ')) return raw;
        }
      }
      return null;
    });
    if (!token) {
      const response = await page.request.post(BASE + '/api/v3/login', { data: { login: LOGIN, password: PASSWORD } });
      const body = await response.json().catch(() => null);
      token = body && body.token;
    }
    if (!token) throw new Error('нет токена после UI-логина');
    const api = async (pathname) => {
      const response = await page.request.fetch(BASE + pathname, { headers: { Authorization: `Bearer ${token}` }, failOnStatusCode: false });
      const text = await response.text();
      let json = null;
      try { json = JSON.parse(text); } catch { /* non-JSON */ }
      return { status: response.status(), json, text };
    };

    // ---- API-паритет (read-only контракты, которыми живёт UI) ----
    const endpoints = [
      ['/healthz', 200, (j) => j.alive === true],
      ['/readyz', 200, (j) => j.db === true && j.media === true],
      ['/api/v3/system/about', 200, (j) => j.state && j.state.status === 'READY' && j.versions],
      ['/api/v3/ui/system/status', 200, (j) => j.engine && j.cluster],
      ['/api/v3/system/status/network', 200, (j) => j.network && j.network.editable === false],
      ['/api/v3/license', 200, (j) => j.state === 'VALID' && j.type === 'PERMANENT'],
      ['/api/v3/features', 200, (j) => Array.isArray(j.features) && j.features.length > 40],
      ['/api/v3/conferences?page=1&page_size=5', 200, (j) => typeof j.count === 'number' && Array.isArray(j.conferences)],
      ['/api/v3/sessions?page=1&page_size=5', 200, (j) => typeof j.count === 'number' && Array.isArray(j.sessions)],
      ['/api/v3/call_history?page=1&page_size=5', 200, (j) => typeof j.count === 'number' && Array.isArray(j.sessions)],
      ['/api/v3/call_strings?page=1&page_size=5', 200, (j) => typeof j.count === 'number' && Array.isArray(j.call_strings)],
      ['/api/v3/log_events?page=1&page_size=5', 200, (j) => typeof j.count === 'number' && Array.isArray(j.log_events)],
      ['/api/v3/videorecords?page=1&page_size=5', 200, (j) => typeof j.count === 'number' && Array.isArray(j.videorecords)],
      ['/api/v3/settings/calls', 200, (j) => j.payload && j.payload.codecs],
      ['/api/v3/settings/calls/sip', 200, (j) => j.payload !== undefined],
      ['/api/v3/settings/http', 200, (j) => j.port !== undefined],
      ['/api/v3/system/time', 200, (j) => typeof j.time !== 'undefined' && j.timezone],
      ['/api/v3/system/ntp', 200, (j) => typeof j.ntp !== 'undefined'],
      ['/api/v3/settings/calls', 200, (j) => j.payload && j.payload.stop_rules !== undefined],
    ];
    summary.acceptance.api_parity = true;
    const apiParityDetails = {};
    for (const [url, expected, check] of endpoints) {
      const response = await api(url);
      let ok = response.status === expected;
      if (ok && response.json) { try { ok = Boolean(check(response.json)); } catch { ok = false; } }
      apiParityDetails[url] = { status: response.status, ok };
      if (!ok) summary.acceptance.api_parity = false;
    }

    // ---- обход страниц ----
    const pages = [
      ['root', '/pnm/', ['Панель управления']],
      ['conferences', '/pnm/conferences', ['Конференции']],
      ['directory', '/pnm/addresses', ['Справочник']],
      ['history', '/pnm/history', ['История']],
      ['statistics', '/pnm/administration/statistics', ['Статистика', 'Использование памяти']],
      ['mediafiles', '/pnm/administration/mediafiles', ['Медиафайлы']],
      ['power', '/pnm/administration/power', ['Питание']],
      ['diagnostics', '/pnm/administration/diagnostics', ['Диагностика']],
      ['terminals', '/pnm/administration/terminals', ['Терминалы']],
      ['conference_settings', '/pnm/administration/conference_settings', ['Настройки конференции']],
      ['calls', '/pnm/administration/calls', ['Вызовы']],
      ['network', '/pnm/administration/network', ['Сеть']],
      ['video', '/pnm/administration/video', ['Видео']],
      ['calibration', '/pnm/administration/calibration', ['Статистика']],
      ['backup', '/pnm/administration/backup', ['Импорт/Экспорт']],
      ['reset', '/pnm/administration/reset', ['Очистка данных']],
      ['date_time', '/pnm/administration/date_time', ['Дата и время']],
      ['roles', '/pnm/administration/roles', ['Роли']],
      ['security', '/pnm/administration/security', ['Безопасность']],
      ['event_list', '/pnm/administration/event_list', ['Журнал событий']],
      ['layout_skins', '/pnm/administration/layout_skins', ['Стили раскладок']],
      ['license', '/pnm/administration/license', ['Лицензия']],
      ['about', '/pnm/administration/about', ['О программе']],
    ];
    summary.acceptance.page_walk = true;
    for (const [slug, url, labels] of pages) {
      const beforeHttp = httpErrors.length;
      const beforeConsole = consoleErrors.length;
      await page.goto(BASE + url, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(2500);
      const text = await page.locator('body').innerText();
      const missing = labels.filter((label) => !text.includes(label));
      const record = {
        slug, url, missing,
        http: httpErrors.slice(beforeHttp),
        console: consoleErrors.slice(beforeConsole),
      };
      if (missing.length || record.http.length || record.console.length) {
        summary.acceptance.page_walk = false;
        summary.acceptance[`page_problem:${slug}`] = record;
      }
      if (['root', 'conferences', 'directory', 'history', 'statistics', 'calls'].includes(slug)) {
        await shot(`${String(summary.pages.length + 1).padStart(2, '0')}-${slug}.png`);
      }
    }

    // ---- «Калибровка» убрана из UX: deep-link уводит на статистику, API null ----
    const calibration = await api('/api/v3/engine/calibration');
    summary.acceptance.calibration_hidden =
      calibration.status === 200 && calibration.json && calibration.json.calibration === null;

    summary.acceptance.clean_build = build.git_dirty === false && build.git_head.length === 40 && build.binary_sha256 !== null;
    summary.acceptance.binary_built_after_head =
      build.binary_mtime !== null && build.git_head_ct > 0 && build.binary_mtime >= build.git_head_ct;

    const failed = Object.entries(summary.acceptance).filter(([, value]) => value !== true).map(([key]) => key);
    summary.totals = {
      api_parity: apiParityDetails,
      acceptance: Object.keys(summary.acceptance).length,
      failed: failed.length,
      pages: pages.length,
      screenshots: summary.pages.length,
      http_errors: httpErrors.length,
      console_errors: consoleErrors.length,
      vendor_console_errors: vendorErrors.length,
    };
    summary.ok = failed.length === 0 && httpErrors.length === 0 && consoleErrors.length === 0 &&
      summary.acceptance.clean_build && summary.acceptance.binary_built_after_head;
    if (!summary.ok) summary.error = `провалены гейты: ${failed.join(', ') || '-'}; http=${httpErrors.length}; console=${consoleErrors.length}`;
  } catch (error) {
    summary.ok = false;
    summary.error = String(error && error.stack ? error.stack : error);
  } finally {
    summary.http_errors = httpErrors;
    summary.console_errors = consoleErrors;
    summary.vendor_console_errors = vendorErrors;
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    console.log(JSON.stringify({ ok: summary.ok, error: summary.error, acceptance: summary.acceptance, totals: summary.totals }, null, 1));
    await browser.close();
  }
  process.exit(summary.ok ? 0 : 1);
})();

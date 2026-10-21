#!/usr/bin/env node
// T2.3 acceptance evidence: административные настройки PNM (SIP/H.323/RTP,
// HTTP/TLS multipart, SMTP test/reset, video styles/security, NTP/время/зона)
// через vendor-UI 2.2.1.66 на нашем `/api/v3`.
//
// Запуск:
//   PLAYWRIGHT_MODULE=/path/to/playwright \
//   BASE_URL=http://127.0.0.1:8080 \
//   node scripts/evidence/t23-evidence.js
//
// Любой провал acceptance-гейта → `ok:false` и exit 1.

const crypto = require('crypto');
const fs = require('fs');
const net = require('net');
const path = require('path');
const { execSync } = require('child_process');
const OUT_PRELIGHT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t23';
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
const OUT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t23';
const ADMIN_LOGIN = process.env.ADMIN_LOGIN || 'admin';
const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD || 'admin_password';
fs.mkdirSync(OUT, { recursive: true });

const sh = (cmd) => { try { return execSync(cmd, { encoding: 'utf8' }).trim(); } catch { return ''; } };
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

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
const steps = [];
const screenshots = [];
const httpErrors = [];
const consoleErrors = [];
const vendorErrors = [];
let token = null;

const summary = {
  task: 'T2.3',
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
      httpErrors.push(`${response.status()} ${response.request().method()} ${response.url().replace(BASE, '')}`);
    }
  });
  page.on('console', (message) => {
    if (message.type() !== 'error') return;
    const text = message.text();
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
    await page.screenshot({ path: path.join(OUT, name), fullPage: true });
    screenshots.push(name);
  };
  const api = async (pathname, options = {}) => {
    const headers = { Authorization: `Bearer ${token}` };
    const request = { method: options.method || 'GET', headers, failOnStatusCode: false };
    if (options.data !== undefined) {
      headers['Content-Type'] = 'application/json';
      request.data = options.data;
    }
    if (options.multipart) {
      request.multipart = options.multipart;
      delete headers['Content-Type'];
    }
    const response = await page.request.fetch(BASE + pathname, request);
    const text = await response.text();
    let json = null;
    try { json = JSON.parse(text); } catch { /* не-JSON */ }
    return { status: response.status(), json, text };
  };
  const payloadOf = async (pathname) => {
    const response = await api(pathname);
    return { status: response.status, payload: response.json && (response.json.payload || response.json) };
  };

  try {
    // ---------- login через vendor-UI ----------
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

    // ---------- 1. vendor-контракт GET ----------
    await record('settings-get-contracts', async () => {
      const expected = {
        '/api/v3/settings/calls': ['fecc', 'aspeaker', 'limits'],
        '/api/v3/settings/calls/sip': ['sip_port', 'srtp', 'bfcp'],
        '/api/v3/settings/calls/h323': ['h323_id', 'h235', 'h245_port_min'],
        '/api/v3/settings/rtp': ['mtu', 'rtp_port_min', 'dscp_audio'],
        '/api/v3/settings/styles': ['video_background', 'font', 'border'],
        '/api/v3/settings/security': ['password_min_length', 'password_ttl', 'login_block_timeout'],
        '/api/v3/settings/smtp': ['smtp_server', 'smtp_port', 'smtp_secure'],
      };
      summary.acceptance.settings_get_contracts = true;
      for (const [pathname, keys] of Object.entries(expected)) {
        const { status, payload } = await payloadOf(pathname);
        const missing = keys.filter((key) => !payload || payload[key] === undefined);
        if (status !== 200 || missing.length) {
          summary.acceptance.settings_get_contracts = false;
          summary.acceptance[`missing:${pathname}`] = missing;
        }
      }
      const http = await api('/api/v3/settings/http');
      summary.acceptance.http_contract = http.status === 200 && typeof http.json.port === 'number';
    });

    // ---------- 2. PATCH SIP/H.323/RTP и строгая валидация ----------
    await record('settings-patch-persist', async () => {
      const originalSip = (await payloadOf('/api/v3/settings/calls/sip')).payload;
      const patched = await api('/api/v3/settings/calls/sip', {
        method: 'PATCH',
        data: { payload: { sip_port: 5070, srtp: 'ENABLED' } },
      });
      const after = (await payloadOf('/api/v3/settings/calls/sip')).payload;
      summary.acceptance.sip_patch_persist =
        patched.status === 200 && after.sip_port === 5070 && after.srtp === 'ENABLED' && after.sip_server === '';
      const unknown = await api('/api/v3/settings/calls/sip', {
        method: 'PATCH',
        data: { payload: { sip_unknown_field: 1 } },
      });
      summary.acceptance.settings_strict_schema = unknown.status === 422;
      const restore = await api('/api/v3/settings/calls/sip', { method: 'PATCH', data: { payload: originalSip } });
      summary.acceptance.sip_restored = restore.status === 200;

      const originalRtp = (await payloadOf('/api/v3/settings/rtp')).payload;
      const rtp = await api('/api/v3/settings/rtp', { method: 'PATCH', data: { payload: { mtu: 1400 } } });
      const rtpAfter = (await payloadOf('/api/v3/settings/rtp')).payload;
      summary.acceptance.rtp_patch_persist = rtp.status === 200 && rtpAfter.mtu === 1400;
      await api('/api/v3/settings/rtp', { method: 'PATCH', data: { payload: originalRtp } });

      const h323Before = (await payloadOf('/api/v3/settings/calls/h323')).payload;
      const h323 = await api('/api/v3/settings/calls/h323', {
        method: 'PATCH',
        data: { payload: { ...h323Before, h323_id: `mcu-${stamp}`, h323_e164: '1001' } },
      });
      const h323After = (await payloadOf('/api/v3/settings/calls/h323')).payload;
      const h323Restore = await api('/api/v3/settings/calls/h323', { method: 'PATCH', data: { payload: h323Before } });
      const h323Restored = (await payloadOf('/api/v3/settings/calls/h323')).payload;
      summary.acceptance.h323_patch_persist =
        h323.status === 200 && h323After.h323_id === `mcu-${stamp}` && h323After.h323_e164 === '1001';
      summary.acceptance.h323_restored =
        h323Restore.status === 200 && h323Restored.h323_id === h323Before.h323_id && h323Restored.h323_e164 === h323Before.h323_e164;
    });

    // ---------- 3. calls: vendor-поля + legacy media ----------
    await record('calls-vendor-and-legacy', async () => {
      const calls = (await payloadOf('/api/v3/settings/calls')).payload;
      summary.acceptance.calls_vendor_payload = calls.fecc !== undefined && calls.aspeaker !== undefined && calls.limits !== undefined;
      const patched = await api('/api/v3/settings/calls', {
        method: 'PATCH',
        data: { payload: { limits: { max_participants_per_room: 50 } } },
      });
      const after = (await payloadOf('/api/v3/settings/calls')).payload;
      summary.acceptance.calls_patch_persist =
        patched.status === 200 && after.limits && after.limits.max_participants_per_room === 50 && after.fecc !== undefined;
      await api('/api/v3/settings/calls', { method: 'PATCH', data: { payload: calls } });
    });

    // ---------- 4. HTTP/TLS multipart ----------
    await record('http-tls-multipart', async () => {
      const upload = await api('/api/v3/settings/http', {
        method: 'PATCH',
        multipart: {
          port: '8443',
          crt: { name: 'mcu.crt', mimeType: 'application/x-pem-file', buffer: Buffer.from('-----BEGIN CERTIFICATE-----\nT23\n-----END CERTIFICATE-----\n') },
          key: { name: 'mcu.key', mimeType: 'application/x-pem-file', buffer: Buffer.from('-----BEGIN PRIVATE KEY-----\nT23\n-----END PRIVATE KEY-----\n') },
        },
      });
      const after = (await api('/api/v3/settings/http')).json;
      summary.acceptance.http_multipart_upload =
        upload.status === 200 && after.port === 8443 && after.crt && after.crt.common_name === 'mcu.crt' && !!after.key;
      // Vendor-UI вызывает check на КАЖДЫЙ файл отдельно (второй слот — из
      // уже загруженного состояния), поэтому гейт повторяет именно одиночные
      // запросы, а не удобную пару.
      const checkCrt = await api('/api/v3/settings/http/check', {
        method: 'POST',
        multipart: {
          crt: { name: 'mcu.crt', mimeType: 'application/x-pem-file', buffer: Buffer.from('-----BEGIN CERTIFICATE-----\nT23\n-----END CERTIFICATE-----\n') },
        },
      });
      const checkKey = await api('/api/v3/settings/http/check', {
        method: 'POST',
        multipart: {
          key: { name: 'mcu.key', mimeType: 'application/x-pem-file', buffer: Buffer.from('-----BEGIN PRIVATE KEY-----\nT23\n-----END PRIVATE KEY-----\n') },
        },
      });
      const garbage = await api('/api/v3/settings/http/check', {
        method: 'POST',
        multipart: {
          crt: { name: 'bad.crt', mimeType: 'application/x-pem-file', buffer: Buffer.from('not a certificate\n') },
        },
      });
      summary.acceptance.http_check_single_file =
        checkCrt.status === 200 && checkKey.status === 200 && garbage.status === 400;
      const reset = await api('/api/v3/settings/http/reset', { method: 'POST' });
      const resetAfter = (await api('/api/v3/settings/http')).json;
      summary.acceptance.http_reset = reset.status === 200 && resetAfter.port === 443;
    });

    // ---------- 5. SMTP: маска, test, reset с живым TCP ----------
    const smtpServer = net.createServer((socket) => { socket.end('220 t23 local\r\n'); });
    await new Promise((resolve) => smtpServer.listen(2525, '127.0.0.1', resolve));
    try {
      await record('smtp-test-and-reset', async () => {
        await api('/api/v3/smtp/reset', { method: 'POST' });
        const empty = await api('/api/v3/smtp/test', { method: 'PUT', data: {} });
        summary.acceptance.smtp_empty_server_422 = empty.status === 422;
        const patched = await api('/api/v3/settings/smtp', {
          method: 'PATCH',
          data: { payload: { smtp_server: '127.0.0.1', smtp_port: 2525, smtp_password: 't23-secret' } },
        });
        const masked = (await payloadOf('/api/v3/settings/smtp')).payload;
        summary.acceptance.smtp_patch_masked =
          patched.status === 200 && masked.smtp_server === '127.0.0.1' && !String(masked.smtp_password || '').includes('t23-secret');
        const live = await api('/api/v3/smtp/test', { method: 'PUT', data: {} });
        summary.acceptance.smtp_live_test = live.status === 200 && live.json && live.json.ok === true;
        const reset = await api('/api/v3/smtp/reset', { method: 'POST' });
        const resetAfter = (await payloadOf('/api/v3/settings/smtp')).payload;
        summary.acceptance.smtp_reset = reset.status === 200 && resetAfter.smtp_server === '' && resetAfter.smtp_port === 465;
      });
    } finally {
      await new Promise((resolve) => smtpServer.close(resolve));
    }

    // ---------- 6. NTP / время / таймзона ----------
    await record('ntp-time-timezone', async () => {
      const ntp = await api('/api/v3/system/ntp');
      const enable = await api('/api/v3/system/ntp/enable', { method: 'POST' });
      const timeEnabled = await api('/api/v3/system/time');
      const disable = await api('/api/v3/system/ntp/disable', { method: 'POST' });
      const timeDisabled = await api('/api/v3/system/time');
      summary.acceptance.ntp_toggle =
        ntp.status === 200 && enable.status === 200 && timeEnabled.json.time.ntp === true && disable.status === 200 && timeDisabled.json.time.ntp === false;
      // ntp_synchronized отражает реальный статус ОС, а не флаг "NTP включён".
      const hostSync = sh('timedatectl show -p NTPSynchronized --value');
      const expectedSync = hostSync === 'yes';
      summary.acceptance.ntp_status_not_faked = timeEnabled.json.time.ntp_synchronized === expectedSync;
      summary.acceptance.ntp_status_source = hostSync || 'unavailable';

      // Vendor-UI: PATCH /system/time присылает локальную строку.
      const uiTime = '22.10.2026 04:45:00';
      const timePatch = await api('/api/v3/system/time', { method: 'PATCH', data: { time: uiTime } });
      const expectedTime = Math.floor(new Date('2026-10-22T04:45:00+03:00').getTime() / 1000);
      summary.acceptance.system_time_patch_vendor_string =
        timePatch.status === 200 && Math.abs(timePatch.json.time.time - expectedTime) <= 120;
      await api('/api/v3/system/time', { method: 'PATCH', data: { payload: { seconds: 0 } } });

      const now = Math.floor(Date.now() / 1000);
      const timeNumber = await api('/api/v3/system/time', { method: 'PATCH', data: { time: now } });
      summary.acceptance.system_time_patch =
        timeNumber.status === 200 && Math.abs(timeNumber.json.time.time - now) <= 5 && timeNumber.json.time.timezone === 'Europe/Moscow';

      const zone = await api('/api/v3/system/timezone', { method: 'PATCH', data: { timezone: 'UTC' } });
      const profileUtc = await api('/api/v3/profile');
      const invalid = await api('/api/v3/system/timezone', { method: 'PATCH', data: { timezone: 'Mars/Olympus' } });
      const restore = await api('/api/v3/system/timezone', { method: 'PATCH', data: { timezone: 'Europe/Moscow' } });
      const profileMsk = await api('/api/v3/profile');
      summary.acceptance.timezone_patch_profile =
        zone.status === 200 && profileUtc.json.user.time_zone_id === 'UTC' && invalid.status === 422 &&
        restore.status === 200 && profileMsk.json.user.time_zone_id === 'Europe/Moscow';
      summary.acceptance.time_zones_list = (await api('/api/v3/time-zones')).json.count === 428;
    });

    // ---------- 6b. UI: NTP-конфиг реально сохраняется через форму ----------
    await record('settings-ui-ntp-save', async () => {
      const mutations = [];
      const listener = (response) => {
        const url = response.url();
        if (url.includes('/api/v3/') && ['PATCH', 'POST'].includes(response.request().method())) {
          mutations.push({ path: url.replace(BASE, ''), status: response.status(), body: response.request().postData() });
        }
      };
      page.on('response', listener);
      await api('/api/v3/system/ntp/disable', { method: 'POST' });
      await page.goto(`${BASE}/pnm/administration/date_time`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      const switchOn = await page.evaluate(() => {
        const el = [...document.querySelectorAll('input')].find((input) => String(input.id).includes('tc-switch'));
        return el ? el.checked : null;
      });
      if (!switchOn) {
        await page.locator('label.tc-switch__text').first().click();
        await page.waitForTimeout(1200);
      }
      await page.locator('[title="Частота синхронизации"]').click();
      await page.waitForTimeout(600);
      const freq = page.locator('.tc-context-menu-item[title="1 час"]');
      if (await freq.count()) await freq.first().click({ force: true });
      await page.waitForTimeout(600);
      const textInputs = page.locator('.tc-text-field__input:visible');
      const count = await textInputs.count();
      const primary = textInputs.nth(count - 3);
      await primary.click();
      await page.keyboard.type(`pool.${stamp}.ntp.org`, { delay: 25 });
      await page.waitForTimeout(400);
      await page.getByRole('button', { name: 'Сохранить', exact: true }).first().click();
      await page.waitForTimeout(3000);
      page.off('response', listener);
      const patch = mutations.find((m) => m.path === '/api/v3/system/ntp' && m.status === 200);
      const server = (await api('/api/v3/system/ntp')).json.ntp.server;
      summary.acceptance.ntp_ui_save =
        !!patch && String(patch.body || '').includes('"ntp":"pool.') && server === `pool.${stamp}.ntp.org`;
      summary.acceptance.ntp_ui_restarted_services =
        mutations.some((m) => m.path === '/api/v3/system/ntp/enable' && m.status === 200);
      await shot('02-administration-date-time.png');
      // Cleanup: выключить NTP и очистить сервер.
      await api('/api/v3/system/ntp', { method: 'PATCH', data: { payload: { server: null, fallback: [], poll_interval_min: null, poll_interval_max: null } } });
      await api('/api/v3/system/ntp/disable', { method: 'POST' });
    });

    // ---------- 6c. UI: conference_settings one_abonent включается и выключается ----------
    await record('settings-ui-conference-one-abonent', async () => {
      const statuses = [];
      const listener = (response) => {
        if (response.url().includes('/api/v3/settings/calls') && response.request().method() === 'PATCH') statuses.push(response.status());
      };
      page.on('response', listener);
      const saveOne = async (enable) => {
        await page.goto(`${BASE}/pnm/administration/conference_settings`, { waitUntil: 'domcontentloaded' });
        await page.waitForTimeout(2500);
        const boxes = page.locator('input[type=checkbox]');
        if ((await boxes.count()) < 3) return null;
        const box = boxes.nth(2);
        const checked = await box.isChecked();
        if (enable !== checked) await box.click({ force: true });
        await page.getByRole('button', { name: 'Сохранить', exact: true }).first().click();
        await page.waitForTimeout(2500);
        await page.goto(`${BASE}/pnm/administration/conference_settings`, { waitUntil: 'domcontentloaded' });
        await page.waitForTimeout(2000);
        const reloaded = page.locator('input[type=checkbox]');
        return (await reloaded.count()) >= 3 ? reloaded.nth(2).isChecked() : null;
      };
      const enabled = await saveOne(true);
      const disabled = await saveOne(false);
      page.off('response', listener);
      summary.acceptance.conference_settings_ui_one_abonent =
        enabled === true && disabled === false && statuses.length >= 2 && statuses.every((status) => status === 200);
      summary.acceptance.conference_settings_ui_patch_statuses = statuses;
      await shot('03-administration-conference-settings.png');
    });

    // ---------- 6d. SIP save → restart_services 200 (раньше был 501) ----------
    await record('sip-save-restart-services', async () => {
      const original = (await payloadOf('/api/v3/settings/calls/sip')).payload;
      const sip = await api('/api/v3/settings/calls/sip', { method: 'PATCH', data: { payload: { ...original, sip_port: 5062 } } });
      const restart = await api('/api/v3/system/restart_services', { method: 'POST' });
      const restore = await api('/api/v3/settings/calls/sip', { method: 'PATCH', data: { payload: original } });
      summary.acceptance.sip_save_restart_services =
        sip.status === 200 && restart.status === 200 && restart.json.status === 'ok' && restore.status === 200;
    });

    // ---------- 7. UI: страницы настроек грузятся без ошибок ----------
    await record('settings-ui-pages', async () => {
      const pages = [
        ['calls', ['Вызовы', 'SIP/H.323', 'Параметры SIP']],
        ['video', ['Видео', 'Размер хранилища видео']],
        ['date_time', ['Дата и время', 'Часовой пояс', 'Синхронизировать время с NTP-сервером']],
        ['layout_skins', ['Стили раскладок', 'Стиль раскладки']],
        ['conference_settings', ['Настройки конференции']],
        ['security', ['Безопасность', 'Пароли', 'Авторизация']],
        ['network', ['Сеть', 'Порты', 'SMTP', 'HTTPS']],
      ];
      summary.acceptance.settings_ui_pages = true;
      for (const [slug, labels] of pages) {
        await page.goto(`${BASE}/pnm/administration/${slug}`, { waitUntil: 'domcontentloaded' });
        await page.waitForTimeout(3000);
        const text = await page.locator('body').innerText();
        const missing = labels.filter((label) => !text.includes(label));
        if (missing.length || page.url().endsWith('/monitoring/sessions')) {
          summary.acceptance.settings_ui_pages = false;
          summary.acceptance[`ui_missing:${slug}`] = missing;
        }
      }
    });

    // ---------- 8. UI save → PATCH /settings/calls → API отражает ----------
    await record('settings-ui-save', async () => {
      const before = (await payloadOf('/api/v3/settings/calls')).payload;
      const saved = [];
      const listener = (response) => {
        if (response.url().includes('/api/v3/settings/calls') && response.request().method() === 'PATCH') saved.push(response.status());
      };
      page.on('response', listener);
      await page.goto(`${BASE}/pnm/administration/calls`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      const checkbox = page.locator('input[type=checkbox]').first();
      if (await checkbox.count()) {
        await checkbox.click({ force: true });
        await page.waitForTimeout(500);
      }
      const save = page.getByRole('button', { name: 'Сохранить', exact: true }).first();
      await save.click();
      await page.waitForTimeout(3000);
      page.off('response', listener);
      const after = (await payloadOf('/api/v3/settings/calls')).payload;
      const changed = JSON.stringify(before) !== JSON.stringify(after);
      summary.acceptance.settings_ui_save =
        saved.some((status) => status === 200) && changed && after.limits !== undefined;
      await api('/api/v3/settings/calls', { method: 'PATCH', data: { payload: before } });
      const restored = (await payloadOf('/api/v3/settings/calls')).payload;
      summary.acceptance.settings_ui_save_restored = JSON.stringify(restored) === JSON.stringify(before);
      await page.reload({ waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(2500);
      await shot('01-administration-calls.png');
    });

    // ---------- 9. UI: дата/время и безопасность (PNG) ----------
    await record('settings-ui-screenshots', async () => {
      await page.goto(`${BASE}/pnm/administration/date_time`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      const text = await page.locator('body').innerText();
      summary.acceptance.ui_time_shows_ntp = text.includes('Синхронизировать время с NTP-сервером');
      await page.goto(`${BASE}/pnm/administration/security`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      const security = await page.locator('body').innerText();
      summary.acceptance.ui_security_form =
        security.includes('Минимальное количество символов') && security.includes('Блокировка учетной записи');
      await shot('03-administration-security.png');
    });

    // ---------- итог ----------
    const failed = Object.entries(summary.acceptance).filter(([, value]) => value !== true).map(([key]) => key);
    summary.totals = {
      steps: steps.length,
      acceptance: Object.keys(summary.acceptance).length,
      failed: failed.length,
      screenshots: screenshots.length,
      http_errors: [...new Set(httpErrors)].length,
      console_errors: [...new Set(consoleErrors)].length,
      vendor_console_errors: vendorErrors.length,
    };
    summary.steps = steps;
    summary.screenshots = screenshots;
    summary.http_errors = [...new Set(httpErrors)];
    summary.console_errors = [...new Set(consoleErrors)];
    summary.finished_at = new Date().toISOString();
    summary.ok = failed.length === 0 && summary.totals.http_errors === 0 && summary.totals.console_errors === 0 &&
      !build.git_dirty && build.binary_mtime >= build.git_head_ct;
    if (!summary.ok) summary.error = `провалены гейты: ${failed.join(', ') || '-'}; httpErrors=${summary.totals.http_errors} consoleErrors=${summary.totals.console_errors}; gitDirty=${build.git_dirty}; binaryBuiltAfterHead=${build.binary_mtime >= build.git_head_ct}`;
  } catch (error) {
    summary.ok = false;
    summary.error = String(error && error.stack ? error.stack : error);
  } finally {
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    console.log(JSON.stringify({ ok: summary.ok, error: summary.error, acceptance: summary.acceptance, totals: summary.totals }, null, 1));
    await browser.close();
  }
  process.exit(summary.ok ? 0 : 1);
})();

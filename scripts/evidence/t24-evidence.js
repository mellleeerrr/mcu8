#!/usr/bin/env node
// T2.4 acceptance evidence: журнал событий, сеть/диагностика, статистика,
// терминалы и медиафайлы PNM через vendor-UI 2.2.1.66 на нашем `/api/v3`.
//
// Запуск:
//   PLAYWRIGHT_MODULE=/path/to/playwright \
//   BASE_URL=http://127.0.0.1:8080 \
//   node scripts/evidence/t24-evidence.js
//
// Любой провал acceptance-гейта → `ok:false` и exit 1.

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const OUT_PRELIGHT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t24';
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
const OUT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t24';
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
for (const pid of sh('pgrep -f "target/release/vks-server"').split('\n').filter(Boolean)) {
  try {
    const exe = `/proc/${pid}/exe`;
    build.binary_sha256 = crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex');
    build.binary_mtime = Math.floor(fs.statSync(exe).mtimeMs / 1000);
    build.binary_path = exe;
    build.server_pid = Number(pid);
    break;
  } catch { /* следующий процесс */ }
}

function processStartUnix(pid) {
  try {
    const stat = fs.readFileSync(`/proc/${pid}/stat`, 'utf8');
    const rest = stat.slice(stat.lastIndexOf(')') + 2).split(/\s+/);
    const startTicks = Number(rest[19]);
    const btime = Number(
      fs.readFileSync('/proc/stat', 'utf8').split('\n').find((line) => line.startsWith('btime ')).split(/\s+/)[1],
    );
    return btime + Math.floor(startTicks / 100);
  } catch { return null; }
}
const expectedProcessStart = build.server_pid ? processStartUnix(build.server_pid) : null;

const stamp = String(Date.now()).slice(-6);
const steps = [];
const screenshots = [];
const httpErrors = [];
const consoleErrors = [];
const vendorErrors = [];
let token = null;

const summary = {
  task: 'T2.4',
  ok: false,
  started_at: new Date().toISOString(),
  build,
  acceptance: {},
  totals: {},
  error: null,
};

// Chart.js 2.x в vendor-бандле ругается на конфиг осей — это баг вендора,
// а не наш ответ API: отдельный список, чтобы не прятать настоящие ошибки.
const isVendorError = (text) =>
  /Invalid scale configuration for scale: (yAxes|xAxes|responsive|maintainAspectRatio)/.test(text) ||
  /validationHandler.*catch is not a function/.test(text);

(async () => {
  const browser = await chromium.launch({
    args: ['--no-sandbox', '--use-fake-ui-for-media-stream', '--use-fake-device-for-media-stream', '--ignore-certificate-errors'],
  });
  const context = await browser.newContext({ ignoreHTTPSErrors: true, viewport: { width: 1600, height: 1000 } });
  const page = await context.newPage();
  let networkStatusSeen = false;
  let networkInterfacesSeen = false;
  page.on('response', (response) => {
    if (response.url().includes('/api/v3/system/status/network')) networkStatusSeen = true;
    if (response.url().includes('/api/v3/system/network/interfaces')) networkInterfacesSeen = true;
    if (response.status() >= 400 && response.url().includes('/api/v3/')) {
      httpErrors.push(`${response.status()} ${response.request().method()} ${response.url().replace(BASE, '')}`);
    }
  });
  page.on('console', (message) => {
    if (message.type() !== 'error') return;
    const text = message.text();
    if (isVendorError(text)) vendorErrors.push(text);
    else consoleErrors.push(text);
  });
  page.on('pageerror', (error) => consoleErrors.push(String(error)));

  const record = async (name, fn) => {
    const value = await fn();
    steps.push(name);
    summary.totals[name] = value;
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
    const response = await page.request.fetch(BASE + pathname, request);
    const buffer = await response.body();
    const text = buffer.toString('utf8');
    let json = null;
    try { json = JSON.parse(text); } catch { /* не-JSON */ }
    return { status: response.status(), json, text, buffer, headers: response.headers() };
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

    // ---------- 1. log_events: список/фильтры/поиск/даты ----------
    await record('log-events-list', async () => {
      const all = await api('/api/v3/log_events');
      const events = (all.json && all.json.log_events) || [];
      summary.acceptance.log_events_contract =
        all.status === 200 && typeof all.json.count === 'number' && all.json.count >= events.length && events.length > 0 &&
        events.every((event) => typeof event.id === 'number' && event.event_type && event.object_type && event.created_at !== undefined);
      const page = await api('/api/v3/log_events?page=2&page_size=1');
      summary.acceptance.log_events_pagination =
        page.status === 200 && page.json.count === all.json.count && page.json.log_events.length === 1;

      const filtered = await api('/api/v3/log_events?event_types=login_success&search=' + encodeURIComponent(ADMIN_LOGIN));
      const filteredEvents = (filtered.json && filtered.json.log_events) || [];
      summary.acceptance.log_events_filter_search =
        filtered.status === 200 && filteredEvents.length > 0 &&
        filteredEvents.every((event) => event.event_type === 'login_success');
      const from = Math.floor(Date.now() / 1000) - 3600;
      const dated = await api(`/api/v3/log_events?date_from=${from}`);
      summary.acceptance.log_events_date_filter = dated.status === 200 && dated.json.count >= 1;
      fs.writeFileSync(path.join(OUT, 'log_events.json'), JSON.stringify(all.json, null, 1) + '\n');
      fs.writeFileSync(path.join(OUT, 'log_events_filtered.json'), JSON.stringify(filtered.json, null, 1) + '\n');
    });

    // ---------- 2. log_events: ZIP-экспорт и статическая выдача ----------
    await record('log-events-export', async () => {
      const exported = await api('/api/v3/log_events/export');
      const filename = exported.json && exported.json.filename;
      const download = await api(`/static/log_events/${filename}`);
      const csvOk = download.text.includes('ID;');
      const zipOk = download.status === 200 && download.buffer.length > 0 &&
        download.buffer[0] === 0x50 && download.buffer[1] === 0x4b &&
        download.text.includes('PK\u0005\u0006');
      // Реальный unzip: CRC/central directory, а не только магия PK.
      const tmp = path.join(OUT, 'export.zip');
      fs.writeFileSync(tmp, download.buffer);
      let unzipOk = true;
      try { execSync(`unzip -t ${tmp} >/dev/null 2>&1`); } catch { unzipOk = false; }
      // Время CSV — локальное (как в UI), пользователь — из invoker, не пустой.
      const timeMeta = await api('/api/v3/system/time');
      const timezone = timeMeta.json.timezone || 'Europe/Moscow';
      const first = ((await api('/api/v3/log_events')).json.log_events || [])[0];
      const expectedTime = new Intl.DateTimeFormat('ru-RU', {
        timeZone: timezone, day: '2-digit', month: '2-digit', year: 'numeric',
        hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false,
      }).format(new Date(first.created_at * 1000)).replace(',', '');
      const invoker = first && first.invoker && first.invoker.name;
      const missing = await api('/static/log_events/nope.zip');
      const traversal = await api('/static/log_events/..%2Fsecret.zip');
      summary.acceptance.log_events_export_zip =
        exported.status === 200 && typeof filename === 'string' && filename.endsWith('.zip') && zipOk && csvOk && unzipOk;
      summary.acceptance.log_events_export_local_time_invoker =
        download.text.includes(expectedTime) && !!invoker && download.text.includes(String(invoker));
      summary.acceptance.log_events_static_404 = missing.status === 404 && [400, 404].includes(traversal.status);
      fs.writeFileSync(path.join(OUT, 'log_events_export.json'), JSON.stringify({ ...exported.json, bytes: download.buffer.length }, null, 1) + '\n');
    });

    // ---------- 3. сетевые интерфейсы ----------
    await record('network-interfaces', async () => {
      const list = await api('/api/v3/system/network/interfaces');
      const interfaces = (list.json && list.json.interfaces) || [];
      summary.acceptance.network_interfaces_list =
        list.status === 200 && list.json.count === interfaces.length && interfaces.length >= 1 &&
        interfaces.every((item) => typeof item.interface === 'string' && typeof item.ready === 'boolean' && Array.isArray(item.addresses));
      const first = interfaces[0] && interfaces[0].interface;
      const detail = await api(`/api/v3/system/network/interfaces/${encodeURIComponent(first)}`);
      summary.acceptance.network_interface_detail =
        detail.status === 200 && detail.json.interface && detail.json.interface.ready === true &&
        Array.isArray(detail.json.interface.addresses) && !!detail.json.interface.mac;
      const patch = await api(`/api/v3/system/network/interfaces/${encodeURIComponent(first)}`, {
        method: 'PATCH',
        data: { interface: { use_dhcp: true } },
      });
      summary.acceptance.network_interface_patch_501 = patch.status === 501;
      fs.writeFileSync(path.join(OUT, 'network_interfaces.json'), JSON.stringify(list.json, null, 1) + '\n');
      fs.writeFileSync(path.join(OUT, 'network_interface.json'), JSON.stringify(detail.json, null, 1) + '\n');
    });

    // ---------- 4. диагностика: ping / tcpdumps ----------
    await record('diagnostics-ping', async () => {
      const ping = await api('/api/v3/system/utils/ping?address=127.0.0.1');
      const invalid = await api('/api/v3/system/utils/ping?address=' + encodeURIComponent('bad host; rm -rf /'));
      summary.acceptance.utils_ping =
        ping.status === 200 &&
        ((Array.isArray(ping.json.ping) && ping.json.ping.length >= 1) || String(ping.json.stdout || '').includes('PING')) &&
        invalid.status === 422;
    });

    await record('diagnostics-tcpdumps', async () => {
      const status = await api('/api/v3/system/utils/tcpdumps/status');
      const files = await api('/api/v3/system/utils/tcpdumps');
      const start = await api('/api/v3/system/utils/tcpdumps', { method: 'POST', data: { interface: 'lo' } });
      const stop = await api('/api/v3/system/utils/tcpdumps', { method: 'DELETE' });
      summary.acceptance.tcpdumps_status = status.status === 200 && status.json.running === false;
      summary.acceptance.tcpdumps_list = files.status === 200 && Array.isArray(files.json.files);
      summary.acceptance.tcpdumps_mutations_501 = start.status === 501 && stop.status === 501;
      fs.writeFileSync(path.join(OUT, 'tcpdumps_status.json'), JSON.stringify(status.json, null, 1) + '\n');
      fs.writeFileSync(path.join(OUT, 'tcpdumps.json'), JSON.stringify(files.json, null, 1) + '\n');
    });

    // ---------- 5. терминалы ----------
    await record('terminals', async () => {
      const list = await api('/api/v3/terminals');
      const alias = await api('/api/v3/ui/terminals');
      const post = await api('/api/v3/terminals', { method: 'POST', data: { address: '192.168.1.50' } });
      const credentials = await api('/api/v3/terminals/credentials?terminal=1');
      summary.acceptance.terminals_list =
        list.status === 200 && list.json.count === 0 && Array.isArray(list.json.terminals) &&
        alias.status === 200 && alias.json.count === 0;
      summary.acceptance.terminals_mutations_501 = post.status === 501 && credentials.status === 404;
      fs.writeFileSync(path.join(OUT, 'terminals.json'), JSON.stringify(list.json, null, 1) + '\n');
    });

    // ---------- 6. видеозаписи ----------
    await record('videorecords', async () => {
      const list = await api('/api/v3/videorecords');
      const estimated = await api('/api/v3/conferences/1/videorecords/estimated_time');
      const remove = await api('/api/v3/videorecords/1', { method: 'DELETE' });
      summary.acceptance.videorecords_list = list.status === 200 && list.json.count === 0 && Array.isArray(list.json.videorecords);
      summary.acceptance.videorecords_estimated =
        estimated.status === 200 && (typeof estimated.json.recording_time === 'number' || typeof estimated.json.time === 'number');
      summary.acceptance.videorecords_delete_501 = remove.status === 501;
      fs.writeFileSync(path.join(OUT, 'videorecords.json'), JSON.stringify(list.json, null, 1) + '\n');
    });

    // ---------- 7. статистика/пакеты/журнал движка ----------
    await record('system-stats', async () => {
      const memory = await api('/api/v3/system/stats/memory');
      const cpu = await api('/api/v3/system/stats/cpu');
      const disk = await api('/api/v3/system/stats/disk');
      const network = await api('/api/v3/system/stats/network');
      const packages = await api('/api/v3/system/packages');
      const log = await api('/api/v3/engine/log');
      summary.acceptance.stats_memory =
        memory.status === 200 && memory.json.total > 0 && memory.json.used >= 0 && memory.json.free >= 0;
      summary.acceptance.stats_cpu =
        cpu.status === 200 && cpu.json.count >= 1 && cpu.json.load.length === cpu.json.count &&
        cpu.json.load.every((core) => core.user_mode >= 0 && core.kernel_mode >= 0 && core.idle >= 0);
      // Единицы: disk — MB (UI делит на 1024 и подписывает Gb), network — kbps
      // (UI подписывает «кбит/сек»), а не накопительные байты.
      let rootDfMb = null;
      try {
        const df = execSync("df -Pk / | tail -1").toString().trim().split(/\s+/);
        rootDfMb = Math.floor(Number(df[1]) / 1024);
      } catch { /* нет df */ }
      const root = disk.json.spaces.find((space) => space.name === '/');
      summary.acceptance.stats_disk =
        disk.status === 200 && disk.json.count === disk.json.spaces.length && disk.json.spaces.length >= 1 &&
        disk.json.spaces.every((space) => space.name && space.total > 0 && space.used >= 0 && space.free >= 0) &&
        (!root || !rootDfMb || Math.abs(root.total - rootDfMb) <= Math.max(8, rootDfMb / 50));
      summary.acceptance.stats_network =
        network.status === 200 && network.json.count === network.json.interfaces.length &&
        network.json.interfaces.every((item) => item.name && item.rx >= 0 && item.tx >= 0 &&
          item.rx < 100000000 && item.tx < 100000000);
      summary.acceptance.system_packages = packages.status === 200 && Array.isArray(packages.json.packages);
      const startup = log.json && log.json.log && log.json.log[0] && log.json.log[0].timestamp;
      summary.acceptance.engine_log = Boolean(
        log.status === 200 && typeof log.json.language === 'string' && log.json.bounds && Array.isArray(log.json.log) &&
        expectedProcessStart && Math.abs(startup - expectedProcessStart) <= 2,
      );
      summary.totals.engine_log = { startup, expectedProcessStart };
      fs.writeFileSync(path.join(OUT, 'stats.json'), JSON.stringify({ memory: memory.json, cpu: cpu.json, disk: disk.json, network: network.json }, null, 1) + '\n');
      fs.writeFileSync(path.join(OUT, 'engine_log.json'), JSON.stringify(log.json, null, 1) + '\n');
    });

    // ---------- 7b. лицензия/фичи/сетевой статус/session-alias/traceroute ----------
    await record('license-features-network-status', async () => {
      const license = await api('/api/v3/license');
      summary.acceptance.license_permanent_shape = Boolean(
        license.status === 200 && license.json.state === 'VALID' && license.json.type === 'PERMANENT' &&
        license.json.last_expired > 4000000000 && license.json.last_update_failed === false &&
        license.json.serial_id && license.json.params && license.json.params.protocol && license.json.params.video,
      );
      const features = await api('/api/v3/features');
      summary.acceptance.features_array =
        features.status === 200 && Array.isArray(features.json.features) && features.json.features.length > 40;
      const status = await api('/api/v3/system/status/network');
      summary.acceptance.status_network_shape =
        status.status === 200 && status.json.network && status.json.network.editable === true;
      const alias = await api('/api/v3/sessions/any-id/videorecords/estimated_time?quality=high');
      const aliasList = await api('/api/v3/sessions/any-id/videorecords');
      summary.acceptance.session_videorecords_alias =
        alias.status === 200 && typeof alias.json.recording_time === 'number' && aliasList.status === 200;
      const optionPing = await api('/api/v3/system/utils/ping?address=' + encodeURIComponent('-V'));
      summary.acceptance.utils_ping_rejects_option = optionPing.status === 422;
      const traceroute = await api('/api/v3/system/utils/traceroute?address=127.0.0.1');
      const tracerouteInstalled = (() => {
        try { execSync('command -v traceroute >/dev/null 2>&1'); return true; } catch { return false; }
      })();
      summary.acceptance.utils_traceroute_honest =
        tracerouteInstalled
          ? traceroute.status === 200
          : traceroute.status === 503 && String((traceroute.json && traceroute.json.error && traceroute.json.error.description) || '').includes('not installed');
      fs.writeFileSync(path.join(OUT, 'license.json'), JSON.stringify(license.json, null, 1) + '\n');
      fs.writeFileSync(path.join(OUT, 'features.json'), JSON.stringify(features.json, null, 1) + '\n');
    });

    // ---------- 8. UI: все T2.4-страницы без 4xx и неизвестных ошибок ----------
    await record('ui-pages', async () => {
      const pages = [
        ['statistics', ['Статистика', 'Использование памяти']],
        ['mediafiles', ['Медиафайлы', 'Загруженные медиафайлы']],
        ['terminals', ['Терминалы', 'Список терминалов']],
        ['network', ['Сеть', 'Диагностика']],
        ['event_list', ['Журнал событий']],
        ['diagnostics', ['Диагностика', 'Исполнительный модуль']],
        ['backup', ['Импорт/Экспорт', 'База данных']],
        ['reset', ['Очистка данных']],
        ['license', ['Лицензия', 'Информация о лицензии']],
        ['about', ['О программе', 'Информация о программе']],
      ];
      summary.acceptance.ui_pages = true;
      summary.acceptance.ui_network_interfaces_tab = true;
      summary.acceptance.ui_license_not_expired = true;
      const seen = [];
      for (const [slug, labels] of pages) {
        const before = httpErrors.length;
        await page.goto(`${BASE}/pnm/administration/${slug}`, { waitUntil: 'domcontentloaded' });
        await page.waitForTimeout(3000);
        const text = await page.locator('body').innerText();
        const missing = labels.filter((label) => !text.includes(label));
        seen.push({ slug, missing, http: httpErrors.slice(before) });
        if (missing.length || httpErrors.length > before) {
          summary.acceptance.ui_pages = false;
          summary.acceptance[`ui_problem:${slug}`] = { missing, http: httpErrors.slice(before) };
        }
        if (slug === 'statistics') {
          // Фактическая страница статистики (раньше тут оказывался About).
          await shot('01-statistics.png');
          summary.totals.statistics_text = (await page.locator('body').innerText()).slice(0, 2000);
        }
        if (slug === 'license') {
          const text = await page.locator('body').innerText();
          summary.acceptance.ui_license_not_expired =
            !text.includes('01.01.1970') && !text.includes('Продлить лицензию') && !text.includes('Лицензия истекла');
        }
      }
      summary.totals.ui_pages_seen = seen;
      await page.goto(`${BASE}/pnm/administration/event_list`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      await shot('02-event-list.png');
      await page.goto(`${BASE}/pnm/administration/network`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      await shot('03-network.png');
      // Вкладка «Сетевые интерфейсы» должна реально отрисовать ens18:
      // проверяем и запрос /system/status/network, и карточки менеджера.
      const beforeTab = httpErrors.length;
      const tab = page.getByText('Сетевые интерфейсы', { exact: false }).first();
      let tabFound = false;
      if (await tab.count()) {
        tabFound = true;
        await tab.click({ force: true });
        await page.waitForTimeout(3000);
      }
      const tabText = await page.locator('body').innerText();
      const cards = await page.locator('.network-manager-card').count();
      const rendered = (cards > 0 || tabText.includes('ens18')) && networkStatusSeen && networkInterfacesSeen;
      summary.acceptance.ui_network_interfaces_tab =
        httpErrors.length === beforeTab && tabFound && rendered;
      summary.totals.network_tab =
        { tabFound, statusSeen: networkStatusSeen, interfacesSeen: networkInterfacesSeen, cards, rendered };
      await shot('03b-network-interfaces.png');
      // PATCH сети не применяется, но оператор должен это УВИДЕТЬ: дергаем
      // тот же путь, что шлёт UI, и проверяем тост api-errors.js.
      const patchStatus = await page.evaluate(
        (authToken) => new Promise((resolve) => {
          const xhr = new XMLHttpRequest();
          xhr.open('PATCH', '/api/v3/system/network/ens18');
          xhr.setRequestHeader('Authorization', `Bearer ${authToken}`);
          xhr.setRequestHeader('Content-Type', 'application/json');
          xhr.onload = () => resolve(xhr.status);
          xhr.onerror = () => resolve(0);
          xhr.send('{}');
        }),
        token,
      );
      await page.waitForTimeout(700);
      const toast = await page.locator('.vks-api-error-toast').last().innerText().catch(() => '');
      summary.acceptance.network_patch_error_visible =
        patchStatus === 501 && /not applied|Не удалось применить/i.test(toast);
      summary.totals.network_save = { patchStatus, toast };
      await shot('03c-network-save-error.png');
      await page.goto(`${BASE}/pnm/administration/terminals`, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(3000);
      await shot('04-terminals.png');
    });

    // ---------- 9. итог ----------
    summary.acceptance.clean_build =
      build.git_dirty === false && build.git_head.length === 40 && build.binary_sha256 !== null;
    summary.acceptance.binary_built_after_head =
      build.binary_mtime !== null && build.git_head_ct > 0 && build.binary_mtime >= build.git_head_ct;
    const acceptance = Object.entries(summary.acceptance);
    const failed = acceptance.filter(([, ok]) => ok !== true).map(([key]) => key);
    summary.acceptance_failed = failed;
    summary.console_errors = consoleErrors;
    summary.vendor_errors = vendorErrors;
    summary.http_errors = httpErrors;
    summary.ok =
      build.git_dirty === false &&
      build.binary_mtime !== null &&
      build.binary_mtime >= build.git_head_ct &&
      failed.length === 0 &&
      consoleErrors.length === 0 &&
      httpErrors.length === 0;
    summary.totals = {
      ...summary.totals,
      gates: acceptance.length,
      gates_passed: acceptance.length - failed.length,
      http_errors: httpErrors.length,
      console_errors: consoleErrors.length,
      vendor_errors: vendorErrors.length,
      screenshots: screenshots.length,
      steps: steps.length,
    };
  } catch (error) {
    summary.error = String((error && error.stack) || error);
  } finally {
    summary.finished_at = new Date().toISOString();
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    await browser.close();
    const acceptance = Object.entries(summary.acceptance);
    const failed = acceptance.filter(([, ok]) => ok !== true).map(([key]) => key);
    console.log(`T2.4 evidence: ${summary.ok ? 'OK' : 'FAIL'} ${acceptance.length - failed.length}/${acceptance.length} gates`);
    if (failed.length) console.log('failed:', failed.join(', '));
    if (summary.error) console.log(summary.error);
    process.exit(summary.ok ? 0 : 1);
  }
})();

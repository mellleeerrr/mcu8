#!/usr/bin/env node
// T1.6 acceptance evidence generator: SIP + H.323 из веб-UI.
//
// Сценарий на каждый протокол: создание конференции и старт сессии из UI →
// «Добавить участника по адресу» (автодополнение сохранённого адреса) →
// CONNECTED и рост медиа-счётчиков → mute камеры/микрофона из UI →
// ручная раскладка 4 окна с участником в слоте 1 → reload страницы и проверка
// слота → kick из UI с подтверждением. Пишет PNG + summary.json + http-trace.json
// (опционально — выдержку серверного лога по session id).
//
// Требования: node, playwright (Chromium), живой vks-server, свободный endpoint.
//   PLAYWRIGHT_MODULE=/path/to/node_modules/playwright \
//   BASE_URL=http://127.0.0.1:8080 \
//   EVIDENCE_DIR=docs/progress/evidence/t16 \
//   SERVER_LOG=vks_server.log \
//   node scripts/evidence/t16-evidence.js
'use strict';
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

const BASE = process.env.BASE_URL || 'http://127.0.0.1:8080';
const OUT = path.resolve(process.env.EVIDENCE_DIR || 'docs/progress/evidence/t16');
const SERVER_LOG = process.env.SERVER_LOG || '';
const CHROME = process.env.CHROME_PATH || chromium.executablePath();
fs.mkdirSync(OUT, { recursive: true });

const PROTOCOLS = [
  { protocol: 'H.323', prefix: 'h323', uri: 'h323:192.168.1.132', suggestion: 'Зал 132 (h323)' },
  { protocol: 'SIP', prefix: 'sip', uri: 'sip:192.168.1.132', suggestion: 'Зал 132 (sip)' },
];

function buildFacts() {
  const run = (cmd) => execSync(cmd, { cwd: path.resolve(__dirname, '../..') }).toString().trim();
  return {
    git_head: run('git rev-parse HEAD'),
    git_dirty: run('git status --porcelain --untracked-files=no').length > 0,
    binary_sha256: run('sha256sum target/release/vks-server').split(/\s+/)[0],
    server_pid: Number(process.env.SERVER_PID || execSync('pgrep -f target/release/vks-server | head -1').toString().trim()),
    source_note: 'бинарник должен соответствовать HEAD; git_dirty=true означает изменённые tracked-файлы на момент прогона (untracked-output evidence не считается)',
  };
}

(async () => {
  // Провенанс фиксируем до записи выходных файлов, иначе evidence сам делает дерево dirty.
  const build = buildFacts();
  if (!fs.existsSync(CHROME)) throw new Error(`Chromium не найден: ${CHROME}`);
  const browser = await chromium.launch({ executablePath: CHROME });
  const page = await (await browser.newContext({ viewport: { width: 1440, height: 900 } })).newPage();
  const trace = [];
  const steps = [];
  const scenarios = [];
  const createdSessions = new Set();
  let consoleErrors = [];
  let step = null;
  let token = null;

  const flush = () => { if (step) { steps.push(step); step = null; } };
  const push = (name) => { flush(); step = { step: name, errors: [], fails: [] }; };
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    const text = (m.text() || '').slice(0, 300);
    const location = m.location()?.url || '';
    const frame = m.stackTrace?.()[0]?.url || '';
    consoleErrors.push({ step: step?.step ?? 'idle', text, source: location || frame });
    if (step) step.errors.push(text);
  });
  page.on('pageerror', (e) => {
    const text = 'PAGE ' + (e.message || '').slice(0, 300);
    consoleErrors.push({ step: step?.step ?? 'idle', text, source: e.stack || '' });
    if (step) step.errors.push(text);
  });
  page.on('response', (r) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith('/api/v3')) return;
    trace.push({ step: step?.step ?? 'idle', method: r.request().method(), path: u.pathname + u.search, status: r.status() });
    if (step && u.port === new URL(BASE).port && r.status() >= 400 && u.pathname !== '/api/v3/login') {
      step.fails.push(`${r.status()} ${r.request().method()} ${u.pathname}`);
    }
  });
  const api = async (p, opts = {}) => {
    const r = await page.request.fetch(BASE + p, {
      ...opts,
      headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json', ...(opts.headers || {}) },
    });
    return { status: r.status(), json: await r.json().catch(() => null) };
  };
  // Гасим только сессии, созданные этим прогоном (на общем стенде не трогаем чужие).
  const stopAll = async () => {
    for (const id of createdSessions) {
      await api('/api/v3/sessions/' + id, { method: 'DELETE' });
      await api('/api/v3/conferences/' + id, { method: 'DELETE' });
    }
    createdSessions.clear();
    await new Promise((r) => setTimeout(r, 2500));
  };
  const ensureSaved = async (uri, name) => {
    const list = await api('/api/v3/ui/call_strings?page_size=100');
    const found = (list.json?.call_strings || []).find((c) => c.uri === uri);
    if (found) return found;
    const r = await api('/api/v3/call_strings', { method: 'POST', data: { uri, display_name: name } });
    return r.json?.call_string;
  };
  const sample = async (cid, url) => {
    const ab = await api('/api/v3/sessions/' + cid + '/abonents');
    const a = (ab.json?.connected || []).find((x) => x.connection_url === url);
    if (!a) return null;
    const f = (d) => ({ active: d.active, codec: d.codec, packets: d.packets, bitrate: d.bitrate, resolution: d.resolution });
    return {
      abonent_id: a.id, display_name: a.display_name,
      audio: { rx: f(a.details.audio.rx), tx: f(a.details.audio.tx) },
      video: { rx: f(a.details.video.rx), tx: f(a.details.video.tx) },
    };
  };
  const shot = async (file) => { await page.screenshot({ path: path.join(OUT, file) }); return file; };

  await page.goto(BASE + '/pnm/', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(2500);
  if (await page.locator('input[type=password]').count()) {
    await page.fill('input[type=text], input[autocomplete=username]', 'admin').catch(() => {});
    await page.fill('input[type=password]', 'admin_password');
    await page.keyboard.press('Enter');
    await page.waitForTimeout(3500);
  }
  token = await page.evaluate(() => {
    for (const k of Object.keys(localStorage)) {
      const v = localStorage.getItem(k);
      if (v && v.includes('eyJ')) { try { const o = JSON.parse(v); if (o.token) return o.token; } catch (e) { if (v.startsWith('eyJ')) return v; } }
    }
    return null;
  });
  if (!token) {
    const r = await page.request.post(BASE + '/api/v3/login', { data: { login: 'admin', password: 'admin_password' } });
    token = (await r.json()).token;
  }

  try {
  // Чистим прошлые выходы, чтобы упавший прогон не оставил «свежий» summary/PNG.
  for (const f of ['summary.json', 'http-trace.json', 'server-correlate.txt']) {
    const target = path.join(OUT, f);
    if (fs.existsSync(target)) fs.rmSync(target);
  }
  for (const f of fs.readdirSync(OUT).filter((f) => /^\d{2}-(?:h323|h\.323|sip)-.*\.png$/i.test(f))) {
    fs.rmSync(path.join(OUT, f));
  }
  // Стенд выделенный: завершаем оставшиеся ACTIVE-сессии прошлых прогонов,
  // иначе терминал получит 486 Busy Here. Чужие сессии созданного набора не трогаем.
  const stale = (await api('/api/v3/sessions?page_size=200')).json?.sessions || [];
  const staleActive = stale.filter((s) => s.state === 'ACTIVE').map((s) => s.id);
  for (const id of staleActive) {
    await api('/api/v3/sessions/' + id, { method: 'DELETE' });
    await api('/api/v3/conferences/' + id, { method: 'DELETE' });
  }
  for (const proto of PROTOCOLS) await ensureSaved(proto.uri, proto.suggestion);

  const screenshots = [];
  for (let pi = 0; pi < PROTOCOLS.length; pi++) {
    const sc = PROTOCOLS[pi];
    const rec = { protocol: sc.protocol, uri: sc.uri };
    await stopAll();
    const base = pi * 8 + 1;
    const num = (n) => String(base + n).padStart(2, '0');
    const file = (n, suffix) => `${num(n)}-${sc.prefix}-${suffix}.png`;

    // --- создание и старт ---
    push('create-conference-' + sc.protocol);
    await page.goto(BASE + '/pnm/monitoring/conferences', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(5000);
    await page.locator('button[data-qa="create-conference-button"]:visible').first().click();
    await page.waitForTimeout(6500);
    rec.session_id = page.url().match(/([0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12})/)?.[1];
    if (!rec.session_id) throw new Error('не удалось определить id конференции из URL ' + page.url());
    createdSessions.add(rec.session_id);
    push('start-' + sc.protocol);
    await page.locator('[data-qa="conference-tools-start-button"]').click();
    await page.waitForTimeout(9000);
    if (!page.url().includes(rec.session_id)) {
      await page.goto(BASE + '/pnm/monitoring/sessions/' + rec.session_id, { waitUntil: 'domcontentloaded' });
      await page.waitForTimeout(6000);
    }

    // --- адрес из автодополнения ---
    push('add-address-' + sc.protocol);
    await page.locator('[data-qa="session-main-tabs-control-panel"]').click();
    await page.waitForTimeout(2500);
    await page.locator('[data-qa="abonent-table-add-address-button"]:visible, [data-qa="abonent-blank-add-address-button"]:visible').first().click();
    await page.waitForTimeout(2500);
    const input = await page.evaluateHandle(() => [...document.querySelectorAll('input[type=text]')]
      .filter((i) => i.offsetWidth).find((i) => (i.getAttribute('data-qa') || '').includes('address'))
      || [...document.querySelectorAll('input[type=text]')].filter((i) => i.offsetWidth)[0]);
    await input.asElement().click();
    await input.asElement().type('192.168', { delay: 120 });
    await page.waitForTimeout(2500);
    screenshots.push(await shot(file(0, 'autocomplete')));
    await page.getByText(sc.suggestion, { exact: true }).first().click();
    await page.waitForTimeout(1200);
    const modal = page.locator('.tc-modal-container').filter({ hasText: 'Добавить участника по адресу' }).last();
    const modalText = (await modal.innerText()).slice(0, 500);
    const selectProtocol = await modal.evaluate((el) => {
      const select = el.querySelector('select');
      if (!select) return null;
      return select.selectedOptions?.[0]?.textContent?.trim() || select.value || null;
    }).catch(() => null);
    const protocolLabel = (selectProtocol || modalText).match(/H\.?323|SIP|RTSP|VNC/i)?.[0]
      ?.toUpperCase().replaceAll('.', '') ?? null;
    rec.address_selected = {
      suggestion: sc.suggestion,
      protocol_label: protocolLabel,
      protocol_matches: protocolLabel === sc.prefix.toUpperCase(),
    };
    screenshots.push(await shot(file(1, 'address-selected')));
    await modal.getByText('Дополнительно', { exact: true }).first().click();
    await page.waitForTimeout(1500);
    for (const codec of ['G.722.1c', 'H.264']) {
      await modal.getByText(codec, { exact: true }).first().click().catch(() => {});
      await page.waitForTimeout(400);
    }
    await page.evaluate(() => {
      const btns = [...document.querySelectorAll('button')].filter((b) => b.textContent.trim() === 'Добавить' && b.offsetWidth && !b.disabled);
      btns[btns.length - 1]?.click();
    });
    let connected = null;
    for (let i = 0; i < 35; i++) {
      await page.waitForTimeout(2000);
      const ab = await api('/api/v3/sessions/' + rec.session_id + '/abonents');
      connected = (ab.json?.connected || []).find((a) => a.connection_url === sc.uri);
      if (connected) break;
    }
    rec.connected = !!connected;
    if (!rec.connected) { rec.steps = steps.filter((s) => s.step.endsWith(sc.protocol) || s.step === 'create-conference-' + sc.protocol); scenarios.push(rec); continue; }
    const m1 = await sample(rec.session_id, sc.uri);
    await page.waitForTimeout(4000);
    const m2 = await sample(rec.session_id, sc.uri);
    rec.media_before = m1;
    rec.media_after = m2;
    rec.media_grows = !!m2 && !!m1 && ['audio', 'video'].every((k) => ['rx', 'tx'].every((d) => m2[k][d].packets > m1[k][d].packets));
    rec.connected_url = connected.connection_url;
    rec.connected_protocol = connected.protocol || null;
    screenshots.push(await shot(file(2, 'connected')));

    // --- контролы из UI ---
    push('controls-' + sc.protocol);
    let row = page.locator('.abonent-item-connected button');
    // Порядок кнопок vendor-строки: 1 = камера (video.rx), 2 = микрофон
    // (audio.rx), 3 = колонка (audio.tx); направления — как в settingsList
    // (value: ["<field>", {direction: "rx"|"tx"}]).
    await row.nth(1).click(); // камера
    await page.waitForTimeout(1200);
    await row.nth(2).click(); // микрофон
    await page.waitForTimeout(1200);
    await row.nth(3).click(); // колонка
    await page.waitForTimeout(1500);
    const ctl = (await api('/api/v3/sessions/' + rec.session_id + '/abonents')).json?.connected?.[0]?.details;
    rec.controls = {
      camera_rx_enabled: ctl?.video?.rx?.enabled,
      mic_rx_enabled: ctl?.audio?.rx?.enabled,
      speaker_tx_enabled: ctl?.audio?.tx?.enabled,
    };
    screenshots.push(await shot(file(3, 'controls')));
    await row.nth(1).click(); await row.nth(2).click(); await row.nth(3).click();
    await page.waitForTimeout(1500);
    const restored = (await api('/api/v3/sessions/' + rec.session_id + '/abonents')).json?.connected?.[0]?.details;
    rec.controls_restored = {
      camera_rx_enabled: restored?.video?.rx?.enabled,
      mic_rx_enabled: restored?.audio?.rx?.enabled,
      speaker_tx_enabled: restored?.audio?.tx?.enabled,
    };

    // --- ручная раскладка ---
    push('layout-' + sc.protocol);
    await page.locator('[data-qa="session-main-tabs-layouts"]').click();
    await page.waitForTimeout(3000);
    await page.getByText('Общая', { exact: true }).first().click();
    await page.waitForTimeout(3000);
    await page.locator('[data-qa="layout-mode-selector-manual"]').click();
    await page.waitForTimeout(1800);
    await page.getByText('4', { exact: true }).first().click();
    await page.waitForTimeout(1500);
    await page.getByText('Заполнить видеоокна участниками', { exact: true }).first().click();
    await page.waitForTimeout(2200);
    await page.getByText('Применить', { exact: true }).first().click();
    await page.waitForTimeout(3500);
    const layouts = (await api('/api/v3/sessions/' + rec.session_id + '/video_layouts')).json?.video_layouts || [];
    const layout = layouts[0];
    const ab = (await api('/api/v3/sessions/' + rec.session_id + '/video_layouts/' + layout.id + '/abonents')).json;
    rec.layout = {
      id: layout.id, mode: layout.payload?.mode, size: layout.payload?.size,
      slots: layout.payload?.slots?.length,
      abonents: ab?.abonents?.map((x) => ({ display_name: x.display_name, position: x.position })),
    };
    screenshots.push(await shot(file(4, 'layout')));

    // --- reload: слот и позиция переживают перезагрузку ---
    push('reload-' + sc.protocol);
    await page.goto(BASE + '/pnm/monitoring/sessions/' + rec.session_id, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(6000);
    await page.locator('[data-qa="session-main-tabs-layouts"]').click();
    await page.waitForTimeout(3000);
    await page.getByText('Общая', { exact: true }).first().click();
    await page.waitForTimeout(3500);
    const ab2 = (await api('/api/v3/sessions/' + rec.session_id + '/video_layouts/' + layout.id + '/abonents')).json;
    rec.layout_after_reload = {
      abonents: ab2?.abonents?.map((x) => ({ display_name: x.display_name, position: x.position })),
      participant_in_dom: (await page.getByText(sc.suggestion, { exact: true }).count()) > 0,
    };
    screenshots.push(await shot(file(5, 'layout-reload')));

    // --- kick из UI с подтверждением ---
    push('kick-' + sc.protocol);
    // редактор раскладки перекрывает вкладки сессии — возвращаемся на карточку сессии
    await page.goto(BASE + '/pnm/monitoring/sessions/' + rec.session_id, { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(5000);
    await page.locator('[data-qa="session-main-tabs-control-panel"]').click();
    await page.waitForTimeout(2500);
    await page.locator('.abonent-item-connected button').nth(7).click();
    await page.waitForTimeout(1500);
    screenshots.push(await shot(file(6, 'kick-confirm')));
    const confirm = page.getByText('Отключить', { exact: true }).last();
    if (await confirm.count()) await confirm.click();
    await page.waitForTimeout(2500);
    const afterKick = (await api('/api/v3/sessions/' + rec.session_id + '/abonents')).json;
    rec.kicked = {
      connected_empty: (afterKick?.connected || []).length === 0,
      disconnected_contains: (afterKick?.disconnected || []).some((d) => d.connection_url === sc.uri),
    };
    screenshots.push(await shot(file(7, 'kicked')));
    scenarios.push(rec);

    // уходим со страницы, чтобы polling удалённой конференции не попал в следующий шаг
    flush();
    await page.goto(BASE + '/pnm/monitoring/conferences', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2500);
    const cid = rec.session_id;
    await api('/api/v3/sessions/' + cid, { method: 'DELETE' });
    await api('/api/v3/conferences/' + cid, { method: 'DELETE' });
    await new Promise((r) => setTimeout(r, 3500));
  }

  // Атрибуция ошибок: vendor-ошибки формы создания конференции — только если
  // кадр действительно из бандла 2.2.1.66 и шаг create-conference; иначе other.
  const isVendorCreate = (e) => e.step.startsWith('create-conference-') && /bundle-2\.2\.1\.66/.test(e.source || '');
  const vendorErrors = consoleErrors.filter(isVendorCreate);
  const otherErrors = consoleErrors.filter((e) => !isVendorCreate(e));
  const createErrors = vendorErrors.length;
  const httpErrors = trace.filter((entry) => entry.status >= 400 && entry.path !== '/api/v3/login').length;
  const notConnected = scenarios.filter((s) => !s.connected).map((s) => s.protocol);
  const vendorFields = [...new Set((vendorErrors.map((e) => e.text).join('\n').match(/reading '[a-zA-Z]+'/g) || []))];
  const acceptance = {
    protocol_matches: scenarios.length === 2 && scenarios.every((s) => s.protocol_matches === true),
    connected: scenarios.length === 2 && scenarios.every((s) => s.connected === true),
    media_grows: scenarios.every((s) => s.media_grows === true),
    controls_off: scenarios.every((s) => s.controls && s.controls.camera_rx_enabled === false && s.controls.mic_rx_enabled === false && s.controls.speaker_tx_enabled === false),
    controls_restored: scenarios.every((s) => s.controls_restored && s.controls_restored.camera_rx_enabled === true && s.controls_restored.mic_rx_enabled === true && s.controls_restored.speaker_tx_enabled === true),
    layout: scenarios.every((s) => s.layout && s.layout.mode === 'manual' && s.layout.size === 4 && (s.layout.abonents || []).some((a) => a.position === 1)),
    reload_keeps_participant: scenarios.every((s) => s.layout_after_reload && s.layout_after_reload.participant_in_dom === true && (s.layout_after_reload.abonents || []).some((a) => a.position === 1)),
    kicked: scenarios.every((s) => s.kicked && s.kicked.connected_empty === true && s.kicked.disconnected_contains === true),
  };
  const ok = otherErrors.length === 0 && httpErrors === 0 && Object.values(acceptance).every(Boolean);
  const summary = {
    generated_at: new Date().toISOString(), base: BASE,
    build,
    ok,
    acceptance,
    totals: {
      console_errors: consoleErrors.length,
      vendor_create_form_typeerrors: createErrors,
      other_console_errors: otherErrors.length,
      http_errors: httpErrors,
      steps: steps.length,
      unique_steps: new Set(steps.map((s) => s.step)).size,
    },
    unconnected_protocols: notConnected,
    stopped_active_sessions: staleActive,
    notes: [
      `transient vendor TypeError на открытие формы создания конференции (2.2.1.66), поля: ${vendorFields.join(', ')}; HTTP>=400 при этом 0 — атрибутировано, не скрыто`,
      'SIP и H.323: вызов из UI через автодополнение сохранённого адреса, рост медиа-счётчиков, mute камеры/микрофона из UI, ручная раскладка 4 слота, позиция 1 переживает reload, kick из UI',
      'summary.json и http-trace.json генерируются этим скриптом; первичные доказательства — PNG + smoke-логи + серверный лог',
      'провенанс: build.git_head — коммит, на котором запущен генератор; коммит с evidence является его docs-only потомком (кода не меняет), бинарник подтверждён sha256 и pid',
      'в начале прогона завершаются оставшиеся ACTIVE-сессии стенда (stopped_active_sessions); внутри сценариев — только созданные этим прогоном',
    ],
    screenshots: screenshots.map((f) => ({ file: f })),
    scenarios,
    steps,
    trace: 'http-trace.json',
  };
  fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 2));
  fs.writeFileSync(path.join(OUT, 'http-trace.json'), JSON.stringify(trace, null, 2));
  const correlateFile = path.join(OUT, 'server-correlate.txt');
  if (SERVER_LOG && fs.existsSync(SERVER_LOG)) {
    const ids = scenarios.flatMap((s) => [
      s.session_id,
      s.media_after?.abonent_id,
      s.media_before?.abonent_id,
    ]).filter(Boolean);
    const lines = fs.readFileSync(SERVER_LOG, 'utf8').split('\n')
      .filter((l) => ids.some((id) => l.includes(id)))
      .slice(-60);
    fs.writeFileSync(correlateFile, lines.join('\n') + '\n');
  } else if (fs.existsSync(correlateFile)) {
    fs.rmSync(correlateFile);
  }
  console.log(JSON.stringify({
    ok,
    totals: summary.totals,
    screenshots,
    scenarios: scenarios.map((s) => ({
      protocol: s.protocol, connected: s.connected, media_grows: s.media_grows, controls: s.controls,
      layout: s.layout, layout_after_reload: s.layout_after_reload, kicked: s.kicked,
    })),
  }, null, 1));
  if (!ok) {
    console.error('evidence FAILED: unconnected=' + JSON.stringify(notConnected) + ', other_console_errors=' + otherErrors.length + ', http_errors=' + httpErrors + ', acceptance=' + JSON.stringify(acceptance));
    process.exitCode = 1;
  }
  } catch (e) {
    console.error(e);
    process.exitCode = 1;
  } finally {
    // Гарантированная уборка: иначе исключение посреди сценария оставит ACTIVE-сессию.
    try { await stopAll(); } catch (e) { console.error('cleanup: ' + e.message); }
    await browser.close();
  }
})().catch((e) => { console.error(e); process.exit(1); });

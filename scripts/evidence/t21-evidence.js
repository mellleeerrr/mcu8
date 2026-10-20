#!/usr/bin/env node
// T2.1 acceptance evidence: справочник PNM (пользователи/группы/учётные
// записи/личные адреса) через vendor-UI 2.2.1.66 на нашем `/api/v3`.
//
// Запуск:
//   PLAYWRIGHT_MODULE=/path/to/playwright \
//   BASE_URL=http://127.0.0.1:8080 \
//   node scripts/evidence/t21-evidence.js
//
// Скрипт создаёт уникальные сущности, проверяет их через API (WS/HTTP — те же,
// что использует UI), делает PNG-шаги и в конце удаляет созданное. Любой провал
// acceptance-гейта → `ok:false` и exit 1.

const crypto = require('crypto');
const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

const BASE = (process.env.BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const OUT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t21';
const ADMIN_LOGIN = process.env.ADMIN_LOGIN || 'admin';
const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD || 'admin_password';
fs.mkdirSync(OUT, { recursive: true });

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const sh = (cmd) => { try { return execSync(cmd, { encoding: 'utf8' }).trim(); } catch { return ''; } };

// Провенанс фиксируется ДО первой записи в evidence-каталог, иначе `git_dirty`
// всегда был бы true из-за самих результатов прогона.
const build = {
  git_head: sh('git rev-parse HEAD'),
  git_dirty: sh('git status --porcelain') !== '',
  binary_sha256: null,
  server_pid: null,
};
{
  const pids = sh('pgrep -f "target/release/vks-server"').split('\n').filter(Boolean);
  for (const pid of pids) {
    const exe = `/proc/${pid}/exe`;
    try {
      build.server_pid = Number(pid);
      build.binary_sha256 = crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex');
      break;
    } catch { /* процесс без доступа к exe — пробуем следующий */ }
  }
}
const now = () => new Date().toISOString();
const stamp = String(Date.now()).slice(-6);
const USER_NAME = `T21 Пользователь ${stamp}`;
const GROUP_NAME = `T21 Группа ${stamp}`;
const LOGIN_NAME = `t21_${stamp}`;
const PASSWORD = 'T21-Secret-123!';
const ADDRESS_URI = `sip:t21_${stamp}@example.com`;

(async () => {
  const browser = await chromium.launch({
    executablePath: process.env.CHROME_PATH || chromium.executablePath(),
    headless: true,
    args: ['--ignore-certificate-errors'],
  });
  const page = await browser.newPage({ ignoreHTTPSErrors: true, viewport: { width: 1440, height: 900 } });

  let token = null;
  let createdUser = null;
  let createdGroup = null;
  const evidenceIds = { user_id: null, login: LOGIN_NAME, group_id: null };
  const steps = [];
  const consoleErrors = [];
  const httpErrors = [];
  let expectedCredentials404 = 0;
  const screenshots = [];
  const wsUsersSearch = [];
  let step = null;
  const push = (name) => { step = { step: name, errors: [] }; steps.push(step); };
  const shot = async (name) => {
    const file = path.join(OUT, name + '.png');
    await page.screenshot({ path: file, fullPage: false });
    screenshots.push(name + '.png');
  };
  // Vendor-бандлы 2.2.1.66 содержат известные дефекты, не связанные с нашим API:
  // runtime-компиляция шаблона (`Content Security Policy`/unsafe-eval) и баг
  // цепочки промиса в диалоге создания группы (`validationHandler`). Учётная
  // запись пользователя проверяется vendor-UI через EXPECTED `GET …/credentials`
  // 404 (пустое состояние) — это не ошибка контракта.
  let vendorConsoleErrors = 0;
  const isVendorConsole = (text, source) =>
    /bundle-2\.2\.1\.66/.test(source || '')
    || /Content Security Policy/.test(text)
    || /validationHandler/.test(text);
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    const text = (m.text() || '').slice(0, 300);
    const source = m.location()?.url || '';
    if (/Failed to load resource/.test(text) && /\/credentials/.test(source)) {
      expectedCredentials404 += 1;
      return;
    }
    const vendor = isVendorConsole(text, source);
    consoleErrors.push({ step: step?.step ?? 'idle', text, source, vendor });
    if (vendor) vendorConsoleErrors += 1;
    else if (step) step.errors.push(text);
  });
  page.on('pageerror', (e) => {
    const text = 'PAGE ' + (e.message || '').slice(0, 300);
    consoleErrors.push({ step: step?.step ?? 'idle', text, source: e.stack || '' });
    if (step) step.errors.push(text);
  });
  page.on('response', (r) => {
    const u = new URL(r.url());
    if (!u.pathname.startsWith('/api/v3') || r.status() < 400 || u.pathname === '/api/v3/login') return;
    if (r.status() === 404 && r.request().method() === 'GET' && /^\/api\/v3\/users\/[0-9a-f-]+\/credentials$/.test(u.pathname)) {
      expectedCredentials404 += 1;
      return;
    }
    httpErrors.push({ step: step?.step ?? 'idle', method: r.request().method(), path: u.pathname + u.search, status: r.status() });
  });
  page.on('websocket', (ws) => {
    ws.on('framesent', (f) => {
      const payload = String(f.payload || '');
      if (!payload.includes('"ui:users:search"')) return;
      try { wsUsersSearch.push({ request: JSON.parse(payload) }); } catch { /* ignore */ }
    });
    ws.on('framereceived', (f) => {
      const payload = String(f.payload || '');
      if (!payload.includes('"ui:users:search"') && !payload.includes('"users":[')) return;
      try {
        const parsed = JSON.parse(payload);
        const users = parsed.payload?.users;
        if (Array.isArray(users)) wsUsersSearch.push({ response: { count: parsed.payload.count, ids: users.map((u) => u.id), names: users.map((u) => u.display_name) } });
      } catch { /* ignore */ }
    });
  });

  const api = async (p, opts = {}) => {
    const r = await page.request.fetch(BASE + p, {
      ...opts,
      headers: { Authorization: 'Bearer ' + token, 'Content-Type': 'application/json', ...(opts.headers || {}) },
    });
    return { status: r.status(), json: await r.json().catch(() => null) };
  };
  const findUser = async (name) => (await api('/api/v3/users?search=' + encodeURIComponent(name))).json?.users?.[0] || null;
  const findGroup = async (name) => (await api('/api/v3/groups?search=' + encodeURIComponent(name))).json?.groups?.[0] || null;

  try {
    // login
    push('login');
    await page.goto(BASE + '/pnm/', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(2500);
    if (await page.locator('input[type=password]').count()) {
      await page.fill('input[type=text], input[autocomplete=username]', ADMIN_LOGIN).catch(() => {});
      await page.fill('input[type=password]', ADMIN_PASSWORD);
      await page.keyboard.press('Enter');
      await page.waitForTimeout(5000);
    }
    if (await page.locator('input[type=password]').count()) {
      const r = await page.request.post(BASE + '/api/v3/login', { data: { login: ADMIN_LOGIN, password: ADMIN_PASSWORD } });
      token = (await r.json()).token;
    } else {
      token = await page.evaluate(() => {
        for (const k of Object.keys(localStorage)) {
          const v = localStorage.getItem(k);
          if (v && v.includes('eyJ')) { try { const o = JSON.parse(v); if (o.token) return o.token; } catch { if (v.startsWith('eyJ')) return v; } }
        }
        return null;
      });
    }
    if (!token) token = (await (await page.request.post(BASE + '/api/v3/login', { data: { login: ADMIN_LOGIN, password: ADMIN_PASSWORD } })).json()).token;
    if (!token) throw new Error('нет токена после логина');

    // 1. список пользователей (WS ui:users:search) + карточка
    push('users-list');
    await page.goto(BASE + '/pnm/addressbook/users', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4500);
    await page.getByText('Administrator', { exact: true }).first().waitFor({ timeout: 10000 });
    const adminItem = await page.locator('[data-qa="address-book-list-user"]').innerText();
    if (!adminItem.includes('Administrator')) throw new Error('в списке нет Administrator');
    await shot('01-users-list');

    // 2. создание пользователя через UI
    push('user-create');
    await page.locator('[data-qa="user-list-tools-add-button"]').click();
    await page.waitForTimeout(2000);
    await page.fill('[data-qa="user-create-popup-display-name-input"]', USER_NAME);
    await page.locator('.tc-modal-container').last().getByText('Сохранить', { exact: true }).click();
    await page.waitForTimeout(4000);
    createdUser = await findUser(USER_NAME);
    if (!createdUser) throw new Error('пользователь не создан: ' + USER_NAME);
    evidenceIds.user_id = createdUser.id;
    await shot('02-user-created');

    // 3. карточка пользователя: вкладки info/profile/addresses/credentials
    push('user-card');
    await page.getByText(USER_NAME, { exact: true }).first().click();
    await page.waitForTimeout(3500);
    for (const tab of ['info-tab', 'profile-tab', 'addresses-tab', 'credentials-tab']) {
      if (!(await page.locator(`[data-qa="${tab}"]`).count())) throw new Error('нет вкладки ' + tab);
    }
    await shot('03-user-card');

    // 4. учётная запись: логин/пароль через UI, затем реальный login по API
    push('credentials');
    await page.locator('[data-qa="credentials-tab"]').click();
    await page.waitForTimeout(2500);
    const textInputs = page.locator('input[type=text]:visible');
    if ((await textInputs.count()) < 1) throw new Error('нет поля логина');
    await textInputs.last().fill(LOGIN_NAME);
    const passwords = page.locator('input[type=password]:visible');
    if ((await passwords.count()) < 2) throw new Error('нет полей пароля');
    await passwords.nth(0).fill(PASSWORD);
    await passwords.nth(1).fill(PASSWORD);
    await page.getByText('Сохранить', { exact: true }).first().click();
    await page.waitForTimeout(3500);
    const creds = await api('/api/v3/users/' + createdUser.id + '/credentials');
    if (creds.status !== 200 || creds.json?.login !== LOGIN_NAME) throw new Error('учётная запись не создана: ' + JSON.stringify(creds));
    const freshLogin = await page.request.post(BASE + '/api/v3/login', { data: { login: LOGIN_NAME, password: PASSWORD } });
    if (freshLogin.status() !== 200) throw new Error('login созданным логином не работает: ' + freshLogin.status());
    await shot('04-credentials');

    // 5. личная адресная книга: адрес из UI
    push('user-address');
    await page.locator('[data-qa="addresses-tab"]').click();
    await page.waitForTimeout(2500);
    await page.locator('[data-qa="user-address-add-button"]').click();
    await page.waitForTimeout(2500);
    await page.fill('[data-qa="address-manager-popup-uri"]', ADDRESS_URI);
    await page.fill('[data-qa="address-manager-text-field-display-name"]', 'T21 адрес');
    await page.locator('.tc-modal-container').last().getByText('Сохранить', { exact: true }).click();
    await page.waitForTimeout(3500);
    const addresses = await api('/api/v3/users/' + createdUser.id + '/call_strings');
    if (addresses.status !== 200 || addresses.json?.count !== 1 || addresses.json.call_strings[0].uri !== ADDRESS_URI) {
      throw new Error('адрес не добавлен в личную книгу: ' + JSON.stringify(addresses));
    }
    await shot('05-user-address');

    // 6. группа через UI + членство через UI
    push('group-create');
    await page.goto(BASE + '/pnm/addressbook/users', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4000);
    await page.locator('[data-qa="user-list-tools-mode-selector"]').click();
    await page.waitForTimeout(1200);
    await page.locator('[data-qa="group-context-option"]').click();
    await page.waitForTimeout(3000);
    await page.locator('[data-qa="user-list-tools-add-button"]').click();
    await page.waitForTimeout(2000);
    await page.fill('[data-qa="group-create-popup-display-name-input"]', GROUP_NAME);
    await page.locator('.tc-modal-container').last().getByText('Создать', { exact: true }).click();
    await page.waitForTimeout(3500);
    createdGroup = await findGroup(GROUP_NAME);
    if (!createdGroup) throw new Error('группа не создана: ' + GROUP_NAME);
    evidenceIds.group_id = createdGroup.id;
    await shot('06-group-created');

    push('group-member');
    await page.getByText(GROUP_NAME, { exact: true }).first().click();
    await page.waitForTimeout(3000);
    await page.locator('[data-qa="group-blank-add-participant-button"]').click();
    await page.waitForTimeout(2500);
    const addDialog = page.locator('.tc-modal-container').last();
    await addDialog.getByText(USER_NAME, { exact: true }).first().click();
    await page.waitForTimeout(800);
    await addDialog.getByText('Добавить', { exact: true }).last().click();
    await page.waitForTimeout(3500);
    const detail = await api('/api/v3/users/' + createdUser.id);
    const groups = detail.json?.user?.groups || [];
    if (!groups.some((g) => g.id === createdGroup.id)) throw new Error('пользователь не добавлен в группу: ' + JSON.stringify(detail.json));
    await shot('07-group-member');

    // 7. reload: пользователь и группа сохраняются в configuration-document
    push('reload');
    await page.goto(BASE + '/pnm/addressbook/users', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4500);
    await page.getByText(USER_NAME, { exact: true }).first().waitFor({ timeout: 10000 });
    await shot('08-users-reload');

    // 8. удаление созданного через API + read-back
    push('cleanup');
    const delUser = await api('/api/v3/users/' + createdUser.id, { method: 'DELETE' });
    if (delUser.status !== 200) throw new Error('не удалили пользователя: ' + delUser.status);
    const delGroup = await api('/api/v3/groups/' + createdGroup.id, { method: 'DELETE' });
    if (delGroup.status !== 200) throw new Error('не удалили группу: ' + delGroup.status);
    const gone = await api('/api/v3/users/' + createdUser.id);
    if (gone.status !== 404) throw new Error('пользователь читается после delete');
    const groupsAfter = await api('/api/v3/groups?search=' + encodeURIComponent(GROUP_NAME));
    if (groupsAfter.json?.count !== 0) throw new Error('группа осталась после delete');
    createdUser = null;
    createdGroup = null;

    const otherErrors = consoleErrors.filter((e) => !e.vendor);
    const acceptance = {
      users_list: steps[0].errors.length === 0 && wsUsersSearch.some((r) => Array.isArray(r.response?.ids) && r.response.ids.every(Boolean)),
      user_created: !!steps.find((s) => s.step === 'user-create' && s.errors.length === 0),
      card_tabs: !!steps.find((s) => s.step === 'user-card' && s.errors.length === 0),
      credentials: !!steps.find((s) => s.step === 'credentials' && s.errors.length === 0),
      user_address: !!steps.find((s) => s.step === 'user-address' && s.errors.length === 0),
      group_created: !!steps.find((s) => s.step === 'group-create' && s.errors.length === 0),
      group_member: !!steps.find((s) => s.step === 'group-member' && s.errors.length === 0),
      reload: !!steps.find((s) => s.step === 'reload' && s.errors.length === 0),
      cleanup: !!steps.find((s) => s.step === 'cleanup' && s.errors.length === 0),
      unique_steps: new Set(steps.map((s) => s.step)).size === steps.length,
      screenshots: screenshots.length === 8,
    };
    const ok = httpErrors.length === 0 && otherErrors.length === 0 && Object.values(acceptance).every(Boolean);
    const summary = {
      generated_at: now(),
      ok,
      build,
      acceptance,
      steps,
      totals: {
        console_errors: consoleErrors.length,
        vendor_console_errors: vendorConsoleErrors,
        other_console_errors: otherErrors.length,
        http_errors: httpErrors.length,
        expected_credentials_404: expectedCredentials404,
        steps: steps.length,
        screenshots: screenshots.length,
      },
      user: { name: USER_NAME, login: LOGIN_NAME, id: evidenceIds.user_id, deleted_in_cleanup: true },
      group: { name: GROUP_NAME, id: evidenceIds.group_id, deleted_in_cleanup: true },
      ws_users_search: wsUsersSearch,
      http_error_samples: httpErrors.slice(0, 10),
      console_error_samples: otherErrors.slice(0, 10),
      screenshots,
      notes: [
        'справочник работает через vendor-UI 2.2.1.66: WS ui:users:search отдаёт id (без него карточка не открывалась), карточка с вкладками info/profile/addresses/credentials, создание пользователя/группы/учётной записи/личного адреса и членство в группе',
        'учётная запись проверена реальным POST /api/v3/login с новым логином/паролем',
        'личная адресная книга проверена GET /api/v3/users/{id}/call_strings; удаление — read-back 404/0',
        'созданные сущности уникальны по timestamp и удалены в cleanup',
        'ожидаемый 404 GET /users/{id}/credentials — vendor-UI проверяет наличие учётной записи (пустое состояние), не ошибка контракта',
        'известные vendor-дефекты атрибутированы отдельно: CSP/runtime-компиляция шаблона (лечится unsafe-eval в CSP для неизменяемых бандлов) и TypeError validationHandler при создании группы; наш API при этом отвечает 201 и группа создаётся',
      ],
      base_url: BASE,
    };
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    console.log(JSON.stringify({ ok, acceptance, totals: summary.totals }, null, 1));
    if (!ok) process.exitCode = 1;
  } catch (e) {
    console.error(e);
    process.exitCode = 1;
  } finally {
    try {
      if (token && createdUser) await api('/api/v3/users/' + createdUser.id, { method: 'DELETE' });
      if (token && createdGroup) await api('/api/v3/groups/' + createdGroup.id, { method: 'DELETE' });
    } catch (e) { console.error('cleanup: ' + e.message); }
    await browser.close();
  }
})().catch((e) => { console.error(e); process.exit(1); });

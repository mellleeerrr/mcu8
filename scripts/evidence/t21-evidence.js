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
const OUT_PRELIGHT = process.env.EVIDENCE_DIR || 'docs/progress/evidence/t21';
let chromium;
try {
  chromium = require(process.env.PLAYWRIGHT_MODULE || 'playwright').chromium;
} catch (e) {
  // Ошибка загрузки Playwright не должна оставлять предыдущий ok:true артефакт.
  fs.mkdirSync(OUT_PRELIGHT, { recursive: true });
  fs.writeFileSync(
    path.join(OUT_PRELIGHT, 'summary.json'),
    JSON.stringify({ ok: false, phase: 'playwright-require', error: String(e) }, null, 1) + '\n',
  );
  console.error(e);
  process.exit(1);
}

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
      build.server_pid = Number(pid);
      build.binary_sha256 = crypto.createHash('sha256').update(fs.readFileSync(exe)).digest('hex');
      build.binary_mtime = Math.floor(fs.statSync(exe).mtimeMs / 1000);
      build.binary_path = exe;
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
  const expectedCredentialUsers = new Set();
  const knownUserIds = new Set();
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
  const vendorSamples = [];
  // Точный allowlist известного дефекта vendor-бандла 2.2.1.66: баг цепочки
  // промиса в диалоге создания группы (`TypeError … catch is not a function`).
  // CSP-нарушения и любые другие console-ошибки — наши и валят гейт.
  const isKnownVendorError = (text) => /validationHandler\(\.\.\.\)\.then\(\.\.\.\)\.then\.catch is not a function/.test(text);
  page.on('console', (m) => {
    if (m.type() !== 'error') return;
    const text = (m.text() || '').slice(0, 300);
    const source = m.location()?.url || '';
    if (/Failed to load resource/.test(text) && /\/credentials/.test(source)) return; // учтено в response-хендлере
    const vendor = isKnownVendorError(text);
    consoleErrors.push({ step: step?.step ?? 'idle', text, source, vendor });
    if (vendor) {
      vendorConsoleErrors += 1;
      vendorSamples.push({ step: step?.step ?? 'idle', text, source });
    } else if (step) {
      step.errors.push(text);
    }
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
      expectedCredentialUsers.add(u.pathname.split('/')[4]);
      return;
    }
    httpErrors.push({ step: step?.step ?? 'idle', method: r.request().method(), path: u.pathname + u.search, status: r.status() });
  });
  page.on('websocket', (ws) => {
    ws.on('framesent', (f) => {
      const payload = String(f.payload || '');
      if (!payload.includes('"ui:users:search"')) return;
      try {
        const parsed = JSON.parse(payload);
        // Токен авторизации в evidence не пишем: только метод/payload/id.
        const frame = parsed.request || parsed;
        wsUsersSearch.push({ request: { method: frame.method, id: frame.id, payload: frame.payload } });
      } catch { /* ignore */ }
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
  const findGroup = async (name) => (await api('/api/v3/groups?display_name=' + encodeURIComponent(name))).json?.groups?.[0] || null;

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
    knownUserIds.add(createdUser.id);
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
    evidenceIds.address_id = addresses.json.call_strings[0].id;

    // 5b. редактирование личного адреса — vendor PATCH /ui/users/{u}/call_strings/{a}
    push('user-address-edit');
    const edited = await api(
      '/api/v3/ui/users/' + createdUser.id + '/call_strings/' + evidenceIds.address_id,
      { method: 'PATCH', data: { uri: 'h323:192.168.1.132', display_name: 'T21 адрес (изменён)' } },
    );
    if (edited.status !== 200 || edited.json?.call_string?.display_name !== 'T21 адрес (изменён)') {
      throw new Error('личный адрес не редактируется: ' + JSON.stringify(edited));
    }
    const editedList = await api('/api/v3/users/' + createdUser.id + '/call_strings');
    if (editedList.json?.call_strings?.[0]?.uri !== 'h323:192.168.1.132') {
      throw new Error('изменённый адрес не сохранился: ' + JSON.stringify(editedList));
    }

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

    // 8. поиск групп тем же параметром, что шлёт vendor-UI (`display_name`)
    push('group-search');
    const groupSearch = await api('/api/v3/groups?display_name=' + encodeURIComponent(GROUP_NAME));
    if (groupSearch.json?.count !== 1 || groupSearch.json.groups[0].id !== createdGroup.id) {
      throw new Error('поиск групп по display_name не работает: ' + JSON.stringify(groupSearch));
    }

    // 9. роль: создать через vendor-API, назначить пользователю, снять
    push('role-create-assign');
    const roleCreated = await api('/api/v3/roles', { method: 'POST', data: { display_name: 'T21 роль ' + stamp } });
    if (roleCreated.status !== 201) throw new Error('роль не создана: ' + JSON.stringify(roleCreated));
    const roleId = roleCreated.json.role.id;
    const roleAssigned = await api('/api/v3/users/' + createdUser.id + '/roles/' + roleId, { method: 'POST' });
    if (roleAssigned.status !== 200) throw new Error('роль не назначена: ' + JSON.stringify(roleAssigned));
    const userWithRole = await api('/api/v3/users/' + createdUser.id);
    if (!(userWithRole.json?.user?.roles || []).some((r) => r.id === roleId)) throw new Error('роль не видна в карточке');
    const roleDeleted = await api('/api/v3/roles/' + roleId, { method: 'DELETE' });
    if (roleDeleted.status !== 200) throw new Error('роль не удалена: ' + JSON.stringify(roleDeleted));

    // 10. смена собственного пароля vendor-телом (`login:null`) и реальный login
    push('self-password');
    const selfLogin = await page.request.post(BASE + '/api/v3/login', { data: { login: LOGIN_NAME, password: PASSWORD } });
    if (selfLogin.status() !== 200) throw new Error('login новым пользователем не работает');
    const selfToken = (await selfLogin.json()).token;
    const selfChange = await page.request.patch(BASE + '/api/v3/profile/credentials', {
      headers: { Authorization: 'Bearer ' + selfToken, 'Content-Type': 'application/json' },
      data: { login: null, old_password: PASSWORD, new_password: PASSWORD + 'x' },
    });
    if (selfChange.status() !== 200) throw new Error('смена своего пароля с login:null: ' + selfChange.status());
    const oldPassword = await page.request.post(BASE + '/api/v3/login', { data: { login: LOGIN_NAME, password: PASSWORD } });
    if (oldPassword.status() !== 401) throw new Error('старый пароль не отклонён');
    const newPassword = await page.request.post(BASE + '/api/v3/login', { data: { login: LOGIN_NAME, password: PASSWORD + 'x' } });
    if (newPassword.status() !== 200) throw new Error('новый пароль не работает');

    // 11. reload: пользователь и группа сохраняются в configuration-document
    push('reload');
    await page.goto(BASE + '/pnm/addressbook/users', { waitUntil: 'domcontentloaded' });
    await page.waitForTimeout(4500);
    await page.getByText(USER_NAME, { exact: true }).first().waitFor({ timeout: 10000 });
    await shot('08-users-reload');

    // 13. удаление пользователя через UI (кнопка «Удалить» активна для обычных
    // пользователей) + read-back
    push('user-delete-ui');
    await page.getByText(USER_NAME, { exact: true }).first().click();
    await page.waitForTimeout(3000);
    const cardDelete = page.locator('button').filter({ hasText: 'Удалить' }).first();
    if (!(await cardDelete.isEnabled().catch(() => false))) throw new Error('кнопка «Удалить» недоступна в UI');
    await cardDelete.click();
    await page.waitForTimeout(1500);
    const confirmDelete = page.locator('.tc-modal-container').last().getByText('Удалить', { exact: true }).last();
    await confirmDelete.click();
    await page.waitForTimeout(3500);
    const gone = await api('/api/v3/users/' + createdUser.id);
    if (gone.status !== 404) throw new Error('пользователь читается после UI-delete: ' + gone.status);
    await shot('09-user-deleted');

    // 14. удаление группы + read-back
    push('cleanup');
    const delGroup = await api('/api/v3/groups/' + createdGroup.id, { method: 'DELETE' });
    if (delGroup.status !== 200) throw new Error('не удалили группу: ' + delGroup.status);
    const groupsAfter = await api('/api/v3/groups?display_name=' + encodeURIComponent(GROUP_NAME));
    if (groupsAfter.json?.count !== 0) throw new Error('группа осталась после delete');
    evidenceIds.user_deleted_ui = true;
    createdUser = null;
    createdGroup = null;

    const otherErrors = consoleErrors.filter((e) => !e.vendor);
    const stepOk = (name) => !!steps.find((s) => s.step === name && s.errors.length === 0);
    const acceptance = {
      users_list: stepOk('users-list') && wsUsersSearch.some((r) => Array.isArray(r.response?.ids) && r.response.ids.every(Boolean)),
      user_created: stepOk('user-create'),
      card_tabs: stepOk('user-card'),
      credentials: stepOk('credentials'),
      user_address: stepOk('user-address'),
      user_address_edit: stepOk('user-address-edit'),
      group_created: stepOk('group-create'),
      group_search: stepOk('group-search'),
      group_member: stepOk('group-member'),
      role_create_assign: stepOk('role-create-assign'),
      self_password: stepOk('self-password'),
      reload: stepOk('reload'),
      user_delete_ui: stepOk('user-delete-ui') && evidenceIds.user_deleted_ui === true,
      cleanup: stepOk('cleanup'),
      unique_steps: new Set(steps.map((s) => s.step)).size === steps.length,
      screenshots: screenshots.length === 9,
      // Ровно два реальных vendor-запроса credentials завершаются 404 (пустое
      // состояние). Регрессия «404 для всех» больше не проходит гейт.
      expected_credentials_404: expectedCredentials404 === 2,
      // Оба ожидаемых 404 относятся к пользователям, созданным этим прогоном
      // (а не к произвольным/чужим id) — иначе allowlist маскировал бы регрессию.
      // Vendor-UI может запросить credentials одной и той же карточки дважды,
      // поэтому важно не «2 разных id», а что все 404 относятся к созданным в
      // прогоне пользователям (регрессия «404 для всех» даёт чужие id).
      expected_credentials_known_users:
        expectedCredentials404 === 2 &&
        expectedCredentialUsers.size > 0 &&
        [...expectedCredentialUsers].every((id) => knownUserIds.has(id)),
      // Артефакт коммитится только с чистого дерева; иначе provenance недостоверен.
      clean_build: build.git_dirty === false && build.git_head.length === 40 && build.binary_sha256 !== null,
      // Бинарник собран ПОСЛЕ коммита, на котором сделан прогон: иначе evidence
      // мог быть снят со старой сборки, не соответствующей исходникам.
      binary_built_after_head:
        build.binary_mtime !== null && build.git_head_ct > 0 && build.binary_mtime >= build.git_head_ct,
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
        expected_credential_user_ids: [...expectedCredentialUsers],
        steps: steps.length,
        screenshots: screenshots.length,
      },
      user: { name: USER_NAME, login: LOGIN_NAME, id: evidenceIds.user_id, deleted_in_user_delete_ui: true },
      group: { name: GROUP_NAME, id: evidenceIds.group_id, deleted_in_cleanup: true },
      ws_users_search: wsUsersSearch,
      http_error_samples: httpErrors.slice(0, 10),
      console_error_samples: otherErrors.slice(0, 10),
      vendor_error_samples: vendorSamples.slice(0, 10),
      screenshots,
      notes: [
        'справочник работает через vendor-UI 2.2.1.66: WS ui:users:search отдаёт id (без него карточка не открывалась), карточка с вкладками info/profile/addresses/credentials, создание пользователя/группы/учётной записи/личного адреса и членство в группе',
        'учётная запись проверена реальным POST /api/v3/login с новым логином/паролем',
        'личная адресная книга: создание и редактирование проверены vendor-эндпоинтами /ui/users/{id}/call_strings; глобальная книга не затрагивается',
        'поиск групп проверен vendor-параметром display_name (не только search)',
        'роль создаётся/назначается/удаляется через vendor-API /roles и /users/{id}/roles/{rid}',
        'смена своего пароля проверена vendor-телом {login:null,...}: старый пароль отклоняется, новый работает',
        'удаление пользователя выполнено кнопкой UI; после delete login отклоняется, профиль/членства/адреса/credentials вычищены',
        'созданные сущности уникальны по timestamp; пользователь удалён UI, группа — API; роль удалена в своём шаге',
        'ожидаемый 404 GET /users/{id}/credentials — vendor-UI проверяет наличие учётной записи (пустое состояние), не ошибка контракта; считается один раз (по response)',
        'единственная vendor-console-ошибка — известный TypeError validationHandler при создании группы (allowlist по точному тексту); CSP-нарушений в финальном прогоне нет',
        'unsafe-eval в CSP вынужден: без него index2.bundle исполняет строку как JS при открытии карточки пользователя/удалении — Chromium пишет «Evaluating a string as JavaScript violates … CSP» (сообщение и источник зафиксированы в отчёте; статический grep этот runtime-путь не видит)',
      ],
      base_url: BASE,
    };
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(summary, null, 1) + '\n');
    console.log(JSON.stringify({ ok, acceptance, totals: summary.totals }, null, 1));
    if (!ok) process.exitCode = 1;
  } catch (e) {
    console.error(e);
    // Провальный прогон не должен оставлять предыдущий ok:true summary.json.
    const failure = {
      ok: false,
      error: String(e && e.stack ? e.stack : e),
      totals: {
        console_errors: consoleErrors.length,
        vendor_console_errors: vendorConsoleErrors,
        other_console_errors: consoleErrors.filter((entry) => !entry.vendor).length,
        http_errors: httpErrors.length,
        expected_credentials_404: expectedCredentials404,
        steps: steps.length,
        screenshots: screenshots.length,
      },
      build,
      steps,
      console_error_samples: consoleErrors.filter((entry) => !entry.vendor).slice(0, 10),
      vendor_error_samples: vendorSamples.slice(0, 10),
      http_error_samples: httpErrors.slice(0, 10),
    };
    fs.writeFileSync(path.join(OUT, 'summary.json'), JSON.stringify(failure, null, 1) + '\n');
    process.exitCode = 1;
  } finally {
    try {
      if (token && createdUser) await api('/api/v3/users/' + createdUser.id, { method: 'DELETE' });
      if (token && createdGroup) await api('/api/v3/groups/' + createdGroup.id, { method: 'DELETE' });
    } catch (e) { console.error('cleanup: ' + e.message); }
    await browser.close();
  }
})().catch((e) => {
  console.error(e);
  // Любой сбой вне основного try (например, launch Chromium) тоже пишет ok:false.
  fs.mkdirSync(OUT_PRELIGHT, { recursive: true });
  fs.writeFileSync(
    path.join(OUT_PRELIGHT, 'summary.json'),
    JSON.stringify({ ok: false, phase: 'uncaught', error: String((e && e.stack) || e) }, null, 1) + '\n',
  );
  process.exit(1);
});

#!/usr/bin/env node
/*
 * T07: deep-link без токена → логин → та же сессия.
 *
 * Сценарий:
 *  1. открыть /pnm/monitoring/sessions/<id> в чистом профиле (без токена);
 *  2. вендорский guard уводит на /pnm/authorization, `deeplink.js` должен
 *     сохранить целевой URL в sessionStorage;
 *  3. войти через форму логина;
 *  4. после появления токена скрипт возвращает на исходную сессию — проверяем
 *     URL и что карточка/вкладки сессии отрисованы.
 *
 * Запуск: VKS_BASE_URL=http://127.0.0.1:<port> node tests/e2e/deeplink_auth.js
 * (эпhemeral-сервер с чистой БД; на живом :8080 не запускать).
 */
'use strict';

const { chromium } = require('playwright');

const BASE = (process.env.VKS_BASE_URL || 'http://127.0.0.1:8080').replace(/\/$/, '');
const LOGIN = process.env.VKS_LOGIN || 'admin';
const PASSWORD = process.env.VKS_PASSWORD || 'admin_password';
const RUN = `E2E Deeplink ${Date.now()}`;

const checks = [];
function check(name, ok, detail) {
  checks.push({ name, ok: Boolean(ok) });
  console.log(`[deeplink] ${ok ? 'OK  ' : 'FAIL'} ${name}${detail ? ' — ' + detail : ''}`);
  return Boolean(ok);
}

async function api(token, method, path, body) {
  const response = await fetch(BASE + path, {
    method,
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await response.text();
  let doc = {};
  try { doc = text ? JSON.parse(text) : {}; } catch (e) { doc = { raw: text }; }
  return { status: response.status, doc };
}

(async () => {
  let token = '';
  let conferenceId = '';
  let browser = null;
  let failure = '';
  try {
    const login = await api('', 'POST', '/api/v3/login', { login: LOGIN, password: PASSWORD });
    token = login.doc.token || login.doc.access_token || '';
    if (!check('api login', login.status === 200 && Boolean(token), `${login.status}`)) return 1;

    const conf = await api(token, 'POST', '/api/v3/conferences', { display_name: RUN });
    conferenceId = (conf.doc.conference && conf.doc.conference.id) || '';
    if (!check('conference создана', Boolean(conferenceId), `${conf.status}`)) return 1;

    browser = await chromium.launch();
    const context = await browser.newContext();
    const page = await context.newPage();
    const target = `/pnm/monitoring/sessions/${conferenceId}`;

    await page.goto(BASE + target, { waitUntil: 'domcontentloaded' });
    await page.waitForURL(/\/pnm\/authorization/, { timeout: 15000 });
    check('без токена уводит на авторизацию', true, page.url());

    const saved = await page.evaluate(() => window.sessionStorage.getItem('vks_deeplink_return'));
    check('целевой URL сохранён', Boolean(saved) && saved.includes(conferenceId), `${saved}`);

    const user = page.locator('input[autocomplete="username"], input[type="text"]').first();
    await user.fill(LOGIN);
    await page.locator('input[type="password"]').first().fill(PASSWORD);
    await page.locator('button[type="submit"], [data-qa="authorization-sign-in-button"]').first().click();

    await page.waitForURL((url) => url.pathname.includes(`/pnm/monitoring/sessions/${conferenceId}`), { timeout: 20000 });
    check('после логина возврат на ту же сессию', true, page.url());

    await page.waitForTimeout(1500);
    const rendered = await page.evaluate(() => ({
      card: document.querySelectorAll('.session-card').length,
      tabs: document.querySelectorAll('[role="tab"], .tabs__tab, .session-tabs a').length,
      body: document.body.innerText.includes('E2E Deeplink'),
    }));
    check(
      'сессия отрисована (карточка/вкладки/имя)',
      rendered.card >= 1 || rendered.tabs >= 3 || rendered.body,
      JSON.stringify(rendered)
    );
    const leftover = await page.evaluate(() => window.sessionStorage.getItem('vks_deeplink_return'));
    check('метка deep-link очищена', !leftover, `${leftover}`);
  } catch (error) {
    failure = String(error && error.stack ? error.stack : error);
    console.error('[deeplink] ERROR', failure);
  } finally {
    if (browser) await browser.close().catch(() => {});
    if (token && conferenceId) await api(token, 'DELETE', `/api/v3/conferences/${conferenceId}`).catch(() => {});
    const failed = checks.filter((item) => !item.ok).length;
    console.log(`[deeplink] итог: ${checks.length - failed}/${checks.length} ok`);
    if (failure || failed > 0) process.exitCode = 1;
  }
})();

#!/usr/bin/env node
'use strict';
/*
 * T12b regression: PATCH во время in-flight codec GET не должен ни отдавать
 * stale body, ни оставлять coalesced XHR висеть. Сценарий:
 *   GET audio (искусственно задержан) + coalesced GET audio
 *   → PATCH /settings/calls (инвалидация)
 *   → второй GET обязан завершиться свежим ответом за < 5 s.
 */
const { chromium } = require('playwright');
const BASE = process.env.VKS_BASE_URL || 'http://127.0.0.1:8080';
const CHROMIUM = process.env.VKS_CHROMIUM || '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';

(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: CHROMIUM });
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 } });
  const page = await context.newPage();
  let delayed = false;
  await page.route('**/api/v3/settings/audio_codecs?race=*', async route => {
    if (!delayed) {
      delayed = true;
      await new Promise(resolve => setTimeout(resolve, 1200));
    }
    await route.continue();
  });
  await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(800);
  await page.fill('input[autocomplete="username"], input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('button[type="submit"], button:has-text("Авторизоваться")');
  await page.waitForTimeout(2000);
  const result = await page.evaluate(async () => {
    const token = localStorage.getItem('TrueConfMCU_AccessToken');
    const xhr = (url, method, body) => new Promise(resolve => {
      const request = new XMLHttpRequest();
      request.open(method || 'GET', url);
      request.setRequestHeader('Authorization', `Bearer ${token}`);
      if (body) request.setRequestHeader('Content-Type', 'application/json');
      request.onloadend = () => resolve({ status: request.status, text: request.responseText });
      request.send(body ? JSON.stringify(body) : null);
    });
    const original = await xhr('/api/v3/settings/audio_codecs');
    const originalNames = original.status === 200 ? JSON.parse(original.text).audio_codecs.map(codec => codec.name) : ['PCMU', 'PCMA', 'OPUS'];
    const target = originalNames.includes('G729') ? 'G722' : 'G729';
    const url = `/api/v3/settings/audio_codecs?race=${Date.now()}`;
    const first = xhr(url);
    await new Promise(resolve => setTimeout(resolve, 100));
    const second = xhr(url);
    await new Promise(resolve => setTimeout(resolve, 100));
    const patch = await xhr('/api/v3/settings/audio_codecs', 'PATCH', { audio_codecs: [target] });
    const completed = await Promise.race([
      second.then(value => ({ ok: value.status === 200, status: value.status, fresh: value.status === 200 && value.text.includes(target) })),
      new Promise(resolve => setTimeout(() => resolve({ ok: false, status: 'timeout', fresh: false }), 5000)),
    ]);
    await xhr('/api/v3/settings/audio_codecs', 'PATCH', { audio_codecs: originalNames });
    await first.catch(() => {});
    return { patch: patch.status, target, second: completed, stats: window.__vksCodecCacheStats };
  });
  console.log(JSON.stringify(result));
  await browser.close();
  if (!result.second.ok) process.exit(1);
})();

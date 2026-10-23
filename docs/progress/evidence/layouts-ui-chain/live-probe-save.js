'use strict';
const fs = require('fs');
const { chromium } = require('playwright');
const BASE = 'http://127.0.0.1:8080';
const URL = '/pnm/monitoring/conferences/49b599d7-b5cd-45a7-8999-47a28c453a92/layouts/7e6d4753-d21a-4955-ad6b-0c17de218427';
const CHROMIUM = '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: CHROMIUM, args: ['--no-sandbox'] });
  const page = await (await browser.newContext({ viewport: { width: 1600, height: 1200 } })).newPage();
  const errs = [], net = [];
  page.on('pageerror', e => errs.push(String(e && e.stack || e)));
  page.on('console', m => { if (m.type() === 'error') errs.push('console: ' + m.text()); });
  page.on('request', r => {
    const u = r.url();
    if (u.includes('/video_layouts') && (r.method() === 'PATCH' || r.method() === 'POST')) {
      net.push({ method: r.method(), url: u.replace(BASE, ''), body: r.postData() });
    }
  });
  await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'networkidle' });
  await page.fill('input[autocomplete="username"], input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('button[type="submit"], button:has-text("Авторизоваться")');
  await page.waitForTimeout(2500);
  await page.goto(BASE + URL, { waitUntil: 'networkidle' });
  await page.waitForTimeout(2500);
  const manual = await page.$('text=Ручная настройка');
  if (manual) { await manual.click().catch(() => {}); await page.waitForTimeout(1800); }
  const buttons = await page.evaluate(() => Array.from(document.querySelectorAll('button')).map(b => (b.textContent || '').trim()).filter(Boolean));
  const card = await page.$('[data-codian-id="codian_07"]');
  if (card) { await card.click().catch(() => {}); await page.waitForTimeout(1000); }
  const clicked = [];
  for (const label of ['Сохранить', 'Применить', 'Создать', 'Save', 'Применить раскладку']) {
    const b = await page.$(`button:has-text("${label}")`);
    if (b && await b.isEnabled().catch(() => false)) {
      await b.click().catch(() => {});
      clicked.push(label);
      await page.waitForTimeout(1800);
      if (net.length) break;
    }
  }
  const after = await page.evaluate(() => {
    const items = Array.from(document.querySelectorAll('[data-codian-id]'));
    const rows = new Set(items.map(e => Math.round(e.getBoundingClientRect().top)));
    return { items: items.length, rows: rows.size, perRow: rows.size ? Math.round(items.length / rows.size) : 0 };
  });
  const out = { buttons: buttons.slice(0, 40), clicked, net, after, errors: errs };
  fs.writeFileSync('final-report2.json', JSON.stringify(out, null, 2));
  console.log(JSON.stringify(out, null, 2));
  // скриншот всей страницы и области палитры
  await page.screenshot({ path: 'final-02-live-page.png', fullPage: false });
  const section = await page.$('[data-vks-codian-group]');
  if (section) {
    await section.scrollIntoViewIfNeeded();
    await page.waitForTimeout(400);
    await section.screenshot({ path: 'final-03-codian-group.png' }).catch(() => {});
  }
  await browser.close();
})().catch(e => { console.error('PROBE FAIL', e); process.exit(1); });

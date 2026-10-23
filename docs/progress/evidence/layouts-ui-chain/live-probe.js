'use strict';
const fs = require('fs');
const { chromium } = require('playwright');
const BASE = 'http://127.0.0.1:8080';
const CID = '49b599d7-b5cd-45a7-8999-47a28c453a92';
const LID = '7e6d4753-d21a-4955-ad6b-0c17de218427';
const CHROMIUM = '/home/pro/.cache/ms-playwright/chromium-1243/chrome-linux64/chrome';
(async () => {
  const browser = await chromium.launch({ headless: true, executablePath: CHROMIUM, args: ['--no-sandbox'] });
  const page = await (await browser.newContext({ viewport: { width: 1600, height: 1200 } })).newPage();
  const errs = [];
  page.on('pageerror', e => errs.push(String(e && e.stack || e)));
  page.on('console', m => { if (m.type() === 'error') errs.push('console: ' + m.text()); });
  let applied = null;
  await page.route(/\/api\/v3\/ui\/conferences\/[^/]+\/video_layouts\/[^/]+$/, async route => {
    if (route.request().method() !== 'PATCH') { await route.continue(); return; }
    applied = JSON.parse(route.request().postData() || '{}');
    await route.continue();
  });
  await page.goto(`${BASE}/pnm/authorization`, { waitUntil: 'networkidle' });
  await page.fill('input[autocomplete="username"], input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('button[type="submit"], button:has-text("Авторизоваться")');
  await page.waitForTimeout(2500);
  const routes = [
    `/pnm/monitoring/conferences/${CID}/layouts/${LID}`,
    `/pnm/monitoring/conferences/${CID}/layouts`,
    `/pnm/monitoring/sessions/${CID}/layouts/${LID}`,
  ];
  let used = null;
  for (const r of routes) {
    await page.goto(BASE + r, { waitUntil: 'networkidle' }).catch(() => {});
    await page.waitForTimeout(1800);
    const has = await page.evaluate(() => !!document.querySelector('.manual-layout-configurator, .base-layout-configurator'));
    if (has) { used = r; break; }
  }
  // переключение в ручной режим, если палитры нет
  const manual = await page.$('text=Ручная настройка');
  if (manual) { await manual.click().catch(() => {}); await page.waitForTimeout(1500); }
  await page.waitForTimeout(1200);
  const report = await page.evaluate(() => {
    const items = Array.from(document.querySelectorAll('[data-codian-id]'));
    const section = document.querySelector('[data-vks-codian-group]');
    const first = items[0] && items[0].querySelector('svg');
    const rects = items.slice(0, 4).map(it => {
      const svg = it.querySelector('svg');
      return svg ? svg.getAttribute('viewBox') : null;
    });
    const r4 = (() => {
      const it = document.querySelector('[data-codian-id="codian_04"] svg');
      if (!it) return [];
      return Array.from(it.querySelectorAll('rect')).slice(0, 5).map(r => [Number(r.getAttribute('x')), Number(r.getAttribute('width'))]);
    })();
    const rows = new Set(items.map(e => Math.round(e.getBoundingClientRect().top)));
    return {
      url: location.pathname,
      items: items.length,
      unique: new Set(items.map(e => e.getAttribute('data-codian-id'))).size,
      rows: rows.size,
      perRow: rows.size ? Math.round(items.length / rows.size) : 0,
      sectionParent: section && section.parentElement.className,
      firstTitle: items[0] && items[0].getAttribute('title'),
      viewBox_01: first && first.getAttribute('viewBox'),
      viewBoxes_sample: rects,
      rects_04: r4,
      panelWidth: (() => { const p = document.querySelector('.base-layout-configurator-side-panel'); return p ? Math.round(p.getBoundingClientRect().width) : null; })(),
    };
  });
  // выбор codian_07 и перехват payload «Применить/Сохранить»
  const card = await page.$('[data-codian-id="codian_07"]');
  if (card) {
    await card.click().catch(() => {});
    await page.waitForTimeout(900);
    const active = await page.evaluate(() => !!document.querySelector('[data-codian-id="codian_07"].layout-scheme-selector-item--active'));
    report.activeAfterClick = active;
    const btn = await page.$('button:has-text("Сохранить")') || await page.$('button:has-text("Применить")');
    if (btn) { await btn.click().catch(() => {}); await page.waitForTimeout(1500); }
    report.appliedPayload = applied;
  }
  report.errors = errs;
  await page.screenshot({ path: 'final-01-codian-palette.png', fullPage: false });
  fs.writeFileSync('final-report.json', JSON.stringify({ usedRoute: used, report }, null, 2));
  console.log(JSON.stringify({ usedRoute: used, report }, null, 2));
  await browser.close();
})().catch(e => { console.error('PROBE FAIL', e); process.exit(1); });

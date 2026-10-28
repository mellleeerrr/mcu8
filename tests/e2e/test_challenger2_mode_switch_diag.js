const { chromium } = require('playwright');

(async () => {
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: 'dark'
  });

  const page = await context.newPage();

  page.on('pageerror', err => {
    console.log('>>> [PAGEERROR FULL DETAILS]:', err.message);
    if (err.stack) console.log(err.stack);
  });

  page.on('console', msg => {
    if (msg.type() === 'error') {
      console.log('>>> [CONSOLE ERROR]:', msg.text());
    }
  });

  // Login
  await page.goto('http://127.0.0.1:8080/pnm/authorization', { waitUntil: 'domcontentloaded' });
  await page.fill('input[type="text"]', 'admin');
  await page.fill('input[type="password"]', 'admin_password');
  await page.click('[data-qa="authorization-sign-in-button"]');
  await page.waitForTimeout(1000);

  await page.goto('http://127.0.0.1:8080/pnm/monitoring/conferences', { waitUntil: 'domcontentloaded' });
  await page.waitForTimeout(1000);

  await page.locator('.main-list-item').first().click();
  await page.waitForTimeout(800);
  await page.locator('.tc-tab, .mdc-tab').filter({ hasText: '?????????' }).first().click();
  await page.waitForTimeout(800);

  const addLayoutBtn = page.locator('[data-qa="conference-add-layout-button"]');
  if (await addLayoutBtn.count() > 0) {
    await addLayoutBtn.click();
  } else {
    await page.locator('.conference-layouts-item').first().click();
  }
  await page.waitForTimeout(1200);

  const manualSelector = page.locator('[data-qa="layout-mode-selector-manual"]');
  const ctorSelector = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');

  console.log('Starting 10 mode switches to observe page errors...');
  for (let i = 1; i <= 10; i++) {
    console.log(`Switch ${i}: to Manual`);
    await manualSelector.click();
    await page.waitForTimeout(500);

    console.log(`Switch ${i}: to Constructor`);
    await ctorSelector.click();
    await page.waitForTimeout(500);
  }

  console.log('Done 10 switches.');
  await browser.close();
})();

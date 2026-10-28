const { chromium } = require('playwright');

(async () => {
  console.log('>>> [Challenger 2 Precise Analysis] Starting...');

  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: 1920, height: 1080 },
    colorScheme: 'dark'
  });

  await context.addInitScript(() => {
    try { localStorage.setItem('VKS_PNM_THEME', 'dark'); } catch (_) {}
  });

  const page = await context.newPage();

  const errors = [];
  page.on('pageerror', err => {
    const info = {
      message: err.message,
      name: err.name,
      stack: err.stack
    };
    console.error('>>> [CAUGHT PAGEERROR]', JSON.stringify(info, null, 2));
    errors.push(info);
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

  // Use selector or regex for layouts tab to avoid encoding issues
  const layoutsTab = page.locator('.tc-tab, .mdc-tab').filter({ hasText: /Раскладки/ }).first();
  await layoutsTab.click();
  await page.waitForTimeout(800);

  const addLayoutBtn = page.locator('[data-qa="conference-add-layout-button"]');
  if (await addLayoutBtn.count() > 0) {
    await addLayoutBtn.click();
  } else {
    await page.locator('.conference-layouts-item').first().click();
  }
  await page.waitForTimeout(1200);

  // Switch to constructor
  const ctorTab = page.locator('[data-vks-constructor-tile="1"], [data-qa="layout-mode-selector-constructor"]');
  await ctorTab.click();
  await page.waitForTimeout(800);

  // Test resolutions
  const resolutions = [
    { width: 1920, height: 1080 },
    { width: 1366, height: 768 },
    { width: 1280, height: 720 }
  ];

  for (const res of resolutions) {
    await page.setViewportSize({ width: res.width, height: res.height });
    await page.waitForTimeout(600);

    const data = await page.evaluate(() => {
      const sidePanel = document.querySelector('.base-layout-configurator-side-panel');
      const ctorPanel = document.querySelector('[data-vks-constructor="1"]');
      const modesContainer = document.querySelector('[data-vks-constructor-modes="1"]');
      const counter = document.querySelector('[data-vks-counter="1"]');
      const firstRow = modesContainer ? modesContainer.querySelector('.layout-scheme-selector__row') : null;
      const firstGroup = firstRow ? firstRow.querySelector('.layout-scheme-selector__group') : null;
      const cards = firstGroup ? Array.from(firstGroup.querySelectorAll('.layout-scheme-selector-item')) : [];

      function metrics(el) {
        if (!el) return null;
        const r = el.getBoundingClientRect();
        return {
          scrollWidth: el.scrollWidth,
          clientWidth: el.clientWidth,
          offsetWidth: el.offsetWidth,
          rectWidth: r.width,
          overflow: el.scrollWidth > el.clientWidth + 1
        };
      }

      return {
        sidePanel: metrics(sidePanel),
        ctorPanel: metrics(ctorPanel),
        counter: metrics(counter),
        modesContainer: metrics(modesContainer),
        firstRow: metrics(firstRow),
        firstGroup: metrics(firstGroup),
        cardWidths: cards.map(c => c.getBoundingClientRect().width)
      };
    });

    console.log(`\n=== METRICS AT ${res.width}x${res.height} ===`);
    console.log(JSON.stringify(data, null, 2));
  }

  // Restore 1080p
  await page.setViewportSize({ width: 1920, height: 1080 });
  await page.waitForTimeout(400);

  // Test 6 rapid switches to isolate the page error
  console.log('\n=== TESTING MODE SWITCHES ===');
  const manualTab = page.locator('[data-qa="layout-mode-selector-manual"]');

  for (let cycle = 1; cycle <= 6; cycle++) {
    console.log(`Cycle ${cycle}: to Manual...`);
    await manualTab.click();
    await page.waitForTimeout(600);

    console.log(`Cycle ${cycle}: to Constructor...`);
    await ctorTab.click();
    await page.waitForTimeout(600);
  }

  console.log(`\n=== ERROR SUMMARY: Total page errors caught = ${errors.length} ===`);
  await browser.close();
})();
